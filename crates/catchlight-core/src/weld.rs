//! Vertex welding: after all other deformation, paired vertices of two Parts
//! are pulled to a weighted meeting point so a seam (neck/torso,
//! shoulder/arm) stays closed under param-driven deforms. The design follows
//! nijilive's welding (the working reference).

use glam::{Affine2, Mat2, Vec2};
use smallvec::SmallVec;

use crate::components::{NodeIdx, NodeKind};
use crate::deform::DeformSource;
use crate::meshgroup::{affine2_from_mat4, linear_mat2};
use crate::puppet::{GlobalTransforms, Puppet};

/// One welded pair of Parts. Puppet-global and canonical: each unordered
/// `{a, b}` pair appears in at most one record, and the solve writes both
/// sides, so there is no mirrored record and no owner.
#[derive(Debug, Clone)]
pub struct Weld {
    pub a: NodeIdx,
    pub b: NodeIdx,
    pub pairs: Vec<WeldPair>,
}

#[derive(Debug, Clone, Copy)]
pub struct WeldPair {
    pub a_vert: u32,
    pub b_vert: u32,
    /// A's share of the meeting point: 1.0 pins A and snaps B to it,
    /// 0.0 the reverse, 0.5 meets midway.
    pub weight: f32,
}

// The solve blends world positions, so both sides' full affines apply on the
// way in; the resulting deltas are direction vectors, so only the inverse
// linear part applies on the way back out.
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

/// Solve every weld, accumulating per-vertex pulls into each Part's
/// `DeformSource::Weld` slot. Runs after `propagate_mesh_group_deforms`
/// (welds must see mesh-group deforms) and before `combine_deforms` (which
/// folds the slot into the rendered deform).
///
/// Records solve sequentially in list order, each reading deforms that
/// include earlier records' writes — chained welds (A–B then B–C) therefore
/// see the already-welded seam, matching nijilive. A record is skipped
/// whole when either side is missing, not an enabled Part, or has a
/// degenerate world transform.
pub fn apply_welds(puppet: &mut Puppet, transforms: &GlobalTransforms) {
    if puppet.welds().is_empty() {
        return;
    }
    let _span = tracing::trace_span!("apply_welds").entered();

    let welds = std::mem::take(&mut puppet.welds);
    let mut cur_a = std::mem::take(&mut puppet.weld_cur_a_scratch);
    let mut cur_b = std::mem::take(&mut puppet.weld_cur_b_scratch);
    // Parts whose Weld slot has been zeroed this frame. The slot buffer is
    // pooled across frames, so the first writer must clear last frame's
    // contents before accumulating.
    let mut touched: SmallVec<[NodeIdx; 8]> = SmallVec::new();

    for weld in &welds {
        apply_one(
            puppet,
            transforms,
            weld,
            &mut cur_a,
            &mut cur_b,
            &mut touched,
        );
    }

    puppet.welds = welds;
    puppet.weld_cur_a_scratch = cur_a;
    puppet.weld_cur_b_scratch = cur_b;
}

fn apply_one(
    puppet: &mut Puppet,
    transforms: &GlobalTransforms,
    weld: &Weld,
    cur_a: &mut Vec<Vec2>,
    cur_b: &mut Vec<Vec2>,
    touched: &mut SmallVec<[NodeIdx; 8]>,
) {
    let (Some(side_a), Some(side_b)) = (side(transforms, weld.a), side(transforms, weld.b)) else {
        return;
    };
    if !read_current(puppet, weld.a, cur_a) || !read_current(puppet, weld.b, cur_b) {
        return;
    }

    // Per pair, aligned with weld.pairs: the world-space pull on each side.
    let mut delta_a: SmallVec<[Vec2; 32]> = SmallVec::new();
    let mut delta_b: SmallVec<[Vec2; 32]> = SmallVec::new();
    {
        let (Some(verts_a), Some(verts_b)) =
            (part_verts(puppet, weld.a), part_verts(puppet, weld.b))
        else {
            return;
        };
        let origin_a = part_origin(puppet, weld.a);
        let origin_b = part_origin(puppet, weld.b);
        for pair in &weld.pairs {
            let (ai, bi) = (pair.a_vert as usize, pair.b_vert as usize);
            let (Some(&base_a), Some(&base_b)) = (verts_a.get(ai), verts_b.get(bi)) else {
                delta_a.push(Vec2::ZERO);
                delta_b.push(Vec2::ZERO);
                continue;
            };
            let (Some(&da), Some(&db)) = (cur_a.get(ai), cur_b.get(bi)) else {
                delta_a.push(Vec2::ZERO);
                delta_b.push(Vec2::ZERO);
                continue;
            };
            let world_a = side_a.world.transform_point2(base_a - origin_a + da);
            let world_b = side_b.world.transform_point2(base_b - origin_b + db);
            let w = pair.weight.clamp(0.0, 1.0);
            // blended = w·A + (1-w)·B, so A moves by (1-w)·(B-A) and B by
            // w·(A-B): higher weight pins A and pulls B toward it.
            delta_a.push((world_b - world_a) * (1.0 - w));
            delta_b.push((world_a - world_b) * w);
        }
    }

    write_pulls(
        puppet,
        weld.a,
        &side_a,
        weld.pairs.iter().map(|p| p.a_vert),
        &delta_a,
        touched,
    );
    write_pulls(
        puppet,
        weld.b,
        &side_b,
        weld.pairs.iter().map(|p| p.b_vert),
        &delta_b,
        touched,
    );
}

/// Sum the part's active deform sources — including the Weld slot when an
/// earlier record already wrote it this frame — without disturbing the
/// stack's combined/generation memo. False when the node isn't an enabled
/// Part.
fn read_current(puppet: &mut Puppet, id: NodeIdx, out: &mut Vec<Vec2>) -> bool {
    let Some(node) = puppet.get_mut(id) else {
        return false;
    };
    if !node.enabled {
        return false;
    }
    let NodeKind::Part(part) = &mut node.kind else {
        return false;
    };
    part.deform_stack.sum_active_into(out);
    true
}

fn part_verts(puppet: &Puppet, id: NodeIdx) -> Option<&[Vec2]> {
    match puppet.get(id).map(|n| &n.kind) {
        Some(NodeKind::Part(p)) => Some(&p.mesh.vertices),
        _ => None,
    }
}

fn part_origin(puppet: &Puppet, id: NodeIdx) -> Vec2 {
    match puppet.get(id).map(|n| &n.kind) {
        Some(NodeKind::Part(p)) => p.mesh.origin,
        _ => Vec2::ZERO,
    }
}

fn write_pulls(
    puppet: &mut Puppet,
    id: NodeIdx,
    side: &Side,
    verts: impl Iterator<Item = u32>,
    world_deltas: &[Vec2],
    touched: &mut SmallVec<[NodeIdx; 8]>,
) {
    let Some(node) = puppet.get_mut(id) else {
        return;
    };
    let NodeKind::Part(part) = &mut node.kind else {
        return;
    };
    let buf = part.deform_stack.source_buf_mut(DeformSource::Weld);
    if !touched.contains(&id) {
        touched.push(id);
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
    use crate::components::{Mesh, MeshIndices, Node, PartData, TextureId, Transform};
    use crate::deform::DeformStack;
    use glam::Vec3;

    // Two single-triangle Parts sharing the seam edge x∈[0,10] at y=0:
    // part A above (apex +y), part B below (apex -y). Seam pairs:
    // A verts 0,1 ↔ B verts 0,1, coincident at rest.
    fn two_part_puppet(offset_b: Vec2) -> (Puppet, NodeIdx, NodeIdx) {
        let mut puppet = Puppet::new();
        let a = insert_part(
            &mut puppet,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, 10.0),
            ],
        );
        let b = insert_part(
            &mut puppet,
            offset_b,
            &[
                Vec2::new(0.0, 0.0) - offset_b,
                Vec2::new(10.0, 0.0) - offset_b,
                Vec2::new(5.0, -10.0) - offset_b,
            ],
        );
        (puppet, a, b)
    }

    fn insert_part(puppet: &mut Puppet, translation: Vec2, verts: &[Vec2]) -> NodeIdx {
        insert_part_origin(puppet, translation, verts, Vec2::ZERO)
    }

    fn insert_part_origin(
        puppet: &mut Puppet,
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
            mesh,
            albedo_texture: TextureId(0),
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
        let root = puppet.root();
        puppet.insert_child(root, node, None)
    }

    fn weld_slot(puppet: &Puppet, id: NodeIdx) -> Vec<Vec2> {
        let Some(node) = puppet.get(id) else {
            return Vec::new();
        };
        let NodeKind::Part(p) = &node.kind else {
            return Vec::new();
        };
        p.deform_stack.combined().to_vec()
    }

    fn run(puppet: &mut Puppet) {
        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);
        puppet.apply_welds(&transforms);
        puppet.combine_deforms();
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
        // GPU draws v - origin. A origin-0 seam at (0,0)/(10,0) and B with
        // origin (3,0) and verts shifted by that origin meet on screen;
        // welding in raw verts would see B 3px to the right and pull.
        let mut puppet = Puppet::new();
        let a = insert_part(
            &mut puppet,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, 10.0),
            ],
        );
        let origin_b = Vec2::new(3.0, 0.0);
        let b = insert_part_origin(
            &mut puppet,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0) + origin_b,
                Vec2::new(10.0, 0.0) + origin_b,
                Vec2::new(5.0, -10.0) + origin_b,
            ],
            origin_b,
        );
        puppet.set_welds(vec![seam_weld(a, b, 0.5)]);
        run(&mut puppet);
        for v in weld_slot(&puppet, a)
            .iter()
            .chain(weld_slot(&puppet, b).iter())
        {
            assert!(v.length() < 1e-5, "expected zero pull, got {v}");
        }
    }

    #[test]
    fn coincident_seam_at_rest_is_a_no_op() {
        let (mut puppet, a, b) = two_part_puppet(Vec2::ZERO);
        puppet.set_welds(vec![seam_weld(a, b, 0.5)]);
        run(&mut puppet);
        assert_eq!(weld_slot(&puppet, a), vec![Vec2::ZERO; 3]);
        assert_eq!(weld_slot(&puppet, b), vec![Vec2::ZERO; 3]);
    }

    // B's node sits translated by (4, 2) while its verts compensate, so the
    // seam is coincident in world space but the two parts' local frames
    // differ — the solve must still be a no-op, proving the world-space
    // round-trip through both transforms is consistent.
    #[test]
    fn differing_local_frames_with_coincident_world_seam_is_a_no_op() {
        let (mut puppet, a, b) = two_part_puppet(Vec2::new(4.0, 2.0));
        puppet.set_welds(vec![seam_weld(a, b, 0.5)]);
        run(&mut puppet);
        for v in weld_slot(&puppet, a)
            .iter()
            .chain(weld_slot(&puppet, b).iter())
        {
            assert!(v.length() < 1e-5, "expected zero pull, got {v}");
        }
    }

    #[test]
    fn midweight_meets_halfway_after_a_deform() {
        let (mut puppet, a, b) = two_part_puppet(Vec2::ZERO);
        // Move A's whole mesh right by 8 via a param-style deform source.
        if let Some(node) = puppet.get_mut(a) {
            if let NodeKind::Part(p) = &mut node.kind {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
        }
        puppet.set_welds(vec![seam_weld(a, b, 0.5)]);
        run(&mut puppet);

        let slot_a = weld_slot(&puppet, a);
        let slot_b = weld_slot(&puppet, b);
        // combined() on A includes the Test deform; the weld pull is on top.
        assert!((slot_a[0] - Vec2::new(8.0 - 4.0, 0.0)).length() < 1e-5);
        assert!((slot_a[1] - Vec2::new(8.0 - 4.0, 0.0)).length() < 1e-5);
        assert!((slot_b[0] - Vec2::new(4.0, 0.0)).length() < 1e-5);
        assert!((slot_b[1] - Vec2::new(4.0, 0.0)).length() < 1e-5);
        // The apexes (vert 2) are not welded and keep their own deforms.
        assert!((slot_a[2] - Vec2::new(8.0, 0.0)).length() < 1e-5);
        assert!(slot_b[2].length() < 1e-5);
    }

    #[test]
    fn weight_endpoints_pin_one_side() {
        for (w, expect_a_moves, expect_b_moves) in [(1.0f32, false, true), (0.0f32, true, false)] {
            let (mut puppet, a, b) = two_part_puppet(Vec2::ZERO);
            if let Some(node) = puppet.get_mut(a) {
                if let NodeKind::Part(p) = &mut node.kind {
                    p.deform_stack
                        .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                        .unwrap();
                }
            }
            puppet.set_welds(vec![seam_weld(a, b, w)]);
            run(&mut puppet);
            let a_pull = weld_slot(&puppet, a)[0] - Vec2::new(8.0, 0.0);
            let b_pull = weld_slot(&puppet, b)[0];
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

    // A rotated part must receive its pull in its own local basis: rotate B
    // by 90° around Z (its verts pre-rotated back so the world seam stays
    // coincident), deform A, and check B's local-space pull is the world
    // pull rotated by -90°.
    #[test]
    fn pull_maps_through_the_inverse_linear_part() {
        let mut puppet = Puppet::new();
        let a = insert_part(
            &mut puppet,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, 10.0),
            ],
        );
        // Local frame rotated +90°: local (x, y) lands at world (-y, x), so
        // world seam verts (0,0) and (10,0) are local (0,0) and (0,-10).
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
            mesh,
            ..PartData::default()
        };
        let node = Node {
            transform: rot,
            base_transform: rot,
            kind: NodeKind::Part(Box::new(part)),
            ..Node::default()
        };
        let root = puppet.root();
        let b = puppet.insert_child(root, node, None);

        if let Some(node) = puppet.get_mut(a) {
            if let NodeKind::Part(p) = &mut node.kind {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
        }
        puppet.set_welds(vec![Weld {
            a,
            b,
            pairs: vec![WeldPair {
                a_vert: 0,
                b_vert: 0,
                weight: 1.0,
            }],
        }]);
        run(&mut puppet);
        // World pull on B is (8, 0); in B's +90°-rotated local frame that is
        // (0, -8).
        let b_pull = weld_slot(&puppet, b)[0];
        assert!(
            (b_pull - Vec2::new(0.0, -8.0)).length() < 1e-4,
            "expected local (0,-8), got {b_pull}"
        );
    }

    #[test]
    fn disabled_part_skips_the_record() {
        let (mut puppet, a, b) = two_part_puppet(Vec2::ZERO);
        if let Some(node) = puppet.get_mut(a) {
            node.enabled = false;
            if let NodeKind::Part(p) = &mut node.kind {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
        }
        puppet.set_welds(vec![seam_weld(a, b, 0.5)]);
        run(&mut puppet);
        assert_eq!(weld_slot(&puppet, b), vec![Vec2::ZERO; 3]);
    }

    // Two records touching the same part must accumulate, and the second
    // must read deforms that include the first's write (sequential solve).
    #[test]
    fn sequential_records_accumulate_and_chain() {
        let (mut puppet, a, b) = two_part_puppet(Vec2::ZERO);
        let c = insert_part(
            &mut puppet,
            Vec2::ZERO,
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(5.0, -20.0),
            ],
        );
        if let Some(node) = puppet.get_mut(a) {
            if let NodeKind::Part(p) = &mut node.kind {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
        }
        // a–b with w=1 snaps b's seam to a's (moved by 8); then b–c with w=1
        // snaps c's seam to b's — which must be the *welded* position.
        puppet.set_welds(vec![seam_weld(a, b, 1.0), seam_weld(b, c, 1.0)]);
        run(&mut puppet);
        let c_pull = weld_slot(&puppet, c)[0];
        assert!(
            (c_pull - Vec2::new(8.0, 0.0)).length() < 1e-5,
            "chain must see the welded seam, got {c_pull}"
        );
    }

    // Across frames the pooled Weld slot must be re-zeroed, not accumulate.
    #[test]
    fn weld_slot_does_not_accumulate_across_frames() {
        let (mut puppet, a, b) = two_part_puppet(Vec2::ZERO);
        if let Some(node) = puppet.get_mut(a) {
            if let NodeKind::Part(p) = &mut node.kind {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
        }
        puppet.set_welds(vec![seam_weld(a, b, 0.0)]);
        run(&mut puppet);
        let first = weld_slot(&puppet, a);
        // Frame 2: same pose. reset + re-apply like a real frame.
        puppet.reset_deforms();
        if let Some(node) = puppet.get_mut(a) {
            if let NodeKind::Part(p) = &mut node.kind {
                p.deform_stack
                    .set(DeformSource::Test, vec![Vec2::new(8.0, 0.0); 3])
                    .unwrap();
            }
        }
        run(&mut puppet);
        assert_eq!(weld_slot(&puppet, a), first);
    }
}
