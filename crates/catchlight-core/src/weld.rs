//! Vertex welding: paired vertices of two Parts are pulled to a weighted
//! meeting point so a seam stays closed under deforms.

use crate::components::{NodeIdx, NodeKind, PartData};
use crate::deform::DeformSource;
use crate::meshgroup::{affine2_from_mat4, linear_mat2};
use crate::puppet::{Arena, GlobalTransforms};
use glam::{Affine2, Mat2, Vec2};

/// Each unordered `{a, b}` pair appears in at most one record.
#[derive(Debug, Clone)]
pub(crate) struct Weld {
    pub a: NodeIdx,
    pub b: NodeIdx,
    pub pairs: Vec<WeldPair>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WeldPair {
    pub a_vert: u32,
    pub b_vert: u32,
    /// A's share of the meeting point: 1.0 pins A, 0.0 pins B.
    pub weight: f32,
}

// Deltas are direction vectors, so only the inverse linear part applies on
// the way back out.
struct Side {
    world: Affine2,
    inv_linear: Mat2,
}

fn side(transforms: &GlobalTransforms, id: NodeIdx) -> Option<Side> {
    let world = affine2_from_mat4(transforms.get(id));
    let linear = linear_mat2(transforms.get(id));
    let det = linear.determinant();
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    Some(Side {
        world,
        inv_linear: linear.inverse(),
    })
}

/// Must run after mesh-group propagation and before `combine_deforms`.
/// Records solve sequentially, each seeing earlier records' writes.
pub(crate) fn apply_welds(arena: &mut Arena, transforms: &GlobalTransforms) {
    if arena.welds.is_empty() {
        return;
    }
    let _span = tracing::trace_span!("apply_welds").entered();

    let welds = std::sync::Arc::clone(&arena.welds);
    let mut scratch = WeldScratch {
        delta_a: std::mem::take(&mut arena.weld_delta_a_scratch),
        delta_b: std::mem::take(&mut arena.weld_delta_b_scratch),
        // The pooled Weld slot must be cleared by its first writer each frame.
        touched: std::mem::take(&mut arena.weld_touched_scratch),
        before: std::mem::take(&mut arena.weld_before_scratch),
    };
    scratch.touched.clear();

    for weld in welds.iter() {
        apply_one(arena, transforms, weld, &mut scratch);
    }

    for (&id, held) in scratch.touched.iter().zip(&scratch.before) {
        if let Some(NodeKind::Part(part)) = arena.get_mut(id).map(|n| &mut n.kind) {
            part.deform_stack.settle_source(DeformSource::Weld, held);
        }
    }
    arena.weld_delta_a_scratch = scratch.delta_a;
    arena.weld_delta_b_scratch = scratch.delta_b;
    arena.weld_touched_scratch = scratch.touched;
    arena.weld_before_scratch = scratch.before;
}

/// Frame-persistent buffers, so a fold allocates nothing once they have grown.
struct WeldScratch {
    delta_a: Vec<Vec2>,
    delta_b: Vec<Vec2>,
    touched: Vec<NodeIdx>,
    /// Parallel to `touched`: each part's Weld source before this frame's pass.
    before: Vec<Vec<Vec2>>,
}

fn apply_one(
    arena: &mut Arena,
    transforms: &GlobalTransforms,
    weld: &Weld,
    scratch: &mut WeldScratch,
) {
    let (Some(side_a), Some(side_b)) = (side(transforms, weld.a), side(transforms, weld.b)) else {
        return;
    };

    let (delta_a, delta_b) = (&mut scratch.delta_a, &mut scratch.delta_b);
    delta_a.clear();
    delta_b.clear();
    {
        let (Some(a), Some(b)) = (welded_part(arena, weld.a), welded_part(arena, weld.b)) else {
            return;
        };
        for pair in &weld.pairs {
            let (ai, bi) = (pair.a_vert as usize, pair.b_vert as usize);
            let (Some(&base_a), Some(&base_b)) = (a.mesh.vertices.get(ai), b.mesh.vertices.get(bi))
            else {
                delta_a.push(Vec2::ZERO);
                delta_b.push(Vec2::ZERO);
                continue;
            };
            let (Some(da), Some(db)) = (
                a.deform_stack.active_offset(ai),
                b.deform_stack.active_offset(bi),
            ) else {
                delta_a.push(Vec2::ZERO);
                delta_b.push(Vec2::ZERO);
                continue;
            };
            let world_a = side_a.world.transform_point2(base_a - a.mesh.origin + da);
            let world_b = side_b.world.transform_point2(base_b - b.mesh.origin + db);
            let w = pair.weight.clamp(0.0, 1.0);
            delta_a.push((world_b - world_a) * (1.0 - w));
            delta_b.push((world_a - world_b) * w);
        }
    }

    write_pulls(
        arena,
        weld.a,
        &side_a,
        weld.pairs.iter().map(|p| p.a_vert),
        &scratch.delta_a,
        &mut scratch.touched,
        &mut scratch.before,
    );
    write_pulls(
        arena,
        weld.b,
        &side_b,
        weld.pairs.iter().map(|p| p.b_vert),
        &scratch.delta_b,
        &mut scratch.touched,
        &mut scratch.before,
    );
}

/// A disabled part takes no part in its welds.
fn welded_part(arena: &Arena, id: NodeIdx) -> Option<&PartData> {
    let node = arena.get(id)?;
    match &node.kind {
        NodeKind::Part(part) if node.enabled => Some(part),
        _ => None,
    }
}

fn write_pulls(
    arena: &mut Arena,
    id: NodeIdx,
    side: &Side,
    verts: impl Iterator<Item = u32>,
    world_deltas: &[Vec2],
    touched: &mut Vec<NodeIdx>,
    before: &mut Vec<Vec<Vec2>>,
) {
    let Some(node) = arena.get_mut(id) else {
        return;
    };
    let NodeKind::Part(part) = &mut node.kind else {
        return;
    };
    let buf = part.deform_stack.source_buf_unsettled(DeformSource::Weld);
    if !touched.contains(&id) {
        let at = touched.len();
        touched.push(id);
        if before.len() <= at {
            before.push(Vec::new());
        }
        before[at].clear();
        before[at].extend_from_slice(buf);
        buf.fill(Vec2::ZERO);
    }
    for (vert, &world_delta) in verts.zip(world_deltas.iter()) {
        if let Some(slot) = buf.get_mut(vert as usize) {
            *slot += side.inv_linear * world_delta;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Mesh, MeshIndices, Node, PartData, TextureIdx, Transform};
    use crate::deform::DeformStack;
    use glam::Vec3;

    fn two_part_arena(offset_b: Vec2) -> (Arena, NodeIdx, NodeIdx) {
        let mut arena = Arena::new();
        let a = insert_part(
            &mut arena,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, 10.0),
            ],
        );
        let b = insert_part(
            &mut arena,
            offset_b,
            &[
                Vec2::new(0.0, 0.0) - offset_b,
                Vec2::new(10.0, 0.0) - offset_b,
                Vec2::new(5.0, -10.0) - offset_b,
            ],
        );
        (arena, a, b)
    }

    fn insert_part(arena: &mut Arena, translation: Vec2, verts: &[Vec2]) -> NodeIdx {
        insert_part_origin(arena, translation, verts, Vec2::ZERO)
    }

    fn insert_part_origin(
        arena: &mut Arena,
        translation: Vec2,
        verts: &[Vec2],
        origin: Vec2,
    ) -> NodeIdx {
        let mesh = Mesh::new(
            verts.to_vec(),
            vec![Vec2::ZERO; verts.len()],
            MeshIndices::U16(vec![0, 1, 2]),
            origin,
        );
        let part = PartData {
            deform_stack: DeformStack::new(verts.len()),
            mesh: mesh.into(),
            albedo_texture: TextureIdx(0),
            ..PartData::default()
        };
        let node = Node {
            name: "part".into(),
            transform: Transform {
                translation: Vec3::new(translation.x, translation.y, 0.0),
                ..Transform::default()
            },
            base_transform: Transform {
                translation: Vec3::new(translation.x, translation.y, 0.0),
                ..Transform::default()
            },
            kind: NodeKind::Part(Box::new(part)),
            ..Node::default()
        };
        let root = arena.root();
        arena.insert_child(root, node)
    }

    fn weld_slot(arena: &Arena, id: NodeIdx) -> Vec<Vec2> {
        let Some(node) = arena.get(id) else {
            return Vec::new();
        };
        let NodeKind::Part(p) = &node.kind else {
            return Vec::new();
        };
        p.deform_stack.combined().to_vec()
    }

    fn run(arena: &mut Arena) {
        let mut transforms = GlobalTransforms::new();
        arena.compute_transforms(&mut transforms);
        arena.apply_welds(&transforms);
        arena.combine_deforms();
    }

    fn seam_weld(a: NodeIdx, b: NodeIdx, weight: f32) -> Weld {
        Weld {
            a,
            b,
            pairs: vec![
                WeldPair {
                    a_vert: 0,
                    b_vert: 0,
                    weight,
                },
                WeldPair {
                    a_vert: 1,
                    b_vert: 1,
                    weight,
                },
            ],
        }
    }

    #[test]
    fn gpu_space_coincident_seam_with_nonzero_origin_is_a_no_op() {
        // The GPU draws v - origin, so welding must use world positions.
        let mut arena = Arena::new();
        let a = insert_part(
            &mut arena,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, 10.0),
            ],
        );
        let origin_b = Vec2::new(3.0, 0.0);
        let b = insert_part_origin(
            &mut arena,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0) + origin_b,
                Vec2::new(10.0, 0.0) + origin_b,
                Vec2::new(5.0, -10.0) + origin_b,
            ],
            origin_b,
        );
        arena.welds = vec![seam_weld(a, b, 0.5)].into();
        run(&mut arena);
        for v in weld_slot(&arena, a)
            .iter()
            .chain(weld_slot(&arena, b).iter())
        {
            assert!(v.length() < 1e-5, "expected zero pull, got {v}");
        }
    }

    #[test]
    fn coincident_seam_at_rest_is_a_no_op() {
        let (mut arena, a, b) = two_part_arena(Vec2::ZERO);
        arena.welds = vec![seam_weld(a, b, 0.5)].into();
        run(&mut arena);
        assert_eq!(weld_slot(&arena, a), vec![Vec2::ZERO; 3]);
        assert_eq!(weld_slot(&arena, b), vec![Vec2::ZERO; 3]);
    }

    #[test]
    fn differing_local_frames_with_coincident_world_seam_is_a_no_op() {
        let (mut arena, a, b) = two_part_arena(Vec2::new(4.0, 2.0));
        arena.welds = vec![seam_weld(a, b, 0.5)].into();
        run(&mut arena);
        for v in weld_slot(&arena, a)
            .iter()
            .chain(weld_slot(&arena, b).iter())
        {
            assert!(v.length() < 1e-5, "expected zero pull, got {v}");
        }
    }

    #[test]
    fn midweight_meets_halfway_after_a_deform() {
        let (mut arena, a, b) = two_part_arena(Vec2::ZERO);
        if let Some(node) = arena.get_mut(a)
            && let NodeKind::Part(p) = &mut node.kind
        {
            p.deform_stack
                .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                .unwrap();
        }
        arena.welds = vec![seam_weld(a, b, 0.5)].into();
        run(&mut arena);

        let slot_a = weld_slot(&arena, a);
        let slot_b = weld_slot(&arena, b);
        assert!((slot_a[0] - Vec2::new(8.0 - 4.0, 0.0)).length() < 1e-5);
        assert!((slot_a[1] - Vec2::new(8.0 - 4.0, 0.0)).length() < 1e-5);
        assert!((slot_b[0] - Vec2::new(4.0, 0.0)).length() < 1e-5);
        assert!((slot_b[1] - Vec2::new(4.0, 0.0)).length() < 1e-5);
        assert!((slot_a[2] - Vec2::new(8.0, 0.0)).length() < 1e-5);
        assert!(slot_b[2].length() < 1e-5);
    }

    #[test]
    fn weight_endpoints_pin_one_side() {
        for (w, expect_a_moves, expect_b_moves) in [(1.0f32, false, true), (0.0f32, true, false)] {
            let (mut arena, a, b) = two_part_arena(Vec2::ZERO);
            if let Some(node) = arena.get_mut(a)
                && let NodeKind::Part(p) = &mut node.kind
            {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
            arena.welds = vec![seam_weld(a, b, w)].into();
            run(&mut arena);
            let a_pull = weld_slot(&arena, a)[0] - Vec2::new(8.0, 0.0);
            let b_pull = weld_slot(&arena, b)[0];
            assert_eq!(a_pull.length() > 1e-5, expect_a_moves, "w={w} side A");
            assert_eq!(b_pull.length() > 1e-5, expect_b_moves, "w={w} side B");
            if expect_b_moves {
                assert!(
                    (b_pull - Vec2::new(8.0, 0.0)).length() < 1e-5,
                    "B snaps to A"
                );
            }
            if expect_a_moves {
                assert!(
                    (a_pull - Vec2::new(-8.0, 0.0)).length() < 1e-5,
                    "A snaps to B"
                );
            }
        }
    }

    #[test]
    fn pull_maps_through_the_inverse_linear_part() {
        let mut arena = Arena::new();
        let a = insert_part(
            &mut arena,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, 10.0),
            ],
        );
        // Local frame rotated +90°: local (x, y) lands at world (-y, x).
        let rot = Transform {
            rotation: Vec3::new(0.0, 0.0, std::f32::consts::FRAC_PI_2),
            ..Transform::default()
        };
        let mesh = Mesh::new(
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.0, -10.0),
                Vec2::new(-10.0, -5.0),
            ],
            vec![Vec2::ZERO; 3],
            MeshIndices::U16(vec![0, 1, 2]),
            Vec2::ZERO,
        );
        let part = PartData {
            deform_stack: DeformStack::new(3),
            mesh: mesh.into(),
            ..PartData::default()
        };
        let node = Node {
            transform: rot,
            base_transform: rot,
            kind: NodeKind::Part(Box::new(part)),
            ..Node::default()
        };
        let root = arena.root();
        let b = arena.insert_child(root, node);

        if let Some(node) = arena.get_mut(a)
            && let NodeKind::Part(p) = &mut node.kind
        {
            p.deform_stack
                .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                .unwrap();
        }
        arena.welds = vec![Weld {
            a,
            b,
            pairs: vec![WeldPair {
                a_vert: 0,
                b_vert: 0,
                weight: 1.0,
            }],
        }]
        .into();
        run(&mut arena);
        let b_pull = weld_slot(&arena, b)[0];
        assert!(
            (b_pull - Vec2::new(0.0, -8.0)).length() < 1e-4,
            "expected local (0,-8), got {b_pull}"
        );
    }

    #[test]
    fn disabled_part_skips_the_record() {
        let (mut arena, a, b) = two_part_arena(Vec2::ZERO);
        if let Some(node) = arena.get_mut(a) {
            node.enabled = false;
            if let NodeKind::Part(p) = &mut node.kind {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
        }
        arena.welds = vec![seam_weld(a, b, 0.5)].into();
        run(&mut arena);
        assert_eq!(weld_slot(&arena, b), vec![Vec2::ZERO; 3]);
    }

    #[test]
    fn sequential_records_accumulate_and_chain() {
        let (mut arena, a, b) = two_part_arena(Vec2::ZERO);
        let c = insert_part(
            &mut arena,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, -20.0),
            ],
        );
        if let Some(node) = arena.get_mut(a)
            && let NodeKind::Part(p) = &mut node.kind
        {
            p.deform_stack
                .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                .unwrap();
        }
        arena.welds = vec![seam_weld(a, b, 1.0), seam_weld(b, c, 1.0)].into();
        run(&mut arena);
        let c_pull = weld_slot(&arena, c)[0];
        assert!(
            (c_pull - Vec2::new(8.0, 0.0)).length() < 1e-5,
            "chain must see the welded seam, got {c_pull}"
        );
    }

    #[test]
    fn weld_slot_does_not_accumulate_across_frames() {
        let (mut arena, a, b) = two_part_arena(Vec2::ZERO);
        if let Some(node) = arena.get_mut(a)
            && let NodeKind::Part(p) = &mut node.kind
        {
            p.deform_stack
                .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                .unwrap();
        }
        arena.welds = vec![seam_weld(a, b, 0.0)].into();
        run(&mut arena);
        let first = weld_slot(&arena, a);
        arena.reset_deforms();
        if let Some(node) = arena.get_mut(a)
            && let NodeKind::Part(p) = &mut node.kind
        {
            p.deform_stack
                .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                .unwrap();
        }
        run(&mut arena);
        assert_eq!(weld_slot(&arena, a), first);
    }

    #[test]
    fn an_unmoved_seam_keeps_the_generation_and_a_moved_one_does_not() {
        fn frame(arena: &mut Arena, a: NodeIdx, shift: f32) {
            arena.reset_deforms();
            if let Some(NodeKind::Part(p)) = arena.get_mut(a).map(|n| &mut n.kind) {
                assert!(
                    p.deform_stack
                        .write_source(DeformSource::Test, &[Vec2::new(shift, 0.0); 3])
                );
            }
            run(arena);
        }
        fn generation(arena: &Arena, id: NodeIdx) -> u64 {
            match &arena.get(id).unwrap().kind {
                NodeKind::Part(p) => p.deform_stack.generation(),
                _ => unreachable!("a part"),
            }
        }

        let (mut arena, a, b) = two_part_arena(Vec2::ZERO);
        arena.welds = vec![seam_weld(a, b, 0.5)].into();
        frame(&mut arena, a, 8.0);
        let first = (generation(&arena, a), generation(&arena, b));
        let pulled = weld_slot(&arena, b);

        frame(&mut arena, a, 8.0);
        assert_eq!((generation(&arena, a), generation(&arena, b)), first);
        assert_eq!(weld_slot(&arena, b), pulled);

        frame(&mut arena, a, 6.0);
        assert_ne!(generation(&arena, a), first.0);
        assert_ne!(generation(&arena, b), first.1);
        assert!((weld_slot(&arena, b)[0] - Vec2::new(3.0, 0.0)).length() < 1e-5);
    }
}
