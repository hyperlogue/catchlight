//! Draws a `RenderList` into a frame the renderer owns, or into a
//! caller-supplied command encoder.

use catchlight_core::{BlendMode, DecodedTexture};
use smallvec::SmallVec;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use thiserror::Error;
use wgpu::util::DeviceExt;

mod arena;
mod blend;
mod bounds;
mod composite;
mod deform;
mod frame;
mod masks;
mod parts;
mod pipelines;
mod resources;
mod targets;
#[cfg(test)]
mod tests;
mod wire;

use blend::*;
use bounds::*;
use composite::*;
use deform::*;
use frame::*;
use masks::*;
use parts::*;
use resources::*;
use targets::*;
use wire::*;

use arena::FrameArena;
pub use pipelines::Pipelines;
use pipelines::{MaskPipelines, ShaderBlend};
pub use {
    deform::DeformSet,
    frame::{CAMERA_RING_SLOTS, Frame, Recorded, Submission},
    targets::{FrameTargets, RenderTarget},
};

/// A mesh `upload_mesh` refuses. A model's own meshes are validated on
/// load, so a rebuild meeting one skips the part rather than failing.
#[derive(Debug, Error)]
pub(crate) enum MeshError {
    #[error("mesh {mesh_id}: vertices.len() ({vertices}) != uvs.len() ({uvs})")]
    VertexUvMismatch {
        mesh_id: u32,
        vertices: usize,
        uvs: usize,
    },

    #[error("mesh {mesh_id}: index {index} out of bounds ({vertices} vertices)")]
    IndexOutOfBounds {
        mesh_id: u32,
        index: u32,
        vertices: usize,
    },
}

#[derive(Debug, Error)]
pub enum RendererError {
    #[error("texture dimensions {width}x{height} are outside the supported range 1..={limit}")]
    TextureDimensionsOutOfRange { width: u32, height: u32, limit: u32 },

    #[error("texture {width}x{height} RGBA8 byte length overflows the host address space")]
    TextureByteLengthOverflow { width: u32, height: u32 },

    #[error(
        "texture {width}x{height} RGBA8 byte length mismatch: expected {expected}, got {actual}"
    )]
    TextureByteLengthMismatch {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },

    #[error(
        "this submission has already issued its {limit} camera views; \
         one submission token belongs to one submit"
    )]
    CameraViewsExhausted { limit: u32 },

    #[error("this submission token was made by a different renderer")]
    ForeignSubmission,

    #[error("preparing a model's textures: {0}")]
    Texture(catchlight_core::TextureError),

    #[error("preparing a model's textures: {0}")]
    LoadLimit(catchlight_core::LoadLimitError),

    #[error("the surface has no format or alpha mode in common with the adapter")]
    SurfaceUnsupported,

    #[error("no usable GPU adapter: {0}")]
    Adapter(wgpu::RequestAdapterError),

    #[error("requesting the GPU device: {0}")]
    Device(wgpu::RequestDeviceError),

    #[error("a {width}x{height} texture cannot be read back")]
    ReadbackSize { width: u32, height: u32 },

    #[error("a {0:?} texture cannot be read back as RGBA8")]
    ReadbackFormat(wgpu::TextureFormat),

    #[error("mapping the readback buffer: {0}")]
    ReadbackMap(wgpu::BufferAsyncError),

    #[error("the device dropped the readback before it completed")]
    ReadbackAbandoned,

    #[error("waiting for the GPU: {0}")]
    Poll(wgpu::PollError),
}

// By hand rather than `#[from]`, which would also report the wrapped error
// as the source that the message already prints.
macro_rules! from_wrapped {
    ($($source:ty => $variant:ident),* $(,)?) => {
        $(impl From<$source> for RendererError {
            fn from(error: $source) -> Self {
                Self::$variant(error)
            }
        })*
    };
}

from_wrapped! {
    catchlight_core::TextureError => Texture,
    catchlight_core::LoadLimitError => LoadLimit,
    wgpu::RequestAdapterError => Adapter,
    wgpu::RequestDeviceError => Device,
    wgpu::BufferAsyncError => ReadbackMap,
    wgpu::PollError => Poll,
}

pub type RendererResult<T> = Result<T, RendererError>;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RenderStats {
    pub drawn_parts: u32,
    pub drawn_composites: u32,
    pub skipped_missing_mesh: u32,
    /// Counts visible parts only; offscreen parts are culled first.
    pub skipped_missing_texture: u32,
    pub skipped_missing_mask_mesh: u32,
    pub skipped_missing_mask_texture: u32,
    pub texture_binds: u32,
    pub pipeline_swaps: u32,
    /// Includes mask-write, blit, and no-draw clear passes.
    pub render_passes: u32,
    pub total_draw_calls: u32,
    pub instance_bytes_uploaded: u64,
    /// Zero when every resident deform generation already matched.
    pub deform_bytes_uploaded: u64,
    pub composite_slots_used: u32,
}

/// Per-frame resource-lifecycle counters, reset at every frame entry.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    /// Excludes [`Recorded::submit`]; non-zero only when an upload (mip
    /// generation) submitted inside the frame.
    pub queue_submits: u32,
    pub queue_writes: u32,
    /// At most 1: `write_buffer` batches at submit start, so a second write to
    /// a live offset would win for every draw that reads it.
    pub instance_buffer_writes: u32,
    /// At most 1, as `instance_buffer_writes`.
    pub part_uniform_buffer_writes: u32,
    pub camera_buffer_writes: u32,
    /// Reads 0: deforms upload outside the frame.
    pub deform_buffer_writes: u32,
    pub instance_buffer_reallocs: u32,
    pub part_uniform_buffer_reallocs: u32,
    pub deform_buffer_reallocs: u32,
    /// Always 0: growing mid-frame strands already-recorded passes on the
    /// freed buffer.
    pub late_buffer_reallocs: u32,
    pub instance_slots_budgeted: u32,
    /// Never above `instance_slots_budgeted`.
    pub instance_slots_reserved: u32,
    pub instance_slots_written: u32,
    pub instance_bytes_written: u64,
    pub part_uniform_slots_budgeted: u32,
    /// Never above `part_uniform_slots_budgeted`.
    pub part_uniform_writes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderBuffer {
    Instance,
    PartUniform,
    Camera,
    Deform,
}

/// Dense `Vec<Option<V>>` keyed by small ids, avoiding SipHash on the
/// per-drawable hot path.
#[derive(Default)]
struct DenseMap<V> {
    slots: Vec<Option<V>>,
}

impl<V> DenseMap<V> {
    fn new() -> Self {
        Self { slots: Vec::new() }
    }

    fn get(&self, index: usize) -> Option<&V> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    fn contains(&self, index: usize) -> bool {
        matches!(self.slots.get(index), Some(Some(_)))
    }

    fn insert(&mut self, index: usize, value: V) {
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || None);
        }
        self.slots[index] = Some(value);
    }

    fn remove(&mut self, index: usize) {
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = None;
        }
    }

    fn take(&mut self, index: usize) -> Option<V> {
        self.slots.get_mut(index).and_then(Option::take)
    }

    fn clear(&mut self) {
        self.slots.clear();
    }

    fn live(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    fn keys(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|_| i))
    }
}

static NEXT_RENDERER_ID: AtomicU64 = AtomicU64::new(0);

pub struct WgpuRenderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub(crate) shared: std::sync::Arc<Pipelines>,
    id: u64,
    part_uniforms: FrameArena,
    part_uniform_bind_group: wgpu::BindGroup,
    mesh_buffers: DenseMap<MeshBuffer>,
    textures: DenseMap<Texture>,
    instances: FrameArena,
    deform_buffer: wgpu::Buffer,
    deform_buffer_capacity: u64,
    deform_buffer_len: u64,
    deform_upload_mirror: Vec<u8>,
    // Folded into the next frame's stats; the upload runs before the frame
    // resets them.
    pending_deform_bytes: u64,
    // Atomic so `&self` blit helpers can count too.
    frame_render_passes: AtomicU32,
    frame_draw_calls: AtomicU32,
    // Owned frames use slot 0; `render_into` spends the ring from the
    // caller's `Submission`.
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    camera_offset: u32,
    current_stats: RenderStats,
    frame_stats: FrameStats,
    // Any buffer recreation after this is set trips a debug assertion.
    frame_sizing_closed: bool,
    /// None without `Features::TIMESTAMP_QUERY`; CPU `tracing` spans only.
    gpu_profiler: Option<wgpu_profiler::GpuProfiler>,
    /// Indexed by set; slot 0 is [`DeformSet::FIRST`].
    deform_sets: Vec<DeformSetState>,
    deform_free: Vec<u32>,
    deform_inactive_scratch: Vec<u32>,
    // Sorted and coalesced so far-apart meshes don't force one write over
    // the untouched span between them.
    deform_write_ranges: Vec<(u64, u64)>,
    /// Frame-scoped: `record_frame` points these at each list before recording it.
    deform_recording_set: DeformSet,
    deform_recording_base: u64,
    /// CPU copy kept so per-part bounds can project to clip space.
    camera_view_proj: glam::Mat4,
    draw_filter_scratch: Vec<usize>,
    instance_scratch: Vec<InstanceRaw>,
    pending_parts_scratch: Vec<PreparedPartDraw>,
    /// A drawable by the composite holding it (`None` for the root list)
    /// and its index there.
    drawable_walk_scratch: Vec<(Option<u32>, usize)>,
    mask_source_ids_scratch: Vec<u32>,
    uniform_offset_scratch: Vec<u32>,
    composite_mask_textures: DenseMap<PreparedCompositeMask>,
    composite_bounds: DenseMap<Option<[f32; 4]>>,
}

impl WgpuRenderer {
    /// Prefer `from_pipelines` to share pipelines across renderers; each
    /// `Pipelines::new` compiles every pipeline again.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        // `Arc`, not `Rc`: catchlight-bevy needs `Pipelines` Send + Sync, which it
        // lacks only on wasm32 with wgpu's `webgl` feature.
        #[allow(clippy::arc_with_non_send_sync)]
        let shared = std::sync::Arc::new(Pipelines::new(&device, surface_format));
        Self::from_pipelines(device, queue, shared)
    }

    pub fn from_pipelines(
        device: wgpu::Device,
        queue: wgpu::Queue,
        shared: std::sync::Arc<Pipelines>,
    ) -> Self {
        let instances = FrameArena::new(
            &device,
            "Instance Buffer",
            wgpu::BufferUsages::VERTEX,
            512 * INSTANCE_STRIDE,
        );
        let part_uniforms = FrameArena::new(
            &device,
            "Part Uniform Buffer",
            wgpu::BufferUsages::UNIFORM,
            256 * shared.part_uniform_stride,
        );
        let part_uniform_bind_group =
            part_uniform_bind_group(&device, &shared, part_uniforms.buffer());

        let deform_buffer_capacity = 8;
        let deform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Deform Buffer Atlas"),
            size: deform_buffer_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Camera Buffer"),
            size: shared.camera_stride * CAMERA_RING_SLOTS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &shared.camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &camera_buffer,
                    offset: 0,
                    size: std::num::NonZeroU64::new(std::mem::size_of::<CameraUniform>() as u64),
                }),
            }],
            label: Some("camera_bind_group"),
        });
        // Slot 0 holds identity so a frame before any update_camera
        // doesn't read uninitialised memory.
        queue.write_buffer(
            &camera_buffer,
            0,
            bytemuck::cast_slice(&[CameraUniform {
                view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            }]),
        );

        let gpu_profiler = if device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            wgpu_profiler::GpuProfiler::new(&device, wgpu_profiler::GpuProfilerSettings::default())
                .map_err(|e| {
                    tracing::warn!("GpuProfiler::new failed: {e}; GPU timestamps disabled");
                    e
                })
                .ok()
        } else {
            None
        };

        Self {
            device,
            queue,
            shared,
            id: NEXT_RENDERER_ID.fetch_add(1, Ordering::Relaxed),
            part_uniforms,
            part_uniform_bind_group,
            mesh_buffers: DenseMap::new(),
            textures: DenseMap::new(),
            instances,
            deform_buffer,
            deform_buffer_capacity,
            deform_buffer_len: 0,
            deform_upload_mirror: Vec::new(),
            pending_deform_bytes: 0,
            frame_render_passes: AtomicU32::new(0),
            frame_draw_calls: AtomicU32::new(0),
            camera_buffer,
            camera_bind_group,
            camera_offset: 0,
            current_stats: RenderStats::default(),
            frame_stats: FrameStats::default(),
            frame_sizing_closed: false,
            gpu_profiler,
            deform_sets: vec![DeformSetState::live()],
            deform_free: Vec::new(),
            deform_inactive_scratch: Vec::new(),
            deform_write_ranges: Vec::new(),
            deform_recording_set: DeformSet::FIRST,
            deform_recording_base: 0,
            camera_view_proj: glam::Mat4::IDENTITY,
            draw_filter_scratch: Vec::new(),
            instance_scratch: Vec::new(),
            pending_parts_scratch: Vec::new(),
            drawable_walk_scratch: Vec::new(),
            mask_source_ids_scratch: Vec::new(),
            uniform_offset_scratch: Vec::new(),
            composite_mask_textures: DenseMap::new(),
            composite_bounds: DenseMap::new(),
        }
    }

    pub fn pipelines(&self) -> &std::sync::Arc<Pipelines> {
        &self.shared
    }

    /// Reset at each frame entry; read it after the render call returns.
    pub fn frame_stats(&self) -> FrameStats {
        self.frame_stats
    }

    fn note_queue_write(&mut self, buffer: RenderBuffer) {
        self.frame_stats.queue_writes += 1;
        match buffer {
            RenderBuffer::Instance => self.frame_stats.instance_buffer_writes += 1,
            RenderBuffer::PartUniform => self.frame_stats.part_uniform_buffer_writes += 1,
            RenderBuffer::Camera => self.frame_stats.camera_buffer_writes += 1,
            RenderBuffer::Deform => self.frame_stats.deform_buffer_writes += 1,
        }
    }

    fn note_realloc(&mut self, buffer: RenderBuffer) {
        match buffer {
            RenderBuffer::Instance => self.frame_stats.instance_buffer_reallocs += 1,
            RenderBuffer::PartUniform => self.frame_stats.part_uniform_buffer_reallocs += 1,
            RenderBuffer::Deform => self.frame_stats.deform_buffer_reallocs += 1,
            RenderBuffer::Camera => {}
        }
        if self.frame_sizing_closed {
            self.frame_stats.late_buffer_reallocs += 1;
            debug_assert!(
                false,
                "{buffer:?} buffer grew after the frame was sized — passes already \
                 recorded this frame still point at the freed buffer",
            );
        }
    }

    /// `total_instances` must cover every draw in the frame; growing mid-frame
    /// strands recorded passes.
    fn begin_frame_instances(&mut self, total_instances: usize, continues: bool) {
        self.frame_stats.instance_slots_budgeted = total_instances as u32;
        let needed = total_instances as u64 * INSTANCE_STRIDE;
        if self.instances.begin_frame(&self.device, needed, continues) {
            self.note_realloc(RenderBuffer::Instance);
        }
    }

    fn reserve_instances(&mut self, count: usize) -> u64 {
        self.frame_stats.instance_slots_reserved += count as u32;
        self.instances.reserve(count as u64 * INSTANCE_STRIDE)
    }

    fn bind_pass_instances(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        if self.shared.base_instance {
            render_pass.set_vertex_buffer(1, self.instances.buffer().slice(..));
        }
    }

    /// `instance_offset` is a whole number of strides into the buffer, so the
    /// derived instance index is exact.
    fn emit_part_draw(
        &self,
        render_pass: &mut wgpu::RenderPass<'_>,
        num_indices: u32,
        instance_offset: u64,
    ) {
        if self.shared.base_instance {
            let instance = (instance_offset / INSTANCE_STRIDE) as u32;
            render_pass.draw_indexed(0..num_indices, 0, instance..instance + 1);
        } else {
            render_pass.set_vertex_buffer(
                1,
                self.instances
                    .buffer()
                    .slice(instance_offset..instance_offset + INSTANCE_STRIDE),
            );
            render_pass.draw_indexed(0..num_indices, 0, 0..1);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
    }

    fn write_instances(&mut self, offset: u64, instances: &[InstanceRaw]) {
        if instances.is_empty() {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(instances);
        self.frame_stats.instance_slots_written += instances.len() as u32;
        self.frame_stats.instance_bytes_written += bytes.len() as u64;
        self.instances.write(offset, bytes);
    }

    fn flush_instance_writes(&mut self) {
        let uploaded = self.instances.flush(&self.queue);
        if uploaded > 0 {
            self.note_queue_write(RenderBuffer::Instance);
        }
        self.current_stats.instance_bytes_uploaded = uploaded;
    }

    fn begin_frame_uniforms(&mut self, count: u32, continues: bool) {
        self.frame_stats.part_uniform_slots_budgeted = count;
        let needed = u64::from(count) * self.shared.part_uniform_stride;
        if self
            .part_uniforms
            .begin_frame(&self.device, needed, continues)
        {
            self.note_realloc(RenderBuffer::PartUniform);
            self.part_uniform_bind_group =
                part_uniform_bind_group(&self.device, &self.shared, self.part_uniforms.buffer());
        }
        // Both frame buffers are now sized; every pass recorded from here
        // on binds them, so nothing may reallocate until the next frame.
        self.frame_sizing_closed = true;
    }

    fn flush_part_uniform_writes(&mut self) {
        if self.part_uniforms.flush(&self.queue) > 0 {
            self.note_queue_write(RenderBuffer::PartUniform);
        }
    }

    /// Only inside a frame: the slot is staged and uploaded with the frame.
    fn write_part_uniform(
        &mut self,
        opacity: f32,
        tint: glam::Vec3,
        screen_tint: glam::Vec3,
        mask_threshold: f32,
    ) -> u32 {
        // Tints are authored in sRGB; fragment math runs in linear.
        let tint_linear = srgb_to_linear_vec3(tint);
        let screen_tint_linear = srgb_to_linear_vec3(screen_tint);
        let uniform = PartUniform::new(opacity, tint_linear, screen_tint_linear, mask_threshold);
        let offset = self.part_uniforms.reserve(self.shared.part_uniform_stride);
        self.part_uniforms
            .write(offset, bytemuck::bytes_of(&uniform));
        self.frame_stats.part_uniform_writes += 1;
        offset as u32
    }

    /// Uploaded into a camera slot of its own by the next render call.
    pub fn update_camera(&mut self, view_proj: glam::Mat4) {
        self.camera_view_proj = view_proj;
    }

    #[doc(hidden)]
    pub fn camera_dynamic_offset(&self) -> u32 {
        self.camera_offset
    }

    fn write_camera(&mut self, slot: u32) {
        let offset = camera_slot_offset(slot, self.shared.camera_stride);
        let camera_uniform = CameraUniform {
            view_proj: self.camera_view_proj.to_cols_array_2d(),
        };
        self.note_queue_write(RenderBuffer::Camera);
        self.queue.write_buffer(
            &self.camera_buffer,
            offset,
            bytemuck::cast_slice(&[camera_uniform]),
        );
        self.camera_offset = offset as u32;
    }

    /// Takes `&self` so the `&self` blit helpers count too.
    fn counted_begin_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        desc: &wgpu::RenderPassDescriptor<'_>,
    ) -> wgpu::RenderPass<'e> {
        self.frame_render_passes.fetch_add(1, Ordering::Relaxed);
        encoder.begin_render_pass(desc)
    }

    /// For when the frame clear can't be folded into a drawing pass.
    fn explicit_clear_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        color: wgpu::Color,
        label: &str,
    ) {
        let _ = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );
    }

    fn finalize_frame_stats(&mut self) {
        self.current_stats.render_passes = self.frame_render_passes.load(Ordering::Relaxed);
        self.current_stats.total_draw_calls = self.frame_draw_calls.load(Ordering::Relaxed);
        self.current_stats.deform_bytes_uploaded = self.pending_deform_bytes;
    }
}
