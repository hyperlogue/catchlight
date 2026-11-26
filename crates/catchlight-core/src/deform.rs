use std::sync::atomic::{AtomicU64, Ordering};

use crate::{NodeIdx, Vec2};
use smallvec::SmallVec;

// Process-global so a `clone_from`'d stack, whose counter rewinds, can never
// reuse a generation the renderer already uploaded. 0 means "never combined".
static DEFORM_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_deform_generation() -> u64 {
    DEFORM_GENERATION.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeformSource {
    Param(u32),
    Node(NodeIdx),
    /// All weld pulls on this part, summed into one slot.
    Weld,
    /// An external writer's scratch slot (the editor's live drag). Nothing else
    /// can reconstruct it, so [`DeformStack::reset`] leaves it alone.
    Scratch,
    #[cfg(test)]
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DeformShapeError {
    #[error("deform has {actual} vertices; expected {expected}")]
    StackLength { expected: usize, actual: usize },
}

// `active` keeps the allocation across frames instead of reallocating.
#[derive(Debug, Clone)]
struct SourceSlot {
    source: DeformSource,
    offsets: Vec<Vec2>,
    active: bool,
    // Param value whose fold filled `offsets`; None after an unkeyed write.
    last_val: Option<Vec2>,
}

// Inline capacity 2: at most one Param plus one MeshGroup-parent source per node.
type Sources = SmallVec<[SourceSlot; 2]>;

#[derive(Debug, Clone, Default)]
pub struct DeformStack {
    vert_count: usize,
    sources: Sources,
    combined: Vec<Vec2>,
    dirty: bool,
    // Active set at the last combine (bit i = sources[i]). Lets combine() notice
    // (de)activations without reset() marking dirty, keeping the generation when
    // a frame re-folds the same param values.
    last_combined_mask: Option<u64>,
    generation: u64,
}

impl DeformStack {
    pub fn new(vert_count: usize) -> Self {
        Self {
            vert_count,
            sources: SmallVec::new(),
            combined: vec![Vec2::ZERO; vert_count],
            dirty: false,
            last_combined_mask: Some(0),
            generation: 0,
        }
    }

    pub fn vert_count(&self) -> usize {
        self.vert_count
    }

    /// Deactivate every source a fold produces. [`DeformSource::Scratch`]
    /// survives: it is an edit in progress that no fold can rebuild.
    pub fn reset(&mut self) {
        for slot in self.sources.iter_mut() {
            if slot.source != DeformSource::Scratch {
                slot.active = false;
            }
        }
    }

    /// None when there are more than 64 sources, which forces a recombine.
    fn active_mask(&self) -> Option<u64> {
        if self.sources.len() > 64 {
            return None;
        }
        let mut mask = 0u64;
        for (i, slot) in self.sources.iter().enumerate() {
            if slot.active {
                mask |= 1 << i;
            }
        }
        Some(mask)
    }

    pub fn source_buf_mut(&mut self, source: DeformSource) -> &mut [Vec2] {
        self.dirty = true;
        let vert_count = self.vert_count;
        let pos = self.sources.iter().position(|s| s.source == source);
        let idx = match pos {
            Some(i) => i,
            None => {
                self.sources.push(SourceSlot {
                    source,
                    offsets: vec![Vec2::ZERO; vert_count],
                    active: true,
                    last_val: None,
                });
                self.sources.len() - 1
            }
        };
        let slot = &mut self.sources[idx];
        slot.active = true;
        slot.last_val = None;
        if slot.offsets.len() != vert_count {
            slot.offsets.resize(vert_count, Vec2::ZERO);
        }
        &mut slot.offsets
    }

    /// Value-memoized `source_buf_mut`. Param folds are a pure function of the
    /// value, so a bit-equal `val` re-marks the slot active without dirtying and
    /// returns `None` so the caller skips its fold.
    pub fn param_buf_mut(&mut self, source: DeformSource, val: Vec2) -> Option<&mut [Vec2]> {
        let vert_count = self.vert_count;
        let pos = self.sources.iter().position(|s| s.source == source);
        let idx = match pos {
            Some(i) => i,
            None => {
                self.dirty = true;
                self.sources.push(SourceSlot {
                    source,
                    offsets: vec![Vec2::ZERO; vert_count],
                    active: true,
                    last_val: Some(val),
                });
                let last = self.sources.len() - 1;
                return Some(&mut self.sources[last].offsets);
            }
        };
        let slot = &mut self.sources[idx];
        slot.active = true;
        if slot.last_val == Some(val) && slot.offsets.len() == vert_count {
            return None;
        }
        slot.last_val = Some(val);
        if slot.offsets.len() != vert_count {
            slot.offsets.resize(vert_count, Vec2::ZERO);
        }
        self.dirty = true;
        Some(&mut slot.offsets)
    }

    pub fn set(&mut self, source: DeformSource, deform: Vec<Vec2>) -> Result<(), DeformShapeError> {
        if deform.len() != self.vert_count {
            return Err(DeformShapeError::StackLength {
                expected: self.vert_count,
                actual: deform.len(),
            });
        }
        let buf = self.source_buf_mut(source);
        buf.copy_from_slice(&deform);
        Ok(())
    }

    /// Set `source`'s offsets, marking the stack dirty only when they change, so
    /// a re-run pass that writes the same numbers causes no new upload. `false`
    /// when `offsets` is not one per vertex, and nothing is written.
    pub(crate) fn write_source(&mut self, source: DeformSource, offsets: &[Vec2]) -> bool {
        if offsets.len() != self.vert_count {
            return false;
        }
        match self.sources.iter_mut().find(|s| s.source == source) {
            Some(slot) => {
                slot.active = true;
                slot.last_val = None;
                if slot.offsets.as_slice() != offsets {
                    slot.offsets.clear();
                    slot.offsets.extend_from_slice(offsets);
                    self.dirty = true;
                }
            }
            None => {
                self.sources.push(SourceSlot {
                    source,
                    offsets: offsets.to_vec(),
                    active: true,
                    last_val: None,
                });
                self.dirty = true;
            }
        }
        true
    }

    /// [`Self::source_buf_mut`] without the dirty mark. The caller must call
    /// [`Self::settle_source`] before anything combines.
    pub(crate) fn source_buf_unsettled(&mut self, source: DeformSource) -> &mut [Vec2] {
        let vert_count = self.vert_count;
        let idx = match self.sources.iter().position(|s| s.source == source) {
            Some(i) => i,
            None => {
                self.dirty = true;
                self.sources.push(SourceSlot {
                    source,
                    offsets: vec![Vec2::ZERO; vert_count],
                    active: true,
                    last_val: None,
                });
                self.sources.len() - 1
            }
        };
        let slot = &mut self.sources[idx];
        slot.active = true;
        slot.last_val = None;
        if slot.offsets.len() != vert_count {
            slot.offsets.resize(vert_count, Vec2::ZERO);
            self.dirty = true;
        }
        &mut slot.offsets
    }

    pub(crate) fn settle_source(&mut self, source: DeformSource, before: &[Vec2]) {
        if let Some(slot) = self.sources.iter().find(|s| s.source == source) {
            if slot.offsets.as_slice() != before {
                self.dirty = true;
            }
        }
    }

    pub(crate) fn reactivate(&mut self, source: DeformSource) {
        if let Some(slot) = self.sources.iter_mut().find(|s| s.source == source) {
            slot.active = true;
        }
    }

    pub fn clear_source(&mut self, source: DeformSource) {
        if let Some(slot) = self.sources.iter_mut().find(|s| s.source == source) {
            slot.active = false;
        }
    }

    pub fn combine(&mut self) {
        let mask = self.active_mask();
        if !self.dirty && mask.is_some() && mask == self.last_combined_mask {
            return;
        }
        if self.combined.len() != self.vert_count {
            self.combined = vec![Vec2::ZERO; self.vert_count];
        }
        // Copy-first for the common single-source case; fall back to zip-add when a
        // source is not exactly vert_count long (copy_from_slice would panic).
        let mut active = self.sources.iter().filter(|s| s.active);
        match active.next() {
            Some(first) if first.offsets.len() == self.combined.len() => {
                self.combined.copy_from_slice(&first.offsets);
                for slot in active {
                    for (out, d) in self.combined.iter_mut().zip(slot.offsets.iter()) {
                        *out += *d;
                    }
                }
            }
            _ => {
                for v in self.combined.iter_mut() {
                    *v = Vec2::ZERO;
                }
                for slot in self.sources.iter().filter(|s| s.active) {
                    for (out, d) in self.combined.iter_mut().zip(slot.offsets.iter()) {
                        *out += *d;
                    }
                }
            }
        }
        self.dirty = false;
        self.last_combined_mask = mask;
        self.generation = next_deform_generation();
    }

    /// Sum the active sources into `out` without touching `combined`, the dirty
    /// flag, the mask, or the generation.
    pub fn sum_active_into(&self, out: &mut Vec<Vec2>) {
        out.clear();
        let mut active = self.sources.iter().filter(|s| s.active);
        match active.next() {
            Some(first) if first.offsets.len() == self.vert_count => {
                out.extend_from_slice(&first.offsets);
                for slot in active {
                    for (o, d) in out.iter_mut().zip(slot.offsets.iter()) {
                        *o += *d;
                    }
                }
            }
            _ => {
                out.resize(self.vert_count, Vec2::ZERO);
                for slot in self.sources.iter().filter(|s| s.active) {
                    for (o, d) in out.iter_mut().zip(slot.offsets.iter()) {
                        *o += *d;
                    }
                }
            }
        }
    }

    pub fn combined(&self) -> &[Vec2] {
        &self.combined
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn active_offset_except(&self, except: DeformSource, index: usize) -> Vec2 {
        self.sources
            .iter()
            .filter(|s| s.active && s.source != except)
            .filter_map(|s| s.offsets.get(index))
            .fold(Vec2::ZERO, |sum, d| sum + *d)
    }

    pub fn is_active(&self) -> bool {
        self.sources.iter().any(|s| s.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_rejects_a_vertex_count_mismatch_without_activating_source() {
        let mut stack = DeformStack::new(2);

        let err = stack.set(DeformSource::Test, vec![Vec2::ZERO]).unwrap_err();

        assert_eq!(
            err,
            DeformShapeError::StackLength {
                expected: 2,
                actual: 1,
            }
        );
        assert!(!stack.is_active());
    }

    #[test]
    fn combine_sums_multiple_sources() {
        let mut s = DeformStack::new(3);
        s.set(
            DeformSource::Test,
            vec![
                Vec2::new(1.0, 0.0),
                Vec2::new(2.0, 0.0),
                Vec2::new(3.0, 0.0),
            ],
        )
        .unwrap();
        s.set(
            DeformSource::Param(0),
            vec![
                Vec2::new(0.1, 1.0),
                Vec2::new(0.2, 2.0),
                Vec2::new(0.3, 3.0),
            ],
        )
        .unwrap();
        s.combine();
        assert_eq!(s.combined()[0], Vec2::new(1.1, 1.0));
        assert_eq!(s.combined()[1], Vec2::new(2.2, 2.0));
        assert_eq!(s.combined()[2], Vec2::new(3.3, 3.0));
    }

    #[test]
    fn reset_empties_without_resizing_combined() {
        let mut s = DeformStack::new(2);
        s.set(DeformSource::Test, vec![Vec2::new(5.0, 5.0); 2])
            .unwrap();
        s.combine();
        s.reset();
        s.combine();
        assert_eq!(s.combined(), &[Vec2::ZERO, Vec2::ZERO]);
        assert!(!s.is_active());
    }

    #[test]
    fn reset_keeps_the_scratch_source_and_drops_the_rest() {
        let mut s = DeformStack::new(1);
        s.set(DeformSource::Param(0), vec![Vec2::new(1.0, 0.0)])
            .unwrap();
        s.set(DeformSource::Scratch, vec![Vec2::new(0.0, 7.0)])
            .unwrap();
        s.reset();
        s.combine();
        assert_eq!(s.combined()[0], Vec2::new(0.0, 7.0));
        s.clear_source(DeformSource::Scratch);
        s.combine();
        assert_eq!(s.combined()[0], Vec2::ZERO);
    }

    #[test]
    fn clear_source_drops_only_one() {
        let mut s = DeformStack::new(1);
        s.set(DeformSource::Test, vec![Vec2::new(1.0, 0.0)])
            .unwrap();
        s.set(DeformSource::Param(0), vec![Vec2::new(0.0, 1.0)])
            .unwrap();
        s.clear_source(DeformSource::Test);
        s.combine();
        assert_eq!(s.combined()[0], Vec2::new(0.0, 1.0));
    }

    #[test]
    fn set_replaces_existing_source() {
        let mut s = DeformStack::new(1);
        s.set(DeformSource::Param(0), vec![Vec2::new(1.0, 0.0)])
            .unwrap();
        s.set(DeformSource::Param(0), vec![Vec2::new(9.0, 0.0)])
            .unwrap();
        s.combine();
        assert_eq!(s.combined()[0], Vec2::new(9.0, 0.0));
    }

    // A follow-up combine() with unchanged inputs must keep its generation.
    #[test]
    fn sum_active_into_matches_combine_and_leaves_state_untouched() {
        let mut s = DeformStack::new(2);
        s.set(DeformSource::Test, vec![Vec2::new(1.0, 2.0); 2])
            .unwrap();
        s.set(DeformSource::Param(0), vec![Vec2::new(0.5, -1.0); 2])
            .unwrap();
        s.combine();
        let gen1 = s.generation();

        let mut out = Vec::new();
        s.sum_active_into(&mut out);
        assert_eq!(out, s.combined());

        s.combine();
        assert_eq!(s.generation(), gen1, "read-only sum must not dirty");

        s.clear_source(DeformSource::Test);
        s.sum_active_into(&mut out);
        assert_eq!(out, vec![Vec2::new(0.5, -1.0); 2]);

        s.clear_source(DeformSource::Param(0));
        s.sum_active_into(&mut out);
        assert_eq!(out, vec![Vec2::ZERO; 2]);
    }

    #[test]
    fn source_buf_mut_reuses_allocation_across_frames() {
        let mut s = DeformStack::new(4);
        {
            let buf = s.source_buf_mut(DeformSource::Param(7));
            for v in buf.iter_mut() {
                *v = Vec2::new(1.0, 2.0);
            }
        }
        s.combine();
        let cap_before = s
            .sources
            .iter()
            .find(|slot| slot.source == DeformSource::Param(7))
            .map(|slot| slot.offsets.capacity())
            .unwrap_or(0);
        s.reset();
        {
            let buf = s.source_buf_mut(DeformSource::Param(7));
            buf.fill(Vec2::ZERO);
        }
        let cap_after = s
            .sources
            .iter()
            .find(|slot| slot.source == DeformSource::Param(7))
            .map(|slot| slot.offsets.capacity())
            .unwrap_or(0);
        assert_eq!(cap_before, cap_after, "pool must reuse allocation");
    }

    #[test]
    fn unchanged_param_value_keeps_combined_and_generation() {
        let mut s = DeformStack::new(2);
        let v = Vec2::new(0.3, 0.7);
        s.param_buf_mut(DeformSource::Param(1), v)
            .expect("first write")
            .fill(Vec2::new(1.0, 2.0));
        s.combine();
        let gen1 = s.generation();
        assert_ne!(gen1, 0);

        s.reset();
        assert!(
            s.param_buf_mut(DeformSource::Param(1), v).is_none(),
            "same value must memo-hit"
        );
        s.combine();
        assert_eq!(s.generation(), gen1, "no-op frame must keep the generation");
        assert_eq!(s.combined()[0], Vec2::new(1.0, 2.0));

        s.reset();
        s.param_buf_mut(DeformSource::Param(1), Vec2::new(0.4, 0.7))
            .expect("changed value must return the buffer")
            .fill(Vec2::new(3.0, 0.0));
        s.combine();
        assert_ne!(s.generation(), gen1);
        assert_eq!(s.combined()[0], Vec2::new(3.0, 0.0));
    }

    #[test]
    fn active_set_change_recombines_without_writes() {
        let mut s = DeformStack::new(1);
        let v = Vec2::new(0.5, 0.5);
        s.param_buf_mut(DeformSource::Param(1), v)
            .expect("first write")
            .fill(Vec2::new(1.0, 0.0));
        s.set(DeformSource::Test, vec![Vec2::new(0.0, 1.0)])
            .unwrap();
        s.combine();
        assert_eq!(s.combined()[0], Vec2::new(1.0, 1.0));
        let gen1 = s.generation();

        s.reset();
        assert!(s.param_buf_mut(DeformSource::Param(1), v).is_none());
        s.combine();
        assert_ne!(s.generation(), gen1);
        assert_eq!(s.combined()[0], Vec2::new(1.0, 0.0));

        s.reset();
        s.combine();
        assert_eq!(s.combined()[0], Vec2::ZERO);

        s.reset();
        assert!(s.param_buf_mut(DeformSource::Param(1), v).is_none());
        s.combine();
        assert_eq!(s.combined()[0], Vec2::new(1.0, 0.0));
    }

    #[test]
    fn source_buf_mut_invalidates_param_memo() {
        let mut s = DeformStack::new(1);
        let v = Vec2::new(0.5, 0.5);
        s.param_buf_mut(DeformSource::Param(1), v)
            .expect("first write")
            .fill(Vec2::new(1.0, 0.0));
        s.combine();

        s.reset();
        s.source_buf_mut(DeformSource::Param(1))
            .fill(Vec2::new(9.0, 0.0));
        s.combine();

        s.reset();
        assert!(
            s.param_buf_mut(DeformSource::Param(1), v).is_some(),
            "unkeyed write must drop the memo"
        );
    }

    // A cloned stack rewinds its generation; the next combine must still not
    // alias a generation the renderer already uploaded.
    #[test]
    fn combine_after_clone_does_not_reuse_a_prior_generation() {
        let mut original = DeformStack::new(1);
        original
            .set(DeformSource::Param(0), vec![Vec2::new(1.0, 0.0)])
            .unwrap();
        original.combine();
        let uploaded = original.generation();

        let mut cloned = original.clone();
        assert_eq!(cloned.generation(), uploaded);
        cloned
            .set(DeformSource::Param(0), vec![Vec2::new(2.0, 0.0)])
            .unwrap();
        cloned.combine();
        assert_ne!(
            cloned.generation(),
            uploaded,
            "cloned stack's combine reused the original's generation"
        );
    }

    #[test]
    fn write_source_marks_dirty_only_when_the_offsets_change() {
        let mut s = DeformStack::new(2);
        let offsets = [Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)];
        assert!(s.write_source(DeformSource::Node(NodeIdx(7)), &offsets));
        s.combine();
        let generation = s.generation();
        assert_eq!(s.combined(), &offsets);

        s.reset();
        assert!(s.write_source(DeformSource::Node(NodeIdx(7)), &offsets));
        s.combine();
        assert_eq!(s.generation(), generation);

        let moved = [Vec2::new(1.0, 2.0), Vec2::new(3.0, 5.0)];
        s.reset();
        assert!(s.write_source(DeformSource::Node(NodeIdx(7)), &moved));
        s.combine();
        assert_ne!(s.generation(), generation);
        assert_eq!(s.combined(), &moved);

        let generation = s.generation();
        s.reset();
        s.combine();
        assert_ne!(s.generation(), generation);
        assert_eq!(s.combined(), &[Vec2::ZERO; 2]);
    }

    #[test]
    fn write_source_refuses_a_vertex_count_mismatch() {
        let mut s = DeformStack::new(2);
        assert!(!s.write_source(DeformSource::Node(NodeIdx(0)), &[Vec2::ONE]));
        assert!(!s.is_active());
    }

    #[test]
    fn an_unsettled_write_is_dirty_only_if_it_moved() {
        let mut s = DeformStack::new(1);
        s.source_buf_unsettled(DeformSource::Weld)[0] = Vec2::new(2.0, 0.0);
        s.settle_source(DeformSource::Weld, &[Vec2::ZERO]);
        s.combine();
        let generation = s.generation();

        s.reset();
        let buf = s.source_buf_unsettled(DeformSource::Weld);
        let held = buf.to_vec();
        buf[0] = Vec2::ZERO;
        buf[0] += Vec2::new(2.0, 0.0);
        s.settle_source(DeformSource::Weld, &held);
        s.combine();
        assert_eq!(s.generation(), generation);

        s.reset();
        let buf = s.source_buf_unsettled(DeformSource::Weld);
        let held = buf.to_vec();
        buf[0] = Vec2::new(5.0, 0.0);
        s.settle_source(DeformSource::Weld, &held);
        s.combine();
        assert_ne!(s.generation(), generation);
        assert_eq!(s.combined()[0], Vec2::new(5.0, 0.0));
    }
}
