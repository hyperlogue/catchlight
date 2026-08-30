#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The protocol names things by the Id the `.clm` stores, so a command is
//! meaningful outside the session that produced it — and `RenameId` is the
//! one thing that changes what a name means. These pin both halves: a rename
//! rewrites the model's own references, and the *new* Id is what every
//! following command must use.

use catchlight_editor_protocol::{
    BindingParams, Command, ErrorCode, NodeId, NodeKindArg, NodePatch, ParamId, Rename, Reply,
    Request, ResponseBody, SeamAddr, SeamId, SlotId,
};
use catchlight_editor_server::Editor;

fn reply(ed: &Editor, id: u64, command: Command) -> Reply {
    ed.handle(Request { id, command })
}

fn body(ed: &Editor, id: u64, command: Command) -> ResponseBody {
    match reply(ed, id, command) {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn node_of(b: ResponseBody) -> NodeId {
    match b {
        ResponseBody::Node { node } => node,
        other => panic!("expected a node, got {other:?}"),
    }
}

fn param_of(b: ResponseBody) -> ParamId {
    match b {
        ResponseBody::Param { param } => param,
        other => panic!("expected a param, got {other:?}"),
    }
}

fn session_of(b: ResponseBody) -> catchlight_editor_protocol::SessionId {
    match b {
        ResponseBody::Session { session } => session,
        other => panic!("expected a session, got {other:?}"),
    }
}

fn root(ed: &Editor, id: u64, session: catchlight_editor_protocol::SessionId) -> NodeId {
    match body(ed, id, Command::NodeTree { session }) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("{other:?}"),
    }
}

/// A quad, so a part has vertices for its seam slots to point at.
fn quad(session: catchlight_editor_protocol::SessionId, node: NodeId) -> Command {
    Command::MeshSet {
        session,
        node,
        verts: vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0],
        uvs: vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
        indices: vec![0, 1, 2, 0, 2, 3],
        origin: [0.0, 0.0],
    }
}

#[test]
fn a_renamed_node_answers_to_its_new_id_and_not_its_old_one() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);
    let generated = node_of(body(
        &ed,
        3,
        Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Group,
            name: Some("Hat".into()),
        },
    ));
    // The Id the editor minted carries its parent as a reading aid, not a
    // path — the rename below replaces the whole string.
    assert!(generated.as_str().starts_with(&format!("{root}/group-")));

    let hat = NodeId::new("hat").unwrap();
    assert!(matches!(
        reply(
            &ed,
            4,
            Command::RenameId {
                session,
                rename: Rename::Node {
                    from: generated.clone(),
                    to: hat.clone(),
                },
            },
        ),
        Reply::Ok { .. }
    ));

    // The following command addresses it by the new Id.
    assert!(matches!(
        reply(
            &ed,
            5,
            Command::NodeSet {
                session,
                node: hat.clone(),
                patch: NodePatch {
                    z_order: Some(3.0),
                    ..Default::default()
                },
            },
        ),
        Reply::Ok { .. }
    ));
    // ...and the old one is gone, which is the breaking half.
    assert!(matches!(
        reply(
            &ed,
            6,
            Command::NodeSet {
                session,
                node: generated,
                patch: NodePatch {
                    z_order: Some(4.0),
                    ..Default::default()
                },
            },
        ),
        Reply::Err {
            code: ErrorCode::NoNode,
            ..
        }
    ));

    // The tree says the same thing, and the label rode along unchanged.
    match body(&ed, 7, Command::NodeTree { session }) {
        ResponseBody::Tree { root } => {
            let child = &root.children[0];
            assert_eq!(child.id, hat);
            assert_eq!(child.name, "Hat");
            assert!((child.z_order - 3.0).abs() < 1e-6);
        }
        other => panic!("{other:?}"),
    }
}

/// Renaming a param has to carry the bindings that name it, or the next
/// command against the new Id would author a second, empty binding.
#[test]
fn a_renamed_param_keeps_the_binding_that_named_it() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);
    let node = node_of(body(
        &ed,
        3,
        Command::NodeAdd {
            session,
            parent: root,
            kind: NodeKindArg::Group,
            name: None,
        },
    ));
    let generated = param_of(body(
        &ed,
        4,
        Command::ParamAdd {
            session,
            name: "Pull".into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            key_positions: Vec::new(),
        },
    ));
    body(
        &ed,
        5,
        Command::BindingKey {
            session,
            params: BindingParams::one(generated.clone()),
            node: node.clone(),
            target: "tx".into(),
            cell: [1, 0],
            value: 25.0,
        },
    );

    let pull = ParamId::new("pull").unwrap();
    assert!(matches!(
        reply(
            &ed,
            6,
            Command::RenameId {
                session,
                rename: Rename::Param {
                    from: generated.clone(),
                    to: pull.clone(),
                },
            },
        ),
        Reply::Ok { .. }
    ));

    match body(&ed, 7, Command::ParamList { session }) {
        ResponseBody::Params { params } => {
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].id, pull);
            assert_eq!(params[0].name, "Pull", "the label is not the Id");
            assert_eq!(params[0].bindings, 1, "the binding followed the rename");
        }
        other => panic!("{other:?}"),
    }

    // Addressing the binding by the new Id reaches the same one: unsetting
    // the only key leaves the binding with none rather than creating a
    // second binding to unset a key from.
    assert!(matches!(
        reply(
            &ed,
            8,
            Command::BindingUnset {
                session,
                params: BindingParams::one(pull.clone()),
                node,
                target: "tx".into(),
                cell: [1, 0],
            },
        ),
        Reply::Ok { .. }
    ));
    match body(&ed, 9, Command::ParamList { session }) {
        ResponseBody::Params { params } => assert_eq!(params[0].bindings, 1),
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(
            reply(
                &ed,
                10,
                Command::ParamDelete {
                    session,
                    param: generated
                }
            ),
            Reply::Err {
                code: ErrorCode::NoParam,
                ..
            }
        ),
        "the old param Id is gone"
    );
}

/// The seam surface end to end: name a seam, give it slots, fill them from
/// the part's own vertices, weld two seams together, then re-author a mesh
/// and watch the slots it emptied come back in the reply and in
/// `UnfilledSlots` — which is what a commit gate reads.
#[test]
fn a_seam_survives_a_mesh_edit_and_says_what_it_lost() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);
    let mut next = 3;
    let mut step = |cmd: Command| -> ResponseBody {
        next += 1;
        body(&ed, next, cmd)
    };

    let mut part = |name: &str| -> NodeId {
        let id = node_of(step(Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Part,
            name: Some(name.into()),
        }));
        step(quad(session, id.clone()));
        id
    };
    let (body_part, hem_part) = (part("Body"), part("Skirt"));

    let collar = SeamId::new("collar").unwrap();
    let hem = SeamId::new("hem").unwrap();
    let (left, right) = (SlotId::new("left").unwrap(), SlotId::new("right").unwrap());

    for (node, seam) in [(&body_part, &collar), (&hem_part, &hem)] {
        step(Command::SeamAdd {
            session,
            node: node.clone(),
            seam: seam.clone(),
        });
        for slot in [&left, &right] {
            step(Command::SlotAdd {
                session,
                node: node.clone(),
                seam: seam.clone(),
                slot: slot.clone(),
            });
        }
    }
    for (i, slot) in [&left, &right].into_iter().enumerate() {
        step(Command::SlotFill {
            session,
            node: body_part.clone(),
            seam: collar.clone(),
            slot: slot.clone(),
            vertex: i as u32,
        });
        step(Command::SlotFill {
            session,
            node: hem_part.clone(),
            seam: hem.clone(),
            slot: slot.clone(),
            vertex: (i + 2) as u32,
        });
    }

    match step(Command::Seams {
        session,
        node: body_part.clone(),
    }) {
        ResponseBody::Seams { seams } => {
            assert_eq!(seams.len(), 1);
            assert_eq!(seams[0].id, collar);
            assert_eq!(
                seams[0]
                    .slots
                    .iter()
                    .map(|s| (s.id.to_string(), s.vertex))
                    .collect::<Vec<_>>(),
                vec![("left".into(), Some(0)), ("right".into(), Some(1))],
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        step(Command::UnfilledSlots { session }),
        ResponseBody::UnfilledSlots { slots } if slots.is_empty()
    ));

    // Weld them. Empty weights means every slot meets midway.
    step(Command::WeldSet {
        session,
        a: SeamAddr {
            node: body_part.clone(),
            seam: collar.clone(),
        },
        b: SeamAddr {
            node: hem_part.clone(),
            seam: hem.clone(),
        },
        weights: Vec::new(),
    });
    match step(Command::Welds { session }) {
        ResponseBody::Welds { welds } => {
            assert_eq!(welds.len(), 1);
            assert_eq!(welds[0].a.node, body_part);
            assert_eq!(welds[0].b.seam, hem);
            assert_eq!(welds[0].weights.len(), 2);
            assert!(welds[0]
                .weights
                .iter()
                .all(|w| (w.weight - catchlight_core::DEFAULT_SLOT_WEIGHT).abs() < 1e-6));
        }
        other => panic!("{other:?}"),
    }
    // Setting the same pair again replaces the weld rather than stacking one.
    step(Command::WeldSet {
        session,
        a: SeamAddr {
            node: hem_part.clone(),
            seam: hem.clone(),
        },
        b: SeamAddr {
            node: body_part.clone(),
            seam: collar.clone(),
        },
        weights: Vec::new(),
    });
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.len() == 1
    ));

    // Re-author the body's mesh: every slot on it empties, and the reply says
    // which. The weld keeps both slots and skips the ones nothing fills.
    match step(quad(session, body_part.clone())) {
        ResponseBody::Emptied { node, slots } => {
            assert_eq!(node, body_part);
            assert_eq!(
                slots
                    .iter()
                    .map(|s| (s.seam.to_string(), s.slot.to_string()))
                    .collect::<Vec<_>>(),
                vec![
                    ("collar".into(), "left".into()),
                    ("collar".into(), "right".into())
                ],
            );
        }
        other => panic!("{other:?}"),
    }
    match step(Command::UnfilledSlots { session }) {
        ResponseBody::UnfilledSlots { slots } => {
            assert_eq!(slots.len(), 2, "the commit gate sees both");
            assert!(slots
                .iter()
                .all(|s| s.node == body_part && s.seam == collar));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.len() == 1 && welds[0].weights.len() == 2
    ));

    // Refilling clears the gate.
    for (i, slot) in [&left, &right].into_iter().enumerate() {
        step(Command::SlotFill {
            session,
            node: body_part.clone(),
            seam: collar.clone(),
            slot: slot.clone(),
            vertex: i as u32,
        });
    }
    assert!(matches!(
        step(Command::UnfilledSlots { session }),
        ResponseBody::UnfilledSlots { slots } if slots.is_empty()
    ));

    // Deleting a seam takes the weld that named it.
    step(Command::SeamDelete {
        session,
        node: body_part.clone(),
        seam: collar,
    });
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.is_empty()
    ));
}

/// Every seam refusal a client has to react to gets its own code, so a mesh
/// editor can tell "you already used that name" from "that vertex is not on
/// this part" without reading English.
#[test]
fn the_seam_errors_a_client_reacts_to_have_their_own_codes() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);
    let part = node_of(body(
        &ed,
        3,
        Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Part,
            name: None,
        },
    ));
    body(&ed, 4, quad(session, part.clone()));
    let seam = SeamId::new("collar").unwrap();
    let slot = SlotId::new("left").unwrap();
    body(
        &ed,
        5,
        Command::SeamAdd {
            session,
            node: part.clone(),
            seam: seam.clone(),
        },
    );
    body(
        &ed,
        6,
        Command::SlotAdd {
            session,
            node: part.clone(),
            seam: seam.clone(),
            slot: slot.clone(),
        },
    );

    let code = |id: u64, cmd: Command| match reply(&ed, id, cmd) {
        Reply::Err { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    };
    assert_eq!(
        code(
            7,
            Command::SeamAdd {
                session,
                node: part.clone(),
                seam: seam.clone(),
            }
        ),
        ErrorCode::DuplicateSeam
    );
    assert_eq!(
        code(
            8,
            Command::SlotAdd {
                session,
                node: part.clone(),
                seam: seam.clone(),
                slot: slot.clone(),
            }
        ),
        ErrorCode::DuplicateSlot
    );
    assert_eq!(
        code(
            9,
            Command::SlotAdd {
                session,
                node: part.clone(),
                seam: SeamId::new("nope").unwrap(),
                slot: slot.clone(),
            }
        ),
        ErrorCode::UnknownSeam
    );
    assert_eq!(
        code(
            10,
            Command::SlotClear {
                session,
                node: part.clone(),
                seam: seam.clone(),
                slot: SlotId::new("nope").unwrap(),
            }
        ),
        ErrorCode::UnknownSlot
    );
    // A weld whose two seams hold different slots cannot be written, so it is
    // refused where it is made.
    let other = node_of(body(
        &ed,
        11,
        Command::NodeAdd {
            session,
            parent: root,
            kind: NodeKindArg::Part,
            name: None,
        },
    ));
    body(&ed, 12, quad(session, other.clone()));
    body(
        &ed,
        13,
        Command::SeamAdd {
            session,
            node: other.clone(),
            seam: seam.clone(),
        },
    );
    assert_eq!(
        code(
            14,
            Command::WeldSet {
                session,
                a: SeamAddr {
                    node: part,
                    seam: seam.clone(),
                },
                b: SeamAddr { node: other, seam },
                weights: Vec::new(),
            }
        ),
        ErrorCode::WeldSlotMismatch
    );
}
