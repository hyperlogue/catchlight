#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Geometry queries observe the complete evaluated frame while retaining
//! authored identities. Synthetic meshes keep expected positions independent
//! of rendering, textures, private fixtures and the inspection implementation.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::geometry::{Bounds2, EvaluatedGeometry, GeometryError};
use catchlight_core::id::SeededHex;
use catchlight_core::{
    Mat4, Model, ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelSpine,
    ModelWeld, Name, NodeId, Puppet, SlotId, SlotPair, Vec2, Vec3,
};

fn triangle() -> ClmMesh {
    ClmMesh {
        verts: vec![0.0, 0.0, 2.0, 0.0, 0.0, 2.0],
        uvs: vec![],
        indices: ClmIndices::U16(vec![0, 1, 2]),
        origin: [0.0, 0.0],
    }
}

fn add_part(model: &mut Model, parent: &NodeId, mesh: ClmMesh, ids: &mut SeededHex) -> NodeId {
    model
        .add_node(
            parent,
            ModelNode::new("art", ModelNodeKind::Part(ModelPart::new(mesh))),
            ids,
        )
        .unwrap()
}

fn close(actual: Vec3, expected: [f32; 3]) {
    assert!(
        actual.abs_diff_eq(Vec3::from_array(expected), 1e-4),
        "{actual:?} != {expected:?}"
    );
}

#[test]
fn coordinates_preserve_authored_order_origin_and_full_world_transform() {
    let mut model = Model::new();
    let mut ids = SeededHex::new(71);
    let root = model.root().unwrap().clone();
    let mut ancestor = ModelNode::new("turned", ModelNodeKind::Group);
    ancestor.transform.translation = [10.0, 20.0, 3.0];
    ancestor.transform.rotation[2] = std::f32::consts::FRAC_PI_2;
    let parent = model.add_node(&root, ancestor, &mut ids).unwrap();
    let mesh = ClmMesh {
        verts: vec![2.0, 3.0, 6.0, 3.0, 2.0, 7.0, 10000.0, 10000.0],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.25, 0.75],
        indices: ClmIndices::U32(vec![2, 0, 1]),
        origin: [2.0, 3.0],
    };
    let part = add_part(&mut model, &parent, mesh, &mut ids);
    model
        .update_node(&part, |node| {
            node.transform.translation = [5.0, 0.0, 2.0];
        })
        .unwrap();
    let mut puppet = Puppet::new(&model);
    let idx = puppet.node_idx(&part).unwrap();
    puppet.set_scratch_deform(idx, &[Vec2::new(1.0, 2.0); 4]);
    let host =
        Mat4::from_translation(Vec3::new(100.0, 200.0, 7.0)) * Mat4::from_scale(Vec3::splat(2.0));
    puppet.tick_with_root(&model, host, 0.0);

    let frame = EvaluatedGeometry::new(&model, &puppet).unwrap();
    let geometry = frame.mesh(&part).unwrap();
    assert_eq!(geometry.node(), &part);
    assert!(geometry.is_part());
    assert_eq!(geometry.origin(), Vec2::new(2.0, 3.0));
    assert_eq!(geometry.rest()[3], Vec2::splat(10000.0));
    assert_eq!(geometry.uvs()[3], Vec2::new(0.25, 0.75));
    assert_eq!(geometry.vertex_count(), 4);
    assert_eq!(geometry.triangle_count(), 1);
    assert_eq!(geometry.triangle(0), Some([2, 0, 1]));
    assert_eq!(geometry.triangle(1), None);
    assert_eq!(geometry.triangle(usize::MAX), None);
    assert_eq!(
        geometry.triangles().collect::<Vec<_>>(),
        vec![(0, [2, 0, 1])]
    );
    let vertices = geometry.vertices().collect::<Vec<_>>();
    assert_eq!(
        vertices.iter().map(|v| v.index).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(vertices[0].rest, Vec2::new(2.0, 3.0));
    assert_eq!(vertices[0].local, Vec2::new(1.0, 2.0));
    assert_eq!(vertices[0].uv, Some(Vec2::new(0.0, 1.0)));
    close(vertices[0].world, [116.0, 252.0, 17.0]);
    close(vertices[1].world, [116.0, 260.0, 17.0]);
    close(vertices[2].world, [108.0, 252.0, 17.0]);
    assert_eq!(geometry.vertex(2), Some(vertices[2]));
    assert_eq!(geometry.vertex(4), None);
    assert_eq!(geometry.vertex(usize::MAX), None);
    assert_eq!(geometry.local_to_world(), puppet.transforms().get(idx));
    let bounds = geometry.world_bounds().unwrap().unwrap();
    assert!(bounds.min.abs_diff_eq(Vec2::new(108.0, 252.0), 1e-4));
    assert!(bounds.max.abs_diff_eq(Vec2::new(116.0, 260.0), 1e-4));
    // Repeated observation cannot consume scratch or re-evaluate the frame.
    assert_eq!(geometry.vertices().collect::<Vec<_>>(), vertices);
    assert_eq!(
        puppet.combined_deform(idx).unwrap(),
        &[Vec2::new(1.0, 2.0); 4]
    );
}

#[test]
fn inherited_meshgroup_and_weld_deformations_are_observed_from_the_same_frame() {
    let mut model = Model::new();
    let mut ids = SeededHex::new(72);
    let root = model.root().unwrap().clone();
    let lattice = ClmMesh {
        verts: vec![-10.0, -10.0, 10.0, -10.0, 10.0, 10.0, -10.0, 10.0],
        uvs: vec![],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    };
    let group = model
        .add_node(
            &root,
            ModelNode::new(
                "lattice",
                ModelNodeKind::MeshGroup(ModelMeshGroup::new(lattice)),
            ),
            &mut ids,
        )
        .unwrap();
    let a = add_part(&mut model, &group, triangle(), &mut ids);
    let b = add_part(&mut model, &root, triangle(), &mut ids);
    model
        .update_node(&b, |node| node.transform.translation[0] = 10.0)
        .unwrap();
    let slot = SlotId::new("join").unwrap();
    for part in [&a, &b] {
        model.slot_add(part, slot.clone()).unwrap();
        model.slot_fill(part, &slot, 0).unwrap();
    }
    model
        .set_welds(vec![ModelWeld::new(
            a.clone(),
            b.clone(),
            vec![SlotPair {
                a: slot.clone(),
                b: slot,
                weight: 0.5,
            }],
        )])
        .unwrap();
    let mut puppet = Puppet::new(&model);
    let idx = puppet.node_idx(&group).unwrap();
    puppet.set_scratch_deform(idx, &[Vec2::new(3.0, 4.0); 4]);
    // A local scratch edit is evaluated with the editor's refold path; the
    // observer does not choose a tick/preview policy on the caller's behalf.
    puppet.refold_with_node_edits(|_| {});
    let frame = EvaluatedGeometry::new(&model, &puppet).unwrap();
    let a = frame.mesh(&a).unwrap();
    let b = frame.mesh(&b).unwrap();
    close(a.vertex(0).unwrap().world, [6.5, 2.0, 0.0]);
    close(b.vertex(0).unwrap().world, [6.5, 2.0, 0.0]);
    close(a.vertex(1).unwrap().world, [5.0, 4.0, 0.0]);
    close(a.vertex(2).unwrap().world, [3.0, 6.0, 0.0]);
    assert_eq!(a.vertex(0).unwrap().uv, None);
    assert!(a.uvs().is_empty());
    let lattice = frame.mesh(&group).unwrap();
    assert!(!lattice.is_part());
    assert_eq!(lattice.triangle_count(), 2);
    close(lattice.vertex(0).unwrap().world, [-7.0, -6.0, 0.0]);
}

#[test]
fn spine_bends_are_in_the_observed_positions_without_an_extra_tick() {
    let mut model = Model::new();
    let mut ids = SeededHex::new(74);
    let root = model.root().unwrap().clone();
    let bend = model
        .add_param(
            ModelParam::new(Name::truncated("bend"), -1.0, 1.0, 0.0),
            &mut ids,
        )
        .unwrap();
    let spine = model
        .add_node(
            &root,
            ModelNode::new(
                "strand",
                ModelNodeKind::Spine(ModelSpine::new(vec![[0.0, -100.0]])),
            ),
            &mut ids,
        )
        .unwrap();
    model
        .set_spine_targets(&spine, vec![Some(bend.clone())])
        .unwrap();
    let mut mesh = triangle();
    mesh.verts = vec![0.0, -100.0, 1.0, -100.0, 0.0, -99.0];
    let part = add_part(&mut model, &spine, mesh, &mut ids);
    let mut puppet = Puppet::new(&model);
    puppet.set_physics_enabled(false);
    puppet.set_param_value(&bend, 0.5);
    puppet.tick(&model, 0.0);
    let frame = EvaluatedGeometry::new(&model, &puppet).unwrap();
    let geometry = frame.mesh(&part).unwrap();
    let tip = geometry.vertex(0).unwrap();
    assert_eq!(tip.rest, Vec2::new(0.0, -100.0));
    close(tip.world, [100.0, 0.0, 0.0]);
    assert_eq!(puppet.param_value(&bend), Some(0.5));
    assert!(!puppet.physics_enabled());
}

#[test]
fn empty_degenerate_excluded_and_nonfinite_bounds_have_explicit_meanings() {
    let mut model = Model::new();
    let mut ids = SeededHex::new(73);
    let root = model.root().unwrap().clone();
    let empty = add_part(&mut model, &root, ClmMesh::default(), &mut ids);
    let mut partial_mesh = triangle();
    partial_mesh.indices = ClmIndices::U16(vec![0, 1]);
    let partial = add_part(&mut model, &root, partial_mesh, &mut ids);
    let mut point_mesh = triangle();
    point_mesh.indices = ClmIndices::U16(vec![1, 1, 1]);
    let point = add_part(&mut model, &root, point_mesh, &mut ids);
    model
        .update_node(&point, |node| {
            node.enabled = false;
            if let ModelNodeKind::Part(part) = &mut node.kind {
                part.opacity = 0.0;
            }
        })
        .unwrap();
    let nonfinite = add_part(&mut model, &root, triangle(), &mut ids);
    let mut puppet = Puppet::new(&model);
    puppet.set_scratch_deform(
        puppet.node_idx(&nonfinite).unwrap(),
        &[Vec2::splat(f32::NAN); 3],
    );
    puppet.tick(&model, 0.0);
    let frame = EvaluatedGeometry::new(&model, &puppet).unwrap();
    let empty = frame.mesh(&empty).unwrap();
    assert_eq!(empty.vertex_count(), 0);
    assert_eq!(empty.vertices().count(), 0);
    assert_eq!(empty.world_bounds().unwrap(), None);
    assert_eq!(frame.mesh(&partial).unwrap().world_bounds().unwrap(), None);
    let point_bounds = Bounds2 {
        min: Vec2::new(2.0, 0.0),
        max: Vec2::new(2.0, 0.0),
    };
    assert_eq!(
        frame.mesh(&point).unwrap().world_bounds().unwrap(),
        Some(point_bounds)
    );
    assert_eq!(
        frame.mesh(&nonfinite).unwrap().world_bounds(),
        Err(GeometryError::NonFiniteWorldVertex {
            node: nonfinite,
            vertex: 0,
        })
    );
    assert_eq!(
        point_bounds.union(Bounds2 {
            min: Vec2::new(-3.0, -2.0),
            max: Vec2::new(1.0, 5.0)
        }),
        Bounds2 {
            min: Vec2::new(-3.0, -2.0),
            max: Vec2::new(2.0, 5.0)
        }
    );
}

#[test]
fn observation_rejects_missing_nodes_unmeshed_nodes_and_stale_frames() {
    let mut model = Model::new();
    let mut puppet = Puppet::new(&model);
    assert!(matches!(
        EvaluatedGeometry::new(&model, &puppet),
        Err(GeometryError::UnevaluatedPuppet)
    ));
    puppet.tick(&model, 0.0);
    let other = Model::new();
    assert!(matches!(
        EvaluatedGeometry::new(&other, &puppet),
        Err(GeometryError::ModelMismatch)
    ));
    let frame = EvaluatedGeometry::new(&model, &puppet).unwrap();
    let missing = NodeId::new("absent").unwrap();
    assert!(matches!(frame.mesh(&missing), Err(GeometryError::UnknownNode(id)) if id == missing));
    let root = model.root().unwrap().clone();
    assert!(matches!(frame.mesh(&root), Err(GeometryError::NotMeshed(id)) if id == root));
    model
        .update_node(&root, |node| node.enabled = false)
        .unwrap();
    assert!(matches!(
        EvaluatedGeometry::new(&model, &puppet),
        Err(GeometryError::StalePuppet)
    ));
    puppet.sync(&model);
    assert!(matches!(
        EvaluatedGeometry::new(&model, &puppet),
        Err(GeometryError::UnevaluatedPuppet)
    ));
    puppet.tick(&model, 0.0);
    assert!(EvaluatedGeometry::new(&model, &puppet).is_ok());
}
