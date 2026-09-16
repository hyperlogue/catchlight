#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Public-protocol composition: exact sparse cells, isolated validation, atomic
//! rollback/no-ops, mapped topology and one-step history publication.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    BindingKey, BindingTarget as Target, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart,
    Name, NodeId, ParamId, ScalarTarget,
};
use catchlight_editor_protocol::*;
use catchlight_editor_server::{Attachments, Editor};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn node(s: &str) -> NodeId {
    NodeId::new(s).unwrap()
}
fn param() -> ParamId {
    ParamId::new("drive").unwrap()
}
fn params() -> BindingParams {
    BindingParams {
        param: param(),
        param_y: None,
    }
}
fn fixture() -> Model {
    let mut model = Model::new();
    let root = model.root().unwrap().clone();
    let mesh = ClmMesh {
        verts: vec![0., 0., 2., 0., 0., 2.],
        uvs: vec![0., 0., 1., 0., 0., 1.],
        indices: ClmIndices::U16(vec![0, 1, 2]),
        origin: [0.; 2],
    };
    model
        .add_node_with_id(
            node("panel"),
            &root,
            ModelNode::new("Panel", ModelNodeKind::Part(ModelPart::new(mesh))),
        )
        .unwrap();
    model
        .add_node_with_id(
            node("empty"),
            &root,
            ModelNode::new(
                "Empty",
                ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
            ),
        )
        .unwrap();
    model
        .add_param_with_id(
            param(),
            ModelParam::new(Name::truncated("Drive"), 0., 1., 0.),
        )
        .unwrap();
    let key = BindingKey::new(param(), node("panel"), Target::Deform);
    model
        .add_binding_with_positions(&key, vec![vec![0., 0.5, 1.]])
        .unwrap();
    model
        .set_deform_vertices(&key, [2, 0], vec![0., 0., 2., 4., 4., 8.])
        .unwrap();
    model
}
fn open(editor: &Editor) -> SessionId {
    let mut attachments = Attachments::none();
    attachments.insert("model", fixture().to_clm_bytes().unwrap());
    let (reply, payload) = editor.handle_with(
        Request {
            id: 1,
            command: Command::SessionNew {
                name: None,
                source: Some(SessionSource::Clm {}),
            },
        },
        attachments,
    );
    assert!(payload.is_none());
    match reply {
        Reply::Ok {
            rev: Some(0),
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("{other:?}"),
    }
}
fn send(editor: &Editor, command: Command) -> Reply {
    editor.handle(Request { id: 2, command })
}
fn ok(reply: Reply) -> (u64, ResponseBody) {
    match reply {
        Reply::Ok {
            rev: Some(rev),
            body,
            ..
        } => (rev, body),
        other => panic!("{other:?}"),
    }
}
fn snapshot(editor: &Editor, session: SessionId) -> Model {
    editor.with_model(session, Clone::clone).unwrap()
}
fn scalar(value: f32) -> EditOp {
    EditOp::BindingCellsSet {
        node: node("panel"),
        params: params(),
        target: BindingTarget::Tx,
        cells: vec![BindingCellWrite {
            cell: [1, 0],
            value: BindingCellValue::Scalar(value),
        }],
    }
}

#[test]
fn validation_is_inert_and_changed_batch_publishes_once() {
    let editor = Editor::new();
    let session = open(&editor);
    let before = snapshot(&editor, session);
    let events = Arc::new(AtomicUsize::new(0));
    let observed = events.clone();
    editor.subscribe(Box::new(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let edits = vec![
        scalar(4.),
        EditOp::NodeSet {
            node: node("panel"),
            patch: NodePatch {
                name: Some("Candidate".into()),
                ..Default::default()
            },
        },
    ];
    let (rev, body) = ok(send(
        &editor,
        Command::EditValidate {
            session,
            if_rev: 0,
            edits: edits.clone(),
        },
    ));
    assert_eq!(rev, 0);
    assert!(matches!(body,ResponseBody::EditResults {changed:true,results} if results.len()==2));
    assert!(snapshot(&editor, session).authored_eq(&before).unwrap());
    assert_eq!(events.load(Ordering::SeqCst), 0);
    let (rev, body) = ok(send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 0,
            edits,
        },
    ));
    assert_eq!(rev, 1);
    assert!(matches!(body,ResponseBody::EditResults {changed:true,results} if results.len()==2));
    assert_eq!(editor.history(session).unwrap(), (1, 0));
    assert_eq!(events.load(Ordering::SeqCst), 1);
    ok(send(&editor, Command::Undo { session, if_rev: 1 }));
    assert!(snapshot(&editor, session).authored_eq(&before).unwrap());
}

#[test]
fn final_content_noop_and_failed_later_operation_publish_nothing() {
    let editor = Editor::new();
    let session = open(&editor);
    let before = snapshot(&editor, session);
    let patch = |name: &str| EditOp::NodeSet {
        node: node("panel"),
        patch: NodePatch {
            name: Some(name.into()),
            ..Default::default()
        },
    };
    let (rev, body) = ok(send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 0,
            edits: vec![patch("Temporary"), patch("Panel")],
        },
    ));
    assert_eq!(rev, 0);
    assert!(matches!(
        body,
        ResponseBody::EditResults { changed: false, .. }
    ));
    let reply = send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 0,
            edits: vec![
                scalar(10.),
                EditOp::NodeReparent {
                    node: node("missing"),
                    to: node("root"),
                },
            ],
        },
    );
    assert!(matches!(
        reply,
        Reply::Err {
            op_index: Some(1),
            code: ErrorCode::NoNode,
            ..
        }
    ));
    assert!(snapshot(&editor, session).authored_eq(&before).unwrap());
    assert_eq!(editor.history(session).unwrap(), (0, 0));
    assert!(matches!(
        send(
            &editor,
            Command::EditApply {
                session,
                if_rev: 9,
                edits: vec![]
            }
        ),
        Reply::Err {
            code: ErrorCode::RevisionConflict,
            op_index: None,
            ..
        }
    ));
}

#[test]
fn exact_cells_preserve_holes_page_authored_vertices_and_accept_empty_deforms() {
    let editor = Editor::new();
    let session = open(&editor);
    let (_, body) = ok(send(
        &editor,
        Command::BindingCellsGet {
            session,
            if_rev: Some(0),
            node: node("panel"),
            params: params(),
            target: BindingTarget::Deform,
            cells: vec![[1, 0], [2, 0]],
            include_derived: true,
            vertices: Some(IndexRange { start: 1, count: 1 }),
        },
    ));
    match body {
        ResponseBody::BindingCells {
            cells,
            vertex_count: Some(3),
            vertices: Some(IndexRange { start: 1, count: 1 }),
            ..
        } => {
            assert!(!cells[0].authored);
            assert_eq!(cells[0].value, None);
            assert_eq!(
                cells[1].value,
                Some(BindingCellValue::Offsets(vec![[2., 4.]]))
            );
        }
        other => panic!("{other:?}"),
    }
    let (rev, _) = ok(send(
        &editor,
        Command::BindingCellsSet {
            session,
            if_rev: 0,
            node: node("empty"),
            params: params(),
            target: BindingTarget::Deform,
            cells: vec![BindingCellWrite {
                cell: [1, 0],
                value: BindingCellValue::Offsets(vec![]),
            }],
        },
    ));
    let model = snapshot(&editor, session);
    let key = BindingKey::new(param(), node("empty"), Target::Deform);
    let cells = catchlight_core::deform_cells(model.binding(&key).unwrap().values()).unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!([cells[0].x, cells[0].y], [1, 0]);
    assert!(cells[0].value.is_empty());
    let duplicate = send(
        &editor,
        Command::BindingCellsSet {
            session,
            if_rev: rev,
            node: node("panel"),
            params: params(),
            target: BindingTarget::Opacity,
            cells: vec![
                BindingCellWrite {
                    cell: [0, 0],
                    value: BindingCellValue::Scalar(0.5)
                };
                2
            ],
        },
    );
    assert!(matches!(
        duplicate,
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    assert!(snapshot(&editor, session)
        .binding(&BindingKey::new(
            param(),
            node("panel"),
            Target::Scalar(ScalarTarget::Opacity)
        ))
        .is_none());
}

#[test]
fn binding_axis_edits_and_values_compose_with_single_undo() {
    let editor = Editor::new();
    let session = open(&editor);
    let before = snapshot(&editor, session);
    let edits = vec![
        EditOp::BindingKeyMove {
            node: node("panel"),
            params: params(),
            target: BindingTarget::Deform,
            axis: param(),
            index: 1,
            value: 0.75,
        },
        EditOp::BindingCellsUnset {
            node: node("panel"),
            params: params(),
            target: BindingTarget::Deform,
            cells: vec![[2, 0]],
        },
        EditOp::BindingCellsSet {
            node: node("panel"),
            params: params(),
            target: BindingTarget::Deform,
            cells: vec![BindingCellWrite {
                cell: [0, 0],
                value: BindingCellValue::Offsets(vec![[2., 0.]; 3]),
            }],
        },
    ];
    ok(send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 0,
            edits,
        },
    ));
    let m = snapshot(&editor, session);
    let key = BindingKey::new(param(), node("panel"), Target::Deform);
    assert_eq!(
        m.binding(&key).unwrap().key_positions(),
        &[vec![0., 0.75, 1.]]
    );
    let cells = catchlight_core::deform_cells(m.binding(&key).unwrap().values()).unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].x, 0);
    ok(send(&editor, Command::Undo { session, if_rev: 1 }));
    assert!(snapshot(&editor, session).authored_eq(&before).unwrap());
}

#[test]
fn mapped_topology_refills_slots_and_preserves_welds_in_one_undo() {
    let editor = Editor::new();
    let session = open(&editor);
    let slot = SlotId::new("seam").unwrap();
    let setup = vec![
        EditOp::MeshSet {
            node: node("empty"),
            verts: vec![[0., 0.], [2., 0.], [0., 2.]],
            uvs: vec![],
            indices: vec![[0, 1, 2]],
            origin: [0.; 2],
            deform_mapping: None,
        },
        EditOp::SlotAdd {
            node: node("panel"),
            slot: slot.clone(),
        },
        EditOp::SlotFill {
            node: node("panel"),
            slot: slot.clone(),
            vertex: 1,
        },
        EditOp::SlotAdd {
            node: node("empty"),
            slot: slot.clone(),
        },
        EditOp::SlotFill {
            node: node("empty"),
            slot: slot.clone(),
            vertex: 1,
        },
        EditOp::WeldSet {
            a: node("panel"),
            b: node("empty"),
            pairs: vec![SlotPair {
                a: slot.clone(),
                b: slot.clone(),
                weight: 0.5,
            }],
        },
    ];
    ok(send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 0,
            edits: setup,
        },
    ));
    let before = snapshot(&editor, session);
    let edits = vec![
        EditOp::MeshSet {
            node: node("panel"),
            verts: vec![[0., 0.], [2., 0.], [0., 2.], [1., 0.]],
            uvs: vec![[0., 0.], [1., 0.], [0., 1.], [0.5, 0.]],
            indices: vec![[0, 3, 2], [3, 1, 2]],
            origin: [0.; 2],
            deform_mapping: Some(vec![
                vec![VertexWeight {
                    vertex: 0,
                    weight: 1.0,
                }],
                vec![VertexWeight {
                    vertex: 1,
                    weight: 1.0,
                }],
                vec![VertexWeight {
                    vertex: 2,
                    weight: 1.0,
                }],
                vec![
                    VertexWeight {
                        vertex: 0,
                        weight: 0.5,
                    },
                    VertexWeight {
                        vertex: 1,
                        weight: 0.5,
                    },
                ],
            ]),
        },
        EditOp::SlotFill {
            node: node("panel"),
            slot: slot.clone(),
            vertex: 3,
        },
    ];
    let (rev, result) = ok(send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 1,
            edits,
        },
    ));
    assert_eq!(rev, 2);
    let ResponseBody::EditResults { results, changed } = result else {
        panic!("edit results")
    };
    assert!(changed);
    assert!(
        matches!(&results[0], ResponseBody::Emptied { slots, .. } if slots == std::slice::from_ref(&slot))
    );
    let after = snapshot(&editor, session);
    let binding = after
        .binding(&BindingKey::new(param(), node("panel"), Target::Deform))
        .unwrap();
    let authored = catchlight_core::deform_cells(binding.values()).unwrap();
    assert_eq!(authored.len(), 1, "holes remain holes");
    assert_eq!([authored[0].x, authored[0].y], [2, 0]);
    assert_eq!(authored[0].value, vec![0., 0., 2., 4., 4., 8., 1., 2.]);
    assert_eq!(after.welds(), before.welds());
    assert!(after.unfilled_slots().is_empty());
    ok(send(
        &editor,
        Command::Undo {
            session,
            if_rev: rev,
        },
    ));
    assert!(snapshot(&editor, session).authored_eq(&before).unwrap());
}

#[test]
fn exact_writes_have_the_same_authored_result_standalone_and_batched() {
    let editor = Editor::new();
    let one = open(&editor);
    let batch = open(&editor);
    let cells = vec![
        BindingCellWrite {
            cell: [0, 0],
            value: BindingCellValue::Scalar(0.0),
        },
        BindingCellWrite {
            cell: [1, 0],
            value: BindingCellValue::Scalar(9.0),
        },
    ];
    ok(send(
        &editor,
        Command::BindingCellsSet {
            session: one,
            if_rev: 0,
            node: node("panel"),
            params: params(),
            target: BindingTarget::Tx,
            cells: cells.clone(),
        },
    ));
    ok(send(
        &editor,
        Command::BindingInterpolate {
            session: one,
            node: node("panel"),
            params: params(),
            target: BindingTarget::Tx,
            mode: Interpolate::Cubic,
        },
    ));
    ok(send(
        &editor,
        Command::EditApply {
            session: batch,
            if_rev: 0,
            edits: vec![
                EditOp::BindingCellsSet {
                    node: node("panel"),
                    params: params(),
                    target: BindingTarget::Tx,
                    cells,
                },
                EditOp::BindingInterpolationSet {
                    node: node("panel"),
                    params: params(),
                    target: BindingTarget::Tx,
                    mode: Interpolate::Cubic,
                },
            ],
        },
    ));
    assert!(snapshot(&editor, one)
        .authored_eq(&snapshot(&editor, batch))
        .unwrap());
    assert_eq!(editor.history(one).unwrap(), (2, 0));
    assert_eq!(editor.history(batch).unwrap(), (1, 0));
}

#[test]
fn geometry_pages_are_isolated_from_preview_and_reads_enforce_their_revision() {
    let editor = Editor::new();
    let session = open(&editor);
    editor
        .with_puppet(session, |model, puppet| {
            puppet.set_param_value(&param(), 1.0);
            let index = puppet.node_idx(&node("panel")).unwrap();
            assert!(puppet.set_scratch_deform(index, &[glam::vec2(50.0, 50.0); 3]));
            puppet.tick(model, 0.0);
        })
        .unwrap();
    let query = Command::GeometryGet {
        session,
        if_rev: Some(0),
        pose: vec![ParamPose {
            param: param(),
            value: 1.0,
        }],
        nodes: vec![node("panel")],
        fields: vec![
            GeometryField::Rest,
            GeometryField::World,
            GeometryField::Triangles,
        ],
        vertices: Some(IndexRange { start: 1, count: 1 }),
        triangles: Some(IndexRange { start: 0, count: 1 }),
    };
    let (_, body) = ok(send(&editor, query.clone()));
    let ResponseBody::GeometrySample { nodes, pose } = body else {
        panic!("geometry")
    };
    assert_eq!(pose[0].value, 1.0);
    assert_eq!(nodes[0].vertices.start, 1);
    assert_eq!(nodes[0].rest.as_ref().unwrap(), &vec![[2., 0.]]);
    assert_eq!(nodes[0].world.as_ref().unwrap(), &vec![[4., 4., 0.]]);
    assert_eq!(nodes[0].triangles.as_ref().unwrap(), &vec![[0, 1, 2]]);
    assert!(nodes[0].local.is_none());
    let before = snapshot(&editor, session);
    let (_, mesh) = ok(send(
        &editor,
        Command::MeshGet {
            session,
            if_rev: Some(0),
            node: node("panel"),
        },
    ));
    assert!(matches!(mesh, ResponseBody::MeshInfo { mesh, .. } if mesh.verts.len()==3));
    let (_, structure) = ok(send(
        &editor,
        Command::ModelGet {
            session,
            if_rev: Some(0),
        },
    ));
    let ResponseBody::ModelStructure { structure, .. } = structure else {
        panic!("structure")
    };
    let loaded: catchlight_core::formats::clm::ClmStructure =
        serde_json::from_value(structure).unwrap();
    assert_eq!(loaded.bindings[0].key_positions, vec![vec![0., 0.5, 1.]]);
    assert!(snapshot(&editor, session).authored_eq(&before).unwrap());
    assert_eq!(editor.history(session).unwrap(), (0, 0));
    ok(send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 0,
            edits: vec![scalar(2.0)],
        },
    ));
    assert!(matches!(
        send(&editor, query),
        Reply::Err {
            code: ErrorCode::RevisionConflict,
            ..
        }
    ));
}

#[test]
fn direct_callers_share_request_and_operation_limits_without_publishing() {
    let editor = Editor::new();
    let session = open(&editor);
    let oversized = Command::NodeSet {
        session,
        node: node("panel"),
        patch: NodePatch {
            name: Some("x".repeat(1024 * 1024)),
            ..Default::default()
        },
    };
    assert!(
        matches!(send(&editor, oversized), Reply::Err { code: ErrorCode::LimitExceeded, limit: Some(LimitInfo { resource, limit: 1_048_576, .. }), .. } if resource == "request_bytes")
    );
    assert!(
        matches!(send(&editor, Command::EditApply { session, if_rev: 0, edits: vec![scalar(2.0); 1025] }), Reply::Err { code: ErrorCode::LimitExceeded, limit: Some(LimitInfo { resource, limit: 1024, requested: 1025 }), .. } if resource == "edit_ops")
    );
    assert_eq!(editor.revision(session), Some(0));
    assert_eq!(editor.history(session).unwrap(), (0, 0));
}

#[test]
fn triangles_only_geometry_refuses_an_overflowed_transform() {
    let editor = Editor::new();
    let session = open(&editor);
    ok(send(
        &editor,
        Command::EditApply {
            session,
            if_rev: 0,
            edits: vec![
                EditOp::NodeSet {
                    node: node("root"),
                    patch: NodePatch {
                        scale: Some([1.0e30, 1.0e30]),
                        ..Default::default()
                    },
                },
                EditOp::NodeSet {
                    node: node("empty"),
                    patch: NodePatch {
                        scale: Some([1.0e30, 1.0e30]),
                        ..Default::default()
                    },
                },
            ],
        },
    ));
    assert!(matches!(
        send(
            &editor,
            Command::GeometryGet {
                session,
                if_rev: Some(1),
                pose: vec![],
                nodes: vec![node("empty")],
                fields: vec![GeometryField::Triangles],
                vertices: None,
                triangles: None
            }
        ),
        Reply::Err {
            code: ErrorCode::BadTarget,
            ..
        }
    ));
}

#[test]
fn mapped_mesh_retains_the_model_error_code_and_batch_index() {
    let editor = Editor::new();
    let session = open(&editor);
    assert!(matches!(
        send(
            &editor,
            Command::EditApply {
                session,
                if_rev: 0,
                edits: vec![EditOp::MeshSet {
                    node: node("missing"),
                    verts: vec![],
                    uvs: vec![],
                    indices: vec![],
                    origin: [0.; 2],
                    deform_mapping: Some(vec![])
                },]
            }
        ),
        Reply::Err {
            code: ErrorCode::NoNode,
            op_index: Some(0),
            ..
        }
    ));
}
