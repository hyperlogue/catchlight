#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Local Puppet scratch previews a drag without publishing an editor revision.
//! The final exact-cell write is one undoable model edit.

use catchlight_editor_protocol::{
    BindingParams, Command, NodeId, ParamId, Reply, Request, ResponseBody, SessionId,
};
use catchlight_editor_server::{Attachments, Editor};

fn body(ed: &Editor, id: u64, command: Command) -> ResponseBody {
    match ed.handle(Request { id, command }) {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn welded_seam() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clm"
    ))
    .expect("welded_seam.clm")
}

#[test]
fn a_hundred_drag_events_and_one_commit_leave_one_undo_entry() {
    let ed = Editor::new();
    let session = open_bytes(&ed, "welded_seam", welded_seam());

    // Any meshed node and any param will do: the test is about which path the
    // commands take, not about what they draw.
    let (node, param, vertices) = ed
        .with_model(session, |model| {
            let node: NodeId = model
                .node_ids()
                .find(|id| model.node_mesh(id).is_some_and(|m| !m.verts.is_empty()))
                .cloned()
                .expect("welded_seam has a meshed node");
            let param: ParamId = model.param_ids().first().cloned().expect("a param");
            let vertices = model.deform_len(&node) / 2;
            (node, param, vertices)
        })
        .unwrap();

    let status = |id: u64| match body(&ed, id, Command::Status { session }) {
        ResponseBody::Status { status } => status,
        other => panic!("{other:?}"),
    };
    let rev_before = status(1).rev;
    assert_eq!(ed.history(session).unwrap(), (0, 0));

    for i in 0..100u32 {
        let nudge = i as f32 * 0.01;
        let offsets: Vec<[f32; 2]> = (0..vertices)
            .map(|v| [nudge + v as f32 * 0.001, nudge - v as f32 * 0.001])
            .collect();
        ed.with_puppet(session, |model, puppet| {
            let index = puppet.node_idx(&node).unwrap();
            assert!(puppet.set_scratch_deform(
                index,
                &offsets
                    .into_iter()
                    .map(glam::Vec2::from)
                    .collect::<Vec<_>>()
            ));
            puppet.tick(model, 0.0);
        })
        .unwrap();
    }

    assert_eq!(
        ed.history(session).unwrap(),
        (0, 0),
        "a drag must not snapshot the model"
    );
    assert_eq!(
        status(2).rev,
        rev_before,
        "a drag must not bump the revision"
    );
    assert!(!status(3).dirty, "a drag must not dirty the model");

    // The commit — the same offsets, authored into a deform keypoint.
    let offsets: Vec<[f32; 2]> = (0..vertices)
        .map(|v| [0.99 + v as f32 * 0.001, 0.99 - v as f32 * 0.001])
        .collect();
    assert!(matches!(
        body(
            &ed,
            4,
            Command::BindingCellsSet {
                if_rev: ed.revision(session).unwrap(),
                session,
                params: BindingParams::one(param),
                node: node.clone(),
                target: catchlight_editor_protocol::BindingTarget::Deform,
                cells: vec![catchlight_editor_protocol::BindingCellWrite {
                    cell: [0, 0],
                    value: catchlight_editor_protocol::BindingCellValue::Offsets(offsets),
                }],
            },
        ),
        ResponseBody::Empty
    ));

    assert_eq!(
        ed.history(session).unwrap(),
        (1, 0),
        "the commit is exactly one undo entry"
    );
    assert!(status(5).rev > rev_before);
    assert!(status(6).dirty);

    // ...and undoing it once gets the whole gesture back.
    assert!(matches!(
        ed.handle(Request {
            id: 7,
            command: Command::Undo {
                session,
                if_rev: ed.revision(session).unwrap()
            }
        }),
        Reply::Ok { .. }
    ));
    assert_eq!(ed.history(session).unwrap(), (0, 1));
    assert!(matches!(
        ed.handle(Request {
            id: 8,
            command: Command::Undo {
                session,
                if_rev: ed.revision(session).unwrap()
            }
        }),
        Reply::Err { .. }
    ));
}

#[test]
fn scratch_is_not_a_server_command() {
    assert!(serde_json::from_value::<Request>(serde_json::json!({
        "id": 1, "cmd": "scratch_deform", "session": 1,
        "node": "panel", "offsets": [[0, 0]]
    }))
    .is_err());
}

/// Session construction validates the supplied model before publishing it.
fn open_bytes(editor: &Editor, title: &str, bytes: Vec<u8>) -> SessionId {
    let mut attachments = Attachments::none();
    attachments.insert("model", bytes);
    match editor
        .handle_with(
            Request {
                id: 0,
                command: Command::SessionNew {
                    name: Some(title.to_string()),
                    source: Some(catchlight_editor_protocol::SessionSource::Clm {}),
                },
            },
            attachments,
        )
        .0
    {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("session_create: {other:?}"),
    }
}
