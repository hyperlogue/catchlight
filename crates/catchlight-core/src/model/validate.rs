//! The rules more than one door holds a model to, each stated once.

use super::*;

// ---- welds ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WeldEnd {
    Part,
    NotAPart(&'static str),
    /// A missing node allowed to dangle: a fragment's weld into its base.
    Deferred,
    Missing,
}

impl WeldEnd {
    /// `may_dangle`: true for a fragment, false for a complete or merged model.
    pub(super) fn of(kind: Option<&ModelNodeKind>, may_dangle: bool) -> Self {
        match kind {
            Some(ModelNodeKind::Part(_)) => Self::Part,
            Some(other) => Self::NotAPart(other.name()),
            None if may_dangle => Self::Deferred,
            None => Self::Missing,
        }
    }

    pub(super) fn is_sound(self) -> bool {
        matches!(self, Self::Part | Self::Deferred)
    }
}

/// `end` is always the weld's own `a` or `b`.
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
    Duplicate,
    /// `slot` is the pair's slot on `a`.
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

/// The part pairs welded so far, unordered.
#[derive(Debug, Default)]
pub(super) struct Welded<'a>(HashSet<(&'a NodeId, &'a NodeId)>);

impl<'a> Welded<'a> {
    pub(super) fn with_capacity(welds: usize) -> Self {
        Self(HashSet::with_capacity(welds))
    }

    pub(super) fn insert(&mut self, a: &'a NodeId, b: &'a NodeId) -> bool {
        self.0.insert(if a < b { (a, b) } else { (b, a) })
    }
}

/// Hold one weld to the weld rule, and record it in `welded`. Whether a slot
/// is *filled* is not checked: resolving a weld skips unfilled pairs.
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
        // `contains` is false for `NaN`, so a non-number weight is refused too.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpineJointError {
    #[error("a spine needs at least one joint")]
    NoJoints,
    #[error("spine joint {0} has a coordinate that is not finite")]
    NonFinite(usize),
    #[error("spine joint {0} repeats the point above it")]
    Coincident(usize),
}

/// A repeated joint is a zero-length link the arc-length assignment would
/// divide by; a non-finite coordinate spreads `NaN` into every vertex.
pub(super) fn spine_joints(joints: &[[f32; 2]]) -> Result<(), SpineJointError> {
    if joints.is_empty() {
        return Err(SpineJointError::NoJoints);
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindingFault {
    DeformOnUnmeshed,
    ColorOnMeshGroup(ScalarTarget),
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DeformFault {
    Length { expected: usize, got: usize },
    NonFinite,
}

impl From<DeformFault> for ModelError {
    fn from(fault: DeformFault) -> Self {
        match fault {
            DeformFault::Length { expected, got } => Self::DeformLength { expected, got },
            DeformFault::NonFinite => Self::NonFinite("a deform offset"),
        }
    }
}

/// `expected` is the node's flat vertex-array length.
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

/// Positions must be one axis per param, each nonempty, finite, strictly
/// increasing and within `0..=1`. Checked before any grid is allocated.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementFault {
    ZOrder,
    Transform,
}

impl PlacementFault {
    pub(super) fn field(self) -> &'static str {
        match self {
            Self::ZOrder => "z-order",
            Self::Transform => "transform",
        }
    }
}

impl From<PlacementFault> for ModelError {
    fn from(fault: PlacementFault) -> Self {
        Self::NonFinite(match fault {
            PlacementFault::ZOrder => "a node's z-order",
            PlacementFault::Transform => "a node's transform",
        })
    }
}

pub(super) fn node_placement(z_order: f32, transform: &ClmTransform) -> Result<(), PlacementFault> {
    if !z_order.is_finite() {
        return Err(PlacementFault::ZOrder);
    }
    let finite = |values: &[f32]| values.iter().all(|v| v.is_finite());
    if !finite(&transform.translation) || !finite(&transform.rotation) || !finite(&transform.scale)
    {
        return Err(PlacementFault::Transform);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AppearanceFault {
    Opacity,
    Tint,
    ScreenTint,
    MaskThreshold,
}

impl AppearanceFault {
    pub(super) fn field(self) -> &'static str {
        match self {
            Self::Opacity => "opacity",
            Self::Tint => "tint",
            Self::ScreenTint => "screen tint",
            Self::MaskThreshold => "mask threshold",
        }
    }
}

impl From<AppearanceFault> for ModelError {
    fn from(fault: AppearanceFault) -> Self {
        Self::NonFinite(match fault {
            AppearanceFault::Opacity => "a node's opacity",
            AppearanceFault::Tint => "a node's tint",
            AppearanceFault::ScreenTint => "a node's screen tint",
            AppearanceFault::MaskThreshold => "a node's mask threshold",
        })
    }
}

/// Only parts and composites are drawn with a look of their own.
pub(super) fn appearance(kind: &ModelNodeKind) -> Result<(), AppearanceFault> {
    let (opacity, tint, screen_tint, mask_threshold) = match kind {
        ModelNodeKind::Part(p) => (p.opacity, p.tint, p.screen_tint, p.mask_threshold),
        ModelNodeKind::Composite(c) => (c.opacity, c.tint, c.screen_tint, c.mask_threshold),
        _ => return Ok(()),
    };
    if !opacity.is_finite() {
        return Err(AppearanceFault::Opacity);
    }
    if tint.iter().any(|v| !v.is_finite()) {
        return Err(AppearanceFault::Tint);
    }
    if screen_tint.iter().any(|v| !v.is_finite()) {
        return Err(AppearanceFault::ScreenTint);
    }
    if !mask_threshold.is_finite() {
        return Err(AppearanceFault::MaskThreshold);
    }
    Ok(())
}

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
    fn a_nodes_placement_is_finite() {
        let identity = ClmTransform::default();
        assert_eq!(node_placement(0.0, &identity), Ok(()));
        assert_eq!(
            node_placement(f32::NAN, &identity),
            Err(PlacementFault::ZOrder)
        );
        let poisons: [fn(&mut ClmTransform); 3] = [
            |t| t.translation[2] = f32::NAN,
            |t| t.rotation[0] = f32::INFINITY,
            |t| t.scale[1] = f32::NEG_INFINITY,
        ];
        for poison in poisons {
            let mut bad = identity;
            poison(&mut bad);
            assert_eq!(node_placement(0.0, &bad), Err(PlacementFault::Transform));
        }
    }

    #[test]
    fn a_nodes_appearance_is_finite() {
        assert_eq!(appearance(&ModelNodeKind::Group), Ok(()));
        assert_eq!(
            appearance(&ModelNodeKind::Composite(ModelComposite::new())),
            Ok(())
        );
        let poisons: [(fn(&mut ModelComposite), AppearanceFault); 4] = [
            (|c| c.opacity = f32::NAN, AppearanceFault::Opacity),
            (|c| c.tint[1] = f32::INFINITY, AppearanceFault::Tint),
            (
                |c| c.screen_tint[2] = f32::NEG_INFINITY,
                AppearanceFault::ScreenTint,
            ),
            (
                |c| c.mask_threshold = f32::NAN,
                AppearanceFault::MaskThreshold,
            ),
        ];
        for (poison, fault) in poisons {
            let mut bad = ModelComposite::new();
            poison(&mut bad);
            assert_eq!(appearance(&ModelNodeKind::Composite(bad)), Err(fault));
        }
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
