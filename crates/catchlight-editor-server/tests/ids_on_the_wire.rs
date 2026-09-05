#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The protocol names things by the Id the `.clm` stores, so a command is
//! meaningful outside the session that produced it — and `RenameId` is the
//! one thing that changes what a name means. These pin both halves: a rename
//! rewrites the model's own references, and the *new* Id is what every
//! following command must use.

use catchlight_editor_protocol::{
    BindingParams, BindingTarget, Command, ErrorCode, NodeId, NodeKindArg, NodePatch, ParamId,
    Rename, Reply, Request, ResponseBody, ScalarTarget, SlotAddr, SlotId, SlotPair,
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
        ResponseBody::Node { node, .. } => node,
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

/// A quad, so a part has vertices for its slots to point at.
fn quad(session: catchlight_editor_protocol::SessionId, node: NodeId) -> Command {
    Command::MeshSet {
        session,
        node,
        verts: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        indices: vec![[0, 1, 2], [0, 2, 3]],
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
            node: None,
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
            node: None,
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
            param: None,
        },
    ));
    body(
        &ed,
        5,
        Command::BindingKey {
            session,
            params: BindingParams::one(generated.clone()),
            node: node.clone(),
            target: ScalarTarget::Tx,
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
                target: BindingTarget::Tx,
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

/// The slot surface end to end: give a part slots, fill them from its own
/// vertices, weld two parts pair by pair, then re-author a mesh and watch the
/// slots it emptied come back in the reply and in `UnfilledSlots` — which is
/// what a commit gate reads.
#[test]
fn a_slot_survives_a_mesh_edit_and_says_what_it_lost() {
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
            node: None,
        }));
        step(quad(session, id.clone()));
        id
    };
    let (body_part, hem_part) = (part("Body"), part("Skirt"));

    let (left, right) = (SlotId::new("left").unwrap(), SlotId::new("right").unwrap());

    for node in [&body_part, &hem_part] {
        for slot in [&left, &right] {
            step(Command::SlotAdd {
                session,
                node: node.clone(),
                slot: Some(slot.clone()),
            });
        }
    }
    for (i, slot) in [&left, &right].into_iter().enumerate() {
        step(Command::SlotFill {
            session,
            node: body_part.clone(),
            slot: slot.clone(),
            vertex: i as u32,
        });
        step(Command::SlotFill {
            session,
            node: hem_part.clone(),
            slot: slot.clone(),
            vertex: (i + 2) as u32,
        });
    }

    match step(Command::Slots {
        session,
        node: body_part.clone(),
    }) {
        ResponseBody::Slots { slots } => {
            assert_eq!(
                slots
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

    // Weld them, a pair at a time.
    let pairs = |weight: f32| {
        [&left, &right]
            .into_iter()
            .map(|s| SlotPair {
                a: s.clone(),
                b: s.clone(),
                weight,
            })
            .collect::<Vec<_>>()
    };
    step(Command::WeldSet {
        session,
        a: body_part.clone(),
        b: hem_part.clone(),
        pairs: pairs(catchlight_core::DEFAULT_SLOT_WEIGHT),
    });
    match step(Command::Welds { session }) {
        ResponseBody::Welds { welds } => {
            assert_eq!(welds.len(), 1);
            assert_eq!(welds[0].a, body_part);
            assert_eq!(welds[0].b, hem_part);
            assert_eq!(welds[0].pairs.len(), 2);
            assert!(welds[0]
                .pairs
                .iter()
                .all(|p| (p.weight - catchlight_core::DEFAULT_SLOT_WEIGHT).abs() < 1e-6));
        }
        other => panic!("{other:?}"),
    }
    // Setting the same pair of parts again replaces the weld rather than
    // stacking one the model would refuse.
    step(Command::WeldSet {
        session,
        a: hem_part.clone(),
        b: body_part.clone(),
        pairs: pairs(catchlight_core::DEFAULT_SLOT_WEIGHT),
    });
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.len() == 1
    ));

    // Re-author the body's mesh: every slot on it empties, and the reply says
    // which. The weld keeps both pairs and skips the ones nothing fills.
    match step(quad(session, body_part.clone())) {
        ResponseBody::Emptied { node, slots } => {
            assert_eq!(node, body_part);
            assert_eq!(
                slots.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                vec!["left".to_string(), "right".to_string()],
            );
        }
        other => panic!("{other:?}"),
    }
    match step(Command::UnfilledSlots { session }) {
        ResponseBody::UnfilledSlots { slots } => {
            assert_eq!(slots.len(), 2, "the commit gate sees both");
            assert!(slots.iter().all(|s| s.node == body_part));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.len() == 1 && welds[0].pairs.len() == 2
    ));

    // Refilling clears the gate.
    for (i, slot) in [&left, &right].into_iter().enumerate() {
        step(Command::SlotFill {
            session,
            node: body_part.clone(),
            slot: slot.clone(),
            vertex: i as u32,
        });
    }
    assert!(matches!(
        step(Command::UnfilledSlots { session }),
        ResponseBody::UnfilledSlots { slots } if slots.is_empty()
    ));

    // Deleting a slot takes the pairs that named it; the weld itself stays.
    for slot in [&left, &right] {
        step(Command::SlotDelete {
            session,
            node: body_part.clone(),
            slot: slot.clone(),
        });
    }
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.len() == 1 && welds[0].pairs.is_empty()
    ));
}

fn slot_of(b: ResponseBody) -> SlotAddr {
    match b {
        ResponseBody::Slot { slot } => slot,
        other => panic!("expected a slot, got {other:?}"),
    }
}

/// A slot gets its Id the way a node and a param do: name one, or let the
/// editor draw a free one and read it back off the reply.
///
/// The reason it has to be the editor drawing is that "free" is scoped to the
/// part, so a client counting its own slots gets it wrong the moment one is
/// deleted.
#[test]
fn a_slot_can_be_added_without_naming_one() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);

    let mut next = 3;
    let mut step = |command: Command| {
        next += 1;
        body(&ed, next, command)
    };

    let part = node_of(step(Command::NodeAdd {
        session,
        parent: root,
        kind: NodeKindArg::Part,
        name: None,
        node: None,
    }));
    step(quad(session, part.clone()));

    // Named: the reply still says what it is, so one code path reads it.
    let named = SlotId::new("collar").unwrap();
    let addr = slot_of(step(Command::SlotAdd {
        session,
        node: part.clone(),
        slot: Some(named.clone()),
    }));
    assert_eq!(
        addr,
        SlotAddr {
            node: part.clone(),
            slot: named.clone()
        }
    );

    // Unnamed: drawn, free, and reported.
    let drawn = slot_of(step(Command::SlotAdd {
        session,
        node: part.clone(),
        slot: None,
    }));
    assert_eq!(drawn.node, part);
    assert_ne!(drawn.slot, named);
    assert!(drawn.slot.as_str().starts_with("slot-"), "{}", drawn.slot);

    // Both landed where the reply said they did.
    match step(Command::Slots {
        session,
        node: part.clone(),
    }) {
        ResponseBody::Slots { slots } => {
            assert_eq!(
                slots.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
                vec![named, drawn.slot.clone()],
            );
        }
        other => panic!("{other:?}"),
    }

    // And a draw for a node that could never carry a slot is still refused.
    next += 1;
    assert!(matches!(
        reply(
            &ed,
            next,
            Command::SlotAdd {
                session,
                node: NodeId::new("root").unwrap(),
                slot: None,
            }
        ),
        Reply::Err { .. }
    ));
}

/// A slot is renamed through the same command every other Id is, because it
/// is the same kind of breaking change — and a weld pairs slots, so the
/// rename has to reach the pairs or one of them points at nothing.
#[test]
fn renaming_a_slot_carries_its_weld_pairs_and_refuses_a_name_in_use() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);

    let mut next = 3;
    let mut step = |command: Command| {
        next += 1;
        body(&ed, next, command)
    };
    let slot = SlotId::new("left").unwrap();
    let mut part = || {
        let node = node_of(step(Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        }));
        step(quad(session, node.clone()));
        step(Command::SlotAdd {
            session,
            node: node.clone(),
            slot: Some(slot.clone()),
        });
        node
    };
    let (top, bottom) = (part(), part());
    step(Command::WeldSet {
        session,
        a: top.clone(),
        b: bottom.clone(),
        pairs: vec![SlotPair {
            a: slot.clone(),
            b: slot.clone(),
            weight: 0.5,
        }],
    });

    let neck = SlotId::new("neck").unwrap();
    step(Command::RenameId {
        session,
        rename: Rename::Slot {
            node: top.clone(),
            from: slot.clone(),
            to: neck.clone(),
        },
    });

    match step(Command::Slots {
        session,
        node: top.clone(),
    }) {
        ResponseBody::Slots { slots } => assert_eq!(slots[0].id, neck),
        other => panic!("{other:?}"),
    }
    match step(Command::Welds { session }) {
        ResponseBody::Welds { welds } => {
            assert_eq!(welds[0].pairs[0].a, neck, "the pair followed the rename");
            assert_eq!(
                welds[0].pairs[0].b, slot,
                "and the far part's slot is not this rename's business"
            );
        }
        other => panic!("{other:?}"),
    }

    // The two refusals a client reacts to, under the codes the slot surface
    // already uses.
    step(Command::SlotAdd {
        session,
        node: top.clone(),
        slot: Some(slot.clone()),
    });
    for (rename, code) in [
        (
            Rename::Slot {
                node: top.clone(),
                from: SlotId::new("gone").unwrap(),
                to: SlotId::new("x").unwrap(),
            },
            ErrorCode::UnknownSlot,
        ),
        (
            Rename::Slot {
                node: top.clone(),
                from: neck.clone(),
                to: slot.clone(),
            },
            ErrorCode::DuplicateSlot,
        ),
    ] {
        next += 1;
        match reply(&ed, next, Command::RenameId { session, rename }) {
            Reply::Err { code: got, .. } => assert_eq!(got, code),
            other => panic!("expected an error, got {other:?}"),
        }
    }
}

/// One weight at a time, which is what a slider sends. `weld_set` can only
/// rewrite a weld whole, so moving one weight through it means reading every
/// other one back and sending it again unchanged.
#[test]
fn a_slot_weight_moves_on_its_own_and_means_the_end_it_names() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);

    let mut next = 3;
    let mut step = |command: Command| {
        next += 1;
        body(&ed, next, command)
    };
    let (left, right) = (SlotId::new("left").unwrap(), SlotId::new("right").unwrap());
    let mut part = || {
        let node = node_of(step(Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        }));
        step(quad(session, node.clone()));
        for slot in [&left, &right] {
            step(Command::SlotAdd {
                session,
                node: node.clone(),
                slot: Some(slot.clone()),
            });
        }
        node
    };
    let (a, b) = (part(), part());
    step(Command::WeldSet {
        session,
        a: a.clone(),
        b: b.clone(),
        pairs: [&left, &right]
            .into_iter()
            .map(|s| SlotPair {
                a: s.clone(),
                b: s.clone(),
                weight: 0.5,
            })
            .collect(),
    });

    let weights = |ed: &Editor, id: u64| match body(ed, id, Command::Welds { session }) {
        ResponseBody::Welds { welds } => welds[0]
            .pairs
            .iter()
            .map(|p| (p.a.to_string(), p.weight))
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };

    step(Command::WeldWeight {
        session,
        a: a.clone(),
        b: b.clone(),
        slot: left.clone(),
        weight: 0.25,
    });
    assert_eq!(
        weights(&ed, 900),
        vec![("left".into(), 0.25), ("right".into(), 0.5)],
        "the pair nobody named kept its weight",
    );

    // Named the other way round the same number is B's share, so A's is 0.75.
    step(Command::WeldWeight {
        session,
        a: b.clone(),
        b: a.clone(),
        slot: left.clone(),
        weight: 0.25,
    });
    assert_eq!(
        weights(&ed, 901),
        vec![("left".into(), 0.75), ("right".into(), 0.5)]
    );

    // A pair nothing welds, and a share that is not one.
    next += 1;
    assert!(matches!(
        reply(
            &ed,
            next,
            Command::WeldWeight {
                session,
                a: a.clone(),
                b: a.clone(),
                slot: left.clone(),
                weight: 0.5,
            }
        ),
        Reply::Err {
            code: ErrorCode::UnknownWeld,
            ..
        }
    ));
    next += 1;
    assert!(matches!(
        reply(
            &ed,
            next,
            Command::WeldWeight {
                session,
                a,
                b,
                slot: left,
                weight: 1.5,
            }
        ),
        Reply::Err {
            code: ErrorCode::WeldWeightOutOfRange,
            ..
        }
    ));
}

/// A weld comes undone two ways, and only one of them keeps the parts.
///
/// Deleting a part cascades — it takes the weld because a weld with one end is
/// not a weld — so before `weld_delete` the only way to unpair two parts was
/// to destroy one of them, along with every slot on it. This pins that the new
/// command leaves both ends exactly where they were, that either order names
/// the same weld, and that a pair nothing joins is its own error code rather
/// than a silent no-op.
#[test]
fn a_weld_is_unmade_without_taking_the_parts_with_it() {
    let ed = Editor::new();
    let session = session_of(body(&ed, 1, Command::SessionNew { name: None }));
    let root = root(&ed, 2, session);

    let mut next = 3;
    let mut step = |command: Command| {
        next += 1;
        body(&ed, next, command)
    };

    let slot = SlotId::new("left").unwrap();
    let mut part = || {
        let node = node_of(step(Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        }));
        step(quad(session, node.clone()));
        step(Command::SlotAdd {
            session,
            node: node.clone(),
            slot: Some(slot.clone()),
        });
        step(Command::SlotFill {
            session,
            node: node.clone(),
            slot: slot.clone(),
            vertex: 0,
        });
        node
    };
    let (top, bottom) = (part(), part());

    step(Command::WeldSet {
        session,
        a: top.clone(),
        b: bottom.clone(),
        pairs: vec![SlotPair {
            a: slot.clone(),
            b: slot.clone(),
            weight: 0.5,
        }],
    });
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.len() == 1
    ));

    // Named the other way round: a weld has no Id, its two ends are what
    // names it, so B-then-A finds the same one.
    step(Command::WeldDelete {
        session,
        a: bottom.clone(),
        b: top.clone(),
    });
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.is_empty()
    ));

    // Both parts are still here, with their slots still filled — which is the
    // whole difference from deleting a slot.
    for node in [&top, &bottom] {
        match step(Command::Slots {
            session,
            node: node.clone(),
        }) {
            ResponseBody::Slots { slots } => {
                assert_eq!(
                    slots
                        .iter()
                        .map(|s| (s.id.to_string(), s.vertex))
                        .collect::<Vec<_>>(),
                    vec![("left".into(), Some(0))],
                    "the slot outlives the weld",
                );
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(matches!(
        step(Command::UnfilledSlots { session }),
        ResponseBody::UnfilledSlots { slots } if slots.is_empty()
    ));

    // Undo brings the weld back, so this is one ordinary edit.
    step(Command::Undo { session });
    assert!(matches!(
        step(Command::Welds { session }),
        ResponseBody::Welds { welds } if welds.len() == 1
    ));

    // And a pair nothing joins says so, rather than reporting a delete that
    // deleted nothing.
    step(Command::WeldDelete {
        session,
        a: top.clone(),
        b: bottom.clone(),
    });
    next += 1;
    assert!(matches!(
        reply(
            &ed,
            next,
            Command::WeldDelete {
                session,
                a: top,
                b: bottom,
            }
        ),
        Reply::Err {
            code: ErrorCode::UnknownWeld,
            ..
        }
    ));
}

/// Every slot refusal a client has to react to gets its own code, so a mesh
/// editor can tell "you already used that name" from "that vertex is not on
/// this part" without reading English.
#[test]
fn the_slot_errors_a_client_reacts_to_have_their_own_codes() {
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
            node: None,
        },
    ));
    body(&ed, 4, quad(session, part.clone()));
    let slot = SlotId::new("left").unwrap();
    body(
        &ed,
        5,
        Command::SlotAdd {
            session,
            node: part.clone(),
            slot: Some(slot.clone()),
        },
    );

    let code = |id: u64, cmd: Command| match reply(&ed, id, cmd) {
        Reply::Err { code, .. } => code,
        other => panic!("expected an error, got {other:?}"),
    };
    assert_eq!(
        code(
            6,
            Command::SlotAdd {
                session,
                node: part.clone(),
                slot: Some(slot.clone()),
            }
        ),
        ErrorCode::DuplicateSlot
    );
    assert_eq!(
        code(
            7,
            Command::SlotClear {
                session,
                node: part.clone(),
                slot: SlotId::new("nope").unwrap(),
            }
        ),
        ErrorCode::UnknownSlot
    );
    // A weld the file could not carry is refused where it is made.
    let other = node_of(body(
        &ed,
        8,
        Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        },
    ));
    body(&ed, 9, quad(session, other.clone()));
    let pair = |a: &str, b: &str| SlotPair {
        a: SlotId::new(a).unwrap(),
        b: SlotId::new(b).unwrap(),
        weight: 0.5,
    };
    assert_eq!(
        code(
            10,
            Command::WeldSet {
                session,
                a: part.clone(),
                b: other.clone(),
                pairs: vec![pair("left", "nope")],
            }
        ),
        ErrorCode::WeldUnknownSlot,
        "a pair naming a slot the far part does not carry"
    );
    body(
        &ed,
        11,
        Command::SlotAdd {
            session,
            node: other.clone(),
            slot: Some(slot.clone()),
        },
    );
    assert_eq!(
        code(
            12,
            Command::WeldSet {
                session,
                a: part.clone(),
                b: other.clone(),
                pairs: vec![pair("left", "left"), pair("left", "left")],
            }
        ),
        ErrorCode::WeldSlotPairedTwice
    );
    assert_eq!(
        code(
            13,
            Command::WeldSet {
                session,
                a: part.clone(),
                b: other.clone(),
                pairs: vec![SlotPair {
                    a: slot.clone(),
                    b: slot.clone(),
                    weight: 1.5,
                }],
            }
        ),
        ErrorCode::WeldWeightOutOfRange
    );
    assert_eq!(
        code(
            14,
            Command::WeldSet {
                session,
                a: part.clone(),
                b: part.clone(),
                pairs: Vec::new(),
            }
        ),
        ErrorCode::BadWeldEnd,
        "a part is not welded to itself"
    );
    assert_eq!(
        code(
            15,
            Command::WeldSet {
                session,
                a: part,
                b: root,
                pairs: Vec::new(),
            }
        ),
        ErrorCode::BadWeldEnd,
        "and only a part carries slots to pair"
    );
}
