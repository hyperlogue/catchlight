#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Regression for the GUI recording flow: add a param, record a key via
//! BindingKeys at a keypoint, then pose the param on the rebaked puppet and
//! check the node actually moves (and returns to rest at the other keypoint).

use catchlight_editor_protocol::{
    BindingKeyEntry, Command, NodeKindArg, Reply, Request, ResponseBody,
};
use catchlight_editor_server::Editor;

fn body(ed: &Editor, id: u64, command: Command) -> ResponseBody {
    match ed.handle(Request { id, command }) {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn recorded_binding_moves_the_rebaked_puppet() {
    // Any rig will do: the test authors its own node, param and binding, and
    // only needs a document to open.
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clm"
    ))
    .expect("welded_seam.clm");
    let ed = Editor::new();
    let session = ed.open_bytes("welded_seam", &bytes).expect("open");

    let root = match body(&ed, 1, Command::NodeTree { session }) {
        ResponseBody::Tree { root } => root.node,
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
        },
    ) {
        ResponseBody::Node { node } => node,
        other => panic!("{other:?}"),
    };

    let param = match body(
        &ed,
        3,
        Command::ParamAdd {
            session,
            name: "probe-param".into(),
            vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_x: Vec::new(),
            axis_y: Vec::new(),
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
            param,
            node,
            cell: [1, 0],
            entries: vec![BindingKeyEntry {
                target: "tx".into(),
                value: 25.0,
            }],
        },
    );

    let x_at = |pose: f32| -> f32 {
        ed.with_puppet(session, |model, puppet| {
            let param = model
                .param_ids()
                .iter()
                .find(|id| {
                    model
                        .param(id)
                        .is_some_and(|p| p.name.as_str() == "probe-param")
                })
                .cloned()
                .expect("param exists on the rebaked puppet");
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
            param,
            node,
            target: "tx".into(),
            cell: [1, 0],
        },
    );
    let after_unset = x_at(1.0);
    assert!(
        (after_unset - rest).abs() < 1e-3,
        "unset binding still moves the node: rest={rest} after_unset={after_unset}"
    );
}
