#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Two sessions never draw the same generated Id for the same edits.
//!
//! Uniqueness is checked *within* a model, so one seed shared by every session
//! made every open document mint `root/part-<the same hex>` first. Nothing in
//! one session noticed, and everything that carries a node out of one and into
//! another did: a copied subtree collided on arrival, and an addon extracted
//! from one document named a node in the other by accident.
//!
//! The draw stays deterministic — a given session Id always mints the same
//! sequence — because a recorded script replayed against a fresh editor should
//! rebuild the same model, Ids included.

use catchlight_editor_protocol::{
    Command, NodeId, NodeKindArg, Reply, Request, ResponseBody, SessionId, SlotAddr,
};
use catchlight_editor_server::Editor;

fn body(ed: &Editor, id: u64, command: Command) -> ResponseBody {
    match ed.handle(Request { id, command }) {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn new_session(ed: &Editor, id: u64) -> SessionId {
    match body(ed, id, Command::SessionNew { name: None }) {
        ResponseBody::Session { session } => session,
        other => panic!("{other:?}"),
    }
}

/// The Ids one fixed sequence of adds draws in `session`: a part, a param and
/// a slot, so every generator the editor owns is covered.
fn drawn(ed: &Editor, base: u64, session: SessionId) -> Vec<String> {
    let node = match body(
        ed,
        base,
        Command::NodeAdd {
            session,
            parent: NodeId::new("root").unwrap(),
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };
    let param = match body(
        ed,
        base + 1,
        Command::ParamAdd {
            session,
            name: "Pull".into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            key_positions: Vec::new(),
            param: None,
        },
    ) {
        ResponseBody::Param { param } => param.to_string(),
        other => panic!("{other:?}"),
    };
    let slot = match body(
        ed,
        base + 2,
        Command::SlotAdd {
            session,
            node: node.clone(),
            slot: None,
        },
    ) {
        ResponseBody::Slot {
            slot: SlotAddr { slot, .. },
        } => slot.to_string(),
        other => panic!("{other:?}"),
    };
    vec![node.to_string(), param, slot]
}

/// Two documents open at once, edited the same way, must not name their new
/// things identically — that is what makes an Id from one recognisable inside
/// a model merged from both.
#[test]
fn two_sessions_draw_different_ids_for_the_same_edits() {
    let ed = Editor::new();
    let first = new_session(&ed, 1);
    let second = new_session(&ed, 2);

    let one = drawn(&ed, 10, first);
    let two = drawn(&ed, 20, second);

    assert_eq!(one.len(), 3);
    for (a, b) in one.iter().zip(&two) {
        assert_ne!(a, b, "two sessions drew the same Id");
    }
    // A closed session's Id is never reissued, so a session opened later
    // does not inherit a closed one's draw either.
    body(&ed, 30, Command::SessionClose { session: first });
    let third = new_session(&ed, 31);
    assert_ne!(third, first);
    for (a, b) in drawn(&ed, 40, third).iter().zip(&one) {
        assert_ne!(a, b);
    }
}

/// Per-session is not per-run: replaying the same script against a fresh
/// editor rebuilds the same model, Ids and all.
#[test]
fn the_same_session_id_draws_the_same_ids_every_time() {
    let once = {
        let ed = Editor::new();
        let session = new_session(&ed, 1);
        drawn(&ed, 10, session)
    };
    let again = {
        let ed = Editor::new();
        let session = new_session(&ed, 1);
        drawn(&ed, 10, session)
    };
    assert_eq!(once, again);
}
