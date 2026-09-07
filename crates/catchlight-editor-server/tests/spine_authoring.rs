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
    ChainArg, Command, ErrorCode, LinkFeelArg, NodeId, NodeInfo, NodeKind, ParamId, Reply, Request,
    ResponseBody, SessionId, SpineInfo,
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
            name: None,
            joints,
            targets,
            chain: None,
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
    assert!(read.physics.is_none(), "a spine is not a pendulum");
    let spine = read.spine.expect("a spine reports its settings");
    assert_eq!(
        spine,
        SpineInfo {
            joints: joints.clone(),
            targets: vec![Some(bend), None],
            chain: None,
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
            chain: Some(spine.chain.clone()),
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
                chain: None,
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
            chain: None,
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
                chain: None,
            },
        ),
        ErrorCode::BadTarget
    );
}

/// A chain's weight is refused at the door on every command that hangs one,
/// so a number the file would refuse to read back never reaches the model.
#[test]
fn a_chains_bad_weight_is_refused_wherever_it_is_hung() {
    let ed = Editor::new();
    let session = session(&ed);
    let bad = ChainArg {
        weight: Some(-1.0),
        ..ChainArg::default()
    };
    assert_eq!(
        code(
            &ed,
            5,
            Command::SpineAdd {
                session,
                parent: node("root"),
                name: None,
                joints: vec![[0.0, -10.0]],
                targets: None,
                chain: Some(bad.clone()),
                node: Some(node("root/sp")),
            },
        ),
        ErrorCode::BadTarget
    );
    assert!(
        children(&ed, session).is_empty(),
        "a refused add makes no node"
    );

    let made = match add(&ed, session, vec![[0.0, -10.0]], None, None) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        code(
            &ed,
            6,
            Command::SpineSet {
                session,
                node: made.clone(),
                joints: None,
                targets: None,
                chain: Some(Some(bad)),
            },
        ),
        ErrorCode::BadTarget
    );
    assert!(
        info(&ed, session, &made)
            .spine
            .expect("a spine")
            .chain
            .is_none(),
        "a refused set hangs nothing"
    );
}

/// Every knob the file reader refuses is refused at the command that named
/// it. Writing is total, so a number the model took would land in the file and
/// be refused on the next load, with the command long gone; a script wants to
/// hear about it now, and on every door a chain comes through.
#[test]
fn every_chain_knob_the_file_refuses_is_refused_at_the_door() {
    let ed = Editor::new();
    let session = session(&ed);
    let with_feel = |feel: LinkFeelArg| ChainArg {
        links: Some(vec![feel]),
        ..ChainArg::default()
    };
    let bad: Vec<(&str, ChainArg)> = vec![
        (
            "gravity 0",
            ChainArg {
                gravity: Some(0.0),
                ..ChainArg::default()
            },
        ),
        (
            "gravity NaN",
            ChainArg {
                gravity: Some(f32::NAN),
                ..ChainArg::default()
            },
        ),
        (
            "weight -1",
            ChainArg {
                weight: Some(-1.0),
                ..ChainArg::default()
            },
        ),
        (
            "gravity_scale NaN",
            with_feel(LinkFeelArg {
                gravity_scale: Some(f32::NAN),
                ..Default::default()
            }),
        ),
        (
            "damping 1.5",
            with_feel(LinkFeelArg {
                damping: Some(1.5),
                ..Default::default()
            }),
        ),
        (
            "damping -0.1",
            with_feel(LinkFeelArg {
                damping: Some(-0.1),
                ..Default::default()
            }),
        ),
        (
            "stiffness -1",
            with_feel(LinkFeelArg {
                stiffness: Some(-1.0),
                ..Default::default()
            }),
        ),
        (
            "stiffness NaN",
            with_feel(LinkFeelArg {
                stiffness: Some(f32::NAN),
                ..Default::default()
            }),
        ),
        (
            "limit 0",
            with_feel(LinkFeelArg {
                limit: Some(0.0),
                ..Default::default()
            }),
        ),
    ];

    for (i, (what, chain)) in bad.iter().enumerate() {
        assert_eq!(
            code(
                &ed,
                200 + i as u64,
                Command::SpineAdd {
                    session,
                    parent: node("root"),
                    name: None,
                    joints: vec![[0.0, -10.0]],
                    targets: None,
                    chain: Some(chain.clone()),
                    node: None,
                },
            ),
            ErrorCode::BadTarget,
            "{what} on spine_add",
        );
    }
    assert!(
        children(&ed, session).is_empty(),
        "no refused add made a node"
    );

    // The same door on a set.
    let made = match add(&ed, session, vec![[0.0, -10.0]], None, None) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };
    for (i, (what, chain)) in bad.iter().enumerate() {
        assert_eq!(
            code(
                &ed,
                300 + i as u64,
                Command::SpineSet {
                    session,
                    node: made.clone(),
                    joints: None,
                    targets: None,
                    chain: Some(Some(chain.clone())),
                },
            ),
            ErrorCode::BadTarget,
            "{what} on spine_set",
        );
    }
    assert!(
        info(&ed, session, &made)
            .spine
            .expect("a spine")
            .chain
            .is_none(),
        "no refused set hung a chain"
    );
}

// -------------------------------------------------------------- spine_fit

/// A tall strip: 20 wide, 100 tall, hanging from y = 60 down to y = -40, with
/// a mesh origin that is deliberately not the vertex origin so the placement
/// arithmetic has something to correct for.
const STRIP: [[f32; 2]; 8] = [
    [-10.0, 60.0],
    [10.0, 60.0],
    [-10.0, 27.0],
    [10.0, 27.0],
    [-10.0, -7.0],
    [10.0, -7.0],
    [-10.0, -40.0],
    [10.0, -40.0],
];

/// A part carrying [`STRIP`] under the root, at the transform the caller
/// names.
fn strip_part(
    ed: &Editor,
    session: SessionId,
    at: &NodeId,
    translate: [f32; 3],
    rotate: f32,
    scale: [f32; 2],
) {
    body(
        ed,
        20,
        Command::NodeAdd {
            session,
            parent: node("root"),
            kind: catchlight_editor_protocol::NodeKindArg::Part,
            name: Some("Hair".into()),
            node: Some(at.clone()),
        },
    );
    body(
        ed,
        21,
        Command::MeshSet {
            session,
            node: at.clone(),
            verts: STRIP.to_vec(),
            uvs: vec![[0.0, 0.0]; STRIP.len()],
            indices: vec![
                [0, 1, 3],
                [0, 3, 2],
                [2, 3, 5],
                [2, 5, 4],
                [4, 5, 7],
                [4, 7, 6],
            ],
            origin: [3.0, 5.0],
        },
    );
    body(
        ed,
        22,
        Command::NodeSet {
            session,
            node: at.clone(),
            patch: catchlight_editor_protocol::NodePatch {
                translate: Some(translate),
                rotate: Some([0.0, 0.0, rotate]),
                scale: Some(scale),
                ..Default::default()
            },
        },
    );
}

/// A node's world matrix, walked up the tree through `node_info` — the same
/// composition the renderer uses, rebuilt here so the test can ask where a
/// vertex actually lands.
fn world(ed: &Editor, session: SessionId, at: &NodeId) -> glam::Mat4 {
    let mut chain: Vec<glam::Mat4> = Vec::new();
    let mut cursor = Some(at.clone());
    while let Some(id) = cursor {
        let info = info(ed, session, &id);
        chain.push(glam::Mat4::from_scale_rotation_translation(
            glam::Vec3::new(info.scale[0], info.scale[1], 1.0),
            glam::Quat::from_euler(
                glam::EulerRot::XYZ,
                info.rotate[0],
                info.rotate[1],
                info.rotate[2],
            ),
            glam::Vec3::from(info.translate),
        ));
        cursor = info.parent;
    }
    chain
        .iter()
        .rev()
        .fold(glam::Mat4::IDENTITY, |acc, m| acc * *m)
}

/// Where every rest vertex of the part draws, in world space. A renderer draws
/// a vertex at `v - origin` in the node's own frame.
fn vertex_world(ed: &Editor, session: SessionId, at: &NodeId) -> Vec<glam::Vec2> {
    let m = world(ed, session, at);
    STRIP
        .iter()
        .map(|v| {
            let p = m.transform_point3(glam::Vec3::new(v[0] - 3.0, v[1] - 5.0, 0.0));
            glam::Vec2::new(p.x, p.y)
        })
        .collect()
}

/// **The part does not move.** A fit inserts a spine between the part and its
/// parent, and that is a change to the tree and to nothing else: every rest
/// vertex draws exactly where it drew, for a part carrying a rotation and a
/// non-unit scale as much as for one at the identity.
#[test]
fn a_fit_leaves_the_parts_world_placement_alone() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = node("root/hair");
    strip_part(&ed, session, &part, [12.0, -7.0, 0.0], 0.6, [1.4, 0.75]);
    let before = vertex_world(&ed, session, &part);
    let parent_before = info(&ed, session, &part).parent;

    let made = match body(
        &ed,
        30,
        Command::SpineFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            node: None,
            name: None,
            chain: None,
        },
    ) {
        ResponseBody::SpineFit { node, .. } => node,
        other => panic!("{other:?}"),
    };

    // The tree changed: the spine took the part's place, and the part hangs
    // under it.
    assert_eq!(info(&ed, session, &made).parent, parent_before);
    assert_eq!(info(&ed, session, &part).parent, Some(made));

    let after = vertex_world(&ed, session, &part);
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert!(
            (*a - *b).length() < 1e-4,
            "vertex {i} moved from {a:?} to {b:?}"
        );
    }
}

/// What a fit authors: a spine of `links` joints reading one param each, in
/// half turns, and no binding anywhere.
#[test]
fn a_fit_authors_a_spine_its_params_and_no_bindings() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = node("root/hair");
    strip_part(&ed, session, &part, [0.0; 3], 0.0, [1.0, 1.0]);

    let (made, params, warnings) = match body(
        &ed,
        30,
        Command::SpineFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            node: Some(node("root/tail")),
            name: Some("Ponytail".into()),
            chain: Some(ChainArg::default()),
        },
    ) {
        ResponseBody::SpineFit {
            node,
            params,
            warnings,
        } => (node, params, warnings),
        other => panic!("{other:?}"),
    };
    assert_eq!(made, node("root/tail"), "a fit may name the Id it makes");
    assert_eq!(params.len(), 3);
    assert!(
        warnings.is_empty(),
        "a strand drawn along gravity rests as drawn: {warnings:?}"
    );

    let read = info(&ed, session, &made);
    assert_eq!(read.kind, NodeKind::Spine);
    assert_eq!(read.name, "Ponytail");
    let spine = read.spine.expect("a spine");
    assert_eq!(spine.joints.len(), 3);
    assert_eq!(
        spine.targets,
        params.iter().cloned().map(Some).collect::<Vec<_>>()
    );
    let chain = spine.chain.expect("the fit hung a chain");
    assert_eq!(chain.links.expect("links").len(), 3);

    // No binding was authored on the part: a spine composes its joints.
    match body(
        &ed,
        31,
        Command::BindingList {
            session,
            node: part,
        },
    ) {
        ResponseBody::Bindings { bindings } => assert!(bindings.is_empty(), "{bindings:?}"),
        other => panic!("{other:?}"),
    }
}

/// A re-fit reuses the spine the part already hangs from rather than nesting a
/// second one, keeps its params, and keeps every knob the rigger tuned on its
/// chain.
#[test]
fn a_refit_reuses_the_spine_and_keeps_the_chains_knobs() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = node("root/hair");
    strip_part(&ed, session, &part, [0.0; 3], 0.0, [1.0, 1.0]);

    let fit = |id: u64, links: u32, chain: Option<ChainArg>| match body(
        &ed,
        id,
        Command::SpineFit {
            session,
            part: part.clone(),
            links,
            axis: None,
            node: None,
            name: None,
            chain,
        },
    ) {
        ResponseBody::SpineFit { node, params, .. } => (node, params),
        other => panic!("{other:?}"),
    };

    let (first, first_params) = fit(
        30,
        3,
        Some(ChainArg {
            gravity: Some(4.5),
            weight: Some(0.25),
            links: Some(vec![LinkFeelArg {
                stiffness: Some(3.0),
                limit: Some(0.25),
                ..Default::default()
            }]),
            ..Default::default()
        }),
    );
    let (again, again_params) = fit(31, 3, None);
    assert_eq!(again, first, "a re-fit does not nest a second spine");
    assert_eq!(again_params, first_params, "and it keeps the params");

    let chain = info(&ed, session, &first)
        .spine
        .expect("a spine")
        .chain
        .expect("a chain");
    assert_eq!(chain.gravity, Some(4.5), "the knobs survive a re-fit");
    assert_eq!(chain.weight, Some(0.25));
    let links = chain.links.expect("links");
    assert_eq!(links.len(), 3);
    // The one feel the caller named was repeated onto the joints it did not.
    assert!(links.iter().all(|l| l.stiffness == Some(3.0)));
    assert!(
        links.iter().all(|l| l.limit == Some(0.25)),
        "a bend limit reads back with the rest of the feel: {links:?}",
    );
}

/// A limit is a bend in half turns, so zero would be a joint that cannot
/// move and anything past one is more than a whole turn. The editor refuses
/// both at the door rather than authoring a model the file would refuse on
/// its next load.
#[test]
fn a_limit_outside_its_range_is_refused() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = node("root/hair");
    strip_part(&ed, session, &part, [0.0; 3], 0.0, [1.0, 1.0]);

    let fit = |id: u64, limit: f32| Command::SpineFit {
        session,
        part: part.clone(),
        links: 2,
        axis: None,
        node: Some(node(&format!("root/s{id}"))),
        name: None,
        chain: Some(ChainArg {
            links: Some(vec![LinkFeelArg {
                limit: Some(limit),
                ..Default::default()
            }]),
            ..Default::default()
        }),
    };

    for (i, bad) in [0.0f32, 1.5, f32::NAN, -0.25].into_iter().enumerate() {
        let id = 80 + i as u64;
        assert_eq!(
            code(&ed, id, fit(id, bad)),
            ErrorCode::BadTarget,
            "a limit of {bad} is refused",
        );
    }

    // The widest limit that means anything is fine, and so is a tiny one.
    for (i, ok) in [1.0f32, 0.001].into_iter().enumerate() {
        let id = 90 + i as u64;
        let ResponseBody::SpineFit { node: made, .. } = body(&ed, id, fit(id, ok)) else {
            panic!("a limit of {ok} was refused");
        };
        let chain = info(&ed, session, &made)
            .spine
            .expect("a spine")
            .chain
            .expect("a chain");
        assert!(chain
            .links
            .expect("links")
            .iter()
            .all(|l| l.limit == Some(ok)));
    }
}

/// A limp weighted link drawn off gravity cannot settle where it is drawn, and
/// the fit says so rather than letting the strand fall out of its pose.
#[test]
fn a_fit_warns_about_a_link_that_cannot_rest_as_drawn() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = node("root/hair");
    // The art hangs along the node's own -Y, but the node is turned a quarter
    // turn, so in the world the strand lies sideways.
    strip_part(
        &ed,
        session,
        &part,
        [0.0; 3],
        std::f32::consts::FRAC_PI_2,
        [1.0, 1.0],
    );

    let warnings = match body(
        &ed,
        30,
        Command::SpineFit {
            session,
            part,
            links: 2,
            axis: None,
            node: None,
            name: None,
            chain: Some(ChainArg::default()),
        },
    ) {
        ResponseBody::SpineFit { warnings, .. } => warnings,
        other => panic!("{other:?}"),
    };
    assert_eq!(warnings.len(), 2, "both links are limp and sideways");
    assert!(
        warnings[0].contains("link 1") && warnings[0].contains("cannot rest as drawn"),
        "{warnings:?}"
    );
}

/// The chain is a tri-state on a set: absent leaves it, a value replaces it,
/// `null` takes it off.
#[test]
fn a_set_can_leave_replace_or_remove_the_chain() {
    let ed = Editor::new();
    let session = session(&ed);
    let made = match add(&ed, session, vec![[0.0, -40.0], [0.0, -80.0]], None, None) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };
    let chain_of = || info(&ed, session, &made).spine.expect("a spine").chain;
    assert!(chain_of().is_none(), "an add without one carries none");

    let set = |id: u64, chain: Option<Option<ChainArg>>| {
        body(
            &ed,
            id,
            Command::SpineSet {
                session,
                node: made.clone(),
                joints: None,
                targets: None,
                chain,
            },
        );
    };

    set(
        40,
        Some(Some(ChainArg {
            gravity: Some(2.0),
            ..Default::default()
        })),
    );
    assert_eq!(chain_of().expect("a chain").gravity, Some(2.0));

    set(41, None);
    assert_eq!(
        chain_of().expect("a chain").gravity,
        Some(2.0),
        "absent leaves it alone"
    );

    set(42, Some(None));
    assert!(chain_of().is_none(), "null takes it off");
}
