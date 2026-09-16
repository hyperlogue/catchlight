#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Regression for the GUI recording flow: add a param, record a key via
//! an explicit rest/key batch at a keypoint, then pose the param on the rebaked puppet and
//! check the node actually moves (and returns to rest at the other keypoint).

use catchlight_editor_protocol::{
    BindingCellValue, BindingCellWrite, BindingParams, BindingTarget, Command, NodeKindArg, Reply,
    Request, ResponseBody, SessionId,
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
    // only needs a model to open.
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
        Command::BindingCellsSet {
            if_rev: ed.revision(session).unwrap(),
            session,
            params: BindingParams::one(param.clone()),
            node: node.clone(),
            target: BindingTarget::Tx,
            cells: vec![
                BindingCellWrite {
                    cell: [0, 0],
                    value: BindingCellValue::Scalar(0.0),
                },
                BindingCellWrite {
                    cell: [1, 0],
                    value: BindingCellValue::Scalar(25.0),
                },
            ],
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
        Command::BindingCellsUnset {
            if_rev: ed.revision(session).unwrap(),
            session,
            params: BindingParams::one(param.clone()),
            node: node.clone(),
            target: BindingTarget::Tx,
            cells: vec![[1, 0]],
        },
    );
    let after_unset = x_at(1.0);
    assert!(
        (after_unset - rest).abs() < 1e-3,
        "unset binding still moves the node: rest={rest} after_unset={after_unset}"
    );
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
