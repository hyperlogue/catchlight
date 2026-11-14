//! Spines: a chain of joints drawn on the art, turning the geometry beneath.
//!
//! **Bend 0 is the art.** A spine holds no cells and no grid; a link's bend is
//! its param's value read straight off, in half turns, and with every bend at
//! zero the spine's contribution is exactly `Vec2::ZERO` on every vertex. The
//! pass returns before it writes anything in that case, so an unbent spine
//! leaves its deform source inactive and nothing downstream re-uploads.
//!
//! **A link's bend turns at the joint above it.** `joints[i]` is the far end
//! of link `i`, so link 0 runs from the node's own origin to `joints[0]`; the
//! hinge link `i` turns about is the node's origin for link 0 and
//! `joints[i - 1]` after that. Positive bend turns a downward link toward +X:
//! the turn is `bend * pi` counterclockwise in the model's Y-up frame,
//! which is the convention `ParticleChainData::link_bends` reports its bends
//! in, so the chain a spine carries writes the params the spine reads.
//!
//! **The joint path is composed; the visible strand rounds its corners.**
//! The physical links still turn rigidly about their parent joints, root to
//! tip. Between adjacent link midpoints, the art follows a quadratic Bezier
//! whose middle control is their shared joint. These nonnegative weights keep
//! the curve within the joint path's convex hull: a hanging length cannot
//! curl outward and back merely because its upper link changed direction.
//! The root and tip are exact; interior joints are controls, not interpolation
//! points. Rounding slightly shortens the visible path rather than inflating
//! it into the spiral made by a partial rotation about each hinge.
//!
//! **Transport the art from its rest curve to its posed curve.** Each baked
//! link coordinate samples both curves. Their tangent difference rotates the
//! vertex's offset from the rest curve, then translates it onto the posed
//! curve. Width and pre-existing deforms therefore travel with the strand.
//! The root cross-section remains attached, with its orientation eased into
//! the first link over that link's first half. Art above the root stays fixed;
//! art beyond the tip follows the last link. All-zero bends remain exactly
//! inactive. This is O(N) per spine and O(1) per vertex.
//!
//! **This is surface reconstruction, not another force.** The simulated
//! particles, their lengths, gravity and inertia are untouched. The authored
//! guide must follow the artwork: a painted tip far from its guide will still
//! be offset from the simulated tip. Coarse triangles and discontinuous rest
//! assignments can still create visible corners; sufficiently sampled art and
//! accurate guides are required.
//!
//! **The guide is embedded in the art.** The bake binds the root and every
//! joint to the rest triangle of the spine's guide part (its largest meshed
//! part) that holds it, or the nearest one, as barycentric weights. Each fold,
//! before the turn, the posed guide is those points read through the part's
//! current deform (every source but this spine's own) and its current
//! transform, in spine space. The transport runs from the posed guide, not the
//! drawing, so a keyform that reshapes or moves the art moves the curve the
//! bends turn it about. Where no guide point moves off the drawing by more
//! than rounding (`GUIDE_STILL_SQ`), the posed guide is left empty and reads
//! as the drawing exactly; with no deform and the part placed as at rest,
//! reading it costs one transform compare. A chain reads the posed guide as
//! its root offset and shape target (`Puppet::chain_root`,
//! `Puppet::chain_posed_bends`); bends stay relative to the guide. The physics
//! pre-pass poses it for the frame it steps (`pose_chain_guides`), so the
//! strand and the transport agree on the same frame's guide.
//!
//! **The assignment comes from rest geometry, once per bake.** Each descendant
//! vertex is mapped into spine space through the rest transforms and projected
//! onto the nearest link; the arc length `s` to that projection gives the link
//! `k` whose span contains it and the ramp `f = clamp((s - S_k) / L_k, 0, 1)`.
//! A vertex above the root projects to the root and lands on `k = 0, f = 0`,
//! whose transform is the identity, so it does not move; one past the tip
//! lands on the last link at `f = 1` and turns with it in full. The assignment
//! never moves after that — it describes the art as drawn, and re-deriving it
//! from a bent pose would let the shape creep.
//!
//! **The pass runs before the mesh groups, and the mesh groups warp its
//! output.** A spine writes `DeformSource::Node(spine_idx)` on each meshed
//! descendant, reading that node's current deform sum the way
//! `meshgroup::propagate_to_child` does — clear the source, read the rest, add
//! the turn. A mesh group above a spine reaches those same parts and folds
//! from their current position, so the group warps art the spine has already
//! bent.
//!
//! **Descent halts at a mesh group and at a nested spine.** A mesh group
//! carries the deform to its own children, so binding them here too would
//! apply it twice — the same rule `meshgroup::descendant_meshed_nodes` keeps.
//! A composite with `propagate_mesh_group` unset halts the walk for the same
//! reason it halts a group's. **Nested spines do not compose exactly:** an
//! outer spine's walk stops at an inner one, so the subtree below the inner
//! spine turns with the inner bends alone and the outer spine's rotations
//! never reach it. Rig one spine per strand until that changes.

#![doc = include_str!("spine_width.md")]

use std::collections::HashMap;

use glam::{Affine2, Mat2, Vec2};

use crate::components::{checked_affine_inverse, NodeIdx, NodeKind};
use crate::deform::DeformSource;
use crate::meshgroup::affine2_from_mat4;
use crate::node::NodeTree;
use crate::puppet::{Arena, GlobalTransforms};

/// Which link turns one vertex, and how far into that link it sits.
///
/// Derived from rest geometry at bake. `ramp` is already clamped to `0..=1`,
/// and `link` always indexes a real link, so the per-frame loop reads both
/// without a bounds decision of its own.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VertexBend {
    pub(crate) link: u32,
    pub(crate) ramp: f32,
}

/// One guide point bound into the guide part's rest mesh: its part-local
/// rest position and the triangle corners and weights it reads the part's
/// deform through. A point outside the mesh reads the deform at its nearest
/// point on the nearest triangle, with weights in [0, 1], so a sliver
/// triangle cannot amplify a small keyform into a large move.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GuidePoint {
    rest: Vec2,
    corners: [u32; 3],
    weights: [f32; 3],
}

/// The spine's guide, bound into its guide part at bake.
#[derive(Debug, Clone)]
pub(crate) struct GuideBinding {
    part: NodeIdx,
    /// The part's local space to the spine's, at rest.
    rest_to_spine: Affine2,
    /// The root, then each joint.
    points: Vec<GuidePoint>,
}

/// One spine's per-vertex assignment, keyed by the node the vertices belong
/// to, and its guide. Empty until the bake fills it.
#[derive(Debug, Clone, Default)]
pub(crate) struct SpinePins {
    pub(crate) per_child: HashMap<NodeIdx, Vec<VertexBend>>,
    pub(crate) guide: Option<GuideBinding>,
}

/// The runtime half of a spine: the polyline as drawn, this frame's bends,
/// the assignment the bake derived, and the particle chain the spine may
/// carry.
///
/// `bends` is written from the params every frame the fold runs, so nothing
/// here needs resetting; `joints` and `pins` are rest data a rebake replaces.
#[derive(Debug, Clone, Default)]
pub struct SpineData {
    /// The far end of each link, in the node's own space. `joints[0]` ends the
    /// link that starts at the node itself.
    pub joints: Vec<Vec2>,
    /// This frame's bend per link, in half turns. As long as `joints`.
    pub bends: Vec<f32>,
    /// The simulation hung on these joints, if the author asked for one. Its
    /// links carry the joints' lengths and drawn directions, defining bend
    /// zero. Gravity, spring targets and limits determine the settled pose.
    pub chain: Option<crate::physics::ParticleChainData>,
    pub(crate) pins: SpinePins,
    /// The last fold's posed guide in the node's own space: the root, then
    /// each joint. Empty reads as the drawing, the root at the origin. What
    /// the transport turns the art about.
    pub(crate) posed_guide: Vec<Vec2>,
    /// The guide as the physics pre-pass posed it, in the same layout: what
    /// a chain hangs from and targets. Kept apart from `posed_guide` so the
    /// final fold, which runs under the caller's root, never feeds rounding
    /// noise back into the strand's input and keeps it awake.
    pub(crate) chain_guide: Vec<Vec2>,
}

impl SpineData {
    /// A spine over `joints`, unbent and carrying nothing.
    pub fn new(joints: Vec<Vec2>) -> Self {
        let bends = vec![0.0; joints.len()];
        Self {
            joints,
            bends,
            chain: None,
            pins: SpinePins::default(),
            posed_guide: Vec::new(),
            chain_guide: Vec::new(),
        }
    }

    /// The rod length and drawn direction of each link, in the physics
    /// (Y-down) frame: link `i` runs from the joint above it — the node's own
    /// origin for the first — to `joints[i]`.
    ///
    /// This is the whole of the geometry a chain needs, which is why a chain
    /// stores none of its own: the drawing is the shape it hangs in.
    pub fn link_geometry(&self) -> impl Iterator<Item = (f32, Vec2)> + '_ {
        let mut above = Vec2::ZERO;
        self.joints.iter().map(move |&joint| {
            let v = joint - above;
            above = joint;
            // Model space is Y-up and the solver is Y-down.
            let flipped = Vec2::new(v.x, -v.y);
            let length = flipped.length();
            let drawn = if length > 1e-12 {
                flipped / length
            } else {
                Vec2::new(0.0, 1.0)
            };
            (length, drawn)
        })
    }

    /// The last render fold's posed guide in the node's own space, root
    /// first, then each joint; empty while it is the drawing. The transport
    /// turns the art about it; chains read the pre-pass's copy instead.
    pub fn posed_guide(&self) -> &[Vec2] {
        &self.posed_guide
    }

    /// The part the guide is embedded in, if the spine has one.
    pub(crate) fn guide_part(&self) -> Option<NodeIdx> {
        self.pins.guide.as_ref().map(|g| g.part)
    }

    /// The guide's root in the node's own space as the physics pre-pass
    /// posed it: where a chain the spine carries hangs from.
    pub fn guide_root(&self) -> Vec2 {
        self.chain_guide.first().copied().unwrap_or(Vec2::ZERO)
    }

    /// How far the pre-pass's guide turns link `k` off the drawing, in half
    /// turns relative to the link above: the bend that would draw the guide's
    /// shape, in the convention the params use. Zero while the guide is the
    /// drawing.
    pub(crate) fn guide_turn(&self, k: usize) -> f32 {
        if self.chain_guide.len() != self.joints.len() + 1 || k >= self.joints.len() {
            return 0.0;
        }
        let absolute = |k: usize| {
            let above = if k == 0 {
                Vec2::ZERO
            } else {
                self.joints[k - 1]
            };
            let drawn = self.joints[k] - above;
            let posed = self.chain_guide[k + 1] - self.chain_guide[k];
            let turn = drawn.perp_dot(posed).atan2(drawn.dot(posed));
            if turn.is_finite() {
                turn
            } else {
                0.0
            }
        };
        let parent = if k == 0 { 0.0 } else { absolute(k - 1) };
        // Each absolute turn is in (-pi, pi]; their difference is wrapped back
        // into that range, so links either side of a half turn read a small
        // relative turn rather than nearly a full one.
        let turn = absolute(k) - parent;
        let turn = turn - std::f32::consts::TAU * (turn / std::f32::consts::TAU).round();
        turn / std::f32::consts::PI
    }
}

/// A rotation of `angle` radians about `pivot`, counterclockwise in a Y-up
/// frame.
fn rotate_about(pivot: Vec2, angle: f32) -> Affine2 {
    let r = Mat2::from_angle(angle);
    Affine2::from_mat2_translation(r, pivot - r * pivot)
}

/// One physical link in its rest and composed frames. Adjacent links supply
/// the three controls used to reconstruct the visible curve.
#[derive(Debug, Clone, Copy)]
struct ComposedLink {
    rest_start: Vec2,
    rest_end: Vec2,
    posed_start: Vec2,
    posed_end: Vec2,
}

/// Compose the joints root to tip, once per spine per frame.
///
/// `root` and `joints` are the curve the art sits on: the drawing, or the
/// posed guide. Returns `None` when every bend is zero, which is the whole
/// spine's identity and the caller's cue to write nothing.
fn compose(root: Vec2, joints: &[Vec2], bends: &[f32]) -> Option<Vec<ComposedLink>> {
    let n = joints.len();
    let angle = |k: usize| {
        let b = bends.get(k).copied().unwrap_or(0.0);
        if b.is_finite() {
            b * std::f32::consts::PI
        } else {
            0.0
        }
    };
    if n == 0 || (0..n).all(|k| angle(k) == 0.0) {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    let mut above = Affine2::IDENTITY;
    for k in 0..n {
        // The hinge above link k, as drawn: the root for the first link, the
        // joint that ends the link above it after that.
        let rest_hinge = if k == 0 { root } else { joints[k - 1] };
        let hinge = above.transform_point2(rest_hinge);
        let angle = angle(k);
        above = rotate_about(hinge, angle) * above;
        out.push(ComposedLink {
            rest_start: rest_hinge,
            rest_end: joints[k],
            posed_start: hinge,
            posed_end: above.transform_point2(joints[k]),
        });
    }
    Some(out)
}

/// Round the joint path with quadratic Beziers between link midpoints.
/// The curve stays inside the control polygon: a vertical hanging section
/// cannot acquire the outward curl produced by rotating vertices about a
/// hinge through only a fraction of its angle. Root and tip remain exact;
/// internal joints are controls, not points the visible curve must pass through.
fn curve_sample(links: &[ComposedLink], bend: VertexBend, posed: bool) -> (Vec2, Vec2) {
    let endpoints = |k: usize| {
        let l = &links[k];
        if posed {
            (l.posed_start, l.posed_end)
        } else {
            (l.rest_start, l.rest_end)
        }
    };
    let k = bend.link as usize;
    let t = bend.ramp;
    let (start, end) = endpoints(k);
    let corner = if t < 0.5 && k > 0 {
        Some((endpoints(k - 1).0, start, end, t + 0.5))
    } else if t >= 0.5 && k + 1 < links.len() {
        Some((start, end, endpoints(k + 1).1, t - 0.5))
    } else {
        None
    };
    if let Some((before, joint, after, u)) = corner {
        let a = (before + joint) * 0.5;
        let c = (joint + after) * 0.5;
        let left = a.lerp(joint, u);
        let right = joint.lerp(c, u);
        let tangent = right - left;
        (
            left.lerp(right, u),
            if tangent.length_squared() > 1e-12 {
                tangent
            } else {
                end - start
            },
        )
    } else {
        (start.lerp(end, t), end - start)
    }
}

fn curve_transform(links: &[ComposedLink], bend: VertexBend) -> Affine2 {
    if bend.link == 0 && bend.ramp == 0.0 {
        return Affine2::IDENTITY;
    }
    let (rest, rest_tangent) = curve_sample(links, bend, false);
    let (posed, posed_tangent) = curve_sample(links, bend, true);
    // atan2 remains accurate for the tiny bends seen near rest; an acos of
    // the normalized dot loses their magnitude to f32 cancellation.
    let mut angle = rest_tangent
        .perp_dot(posed_tangent)
        .atan2(rest_tangent.dot(posed_tangent));
    // Keep the attachment cross-section fixed, blending its orientation into
    // the first link. The centre line still follows the simulated path.
    if bend.link == 0 && bend.ramp < 0.5 {
        let t = 2.0 * bend.ramp;
        angle *= t * t * (3.0 - 2.0 * t);
    }
    let rotation = Mat2::from_angle(angle);
    Affine2::from_mat2_translation(rotation, posed - rotation * rest)
}

/// Arc length at the head of each link, and each link's own length. `out[k]`
/// is `(S_k, L_k)`.
fn spans(joints: &[Vec2]) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(joints.len());
    let mut here = Vec2::ZERO;
    let mut travelled = 0.0f32;
    for &joint in joints {
        let length = (joint - here).length();
        out.push((travelled, length));
        travelled += length;
        here = joint;
    }
    out
}

/// Where one rest vertex sits on the polyline: the nearest link by distance,
/// then the link whose span holds the arc length of that projection.
///
/// The projection is clamped to its link, so a vertex above the root lands on
/// the root at `s = 0` — link 0 at ramp 0, whose transform is the identity —
/// and one past the tip lands on the last link at ramp 1.
fn assign(joints: &[Vec2], spans: &[(f32, f32)], p: Vec2) -> VertexBend {
    let mut best = (f32::INFINITY, 0.0f32);
    let mut here = Vec2::ZERO;
    for (k, &joint) in joints.iter().enumerate() {
        let (start, length) = spans[k];
        let along = joint - here;
        let t = if length > 0.0 {
            ((p - here).dot(along) / (length * length)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let distance = (here + along * t).distance_squared(p);
        if distance < best.0 {
            best = (distance, start + t * length);
        }
        here = joint;
    }
    let s = best.1;
    // The last link whose head is at or before `s`, so the tip's own arc
    // length reads as the end of the last link rather than the start of one
    // past it.
    let mut link = 0usize;
    for (k, &(start, _)) in spans.iter().enumerate() {
        if s >= start {
            link = k;
        }
    }
    let (start, length) = spans[link];
    let ramp = if length > 0.0 {
        ((s - start) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    VertexBend {
        link: link as u32,
        ramp,
    }
}

/// Meshed descendants a spine turns: recurse through parts and composites,
/// collect parts and mesh groups, and stop at each mesh group and each nested
/// spine. See the module doc for why each halt is there.
fn descendant_turned_nodes(tree: &NodeTree, spine_id: NodeIdx, arena: &Arena) -> Vec<NodeIdx> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeIdx> = tree.get_children(spine_id);
    while let Some(id) = stack.pop() {
        match arena.get(id).map(|n| &n.kind) {
            Some(NodeKind::Part(_)) => {
                out.push(id);
                stack.extend(tree.get_children(id));
            }
            Some(NodeKind::Composite(c)) => {
                if c.propagate_mesh_group {
                    stack.extend(tree.get_children(id));
                }
            }
            Some(NodeKind::MeshGroup(_)) => out.push(id),
            _ => {}
        }
    }
    out
}

/// Pre-order among spines, so an outer spine runs before an inner one. The
/// two do not compose (see the module doc), but the order still decides which
/// writes its source first and keeps the pass deterministic.
fn spine_pre_order(arena: &Arena) -> Vec<NodeIdx> {
    arena.tree.with_dfs_order(|dfs| {
        dfs.iter()
            .copied()
            .filter(|id| matches!(arena.get(*id).map(|n| &n.kind), Some(NodeKind::Spine(_))))
            .collect()
    })
}

/// Derive every spine's per-vertex assignment from the arena's rest pose.
///
/// `transforms` must be the rest globals — the caller resets the dynamic state
/// and computes them immediately before calling, exactly as
/// `Arena::rebuild_all_mesh_group_pins` does.
pub(crate) fn bake_spine_pins(
    arena: &Arena,
    transforms: &GlobalTransforms,
    spine_id: NodeIdx,
) -> SpinePins {
    let mut pins = SpinePins::default();
    let Some(node) = arena.get(spine_id) else {
        return pins;
    };
    let NodeKind::Spine(spine) = &node.kind else {
        return pins;
    };
    if spine.joints.is_empty() {
        return pins;
    }
    let spans = spans(&spine.joints);
    let Some(spine_global_inv) = checked_affine_inverse(transforms.get(spine_id)) else {
        return pins;
    };
    for child_id in descendant_turned_nodes(&arena.tree, spine_id, arena) {
        let Some(child) = arena.get(child_id) else {
            continue;
        };
        let (verts, origin): (&[Vec2], Vec2) = match &child.kind {
            NodeKind::Part(p) => (&p.mesh.vertices, p.mesh.origin),
            NodeKind::MeshGroup(mg) => (&mg.mesh.vertices, mg.mesh.origin),
            _ => continue,
        };
        let child_to_spine = affine2_from_mat4(spine_global_inv * transforms.get(child_id));
        let assigned = verts
            .iter()
            .map(|v| {
                assign(
                    &spine.joints,
                    &spans,
                    child_to_spine.transform_point2(*v - origin),
                )
            })
            .collect();
        pins.per_child.insert(child_id, assigned);
    }
    pins.guide = bind_guide(arena, transforms, spine_id, &spine.joints, spine_global_inv);
    pins
}

/// Bind the root and joints into the spine's guide part: the meshed part
/// under it with the largest rest area in spine space.
fn bind_guide(
    arena: &Arena,
    transforms: &GlobalTransforms,
    spine_id: NodeIdx,
    joints: &[Vec2],
    spine_global_inv: glam::Mat4,
) -> Option<GuideBinding> {
    let mut best: Option<(f32, NodeIdx, Affine2)> = None;
    for child_id in descendant_turned_nodes(&arena.tree, spine_id, arena) {
        let Some(NodeKind::Part(p)) = arena.get(child_id).map(|n| &n.kind) else {
            continue;
        };
        let to_spine = affine2_from_mat4(spine_global_inv * transforms.get(child_id));
        let v = |i: u32| {
            p.mesh
                .vertices
                .get(i as usize)
                .map(|v| to_spine.transform_point2(*v - p.mesh.origin))
        };
        let indices: Vec<u32> = p.mesh.indices.iter_u32().collect();
        let area: f32 = indices
            .as_chunks::<3>()
            .0
            .iter()
            .filter_map(|&[a, b, c]| Some((v(b)? - v(a)?).perp_dot(v(c)? - v(a)?).abs()))
            .sum();
        // A broken mesh counts as no area rather than poisoning the choice.
        // Equal areas keep the first part in walk order, which is the tree's
        // child order, so the choice is stable until the art itself changes.
        let area = if area.is_finite() { area } else { 0.0 };
        if best.is_none_or(|(a, ..)| area > a) {
            best = Some((area, child_id, to_spine));
        }
    }
    let (area, part, rest_to_spine) = best?;
    if area.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let NodeKind::Part(p) = &arena.get(part)?.kind else {
        return None;
    };
    let local: Vec<Vec2> = p.mesh.vertices.iter().map(|v| *v - p.mesh.origin).collect();
    let indices: Vec<u32> = p.mesh.indices.iter_u32().collect();
    let to_part = rest_to_spine.inverse();
    let points = std::iter::once(Vec2::ZERO)
        .chain(joints.iter().copied())
        .map(|at| {
            let q = to_part.transform_point2(at);
            bind_point(&local, &indices, q)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(GuideBinding {
        part,
        rest_to_spine,
        points,
    })
}

/// The triangle that holds `q`, or the nearest one, with `q`'s weights in it.
fn bind_point(local: &[Vec2], indices: &[u32], q: Vec2) -> Option<GuidePoint> {
    let mut best: Option<(f32, GuidePoint)> = None;
    for &corners in indices.as_chunks::<3>().0 {
        let [a, b, c] = corners.map(|i| local.get(i as usize).copied());
        let (Some(a), Some(b), Some(c)) = (a, b, c) else {
            continue;
        };
        let inside = crate::meshgroup::barycentric(q, a, b, c);
        if !inside.iter().all(|w| w.is_finite()) {
            continue;
        }
        // Outside every triangle, read the deform at the nearest point on
        // this one: extrapolated weights would scale a sliver triangle's
        // deform by many times its size.
        let (distance, weights) = if inside.iter().all(|w| *w >= 0.0) {
            (0.0, inside)
        } else {
            let (distance, nearest) = [(a, b), (b, c), (c, a)]
                .into_iter()
                .map(|(p0, p1)| {
                    let d = p1 - p0;
                    let t = ((q - p0).dot(d) / d.length_squared().max(1e-12)).clamp(0.0, 1.0);
                    let at = p0 + d * t;
                    (at.distance_squared(q), at)
                })
                .fold((f32::INFINITY, q), |m, e| if e.0 < m.0 { e } else { m });
            let weights = crate::meshgroup::barycentric(nearest, a, b, c);
            if !weights.iter().all(|w| w.is_finite()) {
                continue;
            }
            (distance, weights.map(|w| w.clamp(0.0, 1.0)))
        };
        if best.is_none_or(|(d, _)| distance < d) {
            best = Some((
                distance,
                GuidePoint {
                    rest: q,
                    corners,
                    weights,
                },
            ));
        }
        if distance == 0.0 {
            break;
        }
    }
    best.map(|(_, p)| p)
}

/// Squared displacement (px²) under which every guide point counts as still
/// on the drawing: the rounding of composing a turned parent's transforms.
const GUIDE_STILL_SQ: f32 = 1e-8;

/// This fold's posed guide for `spine_id`: the bound points read through the
/// guide part's deform (all but the spine's own source) and its current
/// placement, in spine space. Empty when no point moves off the drawing by
/// more than rounding, so the drawing is read exactly; otherwise the drawing
/// plus each point's displacement.
fn pose_guide(
    arena: &Arena,
    transforms: &GlobalTransforms,
    spine_id: NodeIdx,
    spine_global_inv: glam::Mat4,
    out: &mut Vec<Vec2>,
) {
    out.clear();
    let Some(NodeKind::Spine(spine)) = arena.get(spine_id).map(|n| &n.kind) else {
        return;
    };
    let Some(guide) = &spine.pins.guide else {
        return;
    };
    let Some(NodeKind::Part(part)) = arena.get(guide.part).map(|n| &n.kind) else {
        return;
    };
    let now = affine2_from_mat4(spine_global_inv * transforms.get(guide.part));
    let stack = &part.deform_stack;
    let deformed = stack.is_active();
    if now == guide.rest_to_spine && !deformed {
        return;
    }
    let own = DeformSource::Node(spine_id);
    let mut moved = false;
    for (k, point) in guide.points.iter().enumerate() {
        let mut delta =
            now.transform_point2(point.rest) - guide.rest_to_spine.transform_point2(point.rest);
        if deformed {
            let offset = point
                .corners
                .iter()
                .zip(point.weights)
                .fold(Vec2::ZERO, |sum, (&i, w)| {
                    sum + stack.active_offset_except(own, i as usize) * w
                });
            delta += now.matrix2 * offset;
        }
        if !delta.is_finite() {
            delta = Vec2::ZERO;
        }
        moved |= delta.length_squared() > GUIDE_STILL_SQ;
        let drawn = if k == 0 {
            Vec2::ZERO
        } else {
            spine.joints[k - 1]
        };
        out.push(drawn + delta);
    }
    if !moved {
        out.clear();
    }
}

/// Pose every chain spine's guide against `transforms` (which must hold the
/// spine and its guide part) and the guide parts' current deforms: the
/// physics pre-pass's half of what `propagate_spine_deforms` records.
pub(crate) fn pose_chain_guides(arena: &mut Arena, transforms: &GlobalTransforms) {
    let mut guide = Vec::new();
    for c in 0..arena.chain_node_ids.len() {
        let spine_id = arena.chain_node_ids[c];
        let Some(spine_global_inv) = checked_affine_inverse(transforms.get(spine_id)) else {
            continue;
        };
        pose_guide(arena, transforms, spine_id, spine_global_inv, &mut guide);
        if let Some(NodeKind::Spine(spine)) = arena.get_mut(spine_id).map(|n| &mut n.kind) {
            std::mem::swap(&mut spine.chain_guide, &mut guide);
        }
    }
}

/// Turn every spine's descendants by this frame's bends.
///
/// Runs in its own pre-order pass after the transforms are computed and before
/// the mesh groups, and writes `DeformSource::Node(spine_id)` on each meshed
/// descendant. `transforms` is read only: a spine moves vertices, never nodes.
pub(crate) fn propagate_spine_deforms(arena: &mut Arena, transforms: &GlobalTransforms) {
    let _span = tracing::debug_span!("propagate_spine_deforms").entered();
    let order = spine_pre_order(arena);
    let mut scratch = std::mem::take(&mut arena.spine_scratch);
    let mut cur_deform = std::mem::take(&mut arena.spine_cur_deform_scratch);
    let mut child_ids: smallvec::SmallVec<[NodeIdx; 8]> = smallvec::SmallVec::new();
    let mut guide = Vec::new();

    for spine_id in order {
        child_ids.clear();
        {
            let Some(node) = arena.get(spine_id) else {
                continue;
            };
            let NodeKind::Spine(spine) = &node.kind else {
                continue;
            };
            child_ids.extend(spine.pins.per_child.keys().copied());
        }
        let Some(spine_global_inv) = checked_affine_inverse(transforms.get(spine_id)) else {
            continue;
        };
        pose_guide(arena, transforms, spine_id, spine_global_inv, &mut guide);
        if let Some(NodeKind::Spine(spine)) = arena.get_mut(spine_id).map(|n| &mut n.kind) {
            std::mem::swap(&mut spine.posed_guide, &mut guide);
        }
        // Composed once per spine, not once per child: the joints move
        // together and the per-vertex loop only samples the result.
        let links = {
            let Some(NodeKind::Spine(spine)) = arena.get(spine_id).map(|n| &n.kind) else {
                continue;
            };
            let composed = match spine.posed_guide.split_first() {
                Some((&root, joints)) => compose(root, joints, &spine.bends),
                None => compose(Vec2::ZERO, &spine.joints, &spine.bends),
            };
            match composed {
                Some(links) => links,
                // Every bend at zero is the art as drawn. The sources were
                // dropped above, so leaving them inactive is exactly zero.
                None => {
                    for &child_id in &child_ids {
                        clear_turn(arena, spine_id, child_id);
                    }
                    continue;
                }
            }
        };
        for &child_id in &child_ids {
            let child_global = transforms.get(child_id);
            let Some(child_global_inv) = checked_affine_inverse(child_global) else {
                continue;
            };
            let child_to_spine = affine2_from_mat4(spine_global_inv * child_global);
            let spine_to_child = affine2_from_mat4(child_global_inv * transforms.get(spine_id));
            turn_child(
                arena,
                spine_id,
                child_id,
                &links,
                &mut scratch,
                &mut cur_deform,
                child_to_spine,
                spine_to_child,
            );
        }
    }

    arena.spine_scratch = scratch;
    arena.spine_cur_deform_scratch = cur_deform;
}

/// Drop one spine's turn from a child's stack without writing a new one — the
/// unbent case, where the contribution is exactly zero.
fn clear_turn(arena: &mut Arena, spine_id: NodeIdx, child_id: NodeIdx) {
    let Some(child) = arena.get_mut(child_id) else {
        return;
    };
    match &mut child.kind {
        NodeKind::Part(p) => p.deform_stack.clear_source(DeformSource::Node(spine_id)),
        NodeKind::MeshGroup(mg) => mg.deform_stack.clear_source(DeformSource::Node(spine_id)),
        _ => {}
    }
}

/// One spine, one meshed descendant: clear the spine's own source, read what
/// is left, turn each vertex from where it currently sits, and write the
/// difference back into the source.
///
/// Reading the current position rather than the rest one is what lets a
/// vertex's own deform bindings and the spine's turn compose: the spine bends
/// the art as it stands this frame, not as it was drawn.
#[allow(clippy::too_many_arguments)]
fn turn_child(
    arena: &mut Arena,
    spine_id: NodeIdx,
    child_id: NodeIdx,
    links: &[ComposedLink],
    scratch: &mut Vec<Vec2>,
    cur_deform: &mut Vec<Vec2>,
    child_to_spine: Affine2,
    spine_to_child: Affine2,
) {
    let cur_len = {
        let Some(child) = arena.get_mut(child_id) else {
            return;
        };
        let stack = match &mut child.kind {
            NodeKind::Part(p) => &mut p.deform_stack,
            NodeKind::MeshGroup(mg) => &mut mg.deform_stack,
            _ => return,
        };
        // Dropping the source first keeps every early return below correct:
        // a path that bails leaves this spine's stale turn dropped, exactly
        // as if it had never run.
        stack.clear_source(DeformSource::Node(spine_id));
        stack.sum_active_into(cur_deform);
        cur_deform.len()
    };

    {
        let Some(NodeKind::Spine(spine)) = arena.get(spine_id).map(|n| &n.kind) else {
            return;
        };
        let Some(pins) = spine.pins.per_child.get(&child_id) else {
            return;
        };
        let Some(child_node) = arena.get(child_id) else {
            return;
        };
        let (verts, origin): (&[Vec2], Vec2) = match &child_node.kind {
            NodeKind::Part(p) => (&p.mesh.vertices, p.mesh.origin),
            NodeKind::MeshGroup(mg) => (&mg.mesh.vertices, mg.mesh.origin),
            _ => return,
        };
        if cur_len != verts.len() || pins.len() != verts.len() {
            return;
        }

        scratch.clear();
        scratch.reserve(verts.len());
        for (i, &base_v) in verts.iter().enumerate() {
            let current = base_v - origin + cur_deform[i];
            let bend = pins[i];
            if bend.link as usize >= links.len() {
                scratch.push(Vec2::ZERO);
                continue;
            }
            let turn = curve_transform(links, bend);
            let turned = spine_to_child
                .transform_point2(turn.transform_point2(child_to_spine.transform_point2(current)));
            let offset = turned - current;
            scratch.push(if offset.is_finite() {
                offset
            } else {
                Vec2::ZERO
            });
        }
    }

    let Some(child) = arena.get_mut(child_id) else {
        return;
    };
    let stack = match &mut child.kind {
        NodeKind::Part(p) => &mut p.deform_stack,
        NodeKind::MeshGroup(mg) => &mut mg.deform_stack,
        _ => return,
    };
    if scratch.len() == stack.vert_count {
        stack
            .source_buf_mut(DeformSource::Node(spine_id))
            .copy_from_slice(scratch);
    }
}
