//! The rules more than one door holds a model to, each stated once.
//!
//! A model is reached four ways: through a mutator, through the `.clm`
//! reader, through [`Model::install`], and — after the fact — through the
//! [`Model::check`] lints and the writer. A rule each of them spells out for
//! itself is a rule they come to disagree on: a file the writer emits and the
//! reader refuses, an install that builds what no edit could. So a rule with
//! more than one door lives here, and the doors call it.
//!
//! Every function takes the facts it needs as plain arguments or lookups and
//! answers with a small *fault* that says what is wrong and nothing about how
//! to say it. The door maps the fault to its own error ([`ModelError`],
//! [`ClmLoadError`], [`InstallError`], a [`CheckWarning`]), because only the
//! door knows what else to name: the reader has a node Id to quote where a
//! mutator was handed one by its caller.
//!
//! The rules:
//!
//! - **A weld joins two different parts, once.** Its ends are two different
//!   nodes, each a part; one weld records an unordered part pair; each pair
//!   names a slot the part on its side carries, no slot twice on a side, with
//!   a weight that is a share within `0..=1` ([`weld`]). An end the model
//!   does not carry is a fragment's weld into its base and is the one thing
//!   deferred, to [`Model::install`], which runs the same rule against the
//!   merged model.
//! - **A spine is at least one link, and every link has a length.** No
//!   joints, a coordinate that is not a number, or a joint on top of the
//!   point above it leaves the turn pass a link with no direction to turn
//!   about ([`spine_joints`]).
//! - **A binding fits its node.** A deform needs a mesh to deform and a
//!   colour needs something drawn ([`binding_target`]); a deform cell holds
//!   one finite `[dx, dy]` per mesh vertex ([`deform_offsets`]); and an axis
//!   of key positions is nonempty, finite, strictly increasing and within
//!   `0..=1` ([`binding_positions_are_valid`]).
//! - **A response input does not outlive the slot it reads.** Whatever
//!   removes a part or a slot drops the inputs that named it
//!   ([`drop_slot_inputs`]).

use super::*;

// ---- welds ----------------------------------------------------------------

/// What one end of a weld names, as far as the caller can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WeldEnd {
    /// A part: the pairs on this side are checked against its slots.
    Part,
    /// A node of another kind, named so the refusal can say which.
    NotAPart(&'static str),
    /// A node the caller does not carry, where that is allowed: a fragment's
    /// weld into its base. Nothing on this side is checked here.
    Deferred,
    /// A node the caller does not carry, where nothing may dangle.
    Missing,
}

impl WeldEnd {
    /// Classify an end from the kind of the node it names. `may_dangle` is
    /// whether a missing node is a requirement rather than a broken weld —
    /// true for a fragment, false for a complete or a merged model.
    pub(super) fn of(kind: Option<&ModelNodeKind>, may_dangle: bool) -> Self {
        match kind {
            Some(ModelNodeKind::Part(_)) => Self::Part,
            Some(other) => Self::NotAPart(other.name()),
            None if may_dangle => Self::Deferred,
            None => Self::Missing,
        }
    }

    /// Whether a model holding a weld with this end can be written and read
    /// back.
    pub(super) fn is_sound(self) -> bool {
        matches!(self, Self::Part | Self::Deferred)
    }
}

/// What is wrong with a weld. `end` is always the weld's own `a` or `b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum WeldFault<'a> {
    SelfPaired,
    EndNotAPart {
        end: &'a NodeId,
        kind: &'static str,
    },
    EndMissing {
        end: &'a NodeId,
    },
    /// The two parts are already welded, whichever way round.
    Duplicate,
    /// `slot` is the pair's slot on `a`, which is how a pair is named.
    WeightOutOfRange {
        slot: &'a SlotId,
    },
    UnknownSlot {
        end: &'a NodeId,
        slot: &'a SlotId,
    },
    SlotPairedTwice {
        end: &'a NodeId,
        slot: &'a SlotId,
    },
}

/// The part pairs welded so far, unordered: one weld records a pair, so the
/// second weld between the same two parts is refused whichever way round it
/// names them.
#[derive(Debug, Default)]
pub(super) struct Welded<'a>(HashSet<(&'a NodeId, &'a NodeId)>);

impl<'a> Welded<'a> {
    pub(super) fn with_capacity(welds: usize) -> Self {
        Self(HashSet::with_capacity(welds))
    }

    /// Record a weld between `a` and `b`. `false` if the pair was already
    /// recorded.
    pub(super) fn insert(&mut self, a: &'a NodeId, b: &'a NodeId) -> bool {
        self.0.insert(if a < b { (a, b) } else { (b, a) })
    }
}

/// Hold one weld to the weld rule, and record it in `welded`.
///
/// `end` classifies the node an end names; `carries` answers whether a part
/// carries a slot, and is only asked about an end that is a [`WeldEnd::Part`].
/// Whether a slot is *filled* is not this rule's business: resolving a weld
/// skips the pairs that are not.
pub(super) fn weld<'a>(
    a: &'a NodeId,
    b: &'a NodeId,
    pairs: impl IntoIterator<Item = (&'a SlotId, &'a SlotId, f32)>,
    welded: &mut Welded<'a>,
    end: impl Fn(&NodeId) -> WeldEnd,
    carries: impl Fn(&NodeId, &SlotId) -> bool,
) -> Result<(), WeldFault<'a>> {
    if a == b {
        return Err(WeldFault::SelfPaired);
    }
    let ends = [(a, end(a)), (b, end(b))];
    for (id, found) in ends {
        match found {
            WeldEnd::Part | WeldEnd::Deferred => {}
            WeldEnd::NotAPart(kind) => return Err(WeldFault::EndNotAPart { end: id, kind }),
            WeldEnd::Missing => return Err(WeldFault::EndMissing { end: id }),
        }
    }
    if !welded.insert(a, b) {
        return Err(WeldFault::Duplicate);
    }
    let mut seen_a = HashSet::new();
    let mut seen_b = HashSet::new();
    for (slot_a, slot_b, weight) in pairs {
        // `contains` is false for `NaN`, so a weight that is not a number is
        // refused along with one that is out of range.
        if !(0.0..=1.0).contains(&weight) {
            return Err(WeldFault::WeightOutOfRange { slot: slot_a });
        }
        for ((id, found), slot) in ends.into_iter().zip([slot_a, slot_b]) {
            if found == WeldEnd::Part && !carries(id, slot) {
                return Err(WeldFault::UnknownSlot { end: id, slot });
            }
        }
        if !seen_a.insert(slot_a) {
            return Err(WeldFault::SlotPairedTwice {
                end: a,
                slot: slot_a,
            });
        }
        if !seen_b.insert(slot_b) {
            return Err(WeldFault::SlotPairedTwice {
                end: b,
                slot: slot_b,
            });
        }
    }
    Ok(())
}

// ---- spines ---------------------------------------------------------------

/// Why a chain of joints is not a spine the turn pass can compose.
///
/// The same three rules whether the joints come from an edit or a file: at
/// least one joint, every coordinate finite, and no joint repeating the point
/// above it — the node's own origin for the first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpineJointError {
    #[error("a spine needs at least one joint")]
    NoJoints,
    #[error("spine joint {0} has a coordinate that is not finite")]
    NonFinite(usize),
    #[error("spine joint {0} repeats the point above it")]
    Coincident(usize),
}

/// Hold a spine's joints to the spine rule.
///
/// A joint that repeats the point above it is a link with no direction, and
/// the arc-length assignment would divide by its length; a non-finite
/// coordinate spreads `NaN` into every vertex the spine touches.
pub(super) fn spine_joints(joints: &[[f32; 2]]) -> Result<(), SpineJointError> {
    if joints.is_empty() {
        return Err(SpineJointError::NoJoints);
    }
    // The link above `joints[0]` starts at the node's own origin, so that is
    // the point the first joint may not repeat.
    let mut above = [0.0f32, 0.0];
    for (i, joint) in joints.iter().enumerate() {
        if !joint[0].is_finite() || !joint[1].is_finite() {
            return Err(SpineJointError::NonFinite(i));
        }
        if joint == &above {
            return Err(SpineJointError::Coincident(i));
        }
        above = *joint;
    }
    Ok(())
}

// ---- bindings -------------------------------------------------------------

/// Why a binding's target cannot sit on a node of some kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindingFault {
    /// A deform on a node that carries no mesh to deform.
    DeformOnUnmeshed,
    /// A colour on a mesh group, which is never drawn and has no colour to
    /// fold it into.
    ColorOnMeshGroup(ScalarTarget),
}

/// Whether a binding with this target can sit on a node of this kind.
pub(super) fn binding_target(
    kind: &ModelNodeKind,
    target: BindingTarget,
) -> Result<(), BindingFault> {
    match target {
        BindingTarget::Deform
            if !matches!(kind, ModelNodeKind::Part(_) | ModelNodeKind::MeshGroup(_)) =>
        {
            Err(BindingFault::DeformOnUnmeshed)
        }
        BindingTarget::Scalar(t) if t.is_color() && matches!(kind, ModelNodeKind::MeshGroup(_)) => {
            Err(BindingFault::ColorOnMeshGroup(t))
        }
        _ => Ok(()),
    }
}

/// What is wrong with one deform cell's offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeformFault {
    Length { expected: usize, got: usize },
    NonFinite,
}

/// How every mutator reports a bad deform cell.
impl From<DeformFault> for ModelError {
    fn from(fault: DeformFault) -> Self {
        match fault {
            DeformFault::Length { expected, got } => Self::DeformLength { expected, got },
            DeformFault::NonFinite => Self::NonFinite("a deform offset"),
        }
    }
}

/// One deform cell holds one `[dx, dy]` per mesh vertex — `expected` is the
/// node's flat vertex-array length — and every offset is a number.
pub(super) fn deform_offsets(offsets: &[f32], expected: usize) -> Result<(), DeformFault> {
    if offsets.len() != expected {
        return Err(DeformFault::Length {
            expected,
            got: offsets.len(),
        });
    }
    if offsets.iter().any(|v| !v.is_finite()) {
        return Err(DeformFault::NonFinite);
    }
    Ok(())
}

/// Whether `positions` is one axis per param, each nonempty, finite, strictly
/// increasing and within `0..=1`, with a grid inside the binding-grid budget.
/// Checked before any grid is allocated.
pub(super) fn binding_positions_are_valid(params: usize, positions: &[Vec<f32>]) -> bool {
    if positions.len() != params
        || positions.iter().any(|axis| {
            axis.is_empty()
                || axis
                    .iter()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                || axis.windows(2).any(|pair| pair[0] >= pair[1])
        })
    {
        return false;
    }
    positions
        .iter()
        .try_fold(1u64, |n, axis| n.checked_mul(axis.len() as u64))
        .is_some_and(|n| n <= crate::load_budget::MAX_BINDING_GRID_CELLS)
}

// ---- nodes ----------------------------------------------------------------

/// Drop every motion response input that reads a slot `gone` says is going
/// away. A no-op on any other kind of node.
pub(super) fn drop_slot_inputs(kind: &mut ModelNodeKind, gone: impl Fn(&NodeId, &SlotId) -> bool) {
    if let ModelNodeKind::MotionResponse(response) = kind {
        response.inputs.retain(|input| {
            !matches!(
                &input.source,
                crate::ResponseSource::PartSlot { part, slot, .. } if gone(part, slot)
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(s: &str) -> NodeId {
        NodeId::new(s).unwrap()
    }

    fn slot(s: &str) -> SlotId {
        SlotId::new(s).unwrap()
    }

    /// One weld between `a` and `b` over the given pairs, both ends parts
    /// that carry every slot but `absent`.
    fn check<'a>(
        a: &'a NodeId,
        b: &'a NodeId,
        pairs: &'a [(SlotId, SlotId, f32)],
        welded: &mut Welded<'a>,
    ) -> Result<(), WeldFault<'a>> {
        weld(
            a,
            b,
            pairs.iter().map(|(a, b, w)| (a, b, *w)),
            welded,
            |_| WeldEnd::Part,
            |_, s| s.as_str() != "absent",
        )
    }

    #[test]
    fn a_weld_is_two_different_parts_once_over_slots_they_carry() {
        let (a, b) = (node("a"), node("b"));
        let good = [(slot("l"), slot("l"), 0.5), (slot("r"), slot("r"), 1.0)];
        let mut welded = Welded::default();
        assert_eq!(check(&a, &b, &good, &mut welded), Ok(()));
        // The same pair the other way round is the same weld.
        assert_eq!(check(&b, &a, &good, &mut welded), Err(WeldFault::Duplicate));
        assert_eq!(
            check(&a, &a, &good, &mut Welded::default()),
            Err(WeldFault::SelfPaired)
        );

        let nan = [(slot("l"), slot("l"), f32::NAN)];
        assert_eq!(
            check(&a, &b, &nan, &mut Welded::default()),
            Err(WeldFault::WeightOutOfRange { slot: &nan[0].0 })
        );
        let unknown = [(slot("l"), slot("absent"), 0.5)];
        assert_eq!(
            check(&a, &b, &unknown, &mut Welded::default()),
            Err(WeldFault::UnknownSlot {
                end: &b,
                slot: &unknown[0].1
            })
        );
        let twice = [(slot("l"), slot("l"), 0.5), (slot("r"), slot("l"), 0.5)];
        assert_eq!(
            check(&a, &b, &twice, &mut Welded::default()),
            Err(WeldFault::SlotPairedTwice {
                end: &b,
                slot: &twice[1].1
            })
        );
    }

    #[test]
    fn a_weld_end_is_a_part_or_a_fragments_requirement() {
        let (a, b) = (node("a"), node("b"));
        let run = |end: WeldEnd| {
            weld(
                &a,
                &b,
                [],
                &mut Welded::default(),
                |id| if id == &b { end } else { WeldEnd::Part },
                |_, _| true,
            )
        };
        assert_eq!(run(WeldEnd::Deferred), Ok(()));
        assert_eq!(
            run(WeldEnd::of(Some(&ModelNodeKind::Group), true)),
            Err(WeldFault::EndNotAPart {
                end: &b,
                kind: "group"
            })
        );
        assert_eq!(
            run(WeldEnd::of(None, false)),
            Err(WeldFault::EndMissing { end: &b })
        );
        assert_eq!(WeldEnd::of(None, true), WeldEnd::Deferred);
    }

    #[test]
    fn a_spine_needs_links_with_a_length() {
        assert_eq!(spine_joints(&[[0.0, -1.0], [0.0, -2.0]]), Ok(()));
        assert_eq!(spine_joints(&[]), Err(SpineJointError::NoJoints));
        assert_eq!(
            spine_joints(&[[0.0, f32::NAN]]),
            Err(SpineJointError::NonFinite(0))
        );
        // The first joint may not sit on the node's own origin.
        assert_eq!(
            spine_joints(&[[0.0, 0.0]]),
            Err(SpineJointError::Coincident(0))
        );
        assert_eq!(
            spine_joints(&[[0.0, -1.0], [0.0, -1.0]]),
            Err(SpineJointError::Coincident(1))
        );
    }

    #[test]
    fn a_deform_cell_is_sized_to_the_mesh_and_finite() {
        assert_eq!(deform_offsets(&[0.0; 8], 8), Ok(()));
        assert_eq!(
            deform_offsets(&[0.0; 6], 8),
            Err(DeformFault::Length {
                expected: 8,
                got: 6
            })
        );
        assert_eq!(
            deform_offsets(&[0.0, f32::INFINITY], 2),
            Err(DeformFault::NonFinite)
        );
    }
}
