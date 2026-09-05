#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Regression: posing a param on a session's puppet must move it exactly like
//! the viewport does (set the value by Id, then tick).

use catchlight_editor_protocol::{Command, Reply, Request, ResponseBody, SessionId};
use catchlight_editor_server::{Attachments, Editor};

#[test]
fn posing_a_param_changes_the_ticked_state() {
    // `welded_seam`'s one param (`pull`) drives a deform, so posing it has to
    // show up in the ticked state.
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clm"
    ))
    .expect("welded_seam.clm");
    let ed = Editor::new();
    let session = open_bytes(&ed, "welded_seam", bytes);

    let moved = ed
        .with_puppet(session, |model, puppet| {
            let params: Vec<_> = model.param_ids().to_vec();
            assert!(!params.is_empty(), "the model has params");

            puppet.tick(model, 0.0);
            let baseline = state_signature(puppet);

            let mut any_moved = false;
            for id in &params {
                let (min, max) = {
                    let p = model.param(id).expect("param");
                    (p.min, p.max)
                };
                puppet.set_param_value(id, max);
                puppet.tick(model, 0.0);
                if state_signature(puppet) != baseline {
                    any_moved = true;
                }
                puppet.set_param_value(id, min.midpoint(max));
            }
            any_moved
        })
        .expect("with_puppet");
    assert!(moved, "no param pose changed transforms or deforms");
}

fn state_signature(puppet: &catchlight_core::Puppet) -> Vec<[i64; 2]> {
    let mut sig = Vec::new();
    let order = puppet.tree().with_dfs_order(|o| o.to_vec());
    for id in order {
        let m = puppet.transforms().get(id);
        let t = m.transform_point3(glam::vec3(0.0, 0.0, 0.0));
        sig.push([(t.x * 1e3) as i64, (t.y * 1e3) as i64]);
        if let Some(node) = puppet.get(id) {
            if let catchlight_core::NodeKind::Part(p) = &node.kind {
                for v in p.deform_stack.combined() {
                    sig.push([(v.x * 1e3) as i64, (v.y * 1e3) as i64]);
                }
            }
        }
    }
    sig
}

/// A session holding `bytes`: a fresh one, then the file imported into it.
///
/// The one way bytes a caller holds become a session's model — no side door
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
