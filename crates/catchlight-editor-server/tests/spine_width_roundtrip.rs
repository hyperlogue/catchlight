#![allow(clippy::unwrap_used, clippy::panic)]
//! Import/export preserves the same width fields the runtime tests evaluate.

#[path = "../../../tests/support/spine_width.rs"]
mod support;

use catchlight_core::Model;
use catchlight_editor_protocol::{Command, Reply, Request, ResponseBody, SessionSource};
use catchlight_editor_server::{Attachments, Editor};

#[test]
fn editor_import_export_keeps_spines_and_paired_width_bindings() {
    let fixture = support::Fixture::new();
    let bytes = fixture.model.to_clm_bytes().unwrap();
    let mut attachments = Attachments::none();
    attachments.insert("model", bytes.clone());
    let editor = Editor::new();
    let (reply, _) = editor.handle_with(
        Request {
            id: 1,
            command: Command::SessionNew {
                name: None,
                source: Some(SessionSource::Clm {}),
            },
        },
        attachments,
    );
    let Reply::Ok {
        body: ResponseBody::Session { session },
        ..
    } = reply
    else {
        panic!("{reply:?}")
    };
    let (reply, payload) = editor.handle_with(
        Request {
            id: 2,
            command: Command::ModelExport { session, if_rev: 0 },
        },
        Attachments::none(),
    );
    assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
    let payload = payload.unwrap();
    let reopened = Model::from_clm_bytes(&payload.bytes).unwrap();
    assert!(fixture.model.authored_eq(&reopened).unwrap());
    assert_eq!(payload.bytes, bytes);
}
