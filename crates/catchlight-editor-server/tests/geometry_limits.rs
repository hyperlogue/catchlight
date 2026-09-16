#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Geometry output pages must not hide unbounded whole-model evaluation work.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    BindingKey, BindingTarget as Target, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart,
    Name, NodeId, ParamId,
};
use catchlight_editor_protocol::*;
use catchlight_editor_server::replica_reply;

fn node(id: &str) -> NodeId {
    NodeId::new(id).unwrap()
}

fn param(id: &str) -> ParamId {
    ParamId::new(id).unwrap()
}

fn add_mesh(model: &mut Model, id: NodeId, vertices: usize) {
    let root = model.root().unwrap().clone();
    model
        .add_node_with_id(
            id,
            &root,
            ModelNode::new(
                "Panel",
                ModelNodeKind::Part(ModelPart::new(ClmMesh {
                    verts: vec![0.0; vertices * 2],
                    uvs: vec![],
                    indices: ClmIndices::U16(vec![]),
                    origin: [0.0; 2],
                })),
            ),
        )
        .unwrap();
}

fn base_model() -> Model {
    let mut model = Model::new();
    add_mesh(&mut model, node("small"), 3);
    for id in [param("x"), param("y")] {
        model
            .add_param_with_id(id, ModelParam::new(Name::truncated("Input"), 0.0, 1.0, 0.0))
            .unwrap();
    }
    model
}

fn oversized_model() -> Model {
    let mut model = base_model();
    add_mesh(&mut model, node("large"), 4096);
    let key = BindingKey::new(param("x"), node("large"), Target::Deform);
    model
        .add_binding_with_positions(
            &key,
            vec![(0..65_536).map(|i| i as f32 / 65_535.0).collect()],
        )
        .unwrap();
    model
        .set_deform_vertices(&key, [0, 0], vec![1.0; 8192])
        .unwrap();
    model
}

fn command() -> Command {
    Command::GeometryGet {
        session: SessionId(1),
        if_rev: Some(7),
        pose: vec![],
        nodes: vec![node("small")],
        fields: vec![GeometryField::World],
        vertices: Some(IndexRange { start: 0, count: 1 }),
        triangles: None,
    }
}

fn read(model: &Model, command: Command) -> Reply {
    replica_reply(model, 7, Request { id: 1, command })
}

#[test]
fn one_vertex_from_a_small_node_checks_dense_work_on_unselected_nodes() {
    // The authored model is small. Expanding this sparse binding would require
    // 2 GiB of vertex offsets before producing the one requested vertex.
    let model = oversized_model();
    let generation = model.generation();
    let reply = read(&model, command());
    assert!(matches!(reply, Reply::Err {
            code: ErrorCode::LimitExceeded,
            limit: Some(LimitInfo { resource, limit: 64_000_000, requested: 268_435_456 }),
            ..
        } if resource == "geometry_dense_vertices"));
    assert_eq!(model.generation(), generation);
}

#[test]
fn zero_vertex_grids_still_have_a_combined_dense_cell_budget() {
    let mut model = base_model();
    let axis: Vec<_> = (0..256).map(|i| i as f32 / 255.0).collect();
    for index in 0..245 {
        let id = node(&format!("empty-{index}"));
        add_mesh(&mut model, id.clone(), 0);
        model
            .add_binding_with_positions(
                &BindingKey::pair(param("x"), param("y"), id, Target::Deform),
                vec![axis.clone(), axis.clone()],
            )
            .unwrap();
    }
    let reply = read(&model, command());
    assert!(matches!(reply, Reply::Err {
            code: ErrorCode::LimitExceeded,
            limit: Some(LimitInfo { resource, limit: 16_000_000, requested: 16_056_320 }),
            ..
        } if resource == "geometry_dense_cells"));
}

#[test]
fn invalid_selectors_ranges_and_pose_are_rejected_before_evaluation() {
    let model = oversized_model();
    let mut missing = command();
    if let Command::GeometryGet { nodes, .. } = &mut missing {
        *nodes = vec![node("missing")];
    }
    assert!(matches!(
        read(&model, missing),
        Reply::Err {
            code: ErrorCode::NoNode,
            ..
        }
    ));
    let mut nonmeshed = command();
    if let Command::GeometryGet { nodes, .. } = &mut nonmeshed {
        *nodes = vec![model.root().unwrap().clone()];
    }
    assert!(matches!(
        read(&model, nonmeshed),
        Reply::Err {
            code: ErrorCode::BadTarget,
            ..
        }
    ));
    let mut range = command();
    if let Command::GeometryGet { vertices, .. } = &mut range {
        *vertices = Some(IndexRange { start: 3, count: 1 });
    }
    assert!(matches!(
        read(&model, range),
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    let mut unknown_input = command();
    if let Command::GeometryGet { pose, .. } = &mut unknown_input {
        *pose = vec![ParamPose {
            param: param("missing"),
            value: 0.0,
        }];
    }
    assert!(matches!(
        read(&model, unknown_input),
        Reply::Err {
            code: ErrorCode::NoParam,
            ..
        }
    ));
    let mut duplicate_field = command();
    if let Command::GeometryGet { fields, .. } = &mut duplicate_field {
        fields.push(GeometryField::World);
    }
    assert!(matches!(
        read(&model, duplicate_field),
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
}

#[test]
fn combined_output_budget_is_checked_before_evaluation() {
    let mut model = oversized_model();
    add_mesh(&mut model, node("wide"), 70_000);
    let mut query = command();
    if let Command::GeometryGet {
        nodes,
        fields,
        vertices,
        ..
    } = &mut query
    {
        *nodes = vec![node("wide")];
        *fields = vec![
            GeometryField::Rest,
            GeometryField::Local,
            GeometryField::World,
            GeometryField::Uvs,
        ];
        *vertices = None;
    }
    assert!(matches!(
        read(&model, query),
        Reply::Err {
            code: ErrorCode::LimitExceeded,
            limit: Some(LimitInfo { resource, limit: 262_144, requested: 280_000 }),
            ..
        } if resource == "geometry_items"
    ));
}

#[test]
fn valid_pages_preserve_selected_fields_and_captured_revision() {
    let model = base_model();
    let mut query = command();
    if let Command::GeometryGet {
        vertices, fields, ..
    } = &mut query
    {
        *vertices = Some(IndexRange { start: 1, count: 2 });
        *fields = vec![
            GeometryField::Rest,
            GeometryField::World,
            GeometryField::Triangles,
        ];
    }
    let reply = read(&model, query);
    let Reply::Ok {
        rev: Some(7),
        body: ResponseBody::GeometrySample { nodes, .. },
        ..
    } = reply
    else {
        panic!("{reply:?}");
    };
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].node, node("small"));
    assert_eq!(nodes[0].vertices, IndexRange { start: 1, count: 2 });
    assert_eq!(nodes[0].rest, Some(vec![[0.0; 2]; 2]));
    assert_eq!(nodes[0].world, Some(vec![[0.0; 3]; 2]));
    assert_eq!(nodes[0].triangles, Some(vec![]));
    assert_eq!(nodes[0].local, None);
}
