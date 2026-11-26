use super::*;

/// One puppet's copy of the cache's mesh layout in the deform atlas, so
/// puppets sharing a [`crate::RenderCache`] do not overwrite each other's
/// deforms within one submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeformSet(pub(super) u32);

impl DeformSet {
    pub const FIRST: Self = Self(0);

    #[doc(hidden)]
    pub fn index(self) -> u32 {
        self.0
    }
}

impl Default for DeformSet {
    fn default() -> Self {
        Self::FIRST
    }
}

#[derive(Default)]
pub(super) struct DeformSetState {
    /// Mesh slots with active deform data, keyed to the `DeformStack`
    /// generation that produced them. None rechecks bytes after a model rebake.
    pub(super) uploaded: DenseMap<Option<u64>>,
    pub(super) still_active: DenseMap<()>,
    /// Per-mesh max absolute deform component. Deform is added in local
    /// space, so expanding the local AABB by this bounds the deformed geometry.
    pub(super) max: DenseMap<f32>,
    /// The next upload zeroes the whole slice first, so a previous tenant's
    /// bytes never draw.
    pub(super) needs_clear: bool,
    pub(super) live: bool,
}

impl DeformSetState {
    pub(super) fn live() -> Self {
        Self {
            live: true,
            ..Self::default()
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.uploaded.clear();
        self.still_active.clear();
        self.max.clear();
        self.needs_clear = true;
    }
}

impl WgpuRenderer {
    /// The mirror is a byte-exact copy of the atlas, so re-uploading it keeps
    /// every set's residency memo true. Only legal between frames.
    pub(super) fn ensure_deform_buffer_capacity(&mut self, needed: u64) {
        let needed = needed.max(8);
        if needed <= self.deform_buffer_capacity {
            return;
        }
        self.note_realloc(RenderBuffer::Deform);
        self.deform_buffer_capacity = needed.next_power_of_two();
        self.deform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Deform Buffer Atlas"),
            size: self.deform_buffer_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !self.deform_upload_mirror.is_empty() {
            self.note_queue_write(RenderBuffer::Deform);
            self.queue
                .write_buffer(&self.deform_buffer, 0, &self.deform_upload_mirror);
        }
    }

    pub(super) fn reset_deform_buffer_layout(&mut self) {
        self.deform_buffer_len = 0;
        self.deform_upload_mirror.clear();
        self.deform_inactive_scratch.clear();
        // Every set's bytes moved: the mesh slots the offsets were derived
        // from are all being handed out again.
        for set in &mut self.deform_sets {
            set.invalidate();
        }
    }

    pub(super) fn reserve_deform_buffer(&mut self, size: u64) -> u64 {
        let offset = self.deform_buffer_len;
        let end = offset + size;
        self.ensure_deform_buffer_capacity(end);
        self.deform_buffer_len = end;
        if self.deform_upload_mirror.len() < end as usize {
            self.deform_upload_mirror.resize(end as usize, 0);
        }
        offset
    }

    /// Padded so every set's base stays 16-byte aligned (`set_vertex_buffer`
    /// needs 4).
    pub(super) fn deform_stride(&self) -> u64 {
        self.deform_buffer_len.next_multiple_of(16)
    }

    /// Where `set`'s copy of the mesh layout starts. Derived rather than
    /// stored, so a rebuild that changes the stride moves every set at once.
    pub(super) fn deform_base(&self, set: DeformSet) -> u64 {
        u64::from(set.0) * self.deform_stride()
    }

    /// Take a deform set for one more puppet of this renderer's model. Release
    /// it with [`Self::release_deform_set`] so the index is reused.
    pub fn acquire_deform_set(&mut self) -> DeformSet {
        let index = match self.deform_free.pop() {
            Some(index) => index,
            None => {
                self.deform_sets.push(DeformSetState::default());
                (self.deform_sets.len() - 1) as u32
            }
        };
        self.deform_sets[index as usize] = DeformSetState::live();
        self.deform_sets[index as usize].needs_clear = true;
        DeformSet(index)
    }

    /// [`DeformSet::FIRST`] is never released.
    pub fn release_deform_set(&mut self, set: DeformSet) {
        if set == DeformSet::FIRST {
            return;
        }
        let Some(state) = self.deform_sets.get_mut(set.0 as usize) else {
            return;
        };
        if !state.live {
            return;
        }
        *state = DeformSetState::default();
        self.deform_free.push(set.0);
    }

    #[doc(hidden)]
    pub fn live_deform_sets(&self) -> usize {
        self.deform_sets.iter().filter(|s| s.live).count()
    }

    /// Runs at upload time because a set's base depends on the mesh layout,
    /// which a rebuild can still change.
    pub(super) fn ensure_deform_sets_sized(&mut self) {
        let needed = self.deform_stride() * self.deform_sets.len() as u64;
        if self.deform_upload_mirror.len() < needed as usize {
            self.deform_upload_mirror.resize(needed as usize, 0);
        }
        self.ensure_deform_buffer_capacity(needed);
    }

    pub(super) fn deform_slice(&self, mesh: &MeshBuffer) -> wgpu::BufferSlice<'_> {
        let start = self.deform_recording_base + mesh.deform_offset;
        self.deform_buffer.slice(start..start + mesh.deform_size)
    }

    /// Meshes that went inactive are zeroed, otherwise the vertex shader keeps
    /// reading a stale config's offsets.
    pub(crate) fn upload_deforms<'a>(
        &mut self,
        set: DeformSet,
        active: impl Iterator<Item = (u32, u64, &'a [glam::Vec2])>,
    ) {
        // Coalesce dirty ranges only across gaps this small: below it,
        // one write over the untouched bytes beats a second write_buffer's
        // fixed overhead; above it, the copied span is pure waste.
        const DEFORM_MERGE_GAP: u64 = 4096;

        self.ensure_deform_sets_sized();
        let base = self.deform_base(set);
        let stride = self.deform_stride();
        let Some(mut state) = self
            .deform_sets
            .get_mut(set.0 as usize)
            .filter(|state| state.live)
            .map(std::mem::take)
        else {
            tracing::warn!(set = set.0, "upload_deforms: no such deform set");
            return;
        };
        state.still_active.clear();
        let mut ranges = std::mem::take(&mut self.deform_write_ranges);
        ranges.clear();
        if state.needs_clear {
            state.needs_clear = false;
            let end = (base + stride).min(self.deform_upload_mirror.len() as u64);
            if end > base {
                self.deform_upload_mirror[base as usize..end as usize].fill(0);
                ranges.push((base, end));
            }
        }
        for (mesh_id, generation, combined) in active {
            let Some(buf) = self.mesh_buffers.get(mesh_id as usize) else {
                continue;
            };
            if combined.len() != buf.vert_count as usize {
                tracing::warn!(
                    node = mesh_id,
                    stack_len = combined.len(),
                    buffer_len = buf.vert_count,
                    "upload_deforms: deform length mismatch, skipping",
                );
                continue;
            }
            if state.uploaded.get(mesh_id as usize).copied() == Some(Some(generation)) {
                state.still_active.insert(mesh_id as usize, ());
                continue;
            }
            let bytes: &[u8] = bytemuck::cast_slice(combined);
            let start = (base + buf.deform_offset) as usize;
            let end = start + bytes.len();
            if self.deform_upload_mirror[start..end] != *bytes {
                self.deform_upload_mirror[start..end].copy_from_slice(bytes);
                ranges.push((start as u64, end as u64));
            }
            state.uploaded.insert(mesh_id as usize, Some(generation));
            state.still_active.insert(mesh_id as usize, ());
            let max_abs = combined
                .iter()
                .fold(0.0f32, |m, v| m.max(v.x.abs()).max(v.y.abs()));
            state.max.insert(mesh_id as usize, max_abs);
        }

        self.deform_inactive_scratch.clear();
        for i in state.uploaded.keys() {
            if !state.still_active.contains(i) {
                self.deform_inactive_scratch.push(i as u32);
            }
        }
        for i in 0..self.deform_inactive_scratch.len() {
            let mesh_id = self.deform_inactive_scratch[i];
            state.uploaded.remove(mesh_id as usize);
            state.max.remove(mesh_id as usize);
            let Some(buf) = self.mesh_buffers.get(mesh_id as usize) else {
                continue;
            };
            let start = (base + buf.deform_offset) as usize;
            let end = start + buf.deform_size as usize;
            self.deform_upload_mirror[start..end].fill(0);
            ranges.push((start as u64, end as u64));
        }

        // pending_deform_bytes counts merged-run lengths, not the min..max span.
        ranges.sort_unstable_by_key(|&(start, _)| start);
        let mut written = 0u64;
        let mut run: Option<(u64, u64)> = None;
        for &(start, end) in &ranges {
            match run {
                Some((run_start, run_end)) if start <= run_end + DEFORM_MERGE_GAP => {
                    run = Some((run_start, run_end.max(end)));
                }
                Some((run_start, run_end)) => {
                    self.note_queue_write(RenderBuffer::Deform);
                    self.queue.write_buffer(
                        &self.deform_buffer,
                        run_start,
                        &self.deform_upload_mirror[run_start as usize..run_end as usize],
                    );
                    written += run_end - run_start;
                    run = Some((start, end));
                }
                None => run = Some((start, end)),
            }
        }
        if let Some((run_start, run_end)) = run {
            self.note_queue_write(RenderBuffer::Deform);
            self.queue.write_buffer(
                &self.deform_buffer,
                run_start,
                &self.deform_upload_mirror[run_start as usize..run_end as usize],
            );
            written += run_end - run_start;
        }
        // Accumulated: N puppets each upload before the frame that draws them all.
        self.pending_deform_bytes += written;

        ranges.clear();
        self.deform_write_ranges = ranges;
        if let Some(slot) = self.deform_sets.get_mut(set.0 as usize) {
            *slot = state;
        }
    }
}
