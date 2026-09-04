//! Mesh groups: vertex-level deforms applied to a subtree.
//!
//! **Mesh-group descent stops at a nested MG.** `descendant_meshed_nodes`
//! recurses through Parts and Composites, collects Parts and nested MGs, and
//! halts at each nested MG; the outer deform reaches the inner MG's children
//! transitively through the pre-order propagation pass. Binding them directly
//! would apply the deform twice. Same reasoning for `translate_children_targets`,
//! which shifts only descendants without a mesh. A Composite with
//! `propagate_mesh_group = false` halts both walks.

use glam::swizzles::Vec4Swizzles;
use glam::{Affine2, Mat2, Mat4, Vec2, Vec4};

use crate::{
    components::{checked_affine_inverse, Mesh, MeshIndices, NodeIdx, NodeKind},
    deform::DeformSource,
    node::NodeTree,
    puppet::{Arena, GlobalTransforms},
};
use std::collections::HashMap;

/// Per-vertex triangle hints baked at load time: the MG triangle that
/// covered the child vertex at its base position, `0` for a vertex no
/// triangle covered. Propagation looks the triangle up again against the
/// vertex's *current* position and only starts from the hint, so a stale
/// one costs a scan, never a wrong answer. Neither the barycentric weights
/// nor the MG↔child transforms are baked: both are recomputed per frame
/// from current globals (see `propagate_mesh_group_deforms`), because
/// params that drive transforms would make load-time values stale.
#[derive(Debug, Clone)]
pub(crate) struct ChildAttachment {
    pub(crate) vertices: Vec<u32>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MeshGroupAttachments {
    pub(crate) per_child: HashMap<NodeIdx, ChildAttachment>,
}

/// O(1) point-in-triangle lookup baked at load time. Each cell stores
/// `triangle_index + 1` (0 means "no triangle covers this cell"). A
/// `lookup(p)` floors `p - bounds_min` to find the cell. Each triangle is
/// rasterized into the integer-cell grid spanning the MG mesh's bounding box
/// by testing the cell's lower-left corner.
///
/// Overlapping triangles use last-write-wins. A 2D MG with overlapping
/// triangles is malformed input.
#[derive(Debug, Clone)]
pub(crate) struct MgTriangleBitmap {
    /// Cell `(x, y)` covers `[bounds_min + (x, y), bounds_min + (x+1, y+1))`.
    cells: Vec<u16>,
    width: u32,
    height: u32,
    bounds_min: Vec2,
}

impl MgTriangleBitmap {
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
        let bounds_min = Vec2::new(min.x.floor(), min.y.floor());
        let width = (max.x.ceil() - min.x.floor() + 1.0).max(1.0) as u32;
        let height = (max.y.ceil() - min.y.floor() + 1.0).max(1.0) as u32;
        // Sanity-cap to avoid pathological allocations on malformed
        // input. 4096*4096 = 16M cells is generous for any reasonable
        // 2D character model.
        if width > 4096 || height > 4096 {
            tracing::warn!(
                width,
                height,
                "MG triangle bitmap exceeds the 4096x4096 cap; falling back to the linear scan"
            );
            return None;
        }

        let tri_count = triangle_count(&mesh.indices);
        // Cells store `tri + 1` as u16 (0 = empty), so the +1 sentinel
        // must not overflow u16.
        if tri_count + 1 > u16::MAX as u32 {
            tracing::warn!(
                triangle_count = tri_count,
                "MG triangle count exceeds the u16 bitmap ceiling; falling back to the linear scan"
            );
            return None;
        }

        let mut cells = vec![0u16; (width * height) as usize];
        for tri in 0..tri_count {
            let Some((a, b, c)) = triangle_vertices_raw(&mesh.vertices, &mesh.indices, tri) else {
                continue;
            };
            let (a, b, c) = (a - mesh.origin, b - mesh.origin, c - mesh.origin);
            let tmin_x = a.x.min(b.x).min(c.x);
            let tmin_y = a.y.min(b.y).min(c.y);
            let tmax_x = a.x.max(b.x).max(c.x);
            let tmax_y = a.y.max(b.y).max(c.y);
            let left = tmin_x.floor() as i32;
            let top = tmin_y.floor() as i32;
            let bwidth = (tmax_x.ceil() - tmin_x.floor() + 1.0).max(0.0) as i32;
            let bheight = (tmax_y.ceil() - tmin_y.floor() + 1.0).max(0.0) as i32;
            for y in 0..bheight {
                for x in 0..bwidth {
                    let pt = Vec2::new((left + x) as f32, (top + y) as f32);
                    let w = barycentric(pt, a, b, c);
                    if w[0].is_nan() || !inside(w) {
                        continue;
                    }
                    let cx = pt.x - bounds_min.x;
                    let cy = pt.y - bounds_min.y;
                    if cx < 0.0 || cy < 0.0 {
                        continue;
                    }
                    let cx = cx as u32;
                    let cy = cy as u32;
                    if cx >= width || cy >= height {
                        continue;
                    }
                    cells[(cy * width + cx) as usize] = (tri + 1) as u16;
                }
            }
        }
        Some(Self {
            cells,
            width,
            height,
            bounds_min,
        })
    }

    fn lookup(&self, p: Vec2) -> Option<u32> {
        let cx = (p.x - self.bounds_min.x).floor();
        let cy = (p.y - self.bounds_min.y).floor();
        if cx < 0.0 || cy < 0.0 {
            return None;
        }
        let cx = cx as u32;
        let cy = cy as u32;
        if cx >= self.width || cy >= self.height {
            return None;
        }
        let cell = self.cells[(cy * self.width + cx) as usize];
        if cell == 0 {
            None
        } else {
            Some((cell - 1) as u32)
        }
    }
}

fn barycentric(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> [f32; 3] {
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

/// Strict variant: returns the triangle only when `p` is INSIDE one, so
/// vertices outside the MG mesh are passed through unchanged.
///
/// `hint` is checked first; correct in the common small-deform case
/// (the triangle that covered the base position usually still covers
/// the small-perturbed position) and saves the linear scan over all
/// MG triangles. On a large Body MG (~2000 child verts over dozens of
/// triangles) this is ~10× faster than the unhinted scan.
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

/// Meshed descendants that receive this MG's vertex-level deform
/// attachments: recurse into Parts and Composites, collect Parts and
/// nested MGs, and stop at each nested MG. The outer deform reaches an inner
/// MG's children transitively through the pre-order propagation pass. Binding
/// those children directly to the outer MG would apply its deform twice.
/// Descendants without a mesh, under a `translateChildren=true` MG, receive a
/// Node-level shift through `apply_translate_children_filter` instead.
fn descendant_meshed_nodes(tree: &NodeTree, root: NodeIdx, arena: &Arena) -> Vec<NodeIdx> {
    let mut out = Vec::new();
    let mut stack: Vec<NodeIdx> = tree.get_children(root);
    while let Some(id) = stack.pop() {
        match arena.get(id).map(|n| &n.kind) {
            Some(NodeKind::Part(_)) => {
                out.push(id);
                stack.extend(tree.get_children(id));
            }
            // `isComposite` in the reference is
            // `typeId == "Composite" && propagateMeshGroup`, so a
            // Composite that opts out halts the walk.
            Some(NodeKind::Composite(c)) => {
                if c.propagate_mesh_group {
                    stack.extend(tree.get_children(id));
                }
            }
            Some(NodeKind::MeshGroup(_)) => {
                out.push(id);
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

/// 2D-affine projection of a Mat4. Keeps the upper-left 2x2 from the
/// X/Y columns and the XY of the W column as the 2D translation. The
/// previous `Mat3::from_mat4` + `Affine2::from_mat3` round-trip silently
/// dropped the translation: `Mat3::from_mat4` keeps columns 0..2 (X/Y/Z
/// basis) and discards W, while `Affine2::from_mat3` then reads the
/// translation out of the Mat3's *Z* column — which on a 3D-style Mat4
/// is `(0, 0, 1)`, not the actual translation. Result: every child→MG
/// and MG→child transform used by the propagation path applied
/// rotation/scale only, putting parts whose local frame has any
/// translation offset relative to their MG (e.g. an eyelash Part
/// inside an Eye MG) at the wrong MG-local point and looking up the
/// wrong bitmap cell.
pub(crate) fn affine2_from_mat4(m: Mat4) -> Affine2 {
    Affine2::from_cols(m.x_axis.xy(), m.y_axis.xy(), m.w_axis.xy())
}

/// Project child vertices into MG-local space using the global
/// transforms before looking up their triangle, and stash the reverse
/// 2x2 so `propagate_mesh_group_deforms` can apply MG-local offsets as
/// child-local offsets the renderer can add to child vertices in
/// child-local space. Without this, any MG whose children don't share
/// its local frame produces misplaced attachments and offsets applied in
/// the wrong basis, which shows up as dramatic distortion when a deform
/// param drives those MGs. `transforms` is the arena's load-time
/// GlobalTransforms; callers should compute it immediately before baking.
pub(crate) fn bake_mesh_group_attachments(
    arena: &Arena,
    transforms: &GlobalTransforms,
    mesh_group_id: NodeIdx,
) -> MeshGroupAttachments {
    let mut out = MeshGroupAttachments::default();
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
    for child_id in descendant_meshed_nodes(&arena.tree, mesh_group_id, arena) {
        let Some(child_verts) = drawable_mesh_vertices(arena, child_id) else {
            continue;
        };
        let child_origin = drawable_mesh_origin(arena, child_id);

        let child_global = transforms.get(child_id);
        let child_to_mg = mg_global_inv * child_global;

        // Strict inside-only lookup leaves vertices outside every MG triangle
        // unchanged. A closest-edge fallback smears edge deformation and
        // causes feature drift at extreme poses.
        //
        // GPU draws `v - origin`; lookups must use the same local space.
        let mut vertices = Vec::with_capacity(child_verts.len());
        for &v in child_verts {
            let v_in_mg = transform_point(child_to_mg, v - child_origin);
            vertices.push(
                find_triangle_strict_hint(&mg_local, &mg_mesh.indices, v_in_mg, 0).unwrap_or(0),
            );
        }
        if !vertices.is_empty() {
            out.per_child.insert(child_id, ChildAttachment { vertices });
        }
    }

    out
}

/// Descendants without a mesh, reached from a `translateChildren=true` MG.
/// Walk through Parts and Composites, collecting Group, Origin, and
/// SimplePhysics nodes at the ends without descending past them. Nested MGs
/// receive vertex deformation through `descendant_meshed_nodes`; applying a
/// Node-level shift to them too would double the deform.
fn translate_children_targets(tree: &NodeTree, mg_id: NodeIdx, arena: &Arena) -> Vec<NodeIdx> {
    // Descend only through Parts and Composites: the stop condition
    // halts descent at (but still includes) everything else, so a nested
    // MG or a collected Node never has its subtree walked. The retain
    // then drops the Parts/Composites/MGs, leaving the Node-level targets.
    // A Composite with propagateMeshGroup=false clears `isComposite` in
    // the reference guard, so the walk halts there too.
    let descends = |id: NodeIdx| match arena.get(id).map(|n| &n.kind) {
        Some(NodeKind::Part(_)) => true,
        Some(NodeKind::Composite(c)) => c.propagate_mesh_group,
        _ => false,
    };
    let mut targets = tree.get_descendants_until(mg_id, |id| !descends(id));
    targets.retain(|&id| {
        !matches!(
            arena.get(id).map(|n| &n.kind),
            Some(NodeKind::Part(_)) | Some(NodeKind::Composite(_)) | Some(NodeKind::MeshGroup(_))
        )
    });
    targets
}

/// Pre-order (root-to-leaf among MeshGroups). The order is load-
/// bearing for nested MGs: an outer MG pushes into a nested MG's
/// stack, so the outer must propagate before the inner combines and
/// pushes to its own children — deform flows outer → inner → Part in
/// a single pass.
fn mesh_group_pre_order(arena: &Arena) -> Vec<NodeIdx> {
    arena.tree.with_dfs_order(|dfs| {
        dfs.iter()
            .copied()
            .filter(|id| {
                matches!(
                    arena.get(*id).map(|n| &n.kind),
                    Some(NodeKind::MeshGroup(_))
                )
            })
            .collect()
    })
}

// Ensure the cache is populated, then hand ownership to the caller.
// Caller is responsible for putting the Vec back via
// `restore_mg_pre_order_cache`. This avoids a per-frame clone while
// keeping the hot-path borrow of the arena mutable.
fn take_mg_pre_order(arena: &mut Arena) -> Vec<NodeIdx> {
    if arena.mg_pre_order_cache.is_none() {
        arena.mg_pre_order_cache = Some(mesh_group_pre_order(arena));
    }
    arena.mg_pre_order_cache.take().unwrap_or_default()
}

/// Per-MeshGroup: combine its own stack (Param sources targeting the
/// MG's lattice, plus Node sources pushed by an outer MG), then push
/// the combined deform to each child in `per_child` via
/// `DeformSource::Node(mg_id)` — Parts and nested MGs alike (see
/// `mesh_group_pre_order` for the ordering that makes the nested
/// chain compose).
///
/// One path, per child: map the child's CURRENT position
/// (`base + cur_deform`, where `cur_deform` excludes any prior
/// Node(mg_id) source) into MG-local, find the triangle at runtime,
/// sample the deformed MG triangle there, transform back to
/// child-local, and emit the absolute-replacement delta. Sampling at
/// the current rather than the base position is what keeps a child's
/// own Param deltas and the MG's deltas from double-pulling at extreme
/// param values.
pub(crate) fn propagate_mesh_group_deforms(arena: &mut Arena, transforms: &GlobalTransforms) {
    let _span = tracing::debug_span!("propagate_mesh_group_deforms").entered();
    let order = take_mg_pre_order(arena);

    // Pull the arena-owned scratch out so we can interleave borrows of
    // the MG (read) and of each child (write) without building a
    // per-child Vec<Vec2> or an outer (NodeIdx, Vec<Vec2>) Vec.
    let mut scratch = std::mem::take(&mut arena.mg_propagate_scratch);
    let mut cur_deform_scratch = std::mem::take(&mut arena.mg_cur_deform_scratch);
    let mut deformed_mg_vertices = std::mem::take(&mut arena.mg_deformed_vertices_scratch);
    let mut child_ids: smallvec::SmallVec<[NodeIdx; 8]> = smallvec::SmallVec::new();

    for &mg_id in &order {
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack.combine();
            }
        }

        // Gather child ids. Propagation still runs when the MG has zero
        // deform but a child has its own non-zero prior deform; the
        // per-child step handles that zero check.
        child_ids.clear();
        {
            let Some(node) = arena.get(mg_id) else {
                continue;
            };
            let NodeKind::MeshGroup(mg) = &node.kind else {
                continue;
            };
            child_ids.extend(mg.attachments.per_child.keys().copied());

            // Pre-compute mg_vertices[i] + mg_combined[i] once per MG;
            // reused across every child of this MG. Several children
            // typically hit the same MG triangle, so this avoids
            // re-doing the add per child-vert.
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

        // Invert the MG's current global transform every frame. Without this, any
        // param that drives transforms (Yaw-Pitch on Eye/LIP MGs, Mouth
        // Shape t.y on Mouth Inner, etc.) puts children at the wrong MG-
        // local point because catchlight's load-time `child_to_mg_2d` is
        // stale relative to runtime globals.
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
                &deformed_mg_vertices,
                child_to_mg_2d,
                mg_to_child_2d,
            );
        }
    }

    arena.mg_propagate_scratch = scratch;
    arena.mg_cur_deform_scratch = cur_deform_scratch;
    arena.mg_deformed_vertices_scratch = deformed_mg_vertices;
    // Put the cache back unless invalidated during propagate (no caller
    // in the hot path mutates the tree, but check to be safe).
    if arena.mg_pre_order_cache.is_none() {
        arena.mg_pre_order_cache = Some(order);
    }
}

/// Apply each `translate_children=true` MG's deformation as a
/// Node-level transform shift on its descendants without a mesh
/// (Origin Nodes, Group Nodes, SimplePhysics nodes).
///
/// For each target Origin node:
///
///   centerMatrix = MG.global.inverse * parent.global
///   cVertex      = centerMatrix * local_translation
///   newPos       = (in-bitmap) deformed_triangle_warp(cVertex)
///   delta        = (parent.global.inv * MG.global)_linear * (newPos - cVertex)
///   transform.translation += delta
///
/// `node.transform.translation` — the base plus whatever this frame's params
/// already shifted — is the vertex, and the resulting delta is added back
/// into it.
///
/// Must run AFTER `compute_transforms` (we read parent.global) and
/// BEFORE the second `compute_transforms` pass that propagates the
/// shifted Origin Node transforms to descendants. The MG's stack is
/// combined here from Param sources only — outer-MG-pushed Node
/// sources are intentionally ignored because this filter uses the MG's own
/// parameter-driven deformation, not an outer MG's contribution.
pub(crate) fn apply_translate_children_filter(
    arena: &mut Arena,
    transforms: &GlobalTransforms,
) -> bool {
    let _span = tracing::trace_span!("apply_translate_children_filter").entered();

    let order = take_mg_pre_order(arena);
    let mut targets: smallvec::SmallVec<[NodeIdx; 8]> = smallvec::SmallVec::new();
    let mut shifted = false;

    for &mg_id in &order {
        // Collect tc=true info up-front under a non-mut borrow.
        let tc = match arena.get(mg_id).map(|n| &n.kind) {
            Some(NodeKind::MeshGroup(mg)) => mg.translate_children,
            _ => continue,
        };
        if !tc {
            continue;
        }

        // Combine the MG's stack so we can read combined() below.
        // Idempotent + cheap when nothing's dirty.
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.deform_stack.combine();
            }
        }

        targets.clear();
        targets.extend(translate_children_targets(&arena.tree, mg_id, arena));
        if targets.is_empty() {
            continue;
        }

        let mg_global = transforms.get(mg_id);
        let Some(mg_global_inv) = checked_affine_inverse(mg_global) else {
            continue;
        };

        for &target_id in &targets {
            let parent_id = match arena.tree.get_parent(target_id) {
                Some(p) => p,
                None => continue,
            };
            let parent_global = transforms.get(parent_id);

            // Project the target's CURRENT position (= base plus the
            // transform delta) into MG-local space: transform.translation
            // already includes any param shift this frame (apply_params has
            // run).
            let cur_local = match arena.get(target_id) {
                Some(n) => Vec2::new(n.transform.translation.x, n.transform.translation.y),
                None => continue,
            };
            let world_cur = parent_global * Vec4::new(cur_local.x, cur_local.y, 0.0, 1.0);
            let cvertex_full = mg_global_inv * world_cur;
            let cvertex_proj = Vec2::new(cvertex_full.x, cvertex_full.y);

            // Find the MG triangle covering cvertex_proj. If outside,
            // skip — newPos = cvertex_proj makes delta = 0.
            let delta_mg = match arena.get(mg_id).map(|n| &n.kind) {
                Some(NodeKind::MeshGroup(mg)) => {
                    let mg_indices = &mg.mesh.indices;
                    let mg_local = local_positions(&mg.mesh);
                    let combined = mg.deform_stack.combined();
                    let tri_idx = match mg.bitmap.as_ref() {
                        Some(bm) => bm.lookup(cvertex_proj),
                        None => find_triangle_strict_hint(&mg_local, mg_indices, cvertex_proj, 0),
                    };
                    let Some(tri_idx) = tri_idx else { continue };
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
                        _ => continue,
                    };
                    let w = barycentric(cvertex_proj, mg_local[a], mg_local[b], mg_local[c]);
                    if w[0].is_nan() {
                        continue;
                    }
                    // delta_mg = barycentric-weighted MG deform at
                    // cvertex_proj. Equivalent to (newPos - cvertex_proj)
                    // since newPos = cvertex_proj + delta_mg once the
                    // base barycentric weights sum to 1.
                    combined[a] * w[0] + combined[b] * w[1] + combined[c] * w[2]
                }
                _ => continue,
            };

            // mg_to_parent linear: (parent.global.inverse * MG.global)
            // upper-left 2x2. Translation is stripped because this maps a
            // displacement vector rather than a point.
            let Some(parent_global_inv) = checked_affine_inverse(parent_global) else {
                continue;
            };
            let mg_to_parent = parent_global_inv * mg_global;
            let mg_to_parent_linear = linear_mat2(mg_to_parent);
            let delta_parent = mg_to_parent_linear * delta_mg;

            if delta_parent == Vec2::ZERO {
                continue;
            }

            if let Some(node) = arena.get_mut(target_id) {
                node.transform.translation.x += delta_parent.x;
                node.transform.translation.y += delta_parent.y;
            }
            arena.mark_transform_dirty(target_id);
            shifted = true;
        }
    }

    if arena.mg_pre_order_cache.is_none() {
        arena.mg_pre_order_cache = Some(order);
    }
    shifted
}

#[allow(clippy::too_many_arguments)]
fn propagate_to_child(
    arena: &mut Arena,
    mg_id: NodeIdx,
    child_id: NodeIdx,
    scratch: &mut Vec<Vec2>,
    cur_deform_scratch: &mut Vec<Vec2>,
    deformed_mg_vertices: &[Vec2],
    child_to_mg_2d: Affine2,
    mg_to_child_2d: Affine2,
) {
    // Phase 0: read `cur_deform` (the child's deform MINUS any prior
    // Node(mg_id) source) into cur_deform_scratch. Deactivating the
    // Node source first (a flag flip) keeps every early-return below
    // correct — a path that bails leaves the stale contribution
    // dropped, exactly as if this MG never propagated. The read-only
    // sum replaces a full combine + copy, so the stack's combined /
    // generation stay untouched until the final combine_deforms.
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

    // Phase 1: read MG + child mesh, fill scratch with offsets.
    let ok = {
        let Some(mg_node) = arena.get(mg_id) else {
            return;
        };
        let NodeKind::MeshGroup(mg) = &mg_node.kind else {
            return;
        };
        let Some(attachment) = mg.attachments.per_child.get(&child_id) else {
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

        // Identity-pass shortcut: if both the MG and the child have zero
        // deform, propagation emits all-zero deltas. Skipping the
        // per-vert triangle scan + the dirty-marking source_buf_mut
        // saves ~1ms per frame on a complex model (a large Body MG has
        // many child verts and many MG triangles; per-vert find_triangle
        // is O(triangles) per vert without the bitmap).
        let mg_zero = mg_combined.iter().all(|v| *v == Vec2::ZERO);
        let child_zero = cur_deform_scratch.iter().all(|v| *v == Vec2::ZERO);
        if mg_zero && child_zero {
            scratch.clear();
            return;
        }

        let mg_local = local_positions(&mg.mesh);
        let mg_indices = &mg.mesh.indices;
        let vert_count = mg_local.len();
        let bitmap = mg.bitmap.as_ref();
        let n = child_verts.len();
        scratch.clear();
        scratch.reserve(n);
        for (i, &base_v) in child_verts.iter().enumerate() {
            let cv_child_local = base_v - child_origin + cur_deform_scratch[i];
            let cv_mg_local = child_to_mg_2d.transform_point2(cv_child_local);

            // Strict inside-only lookup leaves vertices outside the MG mesh
            // unchanged. With the bitmap we get O(1) lookup; the
            // hinted scan is the fallback for empty / oversized MGs.
            let tri_idx = match bitmap {
                Some(bm) => bm.lookup(cv_mg_local),
                None => {
                    let hint = attachment.vertices.get(i).copied().unwrap_or(0);
                    find_triangle_strict_hint(&mg_local, mg_indices, cv_mg_local, hint)
                }
            };
            let Some(tri_idx) = tri_idx else {
                scratch.push(Vec2::ZERO);
                continue;
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
                    scratch.push(Vec2::ZERO);
                    continue;
                }
            };

            let weights = barycentric(
                cv_mg_local,
                mg_local[a_idx],
                mg_local[b_idx],
                mg_local[c_idx],
            );
            if weights[0].is_nan() {
                scratch.push(Vec2::ZERO);
                continue;
            }

            // `deformed_mg_vertices` is filled to exactly `mg_vertices.len()`
            // in both arms of the per-MG precompute, and `a/b/c_idx` are
            // bounded by that same count above.
            let mg_local_pos = deformed_mg_vertices[a_idx] * weights[0]
                + deformed_mg_vertices[b_idx] * weights[1]
                + deformed_mg_vertices[c_idx] * weights[2];

            let new_pos_child_local = mg_to_child_2d.transform_point2(mg_local_pos);
            scratch.push(new_pos_child_local - (base_v - child_origin) - cur_deform_scratch[i]);
        }
        true
    };
    if !ok {
        return;
    }

    // Phase 2: write scratch into the child's pooled deform slot.
    let Some(child) = arena.get_mut(child_id) else {
        return;
    };
    let stack = match &mut child.kind {
        NodeKind::Part(p) => Some(&mut p.deform_stack),
        NodeKind::MeshGroup(mg) => Some(&mut mg.deform_stack),
        _ => None,
    };
    if let Some(stack) = stack {
        if scratch.len() == stack.vert_count {
            let buf = stack.source_buf_mut(DeformSource::Node(mg_id));
            buf.copy_from_slice(scratch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Composite with `propagateMeshGroup=false` clears `isComposite` in the
    /// reference's recursion guard, so the MeshGroup must not bind the Parts
    /// beneath it. With the flag ignored, both Parts get bound and the whole
    /// subtree is warped by a lattice it opted out of.
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
    fn bitmap_lookup_matches_strict_scan_on_quad() {
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
        let bm = MgTriangleBitmap::build(&mesh).expect("bitmap");
        // Inside tri 0 (0,1,2): a point clearly to the right edge.
        let p = Vec2::new(8.0, 1.0);
        let scan = find_triangle_strict_hint(&mesh.vertices, &mesh.indices, p, 0);
        assert_eq!(bm.lookup(p), scan);
        // Inside tri 1 (0,2,3): clearly upper-left.
        let p = Vec2::new(1.0, 8.0);
        let scan = find_triangle_strict_hint(&mesh.vertices, &mesh.indices, p, 0);
        assert_eq!(bm.lookup(p), scan);
        // Outside the quad: the bitmap returns None, matching the
        // strict scan's identity-passthrough convention.
        assert_eq!(bm.lookup(Vec2::new(-5.0, -5.0)), None);
        assert_eq!(bm.lookup(Vec2::new(100.0, 100.0)), None);
    }

    #[test]
    fn bitmap_handles_empty_mesh() {
        let mesh = Mesh::new(
            Vec::<Vec2>::new(),
            Vec::<Vec2>::new(),
            MeshIndices::U16(vec![]),
            Vec2::ZERO,
        );
        assert!(MgTriangleBitmap::build(&mesh).is_none());
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
            mesh: mg_mesh,
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
            mesh: child_mesh,
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

        // Bake the attachments and install them.
        let mut tx = GlobalTransforms::new();
        arena.compute_transforms(&mut tx);
        let attachments = bake_mesh_group_attachments(&arena, &tx, mg_id);
        assert!(attachments.per_child.contains_key(&child_id));
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.attachments = attachments;
            }
        }

        // Synthetic deform on MG: move vertex 2 (10,10) by (+4, 0). A
        // child vertex at (5,5) sits in triangle 0 (0,1,2) with baryc
        // (0, 0.5, 0.5). Expected child offset = 0.5 * (4,0) = (2,0).
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

        propagate_mesh_group_deforms(&mut arena, &tx);

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

    /// When the child has its own non-zero deform before propagation
    /// (e.g. a Param-driven offset), the MG deform is evaluated at the
    /// CURRENT child position and REPLACES the child's Node deform with
    /// `newPos - base`. Evaluating at the BASE position and adding would
    /// double-pull at extreme params.
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
                mesh: mg_mesh,
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
                ),
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
            let attachments = bake_mesh_group_attachments(&arena, &tx, mg_id);
            if let Some(node) = arena.get_mut(mg_id) {
                if let NodeKind::MeshGroup(mg) = &mut node.kind {
                    mg.bitmap = MgTriangleBitmap::build(&mg.mesh);
                    mg.attachments = attachments;
                    // Non-linear MG deform: vertex 1 +(2,0), vertex 2 +(4,0).
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

            // Child has its own Param-source delta of (2, 0) — so the
            // child's "current" position is (5+2, 5) = (7, 5), which
            // changes which barycentric weights are used.
            if let Some(node) = arena.get_mut(child_id) {
                if let NodeKind::Part(p) = &mut node.kind {
                    p.deform_stack
                        .set(DeformSource::Param(1), vec![Vec2::new(2.0, 0.0)])
                        .unwrap();
                }
            }

            (arena, child_id)
        }

        // Barycentric of (7,5) in tri 0 is (0.3, 0.2, 0.5).
        //   mg_local_pos = 0.3*(0,0) + 0.2*((10,0)+(2,0)) + 0.5*((10,10)+(4,0))
        //                = (2.4, 0) + (7, 5) = (9.4, 5).
        //   delta from MG = (9.4, 5) - (5, 5) - (2, 0) = (2.4, 0).
        // Combined = Param(2,0) + Node(2.4, 0) = (4.4, 0).
        //
        // Sampling at the BASE position instead would take the weights of
        // (5,5) — (0.5, 0, 0.5) — for an offset of (2,0), and a combined
        // (4.0, 0): the double pull this avoids.
        let (mut puppet, child_id) = build();
        let mut tx = GlobalTransforms::new();
        puppet.compute_transforms(&mut tx);
        propagate_mesh_group_deforms(&mut puppet, &tx);
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

        // The outer MG's lattice covers the inner's — wherever the
        // inner's own deform carries its vertices, they stay inside a
        // triangle of the outer — and a Part sits under the inner.
        // Driving both MGs must compose: the outer pushes into the
        // inner's stack, and the inner pushes its combined (own +
        // outer) deform to the Part.
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
            mesh: quad(20.0),
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
            mesh: quad(10.0),
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
            ),
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

        // Descent stops at the nested MG but binds it: the outer holds
        // the inner MG, the inner holds the Part — the outer never
        // binds the grandchild Part directly.
        let mut tx = GlobalTransforms::new();
        arena.compute_transforms(&mut tx);
        let outer_attachments = bake_mesh_group_attachments(&arena, &tx, outer_id);
        let inner_attachments = bake_mesh_group_attachments(&arena, &tx, inner_id);
        assert!(!outer_attachments.per_child.contains_key(&part_id));
        assert!(inner_attachments.per_child.contains_key(&part_id));
        assert!(outer_attachments.per_child.contains_key(&inner_id));

        if let Some(node) = arena.get_mut(outer_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.attachments = outer_attachments;
            }
        }
        if let Some(node) = arena.get_mut(inner_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.attachments = inner_attachments;
            }
        }

        // Drive the outer as a uniform (+4,0) — every point inside it
        // shifts by that — and the inner by (+4,0) on its vertex 2
        // alone. The outer pushes Node(outer) = (4,0) onto all four of
        // the inner's vertices, so the inner's combined is (4,0)
        // everywhere and (8,0) at vertex 2. The part at (5,5) sits in
        // the inner's tri 0 with weights (0.5, 0, 0.5), so it lands at
        // 0.5*(4,0) + 0.5*((10,10)+(8,0)) = (11,5): an offset of (6,0),
        // the outer's 4 and the inner's own 2 composed.
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

        propagate_mesh_group_deforms(&mut arena, &tx);
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
        propagate_mesh_group_deforms(&mut arena, &tx);
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

    /// An outer MG's deform (matching the reference's postProcessFilter
    /// for meshed children, meshgroup/package.d:292-294) must carry
    /// through a nested MG to the Part under it.
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
            mesh: quad(),
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
            mesh: quad(),
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
            ),
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
        let outer_attachments = bake_mesh_group_attachments(&arena, &tx, outer_id);
        let inner_attachments = bake_mesh_group_attachments(&arena, &tx, inner_id);
        assert!(outer_attachments.per_child.contains_key(&inner_id));

        // Drive the OUTER only: move its vertex 2 (10,10) by (+4,0).
        if let Some(node) = arena.get_mut(outer_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.bitmap = MgTriangleBitmap::build(&mg.mesh);
                mg.attachments = outer_attachments;
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
                mg.attachments = inner_attachments;
            }
        }

        propagate_mesh_group_deforms(&mut arena, &tx);
        arena.combine_deforms();

        // Outer deform at the inner's lattice vertex (10,10) is (4,0);
        // the inner then carries it to the part at (5,5) with weights
        // (0, 0.5, 0.5) -> (2,0).
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

    #[test]
    fn bake_and_propagate_respect_child_parent_transform_offset() {
        use crate::components::{
            Mesh, MeshGroupData, MeshIndices, Node, NodeKind, PartData, Transform,
        };
        use glam::Vec3;

        // MG quad spans (0..10, 0..10) in MG-local. Put the MG at world
        // (100, 0) and give the child Part an additional translation of
        // (5, 5) relative to the MG, so the child's (0,0) vertex lands
        // at the center of the MG quad in MG-local. A child vertex at
        // (0,0) child-local should therefore bind to triangle 0 (or 1)
        // with the center barycentric — the pre-fix code compared
        // child-local (0,0) to MG quad and fell off to an edge fallback.
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
            mesh: mg_mesh,
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

        // Child Part at (5,5) relative to MG, with a single vertex at
        // (0,0) child-local (-> (5,5) in MG-local, center of tri 0).
        let child_part = PartData {
            mesh: Mesh::new(
                vec![Vec2::ZERO],
                vec![Vec2::ZERO; 1],
                MeshIndices::U16(vec![]),
                Vec2::ZERO,
            ),
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
        let attachments = bake_mesh_group_attachments(&arena, &tx, mg_id);
        let attachment = attachments
            .per_child
            .get(&child_id)
            .expect("child attachments present");
        // Center of tri 0 (verts 0,1,2), whose baryc is (0, 0.5, 0.5).
        assert_eq!(attachment.vertices[0], 0, "should fall inside triangle 0");
        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.attachments = attachments;
                // Move MG vertex 2 (at (10,10)) by (+4,0). Child at
                // barycentric (0, 0.5, 0.5) picks up 0.5*(4,0) = (2,0).
                mg.deform_stack
                    .set(
                        DeformSource::Param(0),
                        vec![Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0), Vec2::ZERO],
                    )
                    .unwrap();
            }
        }
        propagate_mesh_group_deforms(&mut arena, &tx);
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

        // Same quad layout. MG identity-rotated; child rotated +90deg
        // relative to MG. MG-local offset (1, 0) should come out as
        // (0, -1) in child-local (rotation inverse), so that after the
        // child's own local→world rotation, the world delta matches.
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
            mesh: mg_mesh,
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

        // Child vertex (5, -5) child-local. After +90° z-rotation, it
        // maps to (5, 5) MG-local, which sits inside the MG mesh (the
        // strict-inside attachment requires this; out-of-bounds vertices produce
        // zero offset).
        let child_part = PartData {
            mesh: Mesh::new(
                vec![Vec2::new(5.0, -5.0)],
                vec![Vec2::ZERO; 1],
                MeshIndices::U16(vec![]),
                Vec2::ZERO,
            ),
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
        let attachments = bake_mesh_group_attachments(&arena, &tx, mg_id);
        let attachment = attachments.per_child.get(&child_id).unwrap().clone();

        if let Some(node) = arena.get_mut(mg_id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                mg.attachments.per_child.insert(child_id, attachment);
                // Set MG deforms so barycentric sum at the child vertex
                // yields (1, 0) in MG-local.
                mg.deform_stack
                    .set(DeformSource::Param(0), vec![Vec2::new(1.0, 0.0); 4])
                    .unwrap();
            }
        }
        propagate_mesh_group_deforms(&mut arena, &tx);
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
