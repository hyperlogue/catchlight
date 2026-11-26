//! Mesh groups: vertex-level deforms applied to a subtree.
//!
//! A spine passes no group deform down, so the walks skip past it: shifting
//! the spine node and warping the same vertices would move them twice.

use glam::swizzles::Vec4Swizzles;
use glam::{Affine2, Mat2, Mat4, Vec2, Vec4};

use crate::{
    components::{checked_affine_inverse, Mesh, MeshIndices, NodeIdx, NodeKind},
    deform::DeformSource,
    node::NodeTree,
    puppet::{Arena, GlobalTransforms},
};
use std::collections::HashMap;

/// Per-vertex triangle hints baked at load time, `0` for an uncovered vertex.
/// A hint is only a starting point, so a stale one costs a scan, never a
/// wrong answer.
#[derive(Debug, Clone)]
pub(crate) struct ChildPins {
    pub(crate) vertices: Vec<u32>,
    observed_vertices: Option<Box<[usize]>>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MeshGroupPins {
    pub(crate) per_child: HashMap<NodeIdx, ChildPins>,
}

/// Spatial triangle candidates over one mesh group's rest lattice.
///
/// `find` returns exactly what [`find_triangle_strict_hint`] does; bins only
/// skip triangles, containment is always decided against the exact point.
#[derive(Debug, Clone)]
pub(crate) struct MgTriangleBins {
    /// Lower corner in the lattice's local space, floored.
    bounds_min: Vec2,
    /// At least one unit a side.
    extent: Vec2,
    /// Well-conditioned triangles, in every bin their padded bounds touch, in
    /// index order.
    candidates: Vec<Vec<u32>>,
    /// Nearly collinear triangles cannot be culled by bounds: the f32
    /// containment test can accept points beyond them.
    unculled: Vec<u32>,
}

const TRIANGLE_BIN_SIDE: usize = 32;

impl MgTriangleBins {
    pub(crate) fn build(mesh: &Mesh) -> Option<Self> {
        if mesh.indices.is_empty() || mesh.vertices.is_empty() {
            return None;
        }
        let mut min = Vec2::splat(f32::MAX);
        let mut max = Vec2::splat(f32::MIN);
        for v in mesh.vertices.iter() {
            let p = *v - mesh.origin;
            min = min.min(p);
            max = max.max(p);
        }
        if !min.is_finite() || !max.is_finite() {
            return None;
        }
        let bounds_min = min.floor();
        let extent = (max.ceil() - bounds_min + Vec2::ONE).max(Vec2::ONE);
        if !extent.is_finite() {
            return None;
        }

        let tri_count = triangle_count(&mesh.indices);
        let mut candidates = vec![Vec::new(); TRIANGLE_BIN_SIDE * TRIANGLE_BIN_SIDE];
        let mut unculled = Vec::new();
        let mut candidate_count = 0usize;
        for tri in 0..tri_count {
            let Some((a, b, c)) = triangle_vertices_raw(&mesh.vertices, &mesh.indices, tri) else {
                continue;
            };
            let (a, b, c) = (a - mesh.origin, b - mesh.origin, c - mesh.origin);
            let Some(padding) = triangle_padding(a, b, c) else {
                unculled.push(tri);
                continue;
            };
            let lo = Self::bin(a.min(b).min(c) - padding, bounds_min, extent);
            let hi = Self::bin(a.max(b).max(c) + padding, bounds_min, extent);
            candidate_count += (hi.0 - lo.0 + 1) * (hi.1 - lo.1 + 1);
            // Bound the allocation on highly overlapping input; the caller keeps the
            // exact scan.
            if candidate_count > 1_048_576 {
                return None;
            }
            for y in lo.1..=hi.1 {
                for x in lo.0..=hi.0 {
                    candidates[y * TRIANGLE_BIN_SIDE + x].push(tri);
                }
            }
        }
        Some(Self {
            bounds_min,
            extent,
            candidates,
            unculled,
        })
    }

    fn bin(p: Vec2, min: Vec2, extent: Vec2) -> (usize, usize) {
        let scaled = (p - min) / extent * TRIANGLE_BIN_SIDE as f32;
        (
            (scaled.x as usize).min(TRIANGLE_BIN_SIDE - 1),
            (scaled.y as usize).min(TRIANGLE_BIN_SIDE - 1),
        )
    }

    fn find(&self, vertices: &[Vec2], indices: &MeshIndices, p: Vec2, hint: u32) -> Option<u32> {
        let contains = |tri| {
            triangle_vertices_raw(vertices, indices, tri)
                .is_some_and(|(a, b, c)| inside(barycentric(p, a, b, c)))
        };
        if contains(hint) {
            return Some(hint);
        }
        let (x, y) = Self::bin(p, self.bounds_min, self.extent);
        let nearby = self.candidates[y * TRIANGLE_BIN_SIDE + x]
            .iter()
            .copied()
            .find(|&tri| tri != hint && contains(tri));
        // Preserve the full scan's first-match order.
        self.unculled
            .iter()
            .copied()
            .take_while(|&tri| nearby.is_none_or(|found| tri < found))
            .find(|&tri| tri != hint && contains(tri))
            .or(nearby)
    }
}

/// The containment determinant cancels catastrophically on thin triangles,
/// which a fixed epsilon does not cover; those keep the unculled predicate.
fn triangle_padding(a: Vec2, b: Vec2, c: Vec2) -> Option<Vec2> {
    let v0 = b - a;
    let v1 = c - a;
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let product = d00 * d11;
    let cross = d01 * d01;
    let determinant = product - cross;
    let scale = product + cross;
    if !scale.is_finite() || determinant <= scale * 1e-3 || determinant < 1e-20 {
        return None;
    }
    let extent = a.max(b).max(c) - a.min(b).min(c);
    let magnitude = a.abs().max(b.abs()).max(c.abs());
    Some(
        extent * (1e-4 + 128.0 * f32::EPSILON * scale / determinant)
            + magnitude * (16.0 * f32::EPSILON)
            + Vec2::splat(1e-4),
    )
}

pub(crate) fn barycentric(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> [f32; 3] {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;
    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-20 {
        return [f32::NAN, f32::NAN, f32::NAN];
    }
    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    let u = 1.0 - v - w;
    [u, v, w]
}

fn inside(w: [f32; 3]) -> bool {
    const EPS: f32 = 1e-5;
    w[0] >= -EPS && w[1] >= -EPS && w[2] >= -EPS
}

fn triangle_vertices_raw(
    vertices: &[Vec2],
    indices: &MeshIndices,
    tri_idx: u32,
) -> Option<(Vec2, Vec2, Vec2)> {
    let base = tri_idx as usize * 3;
    let i0 = indices.get(base)? as usize;
    let i1 = indices.get(base + 1)? as usize;
    let i2 = indices.get(base + 2)? as usize;
    Some((*vertices.get(i0)?, *vertices.get(i1)?, *vertices.get(i2)?))
}

fn triangle_count(indices: &MeshIndices) -> u32 {
    (indices.len() / 3) as u32
}

/// `None` when `p` is outside every triangle, so such vertices pass through
/// unchanged. `hint` is checked first (~10x faster on large groups).
fn find_triangle_strict_hint(
    vertices: &[Vec2],
    indices: &MeshIndices,
    p: Vec2,
    hint: u32,
) -> Option<u32> {
    let tri_count = triangle_count(indices);
    if tri_count == 0 {
        return None;
    }
    if let Some((a, b, c)) = triangle_vertices_raw(vertices, indices, hint) {
        let w = barycentric(p, a, b, c);
        if !w[0].is_nan() && inside(w) {
            return Some(hint);
        }
    }
    for tri in 0..tri_count {
        if tri == hint {
            continue;
        }
        let Some((a, b, c)) = triangle_vertices_raw(vertices, indices, tri) else {
            continue;
        };
        let w = barycentric(p, a, b, c);
        if w[0].is_nan() {
            continue;
        }
        if inside(w) {
            return Some(tri);
        }
    }
    None
}

fn descendant_meshed_nodes(tree: &NodeTree, root: NodeIdx, arena: &Arena) -> Vec<NodeIdx> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeIdx> = tree.get_children(root);
    while let Some(id) = stack.pop() {
        match arena.get(id).map(|n| &n.kind) {
            Some(NodeKind::Part(_)) => {
                out.push(id);
                stack.extend(tree.get_children(id));
            }
            // A Composite propagates only when `propagate_mesh_group` is set.
            Some(NodeKind::Composite(c)) => {
                if c.propagate_mesh_group {
                    stack.extend(tree.get_children(id));
                }
            }
            Some(NodeKind::MeshGroup(_)) => {
                out.push(id);
            }
            Some(NodeKind::Spine(_) | NodeKind::MotionResponse) => {
                stack.extend(tree.get_children(id));
            }
            _ => {}
        }
    }
    out
}

fn drawable_mesh_vertices(arena: &Arena, id: NodeIdx) -> Option<&[Vec2]> {
    match arena.get(id).map(|n| &n.kind)? {
        NodeKind::Part(p) => Some(&p.mesh.vertices),
        NodeKind::MeshGroup(mg) => Some(&mg.mesh.vertices),
        _ => None,
    }
}

fn drawable_mesh_origin(arena: &Arena, id: NodeIdx) -> Vec2 {
    match arena.get(id).map(|n| &n.kind) {
        Some(NodeKind::Part(p)) => p.mesh.origin,
        Some(NodeKind::MeshGroup(mg)) => mg.mesh.origin,
        _ => Vec2::ZERO,
    }
}

fn local_positions(mesh: &Mesh) -> Vec<Vec2> {
    mesh.vertices.iter().map(|v| *v - mesh.origin).collect()
}

pub(crate) fn linear_mat2(m: Mat4) -> Mat2 {
    Mat2::from_cols(
        Vec2::new(m.x_axis.x, m.x_axis.y),
        Vec2::new(m.y_axis.x, m.y_axis.y),
    )
}

fn transform_point(m: Mat4, p: Vec2) -> Vec2 {
    let r = m * Vec4::new(p.x, p.y, 0.0, 1.0);
    Vec2::new(r.x, r.y)
}

/// Not `Mat3::from_mat4` + `Affine2::from_mat3`: that round-trip reads the
/// translation from the Z column and drops it.
pub(crate) fn affine2_from_mat4(m: Mat4) -> Affine2 {
    Affine2::from_cols(m.x_axis.xy(), m.y_axis.xy(), m.w_axis.xy())
}

/// Pins are found in MG-local space; `transforms` must be the arena's
/// globals computed immediately before baking.
pub(crate) fn bake_mesh_group_pins(
    arena: &Arena,
    transforms: &GlobalTransforms,
    mesh_group_id: NodeIdx,
) -> MeshGroupPins {
    let mut out = MeshGroupPins::default();
    let Some(mg_node) = arena.get(mesh_group_id) else {
        return out;
    };
    let NodeKind::MeshGroup(mg) = &mg_node.kind else {
        return out;
    };
    if mg.mesh.indices.is_empty() || mg.mesh.vertices.is_empty() {
        return out;
    }

    let mg_global = transforms.get(mesh_group_id);
    let Some(mg_global_inv) = checked_affine_inverse(mg_global) else {
        return out;
    };

    let mg_mesh = &mg.mesh;
    let mg_local = local_positions(mg_mesh);
    let rest_triangle = |p: Vec2| match &mg.bins {
        Some(bins) => bins.find(&mg_local, &mg_mesh.indices, p, 0),
        None => find_triangle_strict_hint(&mg_local, &mg_mesh.indices, p, 0),
    };
    for child_id in descendant_meshed_nodes(&arena.tree, mesh_group_id, arena) {
        let Some(child_verts) = drawable_mesh_vertices(arena, child_id) else {
            continue;
        };
        let child_origin = drawable_mesh_origin(arena, child_id);

        let child_global = transforms.get(child_id);
        let child_to_mg = mg_global_inv * child_global;

        // A closest-edge fallback smears edge deformation and drifts features at
        // extreme poses. GPU draws `v - origin`; lookups use the same space.
        let mut vertices = Vec::with_capacity(child_verts.len());
        for &v in child_verts {
            let v_in_mg = transform_point(child_to_mg, v - child_origin);
            vertices.push(rest_triangle(v_in_mg).unwrap_or(0));
        }
        if !vertices.is_empty() {
            out.per_child.insert(
                child_id,
                ChildPins {
                    vertices,
                    observed_vertices: None,
                },
            );
        }
    }

    out
}

/// Mesh-less descendants of a `translateChildren` MG. Nested MGs are excluded:
/// they already receive vertex deformation, and a shift would double it.
fn translate_children_targets(tree: &NodeTree, mg_id: NodeIdx, arena: &Arena) -> Vec<NodeIdx> {
    let descends = |id: NodeIdx| match arena.get(id).map(|n| &n.kind) {
        Some(NodeKind::Part(_)) => true,
        Some(NodeKind::Composite(c)) => c.propagate_mesh_group,
        Some(NodeKind::Spine(_) | NodeKind::MotionResponse) => true,
        _ => false,
    };
    let mut targets = tree.get_descendants_until(mg_id, |id| !descends(id));
    targets.retain(|&id| {
        !matches!(
            arena.get(id).map(|n| &n.kind),
            Some(NodeKind::Part(_))
                | Some(NodeKind::Composite(_))
                | Some(NodeKind::MeshGroup(_))
                | Some(NodeKind::Spine(_) | NodeKind::MotionResponse)
        )
    });
    targets
}

impl Arena {
    /// Valid only when a fold changed inputs unrelated to the group pass.
    pub(super) fn restore_mesh_group_deforms(
        &mut self,
        transforms: &mut GlobalTransforms,
        root: Mat4,
    ) {
        for i in 0..self.mg_written_sources.len() {
            let (child, group) = self.mg_written_sources[i];
            let Some(node) = self.get_mut(child) else {
                continue;
            };
            let stack = match &mut node.kind {
                NodeKind::Part(p) => &mut p.deform_stack,
                NodeKind::MeshGroup(g) => &mut g.deform_stack,
                _ => continue,
            };
            stack.reactivate(DeformSource::Node(group));
        }
        for i in 0..self.mg_shifts.len() {
            let (id, shift) = self.mg_shifts[i];
            if let Some(node) = self.get_mut(id) {
                node.transform.translation.x += shift.x;
                node.transform.translation.y += shift.y;
            }
            self.mark_transform_dirty(id);
            self.recompute_subtree_transforms(transforms, id, root);
        }
    }

    pub(super) fn retain_geometry(
        &mut self,
        needed: &std::collections::HashSet<NodeIdx>,
        points: &[(NodeIdx, Option<u32>)],
    ) {
        let mut vertices: HashMap<NodeIdx, Vec<usize>> = HashMap::new();
        for &(part, vertex) in points {
            if let Some(v) = vertex {
                vertices.entry(part).or_default().push(v as usize);
            }
        }
        for weld in self.welds.iter() {
            for pair in &weld.pairs {
                vertices
                    .entry(weld.a)
                    .or_default()
                    .push(pair.a_vert as usize);
                vertices
                    .entry(weld.b)
                    .or_default()
                    .push(pair.b_vert as usize);
            }
        }
        for indices in vertices.values_mut() {
            indices.sort_unstable();
            indices.dedup();
        }
        // Keep transform slots stable; unobserved branches release their meshes.
        for (slot, node) in self.nodes.iter_mut().enumerate() {
            if !needed.contains(&NodeIdx(slot as u32)) {
                node.kind = NodeKind::Group;
            }
        }
        self.deform_node_ids.retain(|n| needed.contains(n));
        self.mesh_group_node_ids.retain(|n| needed.contains(n));
        let groups: std::collections::HashSet<_> =
            self.mesh_group_node_ids.iter().copied().collect();
        for i in 0..self.mesh_group_node_ids.len() {
            let id = self.mesh_group_node_ids[i];
            if let Some(NodeKind::MeshGroup(group)) = self.get_mut(id).map(|n| &mut n.kind) {
                std::sync::Arc::make_mut(&mut group.pins)
                    .per_child
                    .retain(|child, pins| {
                        pins.observed_vertices =
                            vertices.get(child).map(|v| v.clone().into_boxed_slice());
                        pins.observed_vertices.is_some() || groups.contains(child)
                    });
            }
        }
    }
}

/// Children are sampled at their current position (`base + cur_deform`,
/// excluding this MG's prior source), not the base: sampling at the base
/// double-pulls against the child's own Param deltas at extreme values.
pub(crate) fn propagate_mesh_group_deforms(
    arena: &mut Arena,
    transforms: &mut GlobalTransforms,
    root: Mat4,
) {
    let _span = tracing::trace_span!("propagate_mesh_group_deforms").entered();
    arena.mg_written_sources.clear();
    arena.mg_shifts.clear();

    let mut scratch = std::mem::take(&mut arena.mg_propagate_scratch);
    let mut cur_deform_scratch = std::mem::take(&mut arena.mg_cur_deform_scratch);
    let mut deformed_mg_vertices = std::mem::take(&mut arena.mg_deformed_vertices_scratch);
    let mut mg_local = std::mem::take(&mut arena.mg_local_scratch);
    let mut child_ids: smallvec::SmallVec<[NodeIdx; 8]> = smallvec::SmallVec::new();

    // Ancestor-first, so an outer MG pushes into a nested MG's stack before
    // the inner combines.
    for i in 0..arena.mesh_group_node_ids.len() {
        let mg_id = arena.mesh_group_node_ids[i];
        mg_local.clear();
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack.combine();
                mg_local.extend(mg.mesh.vertices.iter().map(|v| *v - mg.mesh.origin));
            }
        }

        // Shift before pushing, so targets move by own Params plus every outer push.
        let translate_children = matches!(
            arena.get(mg_id).map(|n| &n.kind),
            Some(NodeKind::MeshGroup(mg)) if mg.translate_children
        );
        if translate_children {
            shift_translate_children(arena, mg_id, &mg_local, transforms, root);
        }

        // Propagation still runs at zero MG deform: a child may have its own.
        child_ids.clear();
        {
            let Some(node) = arena.get(mg_id) else {
                continue;
            };
            let NodeKind::MeshGroup(mg) = &node.kind else {
                continue;
            };
            child_ids.extend(mg.pins.per_child.keys().copied());

            let mg_combined = mg.deform_stack.combined();
            let mg_vertices = &mg.mesh.vertices;
            let mg_origin = mg.mesh.origin;
            deformed_mg_vertices.clear();
            deformed_mg_vertices.reserve(mg_vertices.len());
            if mg_combined.len() == mg_vertices.len() {
                for (v, d) in mg_vertices.iter().zip(mg_combined.iter()) {
                    deformed_mg_vertices.push(*v - mg_origin + *d);
                }
            } else {
                deformed_mg_vertices.extend(mg_vertices.iter().map(|v| *v - mg_origin));
            }
        }

        // Per frame: params that drive transforms make load-time
        // `child_to_mg_2d` stale.
        let mg_global = transforms.get(mg_id);
        let Some(mg_global_inv) = checked_affine_inverse(mg_global) else {
            continue;
        };

        for &child_id in &child_ids {
            let child_global = transforms.get(child_id);
            let Some(child_global_inv) = checked_affine_inverse(child_global) else {
                continue;
            };
            let child_to_mg_4 = mg_global_inv * child_global;
            let mg_to_child_4 = child_global_inv * mg_global;
            let child_to_mg_2d = affine2_from_mat4(child_to_mg_4);
            let mg_to_child_2d = affine2_from_mat4(mg_to_child_4);
            propagate_to_child(
                arena,
                mg_id,
                child_id,
                &mut scratch,
                &mut cur_deform_scratch,
                &mg_local,
                &deformed_mg_vertices,
                child_to_mg_2d,
                mg_to_child_2d,
            );
        }
    }

    arena.mg_propagate_scratch = scratch;
    arena.mg_cur_deform_scratch = cur_deform_scratch;
    arena.mg_deformed_vertices_scratch = deformed_mg_vertices;
    arena.mg_local_scratch = mg_local;
}

/// Shift one MG's `translate_children` targets, then recompute the globals
/// under each.
///
///   centerMatrix = MG.global.inverse * parent.global
///   cVertex      = centerMatrix * local_translation
///   newPos       = (in-lattice) deformed_triangle_warp(cVertex)
///   delta        = (parent.global.inv * MG.global)_linear * (newPos - cVertex)
///   transform.translation += delta
///
/// One MG's targets are disjoint subtrees, so a shift never moves an earlier
/// target's parent.
fn shift_translate_children(
    arena: &mut Arena,
    mg_id: NodeIdx,
    mg_local: &[Vec2],
    transforms: &mut GlobalTransforms,
    root: Mat4,
) {
    let _span = tracing::trace_span!("shift_translate_children").entered();

    let targets = translate_children_targets(&arena.tree, mg_id, arena);
    if targets.is_empty() {
        return;
    }

    let mg_global = transforms.get(mg_id);
    let Some(mg_global_inv) = checked_affine_inverse(mg_global) else {
        return;
    };

    for target_id in targets {
        let delta_parent = translate_children_delta(
            arena,
            mg_id,
            mg_local,
            mg_global,
            mg_global_inv,
            transforms,
            target_id,
        );

        if delta_parent == Vec2::ZERO {
            continue;
        }

        if let Some(node) = arena.get_mut(target_id) {
            node.transform.translation.x += delta_parent.x;
            node.transform.translation.y += delta_parent.y;
        }
        arena.mg_shifts.push((target_id, delta_parent));
        arena.mark_transform_dirty(target_id);
        arena.recompute_subtree_transforms(transforms, target_id, root);
    }
}

/// In the target's parent space; `Vec2::ZERO` whenever the shift does not
/// apply.
fn translate_children_delta(
    arena: &Arena,
    mg_id: NodeIdx,
    mg_local: &[Vec2],
    mg_global: Mat4,
    mg_global_inv: Mat4,
    transforms: &GlobalTransforms,
    target_id: NodeIdx,
) -> Vec2 {
    let Some(parent_id) = arena.tree.get_parent(target_id) else {
        return Vec2::ZERO;
    };
    let parent_global = transforms.get(parent_id);

    // transform.translation already includes this frame's param shift.
    let Some(node) = arena.get(target_id) else {
        return Vec2::ZERO;
    };
    let cur_local = Vec2::new(node.transform.translation.x, node.transform.translation.y);
    let world_cur = parent_global * Vec4::new(cur_local.x, cur_local.y, 0.0, 1.0);
    let cvertex_full = mg_global_inv * world_cur;
    let cvertex_proj = Vec2::new(cvertex_full.x, cvertex_full.y);

    let Some(delta_mg) = warp_at(arena, mg_id, cvertex_proj, mg_local) else {
        return Vec2::ZERO;
    };

    // Linear part only: this maps a displacement, not a point.
    let Some(parent_global_inv) = checked_affine_inverse(parent_global) else {
        return Vec2::ZERO;
    };
    let mg_to_parent = parent_global_inv * mg_global;
    linear_mat2(mg_to_parent) * delta_mg
}

/// `None` outside the lattice, where the warp is the identity. `mg_local` is
/// the lattice's rest positions in its own space.
fn warp_at(arena: &Arena, mg_id: NodeIdx, p: Vec2, mg_local: &[Vec2]) -> Option<Vec2> {
    let Some(NodeKind::MeshGroup(mg)) = arena.get(mg_id).map(|n| &n.kind) else {
        return None;
    };
    let mg_indices = &mg.mesh.indices;
    let combined = mg.deform_stack.combined();
    let tri_idx = match &mg.bins {
        Some(bins) => bins.find(mg_local, mg_indices, p, 0),
        None => find_triangle_strict_hint(mg_local, mg_indices, p, 0),
    }?;
    let base_idx = tri_idx as usize * 3;
    let i0 = mg_indices.get(base_idx).map(|i| i as usize);
    let i1 = mg_indices.get(base_idx + 1).map(|i| i as usize);
    let i2 = mg_indices.get(base_idx + 2).map(|i| i as usize);
    let (a, b, c) = match (i0, i1, i2) {
        (Some(a), Some(b), Some(c))
            if a < mg_local.len()
                && b < mg_local.len()
                && c < mg_local.len()
                && a < combined.len()
                && b < combined.len()
                && c < combined.len() =>
        {
            (a, b, c)
        }
        _ => return None,
    };
    let w = barycentric(p, mg_local[a], mg_local[b], mg_local[c]);
    if w[0].is_nan() {
        return None;
    }
    Some(combined[a] * w[0] + combined[b] * w[1] + combined[c] * w[2])
}

#[allow(clippy::too_many_arguments)]
fn propagate_to_child(
    arena: &mut Arena,
    mg_id: NodeIdx,
    child_id: NodeIdx,
    scratch: &mut Vec<Vec2>,
    cur_deform_scratch: &mut Vec<Vec2>,
    mg_local: &[Vec2],
    deformed_mg_vertices: &[Vec2],
    child_to_mg_2d: Affine2,
    mg_to_child_2d: Affine2,
) {
    // Deactivate the prior Node(mg_id) source first, so every early return
    // leaves it dropped, as if this MG never propagated.
    let cur_len = {
        let Some(child) = arena.get_mut(child_id) else {
            return;
        };
        let stack = match &mut child.kind {
            NodeKind::Part(p) => Some(&mut p.deform_stack),
            NodeKind::MeshGroup(mg) => Some(&mut mg.deform_stack),
            _ => None,
        };
        let Some(stack) = stack else { return };
        stack.clear_source(DeformSource::Node(mg_id));
        stack.sum_active_into(cur_deform_scratch);
        cur_deform_scratch.len()
    };

    let ok = {
        let Some(mg_node) = arena.get(mg_id) else {
            return;
        };
        let NodeKind::MeshGroup(mg) = &mg_node.kind else {
            return;
        };
        let Some(pins) = mg.pins.per_child.get(&child_id) else {
            return;
        };

        let mg_combined = mg.deform_stack.combined();

        let Some(child_node) = arena.get(child_id) else {
            return;
        };
        let (child_verts, child_origin): (&[Vec2], Vec2) = match &child_node.kind {
            NodeKind::Part(p) => (&p.mesh.vertices, p.mesh.origin),
            NodeKind::MeshGroup(mg) => (&mg.mesh.vertices, mg.mesh.origin),
            _ => return,
        };
        if cur_len != child_verts.len() {
            return;
        }

        // Zero deform on both sides emits all-zero deltas; skipping saves ~1ms per
        // frame on a complex model.
        let mg_zero = mg_combined.iter().all(|v| *v == Vec2::ZERO);
        let child_zero = cur_deform_scratch.iter().all(|v| *v == Vec2::ZERO);
        if mg_zero && child_zero {
            scratch.clear();
            return;
        }

        let mg_indices = &mg.mesh.indices;
        let vert_count = mg_local.len().min(deformed_mg_vertices.len());
        let bins = mg.bins.as_ref();
        let n = child_verts.len();
        scratch.clear();
        let offset = |i: usize| {
            let base_v = child_verts[i];
            let cv_child_local = base_v - child_origin + cur_deform_scratch[i];
            let cv_mg_local = child_to_mg_2d.transform_point2(cv_child_local);

            // Only strict containment may decide to leave an outside vertex alone.
            let hint = pins.vertices.get(i).copied().unwrap_or(0);
            let tri_idx = match bins {
                Some(bins) => bins.find(mg_local, mg_indices, cv_mg_local, hint),
                None => find_triangle_strict_hint(mg_local, mg_indices, cv_mg_local, hint),
            };
            let Some(tri_idx) = tri_idx else {
                return Vec2::ZERO;
            };

            let base_idx = tri_idx as usize * 3;
            let i0 = mg_indices.get(base_idx).map(|i| i as usize);
            let i1 = mg_indices.get(base_idx + 1).map(|i| i as usize);
            let i2 = mg_indices.get(base_idx + 2).map(|i| i as usize);
            let (a_idx, b_idx, c_idx) = match (i0, i1, i2) {
                (Some(a), Some(b_), Some(c))
                    if a < vert_count && b_ < vert_count && c < vert_count =>
                {
                    (a, b_, c)
                }
                _ => {
                    return Vec2::ZERO;
                }
            };

            let weights = barycentric(
                cv_mg_local,
                mg_local[a_idx],
                mg_local[b_idx],
                mg_local[c_idx],
            );
            if weights[0].is_nan() {
                return Vec2::ZERO;
            }

            let mg_local_pos = deformed_mg_vertices[a_idx] * weights[0]
                + deformed_mg_vertices[b_idx] * weights[1]
                + deformed_mg_vertices[c_idx] * weights[2];

            let new_pos_child_local = mg_to_child_2d.transform_point2(mg_local_pos);
            new_pos_child_local - (base_v - child_origin) - cur_deform_scratch[i]
        };
        if let Some(vertices) = &pins.observed_vertices {
            scratch.resize(n, Vec2::ZERO);
            for &index in vertices.iter() {
                if let Some(dst) = scratch.get_mut(index) {
                    *dst = offset(index);
                }
            }
        } else {
            scratch.extend((0..n).map(offset));
        }
        true
    };
    if !ok {
        return;
    }

    let Some(child) = arena.get_mut(child_id) else {
        return;
    };
    let stack = match &mut child.kind {
        NodeKind::Part(p) => Some(&mut p.deform_stack),
        NodeKind::MeshGroup(mg) => Some(&mut mg.deform_stack),
        _ => None,
    };
    if let Some(stack) = stack {
        if stack.write_source(DeformSource::Node(mg_id), scratch) {
            arena.mg_written_sources.push((child_id, mg_id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_chain_passes_group_deformation_through_to_its_art() {
        use crate::components::Node;
        let mut arena = Arena::new();
        let group = arena.insert_child(
            arena.root(),
            Node {
                kind: NodeKind::MeshGroup(Box::default()),
                ..Node::default()
            },
        );
        let row = arena.insert_child(
            group,
            Node {
                kind: NodeKind::MotionResponse,
                ..Node::default()
            },
        );
        let tip = arena.insert_child(
            row,
            Node {
                kind: NodeKind::MotionResponse,
                ..Node::default()
            },
        );
        let art = arena.insert_child(
            tip,
            Node {
                kind: NodeKind::Part(Box::default()),
                ..Node::default()
            },
        );
        assert_eq!(descendant_meshed_nodes(&arena.tree, group, &arena), [art]);
        assert!(translate_children_targets(&arena.tree, group, &arena).is_empty());
    }

    #[test]
    fn non_propagating_composite_halts_mesh_group_descent() {
        use crate::components::{CompositeData, Node};

        let mut arena = Arena::new();
        let root = arena.root();
        let mg_id = arena.insert_child(
            root,
            Node {
                kind: NodeKind::MeshGroup(Box::default()),
                ..Default::default()
            },
        );

        let mut with_composite = |propagate: bool| {
            let comp_id = arena.insert_child(
                mg_id,
                Node {
                    kind: NodeKind::Composite(Box::new(CompositeData {
                        propagate_mesh_group: propagate,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            );
            arena.insert_child(
                comp_id,
                Node {
                    kind: NodeKind::Part(Box::default()),
                    ..Default::default()
                },
            )
        };
        let opted_out_part = with_composite(false);
        let normal_part = with_composite(true);

        let reached = descendant_meshed_nodes(&arena.tree, mg_id, &arena);
        assert!(
            reached.contains(&normal_part),
            "a propagating Composite must not stop the walk"
        );
        assert!(
            !reached.contains(&opted_out_part),
            "propagate_mesh_group=false must halt descent"
        );
    }

    #[test]
    fn barycentric_inside_triangle_sums_to_one() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(10.0, 0.0);
        let c = Vec2::new(0.0, 10.0);
        let p = Vec2::new(2.0, 3.0);
        let w = barycentric(p, a, b, c);
        assert!((w[0] + w[1] + w[2] - 1.0).abs() < 1e-5);
        assert!(inside(w));
        let recon = a * w[0] + b * w[1] + c * w[2];
        assert!((recon - p).length() < 1e-4);
    }

    #[test]
    fn barycentric_outside_has_negative_weight() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(10.0, 0.0);
        let c = Vec2::new(0.0, 10.0);
        let p = Vec2::new(-1.0, -1.0);
        let w = barycentric(p, a, b, c);
        assert!(!inside(w));
    }

    #[test]
    fn bins_match_strict_scan_on_quad() {
        let mesh = Mesh::new(
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            vec![Vec2::ZERO; 4],
            MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            Vec2::ZERO,
        );
        let bins = MgTriangleBins::build(&mesh).expect("bins");
        for p in [
            Vec2::new(8.0, 1.0),
            Vec2::new(1.0, 8.0),
            // On the shared diagonal, where the hint decides.
            Vec2::new(5.0, 5.0),
            Vec2::new(-5.0, -5.0),
            Vec2::new(100.0, 100.0),
        ] {
            for hint in [0, 1, 7] {
                assert_eq!(
                    bins.find(&mesh.vertices, &mesh.indices, p, hint),
                    find_triangle_strict_hint(&mesh.vertices, &mesh.indices, p, hint),
                    "point {p:?}, hint {hint}"
                );
            }
        }
        assert_eq!(
            bins.find(&mesh.vertices, &mesh.indices, Vec2::new(100.0, 100.0), 0),
            None
        );
    }

    #[test]
    fn bins_cover_a_lattice_of_any_extent() {
        let side = 2.0e6;
        let mesh = Mesh::new(
            vec![
                Vec2::ZERO,
                Vec2::new(side, 0.0),
                Vec2::splat(side),
                Vec2::new(0.0, side),
            ],
            vec![],
            MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            Vec2::ZERO,
        );
        let bins = MgTriangleBins::build(&mesh).expect("bins for a huge lattice");
        for p in [
            Vec2::new(side * 0.75, side * 0.25),
            Vec2::new(side * 0.25, side * 0.75),
            Vec2::splat(-1.0),
        ] {
            assert_eq!(
                bins.find(&mesh.vertices, &mesh.indices, p, 0),
                find_triangle_strict_hint(&mesh.vertices, &mesh.indices, p, 0),
                "point {p:?}"
            );
        }
    }

    #[test]
    fn slivers_cost_one_entry_each_whatever_they_span() {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for i in 0..2000u16 {
            let o = f32::from(i) * 0.001;
            let base = vertices.len() as u16;
            vertices.extend([
                Vec2::new(0.0, o),
                Vec2::new(4000.0, 4000.0 - o),
                Vec2::new(4000.0, 3999.999 - o),
            ]);
            indices.extend([base, base + 1, base + 2]);
        }
        let mesh = Mesh::new(vertices, vec![], MeshIndices::U16(indices), Vec2::ZERO);
        let bins = MgTriangleBins::build(&mesh).expect("bins");
        let listed: usize =
            bins.candidates.iter().map(Vec::len).sum::<usize>() + bins.unculled.len();
        assert!(
            listed <= 2000 * TRIANGLE_BIN_SIDE * 2,
            "{listed} entries for 2000 slivers"
        );
        let p = Vec2::new(2000.0, 2000.0);
        assert_eq!(
            bins.find(&mesh.vertices, &mesh.indices, p, 0),
            find_triangle_strict_hint(&mesh.vertices, &mesh.indices, p, 0),
        );
    }

    #[test]
    fn spatial_candidates_preserve_strict_lookup_at_fractional_edges() {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for y in 0..16 {
            for x in 0..16 {
                let base = vertices.len() as u16;
                let p = Vec2::new(x as f32 * 2.0 + 0.2, y as f32 * 2.0 + 0.2);
                vertices.extend([p, p + Vec2::new(0.6, 0.0), p + Vec2::new(0.0, 0.6)]);
                indices.extend([base, base + 1, base + 2]);
            }
        }
        let mesh = Mesh::new(vertices, vec![], MeshIndices::U16(indices), Vec2::ZERO);
        let bins = MgTriangleBins::build(&mesh).unwrap();
        for triangle in mesh.vertices.as_chunks::<3>().0 {
            for offset in [
                Vec2::ZERO,
                Vec2::splat(0.1),
                Vec2::splat(0.5),
                Vec2::new(-0.000003, 0.1),
                Vec2::new(0.6, 0.0),
                Vec2::splat(-0.01),
            ] {
                let p = triangle[0] + offset;
                for hint in [0, 17, 255] {
                    assert_eq!(
                        bins.find(&mesh.vertices, &mesh.indices, p, hint),
                        find_triangle_strict_hint(&mesh.vertices, &mesh.indices, p, hint),
                        "point {p:?}, hint {hint}"
                    );
                }
            }
        }
        assert!(bins.candidates.iter().all(|bin| bin.len() <= 4));
    }

    #[test]
    fn spatial_candidates_preserve_ill_conditioned_containment() {
        let mesh = Mesh::new(
            vec![
                Vec2::ZERO,
                Vec2::X,
                Vec2::Y,
                Vec2::new(6.1465526, 3.4940288),
                Vec2::new(25.191957, 15.446337),
                Vec2::new(21.09729, 12.879735),
                Vec2::new(37.3, 23.04),
                Vec2::new(37.4, 23.04),
                Vec2::new(37.3, 23.14),
                Vec2::splat(50.0),
            ],
            vec![],
            MeshIndices::U16(vec![0, 1, 2, 3, 4, 5, 6, 7, 8]),
            Vec2::ZERO,
        );
        let bins = MgTriangleBins::build(&mesh).unwrap();
        let p = Vec2::new(37.332767, 23.065533);
        let scan = find_triangle_strict_hint(&mesh.vertices, &mesh.indices, p, 0);
        assert_eq!(scan, Some(1));
        assert_eq!(bins.find(&mesh.vertices, &mesh.indices, p, 0), scan);
        assert_eq!(bins.find(&mesh.vertices, &mesh.indices, p, 2), Some(2));
    }

    #[test]
    fn spatial_candidates_match_scan_across_scales_and_shapes() {
        let mut seed = 0x31579u32;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        for scale in [0.001, 1.0, 64.0] {
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            for i in 0..128 {
                let a = Vec2::new(random() - 0.5, random() - 0.5) * 16.0 * scale;
                let edge = Vec2::new(random() - 0.5, random() - 0.5) * 8.0 * scale;
                let c = if i % 2 == 0 {
                    a + edge * random() + edge.perp() * 10f32.powf(-6.0 * random())
                } else {
                    a + Vec2::new(random() - 0.5, random() - 0.5) * 8.0 * scale
                };
                let base = vertices.len() as u16;
                vertices.extend([a, a + edge, c]);
                indices.extend([base, base + 1, base + 2]);
            }
            let origin = Vec2::new(3.25, -7.5) * scale;
            let mesh = Mesh::new(vertices, vec![], MeshIndices::U16(indices), origin);
            let bins = MgTriangleBins::build(&mesh).unwrap();
            let local = local_positions(&mesh);
            for triangle in local.as_chunks::<3>().0 {
                let [a, b, c] = *triangle;
                for _ in 0..24 {
                    let p = a + (b - a) * (random() * 3.0 - 1.0) + (c - a) * (random() * 3.0 - 1.0);
                    for hint in [0, 63, 127] {
                        assert_eq!(
                            bins.find(&local, &mesh.indices, p, hint),
                            find_triangle_strict_hint(&local, &mesh.indices, p, hint),
                            "scale={scale}, point={p:?}, hint={hint}, triangle={triangle:?}",
                        );
                    }
                }
            }
        }
    }

    fn fractional_fixture(
        vertices: Vec<Vec2>,
        indices: Vec<u16>,
        offsets: Vec<Vec2>,
        points: &[Vec2],
        use_bins: bool,
    ) -> (Vec<Vec2>, Vec<Vec2>) {
        use crate::components::{MeshGroupData, Node, PartData};
        let mut arena = Arena::new();
        let count = vertices.len();
        let mg_id = arena.insert_child(
            arena.root(),
            Node {
                kind: NodeKind::MeshGroup(Box::new(MeshGroupData {
                    mesh: Mesh::new(
                        vertices,
                        vec![Vec2::ZERO; count],
                        MeshIndices::U16(indices),
                        Vec2::ZERO,
                    )
                    .into(),
                    deform_stack: crate::deform::DeformStack::new(count),
                    translate_children: true,
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
        let part_id = arena.insert_child(
            mg_id,
            Node {
                kind: NodeKind::Part(Box::new(PartData {
                    mesh: Mesh::new(
                        points.to_vec(),
                        vec![Vec2::ZERO; points.len()],
                        MeshIndices::U16(vec![]),
                        Vec2::ZERO,
                    )
                    .into(),
                    deform_stack: crate::deform::DeformStack::new(points.len()),
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
        let anchors: Vec<_> = points
            .iter()
            .map(|p| {
                let mut node = Node::default();
                node.transform.translation.x = p.x;
                node.transform.translation.y = p.y;
                arena.insert_child(mg_id, node)
            })
            .collect();
        let mut transforms = GlobalTransforms::new();
        arena.compute_transforms(&mut transforms);
        let pins = bake_mesh_group_pins(&arena, &transforms, mg_id);
        let NodeKind::MeshGroup(mg) = &mut arena.get_mut(mg_id).unwrap().kind else {
            panic!()
        };
        mg.pins = pins.into();
        if use_bins {
            mg.bins = MgTriangleBins::build(&mg.mesh).map(Into::into);
        }
        mg.deform_stack
            .set(DeformSource::Param(0), offsets)
            .unwrap();
        propagate_mesh_group_deforms(&mut arena, &mut transforms, Mat4::IDENTITY);
        arena.combine_deforms();
        let NodeKind::Part(part) = &arena.get(part_id).unwrap().kind else {
            panic!()
        };
        let warped = points
            .iter()
            .zip(part.deform_stack.combined())
            .map(|(p, d)| *p + *d)
            .collect();
        let shifted = anchors
            .iter()
            .map(|id| {
                let t = arena.get(*id).unwrap().transform.translation;
                Vec2::new(t.x, t.y)
            })
            .collect();
        (warped, shifted)
    }

    #[test]
    fn fractional_warp_matches_exact_piecewise_affine_surface() {
        let points = [
            Vec2::new(0.99, 0.97),
            Vec2::new(1.01, 0.97),
            Vec2::new(0.99, 0.971),
            Vec2::new(0.25, 0.75),
            Vec2::new(0.75, 0.25),
        ];
        for bins in [false, true] {
            let (warped, shifted) = fractional_fixture(
                vec![
                    Vec2::ZERO,
                    Vec2::new(2., 0.),
                    Vec2::splat(2.),
                    Vec2::new(0., 2.),
                ],
                vec![0, 1, 2, 0, 2, 3],
                vec![
                    Vec2::ZERO,
                    Vec2::new(0.7, 0.),
                    Vec2::ZERO,
                    Vec2::new(0., 0.5),
                ],
                &points,
                bins,
            );
            for (i, p) in points.iter().enumerate() {
                let delta = if p.x >= p.y {
                    Vec2::new(0.35 * (p.x - p.y), 0.)
                } else {
                    Vec2::new(0., 0.25 * (p.y - p.x))
                };
                assert!(
                    (warped[i] - (*p + delta)).length() < 1e-5,
                    "part at {p:?}, bins={bins}: {:?}",
                    warped[i]
                );
                assert!(
                    (shifted[i] - (*p + delta)).length() < 1e-5,
                    "anchor at {p:?}, bins={bins}: {:?}",
                    shifted[i]
                );
            }
            let a = warped[1] - warped[0];
            let b = warped[2] - warped[0];
            assert!(a.perp_dot(b) > 0., "a thin child triangle must not flip");
        }
    }

    #[test]
    fn a_fractional_point_decides_its_own_surface_membership() {
        for bins in [false, true] {
            let p = Vec2::splat(0.3);
            let (warped, shifted) = fractional_fixture(
                vec![Vec2::splat(0.2), Vec2::new(0.8, 0.2), Vec2::new(0.2, 0.8)],
                vec![0, 1, 2],
                vec![Vec2::X; 3],
                &[p],
                bins,
            );
            assert!((warped[0] - p - Vec2::X).length() < 1e-5);
            assert!((shifted[0] - p - Vec2::X).length() < 1e-5);
            let p = Vec2::splat(1.9);
            let (warped, shifted) = fractional_fixture(
                vec![Vec2::ZERO, Vec2::new(2., 0.), Vec2::new(0., 2.)],
                vec![0, 1, 2],
                vec![Vec2::X; 3],
                &[p],
                bins,
            );
            assert!((warped[0] - p).length() < 1e-5);
            assert!((shifted[0] - p).length() < 1e-5);
        }
    }

    #[test]
    fn bins_handle_an_empty_mesh() {
        let mesh = Mesh::new(
            Vec::<Vec2>::new(),
            Vec::<Vec2>::new(),
            MeshIndices::U16(vec![]),
            Vec2::ZERO,
        );
        assert!(MgTriangleBins::build(&mesh).is_none());
    }

    #[test]
    fn propagate_sets_deform_source_node_on_child() {
        use crate::components::{Mesh, MeshGroupData, MeshIndices, Node, NodeKind, PartData};

        let mut arena = Arena::new();
        let mg_mesh = Mesh::new(
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            vec![Vec2::ZERO; 4],
            MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            Vec2::ZERO,
        );
        let mg = MeshGroupData {
            mesh: mg_mesh.into(),
            deform_stack: crate::deform::DeformStack::new(4),
            ..Default::default()
        };
        let mg_id = arena.insert_child(
            arena.root(),
            Node {
                kind: NodeKind::MeshGroup(Box::new(mg)),
                ..Default::default()
            },
        );

        let child_mesh = Mesh::new(
            vec![Vec2::new(5.0, 5.0), Vec2::new(2.0, 2.0)],
            vec![Vec2::ZERO; 2],
            MeshIndices::U16(vec![]),
            Vec2::ZERO,
        );
        let part = PartData {
            mesh: child_mesh.into(),
            deform_stack: crate::deform::DeformStack::new(2),
            ..Default::default()
        };
        let child_id = arena.insert_child(
            mg_id,
            Node {
                kind: NodeKind::Part(Box::new(part)),
                ..Default::default()
            },
        );

        let mut tx = GlobalTransforms::new();
        arena.compute_transforms(&mut tx);
        let pins = bake_mesh_group_pins(&arena, &tx, mg_id);
        assert!(pins.per_child.contains_key(&child_id));
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.pins = pins.into();
            }
        }

        // Vertex 2 moves (+4,0); the child at (5,5) has baryc (0, 0.5, 0.5).
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack
                    .set(
                        DeformSource::Param(0),
                        vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO],
                    )
                    .unwrap();
            }
        }

        propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);

        arena.combine_deforms();
        if let Some(node) = arena.get(child_id) {
            if let NodeKind::Part(p) = &node.kind {
                let c = p.deform_stack.combined();
                assert_eq!(c.len(), 2);
                assert!((c[0].x - 2.0).abs() < 1e-4, "got {:?}", c[0]);
                assert!(c[0].y.abs() < 1e-4, "got {:?}", c[0]);
                assert!((c[1].x - 0.8).abs() < 1e-4, "got {:?}", c[1]);
            } else {
                panic!("child isn't a part");
            }
        }
    }

    #[test]
    fn mg_deform_is_sampled_at_the_child_current_position() {
        use crate::components::{Mesh, MeshGroupData, MeshIndices, Node, NodeKind, PartData};

        fn build() -> (Arena, NodeIdx) {
            let mut arena = Arena::new();
            let mg_mesh = Mesh::new(
                vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(10.0, 0.0),
                    Vec2::new(10.0, 10.0),
                    Vec2::new(0.0, 10.0),
                ],
                vec![Vec2::ZERO; 4],
                MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
                Vec2::ZERO,
            );
            let mg = MeshGroupData {
                mesh: mg_mesh.into(),
                deform_stack: crate::deform::DeformStack::new(4),
                ..Default::default()
            };
            let mg_id = arena.insert_child(
                arena.root(),
                Node {
                    kind: NodeKind::MeshGroup(Box::new(mg)),
                    ..Default::default()
                },
            );

            let part = PartData {
                mesh: Mesh::new(
                    vec![Vec2::new(5.0, 5.0)],
                    vec![Vec2::ZERO; 1],
                    MeshIndices::U16(vec![]),
                    Vec2::ZERO,
                )
                .into(),
                deform_stack: crate::deform::DeformStack::new(1),
                ..Default::default()
            };
            let child_id = arena.insert_child(
                mg_id,
                Node {
                    kind: NodeKind::Part(Box::new(part)),
                    ..Default::default()
                },
            );

            let mut tx = GlobalTransforms::new();
            arena.compute_transforms(&mut tx);
            let pins = bake_mesh_group_pins(&arena, &tx, mg_id);
            if let Some(node) = arena.get_mut(mg_id) {
                if let NodeKind::MeshGroup(mg) = &mut node.kind {
                    mg.bins = MgTriangleBins::build(&mg.mesh).map(Into::into);
                    mg.pins = pins.into();
                    mg.deform_stack
                        .set(
                            DeformSource::Param(0),
                            vec![
                                Vec2::ZERO,
                                Vec2::new(2.0, 0.0),
                                Vec2::new(4.0, 0.0),
                                Vec2::ZERO,
                            ],
                        )
                        .unwrap();
                }
            }

            if let Some(node) = arena.get_mut(child_id) {
                if let NodeKind::Part(p) = &mut node.kind {
                    p.deform_stack
                        .set(DeformSource::Param(1), vec![Vec2::new(2.0, 0.0)])
                        .unwrap();
                }
            }

            (arena, child_id)
        }

        // Baryc of (7,5) in tri 0 is (0.3, 0.2, 0.5): MG gives (2.4,0), combined
        // (4.4,0). Sampling at the base would give (4.0,0).
        let (mut puppet, child_id) = build();
        let mut tx = GlobalTransforms::new();
        puppet.compute_transforms(&mut tx);
        propagate_mesh_group_deforms(&mut puppet, &mut tx, Mat4::IDENTITY);
        puppet.combine_deforms();
        let node = puppet.get(child_id).unwrap();
        let NodeKind::Part(part) = &node.kind else {
            panic!()
        };
        let c = part.deform_stack.combined()[0];
        assert!((c.x - 4.4).abs() < 1e-4, "got {:?}", c);
        assert!(c.y.abs() < 1e-4, "got {:?}", c);
    }

    #[test]
    fn nested_mesh_groups_compose_via_post_order() {
        use crate::components::{Mesh, MeshGroupData, MeshIndices, Node, NodeKind, PartData};

        let mut arena = Arena::new();

        let quad = |size: f32| {
            Mesh::new(
                vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(size, 0.0),
                    Vec2::new(size, size),
                    Vec2::new(0.0, size),
                ],
                vec![Vec2::ZERO; 4],
                MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
                Vec2::ZERO,
            )
        };

        let outer = MeshGroupData {
            mesh: quad(20.0).into(),
            deform_stack: crate::deform::DeformStack::new(4),
            ..Default::default()
        };
        let outer_id = arena.insert_child(
            arena.root(),
            Node {
                kind: NodeKind::MeshGroup(Box::new(outer)),
                ..Default::default()
            },
        );

        let inner = MeshGroupData {
            mesh: quad(10.0).into(),
            deform_stack: crate::deform::DeformStack::new(4),
            ..Default::default()
        };
        let inner_id = arena.insert_child(
            outer_id,
            Node {
                kind: NodeKind::MeshGroup(Box::new(inner)),
                ..Default::default()
            },
        );

        let part = PartData {
            mesh: Mesh::new(
                vec![Vec2::new(5.0, 5.0)],
                vec![Vec2::ZERO; 1],
                MeshIndices::U16(vec![]),
                Vec2::ZERO,
            )
            .into(),
            deform_stack: crate::deform::DeformStack::new(1),
            ..Default::default()
        };
        let part_id = arena.insert_child(
            inner_id,
            Node {
                kind: NodeKind::Part(Box::new(part)),
                ..Default::default()
            },
        );

        let mut tx = GlobalTransforms::new();
        arena.compute_transforms(&mut tx);
        let outer_pins = bake_mesh_group_pins(&arena, &tx, outer_id);
        let inner_pins = bake_mesh_group_pins(&arena, &tx, inner_id);
        assert!(!outer_pins.per_child.contains_key(&part_id));
        assert!(inner_pins.per_child.contains_key(&part_id));
        assert!(outer_pins.per_child.contains_key(&inner_id));

        if let Some(node) = arena.get_mut(outer_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.pins = outer_pins.into();
            }
        }
        if let Some(node) = arena.get_mut(inner_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.pins = inner_pins.into();
            }
        }

        // Outer pushes (4,0) everywhere on the inner; inner adds (+4,0) at vertex 2.
        // The part at (5,5) lands at (11,5): offset (6,0).
        if let Some(node) = arena.get_mut(outer_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack
                    .set(DeformSource::Param(0), vec![Vec2::new(4.0, 0.0); 4])
                    .unwrap();
            }
        }
        if let Some(node) = arena.get_mut(inner_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack
                    .set(
                        DeformSource::Param(0),
                        vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO],
                    )
                    .unwrap();
            }
        }

        propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
        arena.combine_deforms();

        let node = arena.get(part_id).expect("part");
        let NodeKind::Part(p) = &node.kind else {
            panic!("part isn't a part")
        };
        let c = p.deform_stack.combined();
        assert!(
            (c[0].x - 6.0).abs() < 1e-4,
            "nested part offset = {:?}",
            c[0]
        );
        assert!(c[0].y.abs() < 1e-4, "nested part offset = {:?}", c[0]);

        arena.reset_deforms();
        if let Some(node) = arena.get_mut(outer_id) {
            node.transform.scale = Vec2::splat(1e-7);
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack
                    .set(
                        DeformSource::Param(0),
                        vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO],
                    )
                    .unwrap();
            }
        }
        if let Some(node) = arena.get_mut(inner_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack
                    .set(
                        DeformSource::Param(0),
                        vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO],
                    )
                    .unwrap();
            }
        }
        arena.mark_transform_dirty(outer_id);
        arena.compute_transforms(&mut tx);
        propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
        arena.combine_deforms();

        let Some(Node {
            kind: NodeKind::Part(part),
            ..
        }) = arena.get(part_id)
        else {
            panic!("nested part disappeared");
        };
        assert!(part
            .deform_stack
            .combined()
            .iter()
            .all(|offset| offset.is_finite() && *offset == Vec2::ZERO));
    }

    #[test]
    fn outer_mg_deform_reaches_part_through_nested_mg() {
        use crate::components::{Mesh, MeshGroupData, MeshIndices, Node, NodeKind, PartData};

        let mut arena = Arena::new();

        let quad = || {
            Mesh::new(
                vec![
                    Vec2::new(0.0, 0.0),
                    Vec2::new(10.0, 0.0),
                    Vec2::new(10.0, 10.0),
                    Vec2::new(0.0, 10.0),
                ],
                vec![Vec2::ZERO; 4],
                MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
                Vec2::ZERO,
            )
        };

        let outer = MeshGroupData {
            mesh: quad().into(),
            deform_stack: crate::deform::DeformStack::new(4),
            ..Default::default()
        };
        let outer_id = arena.insert_child(
            arena.root(),
            Node {
                kind: NodeKind::MeshGroup(Box::new(outer)),
                ..Default::default()
            },
        );

        let inner = MeshGroupData {
            mesh: quad().into(),
            deform_stack: crate::deform::DeformStack::new(4),
            ..Default::default()
        };
        let inner_id = arena.insert_child(
            outer_id,
            Node {
                kind: NodeKind::MeshGroup(Box::new(inner)),
                ..Default::default()
            },
        );

        let part = PartData {
            mesh: Mesh::new(
                vec![Vec2::new(5.0, 5.0)],
                vec![Vec2::ZERO; 1],
                MeshIndices::U16(vec![]),
                Vec2::ZERO,
            )
            .into(),
            deform_stack: crate::deform::DeformStack::new(1),
            ..Default::default()
        };
        let part_id = arena.insert_child(
            inner_id,
            Node {
                kind: NodeKind::Part(Box::new(part)),
                ..Default::default()
            },
        );

        let mut tx = GlobalTransforms::new();
        arena.compute_transforms(&mut tx);
        let outer_pins = bake_mesh_group_pins(&arena, &tx, outer_id);
        let inner_pins = bake_mesh_group_pins(&arena, &tx, inner_id);
        assert!(outer_pins.per_child.contains_key(&inner_id));

        if let Some(node) = arena.get_mut(outer_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.bins = MgTriangleBins::build(&mg.mesh).map(Into::into);
                mg.pins = outer_pins.into();
                mg.deform_stack
                    .set(
                        DeformSource::Param(0),
                        vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO],
                    )
                    .unwrap();
            }
        }
        if let Some(node) = arena.get_mut(inner_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.pins = inner_pins.into();
            }
        }

        propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
        arena.combine_deforms();

        let node = arena.get(part_id).unwrap();
        let NodeKind::Part(p) = &node.kind else {
            panic!()
        };
        let c = p.deform_stack.combined();
        assert!(
            (c[0].x - 2.0).abs() < 1e-4,
            "nested part offset = {:?}",
            c[0]
        );
        assert!(c[0].y.abs() < 1e-4, "nested part offset = {:?}", c[0]);
    }

    /// A nested `translate_children` MG shifts by the outer MG's same-frame
    /// deform as well as its own params.
    #[test]
    fn outer_mg_deform_shifts_group_child_through_nested_tc_mg() {
        use crate::components::{Mesh, MeshGroupData, MeshIndices, Node, NodeKind, Transform};
        use glam::Vec3;

        // root > outer MG > inner MG (translate_children) > Group at (5,5).
        fn build() -> (Arena, GlobalTransforms, NodeIdx, NodeIdx, NodeIdx) {
            let quad = || {
                Mesh::new(
                    vec![
                        Vec2::new(0.0, 0.0),
                        Vec2::new(10.0, 0.0),
                        Vec2::new(10.0, 10.0),
                        Vec2::new(0.0, 10.0),
                    ],
                    vec![Vec2::ZERO; 4],
                    MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
                    Vec2::ZERO,
                )
            };

            let mut arena = Arena::new();
            let outer_id = arena.insert_child(
                arena.root(),
                Node {
                    kind: NodeKind::MeshGroup(Box::new(MeshGroupData {
                        mesh: quad().into(),
                        translate_children: false,
                        deform_stack: crate::deform::DeformStack::new(4),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            );
            let inner_id = arena.insert_child(
                outer_id,
                Node {
                    kind: NodeKind::MeshGroup(Box::new(MeshGroupData {
                        mesh: quad().into(),
                        translate_children: true,
                        deform_stack: crate::deform::DeformStack::new(4),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            );
            let at_5_5 = Transform {
                translation: Vec3::new(5.0, 5.0, 0.0),
                ..Default::default()
            };
            let group_id = arena.insert_child(
                inner_id,
                Node {
                    kind: NodeKind::Group,
                    transform: at_5_5,
                    base_transform: at_5_5,
                    ..Default::default()
                },
            );

            let mut tx = GlobalTransforms::new();
            arena.compute_transforms(&mut tx);
            let outer_pins = bake_mesh_group_pins(&arena, &tx, outer_id);
            let inner_pins = bake_mesh_group_pins(&arena, &tx, inner_id);
            assert!(outer_pins.per_child.contains_key(&inner_id));
            for (id, pins) in [(outer_id, outer_pins), (inner_id, inner_pins)] {
                if let Some(node) = arena.get_mut(id) {
                    if let NodeKind::MeshGroup(mg) = &mut node.kind {
                        mg.bins = MgTriangleBins::build(&mg.mesh).map(Into::into);
                        mg.pins = pins.into();
                    }
                }
            }
            (arena, tx, outer_id, inner_id, group_id)
        }

        let corner_pull = || vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO];
        let translation_of = |arena: &Arena, id: NodeIdx| {
            let t = arena.get(id).expect("group").transform.translation;
            Vec2::new(t.x, t.y)
        };

        {
            let (mut arena, mut tx, _outer_id, _inner_id, group_id) = build();
            propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
            assert_eq!(translation_of(&arena, group_id), Vec2::new(5.0, 5.0));
        }

        let own = {
            let (mut arena, mut tx, _outer_id, inner_id, group_id) = build();
            if let Some(node) = arena.get_mut(inner_id) {
                if let NodeKind::MeshGroup(mg) = &mut node.kind {
                    mg.deform_stack
                        .set(DeformSource::Param(0), corner_pull())
                        .unwrap();
                }
            }
            propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
            let shifted = translation_of(&arena, group_id);
            assert!(
                (shifted - Vec2::new(7.0, 5.0)).length() < 1e-4,
                "own shift = {shifted:?}"
            );
            assert_eq!(tx.get(group_id).w_axis.x, shifted.x);
            shifted
        };

        {
            let (mut arena, mut tx, outer_id, _inner_id, group_id) = build();
            if let Some(node) = arena.get_mut(outer_id) {
                if let NodeKind::MeshGroup(mg) = &mut node.kind {
                    mg.deform_stack
                        .set(DeformSource::Param(0), corner_pull())
                        .unwrap();
                }
            }
            propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
            let shifted = translation_of(&arena, group_id);
            assert!(
                (shifted - own).length() < 1e-4,
                "outer-driven shift = {shifted:?}, inner-driven = {own:?}"
            );
            assert!(
                (tx.get(group_id).w_axis.x - shifted.x).abs() < 1e-4,
                "global = {:?}",
                tx.get(group_id).w_axis
            );
        }
    }

    #[test]
    fn bake_and_propagate_respect_child_parent_transform_offset() {
        use crate::components::{
            Mesh, MeshGroupData, MeshIndices, Node, NodeKind, PartData, Transform,
        };
        use glam::Vec3;

        // MG at world (100,0); the child's (0,0) lands at MG-local (5,5).
        let mut arena = Arena::new();

        let mg_mesh = Mesh::new(
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            vec![Vec2::ZERO; 4],
            MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            Vec2::ZERO,
        );
        let mg = MeshGroupData {
            mesh: mg_mesh.into(),
            deform_stack: crate::deform::DeformStack::new(4),
            ..Default::default()
        };
        let mg_id = arena.insert_child(
            arena.root(),
            Node {
                transform: Transform {
                    translation: Vec3::new(100.0, 0.0, 0.0),
                    ..Default::default()
                },
                base_transform: Transform {
                    translation: Vec3::new(100.0, 0.0, 0.0),
                    ..Default::default()
                },
                kind: NodeKind::MeshGroup(Box::new(mg)),
                ..Default::default()
            },
        );

        let child_part = PartData {
            mesh: Mesh::new(
                vec![Vec2::ZERO],
                vec![Vec2::ZERO; 1],
                MeshIndices::U16(vec![]),
                Vec2::ZERO,
            )
            .into(),
            deform_stack: crate::deform::DeformStack::new(1),
            ..Default::default()
        };
        let child_id = arena.insert_child(
            mg_id,
            Node {
                transform: Transform {
                    translation: Vec3::new(5.0, 5.0, 0.0),
                    ..Default::default()
                },
                base_transform: Transform {
                    translation: Vec3::new(5.0, 5.0, 0.0),
                    ..Default::default()
                },
                kind: NodeKind::Part(Box::new(child_part)),
                ..Default::default()
            },
        );

        let mut tx = GlobalTransforms::new();
        arena.compute_transforms(&mut tx);
        let pins = bake_mesh_group_pins(&arena, &tx, mg_id);
        let child_pins = pins.per_child.get(&child_id).expect("child pins present");
        assert_eq!(child_pins.vertices[0], 0, "should fall inside triangle 0");
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.pins = pins.into();
                mg.deform_stack
                    .set(
                        DeformSource::Param(0),
                        vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO],
                    )
                    .unwrap();
            }
        }
        propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
        arena.combine_deforms();

        let node = arena.get(child_id).unwrap();
        let NodeKind::Part(p) = &node.kind else {
            panic!("not a part")
        };
        let c = p.deform_stack.combined();
        assert!((c[0].x - 2.0).abs() < 1e-4, "expected 2.0, got {:?}", c[0]);
        assert!(c[0].y.abs() < 1e-4, "expected 0.0 y, got {:?}", c[0]);
    }

    #[test]
    fn offset_transforms_via_mg_to_child_rotation() {
        use crate::components::{
            Mesh, MeshGroupData, MeshIndices, Node, NodeKind, PartData, Transform,
        };
        use glam::Vec3;
        use std::f32::consts::FRAC_PI_2;

        // Child rotated +90deg relative to MG: MG-local (1,0) is child-local (0,-1).
        let mut arena = Arena::new();

        let mg_mesh = Mesh::new(
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            vec![Vec2::ZERO; 4],
            MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            Vec2::ZERO,
        );
        let mg = MeshGroupData {
            mesh: mg_mesh.into(),
            deform_stack: crate::deform::DeformStack::new(4),
            ..Default::default()
        };
        let mg_id = arena.insert_child(
            arena.root(),
            Node {
                kind: NodeKind::MeshGroup(Box::new(mg)),
                ..Default::default()
            },
        );

        let child_part = PartData {
            mesh: Mesh::new(
                vec![Vec2::new(5.0, -5.0)],
                vec![Vec2::ZERO; 1],
                MeshIndices::U16(vec![]),
                Vec2::ZERO,
            )
            .into(),
            deform_stack: crate::deform::DeformStack::new(1),
            ..Default::default()
        };
        let child_id = arena.insert_child(
            mg_id,
            Node {
                transform: Transform {
                    rotation: Vec3::new(0.0, 0.0, FRAC_PI_2),
                    ..Default::default()
                },
                base_transform: Transform {
                    rotation: Vec3::new(0.0, 0.0, FRAC_PI_2),
                    ..Default::default()
                },
                kind: NodeKind::Part(Box::new(child_part)),
                ..Default::default()
            },
        );

        let mut tx = GlobalTransforms::new();
        arena.compute_transforms(&mut tx);
        let pins = bake_mesh_group_pins(&arena, &tx, mg_id);
        let child_pins = pins.per_child.get(&child_id).unwrap().clone();

        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                std::sync::Arc::make_mut(&mut mg.pins)
                    .per_child
                    .insert(child_id, child_pins);
                mg.deform_stack
                    .set(DeformSource::Param(0), vec![Vec2::new(1.0, 0.0); 4])
                    .unwrap();
            }
        }
        propagate_mesh_group_deforms(&mut arena, &mut tx, Mat4::IDENTITY);
        arena.combine_deforms();

        let node = arena.get(child_id).unwrap();
        let NodeKind::Part(p) = &node.kind else {
            panic!()
        };
        let c = p.deform_stack.combined();
        assert!(c[0].x.abs() < 1e-4, "expected x ~0, got {:?}", c[0]);
        assert!(
            (c[0].y + 1.0).abs() < 1e-4,
            "expected y ~-1, got {:?}",
            c[0]
        );
    }
}
