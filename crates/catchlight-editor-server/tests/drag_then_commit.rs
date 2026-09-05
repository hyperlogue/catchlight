#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A live vertex drag rides the presence path and the commit rides the
//! document path. That split is what keeps the undo history usable: dragging
//! a vertex emits a `ScratchDeform` per mouse move, and if those touched the
//! model, one gesture would bury every earlier edit under a hundred
//! indistinguishable snapshots (and blow the 256 MiB budget on any model with
//! real textures).

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
        assert!(matches!(
            ed.handle(Request {
                id: 100 + i as u64,
                command: Command::ScratchDeform {
                    session,
                    node: node.clone(),
                    offsets,
                },
            }),
            Reply::Ok { .. }
        ));
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
    assert!(!status(3).dirty, "a drag must not dirty the document");

    // The commit — the same offsets, authored into a deform keypoint.
    let offsets: Vec<[f32; 2]> = (0..vertices)
        .map(|v| [0.99 + v as f32 * 0.001, 0.99 - v as f32 * 0.001])
        .collect();
    assert!(matches!(
        body(
            &ed,
            4,
            Command::DeformVertices {
                session,
                params: BindingParams::one(param),
                node: node.clone(),
                cell: [0, 0],
                offsets,
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
            command: Command::Undo { session }
        }),
        Reply::Ok { .. }
    ));
    assert_eq!(ed.history(session).unwrap(), (0, 1));
    assert!(matches!(
        ed.handle(Request {
            id: 8,
            command: Command::Undo { session }
        }),
        Reply::Err { .. }
    ));
}

/// A scratch deform has to name a node the model carries, and to carry one
/// offset per vertex of that node's mesh. Serde already refuses an offset with
/// a coordinate missing; how many of them there are is what is left to check,
/// and the puppet would otherwise be handed the wrong count.
#[test]
fn a_scratch_deform_is_checked_against_the_node_it_names() {
    use catchlight_editor_protocol::ErrorCode;

    let ed = Editor::new();
    let session = open_bytes(&ed, "welded_seam", welded_seam());
    let node = ed
        .with_model(session, |model| {
            model
                .node_ids()
                .find(|id| model.node_mesh(id).is_some_and(|m| !m.verts.is_empty()))
                .cloned()
                .expect("a meshed node")
        })
        .unwrap();

    assert!(matches!(
        ed.handle(Request {
            id: 1,
            command: Command::ScratchDeform {
                session,
                node: NodeId::new("no-such-node").unwrap(),
                offsets: vec![[0.0, 0.0]],
            },
        }),
        Reply::Err {
            code: ErrorCode::NoNode,
            ..
        }
    ));
    assert!(matches!(
        ed.handle(Request {
            id: 2,
            command: Command::ScratchDeform {
                session,
                node: node.clone(),
                offsets: vec![[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]],
            },
        }),
        Reply::Err {
            code: ErrorCode::BadTarget,
            ..
        }
    ));
    // An empty list is how a drag ends: drop the scratch deform.
    assert!(matches!(
        ed.handle(Request {
            id: 3,
            command: Command::ScratchDeform {
                session,
                node,
                offsets: Vec::new(),
            },
        }),
        Reply::Ok { .. }
    ));
    assert_eq!(ed.history(session).unwrap(), (0, 0));
}

/// A session holding `bytes`: a fresh one, then the file imported into it.
///
/// The one way bytes a caller holds become a document — there is no side door
/// that takes them, so a test opens a model exactly as a client does.
fn open_bytes(editor: &Editor, title: &str, bytes: Vec<u8>) -> SessionId {
    let reply = editor.handle(Request {
        id: 0,
        command: Command::SessionNew {
            name: Some(title.to_string()),
        },
    });
    let session = match reply {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("expected a session, got {other:?}"),
    };
    let mut attachments = Attachments::none();
    attachments.insert("model", bytes);
    match editor.handle_with(
        Request {
            id: 0,
            command: Command::ImportFile {
                session,
                parent: None,
            },
        },
        attachments,
    ) {
        (Reply::Ok { .. }, _) => session,
        (other, _) => panic!("import_file: {other:?}"),
    }
}
