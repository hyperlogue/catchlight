#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A driver's outputs are positional, and either one may be bound to nothing.
//!
//! A SimplePhysics node writes two numbers — an angle and a length — and the
//! model has always held them as two slots either of which can be empty. The
//! wire could not say so: `target_params` was a list of Ids, so the second
//! output could only be bound by binding the first, and a rig that wanted just
//! the length had to invent a throwaway param for the angle.

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, ParamId, Reply, Request, ResponseBody, SessionId,
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

/// An editor with one session and two params to drive.
fn fixture() -> (Editor, SessionId, ParamId, ParamId) {
    let ed = Editor::new();
    let session = match body(&ed, 1, Command::SessionNew { name: None }) {
        ResponseBody::Session { session } => session,
        other => panic!("{other:?}"),
    };
    let param = |id: u64, name: &str| {
        let want = ParamId::new(name).unwrap();
        match body(
            &ed,
            id,
            Command::ParamAdd {
                session,
                name: name.into(),
                min: -1.0,
                max: 1.0,
                default: 0.0,
                key_positions: Vec::new(),
                param: Some(want.clone()),
            },
        ) {
            ResponseBody::Param { param } => param,
            other => panic!("{other:?}"),
        }
    };
    let angle = param(2, "swing.angle");
    let length = param(3, "swing.length");
    (ed, session, angle, length)
}

/// What the model holds for `node`, as two positional slots.
fn targets(ed: &Editor, session: SessionId, node: &NodeId) -> [Option<String>; 2] {
    ed.with_model(session, |model| match model.node(node).map(|n| &n.kind) {
        Some(catchlight_core::ModelNodeKind::SimplePhysics(ph)) => {
            let t = ph.target_params();
            [
                t[0].as_ref().map(|p| p.to_string()),
                t[1].as_ref().map(|p| p.to_string()),
            ]
        }
        other => panic!("not a physics node: {other:?}"),
    })
    .unwrap()
}

fn add(ed: &Editor, id: u64, session: SessionId, params: Vec<Option<ParamId>>) -> NodeId {
    match body(
        ed,
        id,
        Command::PhysicsAdd {
            session,
            parent: NodeId::new("root").unwrap(),
            name: None,
            kind: "rigid".into(),
            target_params: params,
            length: None,
            gravity: None,
            frequency: None,
            angle_damping: None,
            length_damping: None,
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    }
}

/// The case the old shape could not express: length driven, angle not.
#[test]
fn a_driver_can_bind_its_second_output_and_not_its_first() {
    let (ed, session, angle, length) = fixture();
    let node = add(&ed, 4, session, vec![None, Some(length.clone())]);

    assert_eq!(
        targets(&ed, session, &node),
        [None, Some("swing.length".into())],
    );

    // And `physics_set` says the same thing the same way, so the two commands
    // do not need two mental models.
    body(
        &ed,
        5,
        Command::PhysicsSet {
            session,
            node: node.clone(),
            kind: None,
            map_mode: None,
            local_only: None,
            target_params: Some(vec![Some(angle.clone()), None]),
            clear_target_params: false,
            gravity: None,
            length: None,
            frequency: None,
            angle_damping: None,
            length_damping: None,
            output_scale: None,
        },
    );
    assert_eq!(
        targets(&ed, session, &node),
        [Some("swing.angle".into()), None],
    );
}

/// Position is what names the output, so a list is read left to right and
/// whatever it does not reach stays unbound.
#[test]
fn a_short_list_leaves_the_outputs_past_its_end_unbound() {
    let (ed, session, angle, length) = fixture();
    let node = add(&ed, 4, session, vec![Some(angle.clone())]);
    assert_eq!(
        targets(&ed, session, &node),
        [Some("swing.angle".into()), None]
    );

    body(
        &ed,
        5,
        Command::PhysicsSet {
            session,
            node: node.clone(),
            kind: None,
            map_mode: None,
            local_only: None,
            target_params: Some(vec![Some(angle), Some(length)]),
            clear_target_params: false,
            gravity: None,
            length: None,
            frequency: None,
            angle_damping: None,
            length_damping: None,
            output_scale: None,
        },
    );
    assert_eq!(
        targets(&ed, session, &node),
        [Some("swing.angle".into()), Some("swing.length".into())],
    );

    // An empty list is a driver bound to nothing, the same state
    // `clear_target_params` reaches.
    body(
        &ed,
        6,
        Command::PhysicsSet {
            session,
            node: node.clone(),
            kind: None,
            map_mode: None,
            local_only: None,
            target_params: Some(Vec::new()),
            clear_target_params: false,
            gravity: None,
            length: None,
            frequency: None,
            angle_damping: None,
            length_damping: None,
            output_scale: None,
        },
    );
    assert_eq!(targets(&ed, session, &node), [None, None]);
}

/// A hole is not an escape from validation: an Id that names no param is
/// still refused, and a third output still does not exist.
#[test]
fn a_hole_does_not_excuse_an_unknown_param_or_a_third_output() {
    let (ed, session, angle, length) = fixture();
    let node = add(&ed, 4, session, vec![Some(angle.clone()), Some(length)]);
    let before = targets(&ed, session, &node);

    let set = |params: Vec<Option<ParamId>>| Command::PhysicsSet {
        session,
        node: node.clone(),
        kind: None,
        map_mode: None,
        local_only: None,
        target_params: Some(params),
        clear_target_params: false,
        gravity: None,
        length: None,
        frequency: None,
        angle_damping: None,
        length_damping: None,
        output_scale: None,
    };

    assert!(matches!(
        reply(&ed, 5, set(vec![None, Some(ParamId::new("nope").unwrap())])),
        Reply::Err {
            code: ErrorCode::NoParam,
            ..
        }
    ));
    assert!(matches!(
        reply(&ed, 6, set(vec![None, None, Some(angle)])),
        Reply::Err {
            code: ErrorCode::BadTarget,
            ..
        }
    ));
    assert_eq!(
        targets(&ed, session, &node),
        before,
        "a refused set left the driver alone",
    );
}

/// `clear_target_params` still wins over a list, holes and all.
#[test]
fn clearing_still_beats_whatever_the_list_says() {
    let (ed, session, angle, length) = fixture();
    let node = add(&ed, 4, session, vec![Some(angle), Some(length.clone())]);

    body(
        &ed,
        5,
        Command::PhysicsSet {
            session,
            node: node.clone(),
            kind: None,
            map_mode: None,
            local_only: None,
            target_params: Some(vec![None, Some(length)]),
            clear_target_params: true,
            gravity: None,
            length: None,
            frequency: None,
            angle_damping: None,
            length_damping: None,
            output_scale: None,
        },
    );

    assert_eq!(targets(&ed, session, &node), [None, None]);
}
