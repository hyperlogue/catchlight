#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Regression for the GUI recording flow: add a param, record a key via
//! BindingKeys at a keypoint, then pose the param on the rebaked puppet and
//! check the node actually moves (and returns to rest at the other keypoint).

use catchlight_editor_protocol::{
    BindingKeyEntry, BindingParams, BindingTarget, Command, NodeKindArg, Reply, Request,
    ResponseBody, ScalarTarget, SessionId,
};
use catchlight_editor_server::{Attachments, Editor};

fn body(ed: &Editor, id: u64, command: Command) -> ResponseBody {
    match ed.handle(Request { id, command }) {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn recorded_binding_moves_the_rebaked_puppet() {
    // Any model will do: the test authors its own node, param and binding, and
    // only needs a document to open.
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clm"
    ))
    .expect("welded_seam.clm");
    let ed = Editor::new();
    let session = open_bytes(&ed, "welded_seam", bytes);

    let root = match body(&ed, 1, Command::NodeTree { session }) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("{other:?}"),
    };
    let node = match body(
        &ed,
        2,
        Command::NodeAdd {
            session,
            parent: root,
            kind: NodeKindArg::Group,
            name: Some("probe".into()),
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };

    let param = match body(
        &ed,
        3,
        Command::ParamAdd {
            session,
            name: "probe-param".into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            key_positions: Vec::new(),
            param: None,
        },
    ) {
        ResponseBody::Param { param } => param,
        other => panic!("{other:?}"),
    };

    // The GUI records at cell [1, 0] (param at max): one translation key.
    body(
        &ed,
        4,
        Command::BindingKeys {
            session,
            params: BindingParams::one(param.clone()),
            node: node.clone(),
            cell: [1, 0],
            entries: vec![BindingKeyEntry {
                target: ScalarTarget::Tx,
                value: 25.0,
            }],
        },
    );

    let x_at = |pose: f32| -> f32 {
        ed.with_puppet(session, |model, puppet| {
            // The Id the ParamAdd reply handed back still names the param
            // after the model has been edited and the puppet rebaked.
            assert!(model.param(&param).is_some(), "the param survived");
            puppet.set_param_value(&param, pose);
            puppet.tick(model, 0.0);
            let order = puppet.tree().with_dfs_order(|o| o.to_vec());
            let id = order
                .into_iter()
                .find(|&id| puppet.get(id).is_some_and(|n| n.name == "probe"))
                .expect("probe node");
            puppet
                .transforms()
                .get(id)
                .transform_point3(glam::vec3(0.0, 0.0, 0.0))
                .x
        })
        .expect("with_puppet")
    };

    let rest = x_at(0.0);
    let posed = x_at(1.0);
    assert!(
        (posed - rest - 25.0).abs() < 1e-3,
        "recorded key must move the node: rest={rest} posed={posed}"
    );

    // Un-authoring the only key must not brick the preview rebuild.
    body(
        &ed,
        5,
        Command::BindingUnset {
            session,
            params: BindingParams::one(param.clone()),
            node: node.clone(),
            target: BindingTarget::Tx,
            cell: [1, 0],
        },
    );
    let after_unset = x_at(1.0);
    assert!(
        (after_unset - rest).abs() < 1e-3,
        "unset binding still moves the node: rest={rest} after_unset={after_unset}"
    );
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
