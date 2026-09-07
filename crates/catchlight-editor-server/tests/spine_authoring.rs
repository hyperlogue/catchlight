#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Authoring a spine through the protocol.
//!
//! Two commands and one reply field. A spine is a polyline of joints and one
//! bend param per link, and `node_info` reads both back under the names
//! `spine_set` takes them under — so the whole read travels straight back as a
//! legal set. The refusals are the file reader's, restated at the door: a
//! spine with no joints, a joint that is not a number, a joint repeating the
//! point above it, and a target list of the wrong length.
//!
//! Every test drives [`Editor::handle`] in process. Nothing here needs a
//! transport, a texture or a GPU.

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, NodeInfo, NodeKind, ParamId, Reply, Request, ResponseBody,
    SessionId, SpineInfo,
};
use catchlight_editor_server::Editor;

// ------------------------------------------------------------------ harness

fn reply(ed: &Editor, id: u64, command: Command) -> Reply {
    ed.handle(Request { id, command })
}

fn body(ed: &Editor, id: u64, command: Command) -> ResponseBody {
    match reply(ed, id, command) {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn code(ed: &Editor, id: u64, command: Command) -> ErrorCode {
    match reply(ed, id, command) {
        Reply::Err { code, .. } => code,
        other => panic!("expected Err, got {other:?}"),
    }
}

fn node(id: &str) -> NodeId {
    NodeId::new(id).unwrap()
}

fn session(ed: &Editor) -> SessionId {
    match body(ed, 1, Command::SessionNew { name: None }) {
        ResponseBody::Session { session } => session,
        other => panic!("{other:?}"),
    }
}

fn info(ed: &Editor, session: SessionId, at: &NodeId) -> NodeInfo {
    match body(
        ed,
        9_000 + at.to_string().len() as u64,
        Command::NodeInfo {
            session,
            node: at.clone(),
        },
    ) {
        ResponseBody::NodeInfo { node } => *node,
        other => panic!("{other:?}"),
    }
}

fn param(ed: &Editor, session: SessionId, id: u64, name: &str) -> ParamId {
    match body(
        ed,
        id,
        Command::ParamAdd {
            session,
            name: name.into(),
            min: -1.0,
            max: 1.0,
            default: 0.0,
            key_positions: Vec::new(),
            param: Some(ParamId::new(name).unwrap()),
        },
    ) {
        ResponseBody::Param { param } => param,
        other => panic!("{other:?}"),
    }
}

/// A spine of two links under the root, with `targets` aimed at whatever the
/// caller passes.
fn add(
    ed: &Editor,
    session: SessionId,
    joints: Vec<[f32; 2]>,
    targets: Option<Vec<Option<ParamId>>>,
    at: Option<NodeId>,
) -> Reply {
    reply(
        ed,
        3,
        Command::SpineAdd {
            session,
            parent: node("root"),
            joints,
            targets,
            node: at,
        },
    )
}

/// What hangs directly under the root, which is how a refused add is shown to
/// have made no node.
fn children(ed: &Editor, session: SessionId) -> Vec<String> {
    match body(ed, 9_200, Command::NodeTree { session }) {
        ResponseBody::Tree { root } => root.children.into_iter().map(|c| c.name).collect(),
        other => panic!("{other:?}"),
    }
}

// -------------------------------------------------------------- spine_add

/// Everything `spine_add` takes comes back out of `node_info` under the name
/// `spine_set` would set it by, and the whole read is a legal set.
#[test]
fn a_spine_reads_back_as_what_was_added() {
    let ed = Editor::new();
    let session = session(&ed);
    let bend = param(&ed, session, 2, "bend");

    let joints = vec![[0.0, -50.0], [8.0, -95.0]];
    let made = match add(
        &ed,
        session,
        joints.clone(),
        Some(vec![Some(bend.clone()), None]),
        Some(node("root/tail")),
    ) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };
    assert_eq!(made, node("root/tail"), "an add may name the Id it makes");

    let read = info(&ed, session, &made);
    assert_eq!(read.kind, NodeKind::Spine);
    assert!(read.chain.is_none(), "a spine is not a particle chain");
    assert!(read.physics.is_none(), "a spine is not a pendulum");
    let spine = read.spine.expect("a spine reports its settings");
    assert_eq!(
        spine,
        SpineInfo {
            joints: joints.clone(),
            targets: vec![Some(bend), None],
        }
    );

    // And it does travel back: the whole read, unchanged, is a legal set.
    body(
        &ed,
        4,
        Command::SpineSet {
            session,
            node: made.clone(),
            joints: Some(spine.joints.clone()),
            targets: Some(spine.targets.clone()),
        },
    );
    assert_eq!(info(&ed, session, &made).spine, Some(spine));
}

/// An absent `targets` binds none, and the reply still names one slot per
/// link: that length is the model's, not the command's.
#[test]
fn an_absent_target_list_leaves_every_link_rigid() {
    let ed = Editor::new();
    let session = session(&ed);
    let made = match add(&ed, session, vec![[0.0, -40.0], [0.0, -80.0]], None, None) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };
    let spine = info(&ed, session, &made).spine.expect("a spine");
    assert_eq!(spine.targets, vec![None, None]);
}

/// The three shapes a spine may not have, each refused as a `BadTarget` and
/// each leaving the tree as it was. These are the file reader's own rules,
/// checked here so a command never authors a model the file would refuse.
#[test]
fn a_spine_the_file_would_refuse_is_refused_here() {
    let ed = Editor::new();
    let session = session(&ed);

    for joints in [
        // No joints at all.
        Vec::new(),
        // A coordinate that is not a number.
        vec![[0.0, f32::NAN]],
        // A joint repeating the one above it.
        vec![[0.0, -40.0], [0.0, -40.0]],
        // The first joint on the node's own origin, which is the point above
        // it: a first link with no length.
        vec![[0.0, 0.0]],
    ] {
        let got = match add(&ed, session, joints.clone(), None, None) {
            Reply::Err { code, .. } => code,
            other => panic!("{joints:?} was accepted: {other:?}"),
        };
        assert_eq!(got, ErrorCode::BadTarget, "for joints {joints:?}");
    }
    assert!(children(&ed, session).is_empty(), "a refusal added a node");
}

/// A target list of the wrong length is refused, on the add and on the set,
/// and the spine keeps the targets it had.
#[test]
fn targets_that_do_not_match_the_links_are_refused() {
    let ed = Editor::new();
    let session = session(&ed);
    let bend = param(&ed, session, 2, "bend");

    assert_eq!(
        match add(
            &ed,
            session,
            vec![[0.0, -40.0], [0.0, -80.0]],
            Some(vec![Some(bend.clone())]),
            None,
        ) {
            Reply::Err { code, .. } => code,
            other => panic!("{other:?}"),
        },
        ErrorCode::BadTarget
    );
    assert!(children(&ed, session).is_empty(), "a refusal added a node");

    let made = match add(
        &ed,
        session,
        vec![[0.0, -40.0], [0.0, -80.0]],
        Some(vec![Some(bend.clone()), None]),
        None,
    ) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        code(
            &ed,
            5,
            Command::SpineSet {
                session,
                node: made.clone(),
                joints: None,
                targets: Some(vec![None, None, None]),
            },
        ),
        ErrorCode::BadTarget
    );
    assert_eq!(
        info(&ed, session, &made).spine.expect("a spine").targets,
        vec![Some(bend), None]
    );
}

/// `joints` and `targets` in one `spine_set` apply in that order, so a command
/// that reshapes and re-aims at once measures its targets against the length
/// it just asked for.
#[test]
fn a_set_reshapes_before_it_re_aims() {
    let ed = Editor::new();
    let session = session(&ed);
    let bend = param(&ed, session, 2, "bend");
    let made = match add(&ed, session, vec![[0.0, -40.0]], None, None) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };

    body(
        &ed,
        5,
        Command::SpineSet {
            session,
            node: made.clone(),
            joints: Some(vec![[0.0, -40.0], [0.0, -80.0], [0.0, -120.0]]),
            targets: Some(vec![Some(bend.clone()), None, Some(bend.clone())]),
        },
    );
    let spine = info(&ed, session, &made).spine.expect("a spine");
    assert_eq!(spine.joints.len(), 3);
    assert_eq!(spine.targets, vec![Some(bend.clone()), None, Some(bend)]);
}

/// A spine command aimed at a node that is not a spine is a `BadTarget`, not
/// a silent no-op.
#[test]
fn a_spine_command_refuses_another_kind() {
    let ed = Editor::new();
    let session = session(&ed);
    assert_eq!(
        code(
            &ed,
            5,
            Command::SpineSet {
                session,
                node: node("root"),
                joints: Some(vec![[0.0, -10.0]]),
                targets: None,
            },
        ),
        ErrorCode::BadTarget
    );
}
