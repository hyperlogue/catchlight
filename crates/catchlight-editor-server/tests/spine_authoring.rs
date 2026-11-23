#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Authoring a spine through the protocol.

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, NodeInfo, NodeKind, ParamId, Reply, Request, ResponseBody,
    SessionId, SpineInfo,
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
    match body(
        ed,
        1,
        Command::SessionNew {
            source: None,
            name: None,
        },
    ) {
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
            param: Some(ParamId::new(name).unwrap()),
        },
    ) {
        ResponseBody::Param { param } => param,
        other => panic!("{other:?}"),
    }
}

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
            node: at,
        },
    )
}

fn children(ed: &Editor, session: SessionId) -> Vec<String> {
    match body(ed, 9_200, Command::NodeTree { session }) {
        ResponseBody::Tree { root } => root.children.into_iter().map(|c| c.name).collect(),
        other => panic!("{other:?}"),
    }
}

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
    assert!(read.motion_response.is_none());
    let spine = read.spine.expect("a spine reports its settings");
    assert_eq!(
        spine,
        SpineInfo {
            joints: joints.clone(),
            targets: vec![Some(bend), None],
        }
    );

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

/// The file reader's own rules, checked here so a command never authors a
/// model the file would refuse.
#[test]
fn a_spine_the_file_would_refuse_is_refused_here() {
    let ed = Editor::new();
    let session = session(&ed);

    for joints in [
        Vec::new(),
        vec![[0.0, f32::NAN]],
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
            deform_mapping: None,
            if_rev: None,
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
        },
    ) {
        ResponseBody::SpineFit { node, .. } => node,
        other => panic!("{other:?}"),
    };

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

    let (made, params) = match body(
        &ed,
        30,
        Command::SpineFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            node: Some(node("root/tail")),
            name: Some("Ponytail".into()),
        },
    ) {
        ResponseBody::SpineFit { node, params } => (node, params),
        other => panic!("{other:?}"),
    };
    assert_eq!(made, node("root/tail"), "a fit may name the Id it makes");
    assert_eq!(params.len(), 3);

    let read = info(&ed, session, &made);
    assert_eq!(read.kind, NodeKind::Spine);
    assert_eq!(read.name, "Ponytail");
    let spine = read.spine.expect("a spine");
    assert_eq!(spine.joints.len(), 3);
    assert_eq!(
        spine.targets,
        params.iter().cloned().map(Some).collect::<Vec<_>>()
    );

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

#[test]
fn a_refit_reuses_the_spine_and_its_params() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = node("root/hair");
    strip_part(&ed, session, &part, [0.0; 3], 0.0, [1.0, 1.0]);

    let fit = |id: u64, links: u32| match body(
        &ed,
        id,
        Command::SpineFit {
            session,
            part: part.clone(),
            links,
            axis: None,
            node: None,
            name: None,
        },
    ) {
        ResponseBody::SpineFit { node, params, .. } => (node, params),
        other => panic!("{other:?}"),
    };

    let (first, first_params) = fit(30, 3);
    let old_part = world(&ed, session, &part);
    let old_frame = world(&ed, session, &first);
    let old_joints = info(&ed, session, &first).spine.unwrap().joints;
    assert_eq!(fit(32, 3), (first.clone(), first_params.clone()));
    let mut parent = first.clone();
    for name in ["upper", "tip"] {
        let response = node(name);
        body(
            &ed,
            40,
            Command::NodeAdd {
                session,
                parent,
                kind: catchlight_editor_protocol::NodeKindArg::MotionResponse,
                name: None,
                node: Some(response.clone()),
            },
        );
        body(
            &ed,
            41,
            Command::NodeSet {
                session,
                node: response.clone(),
                patch: catchlight_editor_protocol::NodePatch {
                    translate: Some([15., -4., 0.]),
                    scale: Some([1.2, 0.85]),
                    rotate: Some([0., 0., 0.2]),
                    ..Default::default()
                },
            },
        );
        parent = response;
    }
    body(
        &ed,
        42,
        Command::NodeReparent {
            session,
            node: part.clone(),
            to: parent.clone(),
        },
    );
    assert_eq!(
        info(&ed, session, &part).fitted_spine.as_ref(),
        Some(&first)
    );
    let expected_frame = world(&ed, session, &part) * old_part.inverse() * old_frame;
    let before = vertex_world(&ed, session, &part);
    let (again, again_params) = fit(31, 3);
    assert_eq!(again, first, "a re-fit does not nest a second spine");
    assert_eq!(again_params, first_params, "and it keeps the params");
    assert_eq!(info(&ed, session, &part).parent, Some(parent));
    let actual_frame = world(&ed, session, &first);
    let joints = info(&ed, session, &first).spine.unwrap().joints;
    let points = |joints: Vec<[f32; 2]>| {
        std::iter::once([0., 0.])
            .chain(joints)
            .map(|[x, y]| glam::Vec3::new(x, y, 0.))
    };
    for (old, new) in points(old_joints).zip(points(joints)) {
        assert!(
            expected_frame
                .transform_point3(old)
                .distance(actual_frame.transform_point3(new))
                < 1e-4
        );
    }
    for (a, b) in before.iter().zip(vertex_world(&ed, session, &part)) {
        assert!(
            a.distance(b) < 1e-4,
            "refit moved a vertex from {a:?} to {b:?}"
        );
    }
}
