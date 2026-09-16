#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Small deform pages bound evaluator work and keep all returned values finite.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    BindingKey, BindingTarget as Target, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart,
    Name, NodeId, ParamId, ScalarTarget,
};
use catchlight_editor_protocol::*;
use catchlight_editor_server::replica_reply;

fn node() -> NodeId {
    NodeId::new("panel").unwrap()
}
fn x() -> ParamId {
    ParamId::new("x").unwrap()
}
fn y() -> ParamId {
    ParamId::new("y").unwrap()
}

fn model(vertices: usize) -> Model {
    let mut model = Model::new();
    let root = model.root().unwrap().clone();
    model
        .add_node_with_id(
            node(),
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
    for param in [x(), y()] {
        model
            .add_param_with_id(
                param,
                ModelParam::new(Name::truncated("Input"), 0.0, 1.0, 0.0),
            )
            .unwrap();
    }
    model
}

fn read(
    model: &Model,
    target: BindingTarget,
    two_inputs: bool,
    cells: Vec<[u32; 2]>,
    vertices: Option<IndexRange>,
) -> Reply {
    replica_reply(
        model,
        7,
        Request {
            id: 1,
            command: Command::BindingCellsGet {
                session: SessionId(1),
                if_rev: Some(7),
                node: node(),
                params: BindingParams {
                    param: x(),
                    param_y: two_inputs.then(y),
                },
                target,
                cells,
                include_derived: true,
                vertices,
            },
        },
    )
}

#[test]
fn one_vertex_page_can_derive_a_large_sparse_binding_without_the_full_dense_mesh() {
    let mut model = model(4096);
    let key = BindingKey::new(x(), node(), Target::Deform);
    model
        .add_binding_with_positions(
            &key,
            vec![(0..65_536).map(|i| i as f32 / 65_535.0).collect()],
        )
        .unwrap();
    model
        .set_deform_vertices(&key, [0, 0], vec![3.0; 4096 * 2])
        .unwrap();
    let reply = read(
        &model,
        BindingTarget::Deform,
        false,
        vec![[65_535, 0], [0, 0]],
        Some(IndexRange {
            start: 4095,
            count: 1,
        }),
    );
    let Reply::Ok {
        rev: Some(7),
        body: ResponseBody::BindingCells { cells, .. },
        ..
    } = reply
    else {
        panic!("{reply:?}");
    };
    assert!(!cells[0].authored);
    assert_eq!(cells[0].value, None);
    assert_eq!(
        cells[0].derived,
        Some(BindingCellValue::Offsets(vec![[3.0, 3.0]]))
    );
    assert!(cells[1].authored);
    assert_eq!(cells[1].derived, cells[1].value);
}

#[test]
fn derived_work_is_bounded_before_fill_and_authored_pages_do_not_pay_for_grid_size() {
    let mut model = model(16);
    let key = BindingKey::new(x(), node(), Target::Deform);
    model
        .add_binding_with_positions(
            &key,
            vec![(0..65_536).map(|i| i as f32 / 65_535.0).collect()],
        )
        .unwrap();
    model
        .set_deform_vertices(&key, [0, 0], vec![1.0; 32])
        .unwrap();
    let excessive = read(
        &model,
        BindingTarget::Deform,
        false,
        vec![[1, 0], [2, 0]],
        Some(IndexRange { start: 0, count: 3 }),
    );
    assert!(
        matches!(excessive, Reply::Err { code: ErrorCode::LimitExceeded, limit: Some(LimitInfo { resource, limit: 262_144, requested: 393_216 }), .. } if resource == "derived_cell_vertices")
    );
    // Authored data is sliced directly even on a large grid.
    assert!(matches!(
        read(
            &model,
            BindingTarget::Deform,
            false,
            vec![[0, 0]],
            Some(IndexRange {
                start: 0,
                count: 16
            })
        ),
        Reply::Ok { .. }
    ));
}

#[test]
fn overflowing_derived_scalar_and_deform_values_are_errors_not_json_nulls() {
    for target in [BindingTarget::Tx, BindingTarget::Deform] {
        let mut model = model(1);
        let core_target = if target == BindingTarget::Deform {
            Target::Deform
        } else {
            Target::Scalar(ScalarTarget::Tx)
        };
        let key = BindingKey::pair(x(), y(), node(), core_target);
        model.add_binding(&key).unwrap();
        for (cell, value) in [([0, 0], -f32::MAX), ([1, 0], f32::MAX), ([0, 1], f32::MAX)] {
            if target == BindingTarget::Deform {
                model
                    .set_deform_vertices(&key, cell, vec![value, value])
                    .unwrap();
            } else {
                model.set_binding_key(&key, cell, value).unwrap();
            }
        }
        assert!(matches!(
            read(&model, target, true, vec![[1, 1]], None),
            Reply::Err {
                code: ErrorCode::BadTarget,
                ..
            }
        ));
        // Stored finite authored corners remain readable even if a different
        // derived corner would overflow.
        assert!(matches!(
            read(&model, target, true, vec![[0, 0]], None),
            Reply::Ok { .. }
        ));
    }
}
