use std::sync::atomic::{AtomicU64, Ordering};

use crate::{NodeIdx, Vec2};
use smallvec::SmallVec;

// Process-global "combine epoch". Each combine() that actually runs claims
// a unique value, so a (mesh_id, generation) pair the renderer has already
// uploaded can never collide with a *different* combine result — even when
// a puppet is `clone_from`'d (which rewinds a per-stack counter back to the
// source's value and would otherwise alias two distinct deforms to the same
// generation). 0 is reserved for "never combined".
static DEFORM_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_deform_generation() -> u64 {
    DEFORM_GENERATION.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeformSource {
    Param(u32),
    Node(NodeIdx),
    /// All weld pulls on this part, summed. One slot regardless of how many
    /// weld records touch the part — the weld pass zeroes it on first touch
    /// each frame and accumulates (see `crate::weld::apply_welds`).
    Weld,
    /// An external writer's scratch slot — the editor's live drag. The one
    /// source a fold does not produce, and so the one [`DeformStack::reset`]
    /// leaves alone: nothing else can reconstruct it.
    Scratch,
    /// A slot for this crate's own unit tests, so they can write a source no
    /// pass owns. Not part of the public enum.
    #[cfg(test)]
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeformShapeError {
    #[error("deform matrix dimensions overflow: {width} x {height}")]
    MatrixDimensionsOverflow { width: usize, height: usize },
    #[error("deform matrix has {actual} cells; expected {expected} for {width} x {height}")]
    MatrixCellCount {
        width: usize,
        height: usize,
        expected: usize,
        actual: usize,
    },
    #[error("deform matrix storage overflows: {cells} cells x {vert_count} vertices")]
    MatrixStorageOverflow { cells: usize, vert_count: usize },
    #[error("deform has {actual} vertices; expected {expected}")]
    StackLength { expected: usize, actual: usize },
}

// Per-source offset buffer. `active` lets us keep the allocation across
// frames (reset() flips active=false; source_buf_mut() flips it back
// true and returns a mut slice). Without this, every frame would
// re-allocate the buffer a binding writes into.
#[derive(Debug, Clone)]
struct SourceSlot {
    source: DeformSource,
    offsets: Vec<Vec2>,
    active: bool,
    // Param value whose fold currently fills `offsets` (param_buf_mut
    // memo key); None when the buffer was last written through the
    // unkeyed paths.
    last_val: Option<Vec2>,
}

// Inline capacity 2 covers the observed hot-path shape: at most one
// Param source + one MeshGroup-parent Node source per node. More spill to
// the heap without losing correctness.
type Sources = SmallVec<[SourceSlot; 2]>;

#[derive(Debug, Clone, Default)]
pub struct DeformStack {
    pub vert_count: usize,
    sources: Sources,
    combined: Vec<Vec2>,
    dirty: bool,
    // Active set at the last combine (bit i = sources[i]; slots are
    // never removed, so indices are stable). Lets combine() notice
    // activations/deactivations without reset() having to mark dirty —
    // the key to keeping `combined` (and its generation, which gates
    // the snapshot copy and the GPU upload) when a frame re-folds the
    // same param values.
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

    /// Deactivate every source a fold produces, so the next one starts from
    /// nothing.
    ///
    /// [`DeformSource::Scratch`] survives. Every other source is re-derived
    /// from the model each fold, which is what makes dropping it safe; the
    /// scratch slot is an edit in progress that only its writer holds, so a
    /// fold that dropped it would delete state nothing can rebuild — and a
    /// live drag would vanish on any frame that happened to fold (a driver
    /// moving is enough). It goes when its writer clears it, or when the
    /// model changes and the puppet rebakes the stack from scratch.
    pub fn reset(&mut self) {
        for slot in self.sources.iter_mut() {
            if slot.source != DeformSource::Scratch {
                slot.active = false;
            }
        }
    }

    /// Active set as a bitmask; None when it can't be represented
    /// (>64 sources — never in practice), which forces a recombine.
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

    // Prefer this in hot paths: returns a writable slice of
    // length `self.vert_count`, pooled across frames. Marks the slot
    // active and the stack dirty.
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

    /// Value-memoized variant of `source_buf_mut` for param-driven
    /// folds: a fold's output is a pure function of the param value
    /// (binding matrices are immutable after import), so when `val` is
    /// bit-equal to the value that produced the slot's current
    /// contents, the slot is just re-marked active — no dirty, no
    /// per-vertex rewrite — and `None` tells the caller to skip its
    /// fold. `combine` then keeps the existing sum and generation, so
    /// the snapshot copy and GPU upload skip too.
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

    /// Set `source`'s offsets, marking the stack dirty only when that changes
    /// what the source holds. `false` when `offsets` is not one per vertex,
    /// and nothing is written.
    ///
    /// The passes downstream of the bindings — spines, mesh groups — re-derive
    /// a child's offsets every time they run, and usually arrive at the
    /// numbers they wrote last fold: a blink re-runs every group, and only the
    /// eyes' children move. Through [`Self::source_buf_mut`] each of those
    /// would mark its stack dirty regardless, and `combine` would hand the
    /// renderer a new generation — an upload — for a deform that did not move.
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

    /// [`Self::source_buf_mut`] without the dirty mark, for a pass that
    /// accumulates into the slot over several steps and cannot say whether
    /// the contents moved until the last one. It owes the stack a
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

    /// Close a [`Self::source_buf_unsettled`] write: mark the stack dirty if
    /// `source` no longer holds `before`, what it held when the pass began.
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
        // Copy-first: the dominant case is a single active source, so seed
        // `combined` from the first one instead of zero-filling then reading
        // it back to add. copy_from_slice needs matching lengths (it panics
        // otherwise, and clippy denies panics); sources are normally exactly
        // vert_count long, but fall back to the zip-add path — which tolerates
        // a short source — when they aren't.
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

    /// Sum the active sources into `out` without touching `combined`,
    /// the dirty flag, the mask, or the generation. Mesh-group
    /// propagation reads "the child's deform minus this MG's source"
    /// every frame; summing read-only avoids a full combine (and its
    /// generation bump) plus a copy per child per frame.
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
            // No active sources, or a length-mismatched first source:
            // zero-fill and zip-add everything, mirroring combine()'s
            // tolerant fallback.
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

    /// Vertex `index`'s offset summed over the active sources other than
    /// `except`: one vertex of what [`Self::sum_active_into`] would give with
    /// `except` cleared, without clearing it or reading the rest.
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

    /// A fold re-derives every source but the scratch one, so reset drops
    /// every source but the scratch one. Otherwise the editor's live drag
    /// would disappear on the next frame that folded for its own reasons.
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
        // ...and it is the writer's to end.
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

    // sum_active_into must produce combine()'s sum while leaving the
    // stack's memo state untouched: a follow-up combine() with unchanged
    // inputs must early-out and keep its generation (the propagation
    // hot path relies on this to avoid a per-child generation bump).
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

        // Deactivated source drops out of the sum; no state change.
        s.clear_source(DeformSource::Test);
        s.sum_active_into(&mut out);
        assert_eq!(out, vec![Vec2::new(0.5, -1.0); 2]);

        // All inactive: zero-filled at vert_count.
        s.clear_source(DeformSource::Param(0));
        s.sum_active_into(&mut out);
        assert_eq!(out, vec![Vec2::ZERO; 2]);
    }

    // Pooling invariant: reset() + source_buf_mut() on the same source
    // across frames must not grow the slot's internal allocation.
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

    // The frame loop is reset() -> fold -> combine(). With an unchanged
    // param value the fold memo-hits, and the recombine must be skipped
    // entirely: same sum, same generation (the generation gates the
    // snapshot copy and the GPU upload downstream).
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

        // A changed value must rewrite, recombine, and re-generation.
        s.reset();
        s.param_buf_mut(DeformSource::Param(1), Vec2::new(0.4, 0.7))
            .expect("changed value must return the buffer")
            .fill(Vec2::new(3.0, 0.0));
        s.combine();
        assert_ne!(s.generation(), gen1);
        assert_eq!(s.combined()[0], Vec2::new(3.0, 0.0));
    }

    // Activation-set changes must recombine even when no buffer was
    // rewritten: a memo-hit slot dropping out (param back at an identity
    // cell) or coming back must change the sum.
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

        // Param(1) memo-hits, Test stays inactive: sum must drop it.
        s.reset();
        assert!(s.param_buf_mut(DeformSource::Param(1), v).is_none());
        s.combine();
        assert_ne!(s.generation(), gen1);
        assert_eq!(s.combined()[0], Vec2::new(1.0, 0.0));

        // All sources inactive: sum must go to zero.
        s.reset();
        s.combine();
        assert_eq!(s.combined()[0], Vec2::ZERO);

        // Reactivation via memo-hit alone must restore the contribution.
        s.reset();
        assert!(s.param_buf_mut(DeformSource::Param(1), v).is_none());
        s.combine();
        assert_eq!(s.combined()[0], Vec2::new(1.0, 0.0));
    }

    // The unkeyed write path must invalidate the memo: contents no
    // longer correspond to any param value.
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

    // The renderer skips a deform upload when (mesh_id, generation) is
    // already resident. A puppet cloned from a pristine snapshot rewinds
    // each stack's generation to the source's value; the next combine must
    // still produce a generation distinct from any previous combine, or two
    // different deforms would alias and the GPU would keep stale data.
    #[test]
    fn combine_after_clone_does_not_reuse_a_prior_generation() {
        let mut original = DeformStack::new(1);
        original
            .set(DeformSource::Param(0), vec![Vec2::new(1.0, 0.0)])
            .unwrap();
        original.combine();
        let uploaded = original.generation();

        // Mirror the harness: clone the stack, then drive it to a *different*
        // deform. The clone starts at `uploaded` but must move past it.
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

    /// Writing a source the numbers it already holds is not a change: the
    /// stack stays clean, so `combine` keeps the sum and its generation.
    #[test]
    fn write_source_marks_dirty_only_when_the_offsets_change() {
        let mut s = DeformStack::new(2);
        let offsets = [Vec2::new(1.0, 2.0), Vec2::new(3.0, 4.0)];
        assert!(s.write_source(DeformSource::Node(NodeIdx(7)), &offsets));
        s.combine();
        let generation = s.generation();
        assert_eq!(s.combined(), &offsets);

        // A fold: reset, then the same pass writes the same numbers.
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

        // A source that does not come back is a change too.
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

    /// The accumulate-then-settle pair a multi-step pass uses: unchanged
    /// contents settle clean, changed ones settle dirty.
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
