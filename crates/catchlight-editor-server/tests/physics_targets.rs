#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A driver's two outputs are named, and either one may be bound to nothing.
//!
//! A SimplePhysics node writes two numbers — an angle and a length — and the
//! model has always held them as two slots either of which can be empty.
//! [`PhysicsTargets`] is the wire's word for exactly that pair: one field per
//! output, absent where nothing is bound. So binding the second output does
//! not mean binding the first, there is no third field to name, and there is
//! no shorter or longer spelling of the pair for the server to refuse at run
//! time.

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, ParamId, PhysicsTargets, Reply, Request, ResponseBody, SessionId,
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

fn add(ed: &Editor, id: u64, session: SessionId, target_params: PhysicsTargets) -> NodeId {
    match body(
        ed,
        id,
        Command::PhysicsAdd {
            session,
            parent: NodeId::new("root").unwrap(),
            name: None,
            kind: "rigid".into(),
            target_params,
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

fn set(session: SessionId, node: &NodeId, target_params: Option<PhysicsTargets>) -> Command {
    Command::PhysicsSet {
        session,
        node: node.clone(),
        kind: None,
        map_mode: None,
        local_only: None,
        target_params,
        gravity: None,
        length: None,
        frequency: None,
        angle_damping: None,
        length_damping: None,
        output_scale: None,
    }
}

/// The case a positional list could not express without a hole: length driven,
/// angle not.
#[test]
fn a_driver_can_bind_its_second_output_and_not_its_first() {
    let (ed, session, angle, length) = fixture();
    let node = add(
        &ed,
        4,
        session,
        PhysicsTargets {
            angle: None,
            length: Some(length.clone()),
        },
    );

    assert_eq!(
        targets(&ed, session, &node),
        [None, Some("swing.length".into())],
    );

    // And `physics_set` says the same thing the same way, so the two commands
    // do not need two mental models.
    body(
        &ed,
        5,
        set(
            session,
            &node,
            Some(PhysicsTargets {
                angle: Some(angle.clone()),
                length: None,
            }),
        ),
    );
    assert_eq!(
        targets(&ed, session, &node),
        [Some("swing.angle".into()), None],
    );
}

/// Present means "both outputs are exactly this", so a set names the whole
/// pair every time and the empty pair detaches both.
#[test]
fn a_set_names_the_whole_pair_and_the_empty_pair_detaches_both() {
    let (ed, session, angle, length) = fixture();
    let node = add(
        &ed,
        4,
        session,
        PhysicsTargets {
            angle: Some(angle.clone()),
            length: None,
        },
    );
    assert_eq!(
        targets(&ed, session, &node),
        [Some("swing.angle".into()), None]
    );

    body(
        &ed,
        5,
        set(
            session,
            &node,
            Some(PhysicsTargets {
                angle: Some(angle),
                length: Some(length),
            }),
        ),
    );
    assert_eq!(
        targets(&ed, session, &node),
        [Some("swing.angle".into()), Some("swing.length".into())],
    );

    // Absent leaves the driver alone; the empty pair is the one spelling of
    // "neither output drives anything".
    body(&ed, 6, set(session, &node, None));
    assert_eq!(
        targets(&ed, session, &node),
        [Some("swing.angle".into()), Some("swing.length".into())],
    );

    body(&ed, 7, set(session, &node, Some(PhysicsTargets::default())));
    assert_eq!(targets(&ed, session, &node), [None, None]);
}

/// A field left absent is not an escape from validation: an Id that names no
/// param is still refused, and a refused set leaves the driver alone.
#[test]
fn an_unbound_output_does_not_excuse_an_unknown_param() {
    let (ed, session, angle, length) = fixture();
    let node = add(
        &ed,
        4,
        session,
        PhysicsTargets {
            angle: Some(angle),
            length: Some(length),
        },
    );
    let before = targets(&ed, session, &node);

    assert!(matches!(
        reply(
            &ed,
            5,
            set(
                session,
                &node,
                Some(PhysicsTargets {
                    angle: None,
                    length: Some(ParamId::new("nope").unwrap()),
                }),
            ),
        ),
        Reply::Err {
            code: ErrorCode::NoParam,
            ..
        }
    ));
    assert_eq!(
        targets(&ed, session, &node),
        before,
        "a refused set left the driver alone",
    );
}

/// What the JSON looks like: one object keyed by output name, and a third
/// output refused before the request is even a command.
#[test]
fn the_pair_travels_as_an_object_keyed_by_output() {
    let (ed, session, _angle, length) = fixture();
    let node = add(&ed, 4, session, PhysicsTargets::default());

    let line = serde_json::to_string(&Request {
        id: 5,
        command: set(
            session,
            &node,
            Some(PhysicsTargets {
                angle: None,
                length: Some(length),
            }),
        ),
    })
    .unwrap();
    assert!(
        line.contains(r#""target_params":{"length":"swing.length"}"#),
        "{line}",
    );

    // Round trips, and applies.
    let back: Request = serde_json::from_str(&line).unwrap();
    body(&ed, back.id, back.command);
    assert_eq!(
        targets(&ed, session, &node),
        [None, Some("swing.length".into())],
    );

    // A third output is refused where it is now impossible rather than
    // improbable: serde will not read a struct of two fields out of a longer
    // sequence, so the request never reaches the editor to be checked.
    let three = line.replace(
        r#""target_params":{"length":"swing.length"}"#,
        r#""target_params":["a","swing.length","c"]"#,
    );
    assert!(serde_json::from_str::<Request>(&three).is_err(), "{three}");
    assert!(
        serde_json::from_str::<Request>(&line.replace(
            r#""target_params":{"length":"swing.length"}"#,
            r#""target_params":{"angle":"a","length":"b","third":"c"}"#,
        ))
        .is_ok(),
        "an unknown member is ignored, as everywhere else on this wire",
    );
}
