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
//! `joints[i - 1]` after that. Positive bend swings the tip toward the spine's
//! +X: the turn is `bend * pi` counterclockwise in the model's Y-up frame,
//! which is the convention `catchlight_editor_core::strand::chain_keyforms`
//! bakes and `ParticleChainData::link_bends` reports.
//!
//! **The rotations compose, root to tip, once per spine per frame.** `T_0` is
//! the identity; the hinge of link `j` is carried to where the links above it
//! have already put it, `c_j' = T_j(c_j)`, and `T_{j+1} = Rot(c_j', theta_j)
//! . T_j`. A vertex in link `k` takes `Rot(c_k', f * theta_k) . T_k` for its
//! own ramp `f`, so it feels every joint above it in full and its own in
//! proportion. That is O(N) per spine and O(1) per vertex, and it is the
//! composition itself rather than a sum of samples of it: two 50 px links bent
//! a sixth of a turn each put the tip where the composed rigid motion puts it,
//! and no segment changes length.
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

/// One spine's per-vertex assignment, keyed by the node the vertices belong
/// to. Empty until the bake fills it.
#[derive(Debug, Clone, Default)]
pub(crate) struct SpinePins {
    pub(crate) per_child: HashMap<NodeIdx, Vec<VertexBend>>,
}

/// The runtime half of a spine: the polyline as drawn, this frame's bends,
/// and the assignment the bake derived.
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
    pub(crate) pins: SpinePins,
}

impl SpineData {
    /// A spine over `joints`, unbent.
    pub fn new(joints: Vec<Vec2>) -> Self {
        let bends = vec![0.0; joints.len()];
        Self {
            joints,
            bends,
            pins: SpinePins::default(),
        }
    }
}

/// A rotation of `angle` radians about `pivot`, counterclockwise in a Y-up
/// frame.
fn rotate_about(pivot: Vec2, angle: f32) -> Affine2 {
    let r = Mat2::from_angle(angle);
    Affine2::from_mat2_translation(r, pivot - r * pivot)
}

/// One link, ready for the per-vertex loop: the composed transform of every
/// joint above it, the hinge it turns about once those have moved it, and its
/// own turn in radians.
#[derive(Debug, Clone, Copy)]
struct ComposedLink {
    above: Affine2,
    hinge: Vec2,
    angle: f32,
}

/// Compose the joints root to tip, once per spine per frame.
///
/// Returns `None` when every bend is zero, which is the whole spine's
/// identity and the caller's cue to write nothing.
fn compose(joints: &[Vec2], bends: &[f32]) -> Option<Vec<ComposedLink>> {
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
        // The hinge above link k, as drawn: the node's own origin for the
        // first link, the joint that ends the link above it after that.
        let rest_hinge = if k == 0 { Vec2::ZERO } else { joints[k - 1] };
        let hinge = above.transform_point2(rest_hinge);
        let angle = angle(k);
        out.push(ComposedLink {
            above,
            hinge,
            angle,
        });
        above = rotate_about(hinge, angle) * above;
    }
    Some(out)
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
    pins
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
        // Composed once per spine, not once per child: the joints move
        // together and the per-vertex loop only samples the result.
        let links = {
            let Some(NodeKind::Spine(spine)) = arena.get(spine_id).map(|n| &n.kind) else {
                continue;
            };
            match compose(&spine.joints, &spine.bends) {
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
            let Some(link) = links.get(bend.link as usize) else {
                scratch.push(Vec2::ZERO);
                continue;
            };
            let turn = rotate_about(link.hinge, bend.ramp * link.angle) * link.above;
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
