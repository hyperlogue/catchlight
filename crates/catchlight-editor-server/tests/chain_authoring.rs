#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Authoring a particle chain through the protocol, and fitting one to art.
//!
//! Three commands and one reply field. `chain_add` and `chain_set` are the
//! plain half: a chain is links, a gravity, a locality and one param per link,
//! and `node_info` reads every one of them back under the name `chain_set`
//! takes it under. `chain_fit` is the whole rig in one edit — the params, the
//! chain, and the deform bindings that bend the art — and what it authors is
//! ordinary enough that the tests grade it through the same reads a client
//! has: `param_list`, `binding_list`, `node_info`.
//!
//! Every test drives [`Editor::handle`] in process. Nothing here needs a
//! transport, a texture or a GPU: the strip these chains hang on is a mesh set
//! by hand, so the numbers a fit produces are numbers this file can predict.

use catchlight_core::physics::ChainLink;
use catchlight_core::ModelParticleChain;
use catchlight_editor_protocol::{
    BindingTarget, ChainInfo, ChainLinkArg, Command, ErrorCode, Interpolate, NodeId, NodeInfo,
    NodeKind, NodeKindArg, ParamId, ParamInfo, Reply, Request, ResponseBody, SessionId,
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

fn params(ed: &Editor, session: SessionId) -> Vec<ParamInfo> {
    match body(ed, 9_100, Command::ParamList { session }) {
        ResponseBody::Params { params } => params,
        other => panic!("{other:?}"),
    }
}

/// What hangs directly under the root, which is how a refused fit is shown to
/// have added no chain.
fn children(ed: &Editor, session: SessionId) -> Vec<String> {
    match body(ed, 9_200, Command::NodeTree { session }) {
        ResponseBody::Tree { root } => root.children.into_iter().map(|c| c.name).collect(),
        other => panic!("{other:?}"),
    }
}

/// A tall strip part: 20 wide, 100 tall, hanging from y = 60 down to y = -40,
/// with a mesh origin that is deliberately not the vertex origin so the
/// placement rule has something to correct for.
///
/// Two columns and four rows, which is enough mesh for a fit to measure and
/// few enough vertices for a keyform to be read by eye.
fn strip(ed: &Editor, session: SessionId, name: &str, parent: &NodeId) -> NodeId {
    let part = match body(
        ed,
        100,
        Command::NodeAdd {
            session,
            parent: parent.clone(),
            kind: NodeKindArg::Part,
            name: Some(name.into()),
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };

    let mut verts = Vec::new();
    let mut uvs = Vec::new();
    for row in 0..4 {
        let y = 60.0 - 100.0 * (row as f32) / 3.0;
        let v = (row as f32) / 3.0;
        verts.push([-10.0, y]);
        verts.push([10.0, y]);
        uvs.push([0.0, v]);
        uvs.push([1.0, v]);
    }
    let mut indices = Vec::new();
    for row in 0..3u32 {
        let a = row * 2;
        indices.push([a, a + 1, a + 2]);
        indices.push([a + 1, a + 3, a + 2]);
    }
    body(
        ed,
        101,
        Command::MeshSet {
            session,
            node: part.clone(),
            verts,
            uvs,
            indices,
            origin: [5.0, -5.0],
        },
    );
    body(
        ed,
        102,
        Command::NodeSet {
            session,
            node: part.clone(),
            patch: catchlight_editor_protocol::NodePatch {
                translate: Some([100.0, 20.0, 0.0]),
                ..Default::default()
            },
        },
    );
    part
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

// -------------------------------------------------------------- chain_add

/// Every field `chain_add` takes comes back out of `node_info` under the name
/// `chain_set` would set it by, which is what makes the reply something a
/// client can edit and send back.
#[test]
fn a_chain_reads_back_as_what_was_added() {
    let ed = Editor::new();
    let session = session(&ed);
    let swing = match body(
        &ed,
        2,
        Command::ParamAdd {
            session,
            name: "swing".into(),
            min: -1.0,
            max: 1.0,
            default: 0.0,
            key_positions: Vec::new(),
            param: Some(ParamId::new("swing").unwrap()),
        },
    ) {
        ResponseBody::Param { param } => param,
        other => panic!("{other:?}"),
    };

    let made = match body(
        &ed,
        3,
        Command::ChainAdd {
            session,
            parent: node("root"),
            name: Some("Ponytail".into()),
            links: vec![
                ChainLinkArg {
                    length: Some(40.0),
                    gravity_scale: Some(0.5),
                    damping: Some(0.25),
                    time_scale: Some(2.0),
                    stiffness: Some(3.0),
                },
                // Every field absent: the editor's own defaults, which the
                // wire deliberately does not restate.
                ChainLinkArg::default(),
            ],
            local_only: Some(true),
            gravity: Some(4.5),
            outputs: Some(vec![Some(swing.clone()), None]),
            node: Some(node("root/tail")),
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };
    assert_eq!(made, node("root/tail"), "an add may name the Id it makes");

    let read = info(&ed, session, &made);
    assert_eq!(read.kind, NodeKind::ParticleChain);
    assert_eq!(read.name, "Ponytail");
    assert!(read.physics.is_none(), "a chain is not a pendulum");
    let chain = read.chain.expect("a chain reports its settings");
    assert_eq!(
        chain,
        ChainInfo {
            local_only: true,
            gravity: 4.5,
            links: vec![
                ChainLinkArg {
                    length: Some(40.0),
                    gravity_scale: Some(0.5),
                    damping: Some(0.25),
                    time_scale: Some(2.0),
                    stiffness: Some(3.0),
                },
                // The defaults, filled in: a reply names every field, so the
                // list travels straight back through `chain_set`. Compared
                // against the core's own default rather than its numbers, so
                // a retune there is not a failure here.
                ChainLinkArg::of(&ChainLink::default()),
            ],
            outputs: vec![Some(swing), None],
        },
    );

    // And it does travel back: the whole read, unchanged, is a legal set.
    body(
        &ed,
        4,
        Command::ChainSet {
            session,
            node: made.clone(),
            links: Some(chain.links.clone()),
            local_only: Some(chain.local_only),
            gravity: Some(chain.gravity),
            outputs: Some(chain.outputs.clone()),
        },
    );
    assert_eq!(info(&ed, session, &made).chain, Some(chain));
}

/// A knob a command leaves out is the core's own default, read off a freshly
/// constructed chain rather than restated as a number here. Gravity is the
/// one that bit: a node's gravity is a multiple of the model-level gravity,
/// which the bake folds in, so a literal copied from the wrong place is off
/// by a factor of g. This would fail if either handler ever substituted one.
#[test]
fn an_omitted_knob_is_the_cores_own_default() {
    let ed = Editor::new();
    let session = session(&ed);
    let fresh = ModelParticleChain::new(vec![ChainLink::default()]);

    let added = match body(
        &ed,
        2,
        Command::ChainAdd {
            session,
            parent: node("root"),
            name: None,
            links: vec![ChainLinkArg::default()],
            local_only: None,
            gravity: None,
            outputs: None,
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };
    let chain = info(&ed, session, &added).chain.unwrap();
    assert_eq!(chain.gravity, fresh.gravity);
    assert_eq!(chain.local_only, fresh.local_only);
    assert_eq!(chain.links, vec![ChainLinkArg::of(&ChainLink::default())]);

    // A fitted chain starts from the same place; the fit sets lengths only.
    let part = strip(&ed, session, "Hair", &node("root"));
    let fitted = match body(
        &ed,
        3,
        Command::ChainFit {
            session,
            part,
            links: 2,
            axis: None,
            on: None,
            chain: None,
            node: None,
        },
    ) {
        ResponseBody::ChainFit { node, .. } => node,
        other => panic!("{other:?}"),
    };
    let chain = info(&ed, session, &fitted).chain.unwrap();
    assert_eq!(chain.gravity, fresh.gravity);
    assert_eq!(chain.local_only, fresh.local_only);
    for link in &chain.links {
        assert_eq!(link.damping, Some(ChainLink::default().damping));
        assert_eq!(link.time_scale, Some(ChainLink::default().time_scale));
        assert_eq!(link.gravity_scale, Some(ChainLink::default().gravity_scale));
        assert_eq!(link.stiffness, Some(ChainLink::default().stiffness));
    }
}

/// A chain of no links has no bend to read out and nothing to drive it.
#[test]
fn an_empty_chain_is_refused() {
    let ed = Editor::new();
    let session = session(&ed);
    assert_eq!(
        code(
            &ed,
            2,
            Command::ChainAdd {
                session,
                parent: node("root"),
                name: None,
                links: Vec::new(),
                local_only: None,
                gravity: None,
                outputs: None,
                node: None,
            },
        ),
        ErrorCode::BadTarget,
    );
}

// -------------------------------------------------------------- chain_set

/// `outputs` indexes `links`, so a list of the wrong length is refused rather
/// than padded — and the refusal is the code a client already branches on for
/// an argument that does not fit the node it names.
#[test]
fn outputs_that_do_not_match_the_links_are_refused() {
    let ed = Editor::new();
    let session = session(&ed);
    let made = match body(
        &ed,
        2,
        Command::ChainAdd {
            session,
            parent: node("root"),
            name: None,
            links: vec![ChainLinkArg::default(), ChainLinkArg::default()],
            local_only: None,
            gravity: None,
            outputs: None,
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };

    assert_eq!(
        code(
            &ed,
            3,
            Command::ChainSet {
                session,
                node: made.clone(),
                links: None,
                local_only: None,
                gravity: None,
                outputs: Some(vec![None, None, None]),
            },
        ),
        ErrorCode::BadTarget,
    );

    // A set that reshapes and re-aims at once applies the links first, so the
    // three outputs fit the three links this very command asked for.
    body(
        &ed,
        4,
        Command::ChainSet {
            session,
            node: made.clone(),
            links: Some(vec![
                ChainLinkArg::default(),
                ChainLinkArg::default(),
                ChainLinkArg::default(),
            ]),
            local_only: None,
            gravity: None,
            outputs: Some(vec![None, None, None]),
        },
    );
    let chain = info(&ed, session, &made).chain.unwrap();
    assert_eq!(chain.links.len(), 3);
    assert_eq!(chain.outputs, vec![None, None, None]);

    // A chain command aimed at something that is not a chain answers the same
    // way an output list of the wrong length does.
    assert_eq!(
        code(
            &ed,
            5,
            Command::ChainSet {
                session,
                node: node("root"),
                links: None,
                local_only: Some(true),
                gravity: None,
                outputs: None,
            },
        ),
        ErrorCode::BadTarget,
    );
}

// -------------------------------------------------------------- chain_fit

/// The whole rig, from one part and a link count: three params keyed the way a
/// bend is keyed, three cubic deform bindings on the art, and a chain hanging
/// at the top of the strip whose links divide its height.
#[test]
fn a_fit_authors_the_params_the_chain_and_the_bindings() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = strip(&ed, session, "Hair", &node("root"));

    let (chain, made, bound, replaced) = match body(
        &ed,
        200,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: None,
            chain: None,
            node: None,
        },
    ) {
        ResponseBody::ChainFit {
            node,
            params,
            bound,
            replaced,
        } => (node, params, bound, replaced),
        other => panic!("{other:?}"),
    };
    assert_eq!(made.len(), 3);
    assert_eq!(bound, part, "absent `on` binds the part itself");
    assert!(replaced.is_empty(), "nothing stood here before");

    // Three params, named after the part, keyed at the seven bends a link's
    // deform is authored at.
    let listed = params(&ed, session);
    assert_eq!(listed.len(), 3);
    for (i, id) in made.iter().enumerate() {
        let info = listed
            .iter()
            .find(|p| &p.id == id)
            .expect("the reply names params the model holds");
        assert_eq!(info.name, format!("Hair bend {}", i + 1));
        assert!(close(info.min, -0.5) && close(info.max, 0.5));
        assert!(close(info.default, 0.0));
        assert_eq!(
            info.key_positions.len(),
            7,
            "a quarter turn each way, in thirds: {:?}",
            info.key_positions,
        );
        // Normalised into 0..1 across the range, ascending, centred on 0.5 —
        // which is the bend of zero the param rests at.
        assert!(close(info.key_positions[0], 0.0));
        assert!(close(info.key_positions[3], 0.5));
        assert!(close(info.key_positions[6], 1.0));
        assert_eq!(info.bindings, 1);
    }

    // The chain hangs beside the part, under the part's own parent.
    let read = info(&ed, session, &chain);
    assert_eq!(read.kind, NodeKind::ParticleChain);
    assert_eq!(read.name, "Hair chain");
    assert_eq!(read.parent.as_ref(), Some(&node("root")));
    // `part.translation + (root - origin)`: the strip's top edge is at y = 60
    // in vertex space, its centre at x = 0, and its mesh origin is (5, -5).
    assert!(close(read.translate[0], 95.0), "{:?}", read.translate);
    assert!(close(read.translate[1], 85.0), "{:?}", read.translate);

    let held = read.chain.expect("a chain reports its settings");
    assert_eq!(
        held.outputs,
        made.iter().cloned().map(Some).collect::<Vec<_>>()
    );
    assert_eq!(held.links.len(), 3);
    let total: f32 = held.links.iter().map(|l| l.length.unwrap()).sum();
    assert!(close(total, 100.0), "the links divide the strip: {total}");
    for link in &held.links {
        assert!(close(link.length.unwrap(), 100.0 / 3.0));
        // Untouched by the fit, which sets a length and nothing else.
        assert!(close(link.damping.unwrap(), 0.5));
        assert!(close(link.time_scale.unwrap(), 1.0));
    }

    // One cubic deform binding per param, on the part, with every cell of its
    // seven authored.
    let bindings = match body(
        &ed,
        201,
        Command::BindingList {
            session,
            node: part.clone(),
        },
    ) {
        ResponseBody::Bindings { bindings } => bindings,
        other => panic!("{other:?}"),
    };
    assert_eq!(bindings.len(), 3);
    for (i, binding) in bindings.iter().enumerate() {
        assert_eq!(binding.target, BindingTarget::Deform);
        assert_eq!(binding.param, made[i]);
        assert_eq!(binding.param_y, None);
        assert_eq!(binding.interpolate, Interpolate::Cubic);
        assert_eq!((binding.width, binding.height), (7, 1));
        assert!(
            binding.authored[0].iter().all(|set| *set),
            "every key position carries a keyform: {:?}",
            binding.authored,
        );
    }
}

/// The keyforms are the thing a fit is for, so one of them is checked against
/// the geometry rather than against its own count.
#[test]
fn a_bend_swings_the_art_below_its_joint_and_leaves_the_rest() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = strip(&ed, session, "Hair", &node("root"));
    let made = match body(
        &ed,
        200,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: None,
            chain: None,
            node: None,
        },
    ) {
        ResponseBody::ChainFit { params, .. } => params,
        other => panic!("{other:?}"),
    };

    // The last link's joint sits two thirds of the way down the strip, so the
    // top row of vertices cannot move under it and the bottom row must.
    let offsets = ed
        .with_model(session, |model| {
            let key = catchlight_core::BindingKey::new(
                made[2].clone(),
                part.clone(),
                catchlight_core::BindingTarget::Deform,
            );
            let binding = model.binding(&key).expect("the fit authored this");
            let cells = catchlight_core::deform_cells(binding.values()).expect("a deform binding");
            // The last key position is the greatest bend, a quarter turn.
            cells
                .iter()
                .find(|c| (c.x, c.y) == (6, 0))
                .expect("the last cell")
                .value
                .clone()
        })
        .unwrap();

    assert_eq!(offsets.len(), 16, "one [dx, dy] per vertex of the strip");
    // Vertices 0..4 are the top two rows, above the last joint.
    for i in 0..4 {
        assert!(close(offsets[i * 2], 0.0) && close(offsets[i * 2 + 1], 0.0));
    }
    // The bottom row swings toward +X, which is what a positive bend means.
    assert!(offsets[12] > 1.0, "{:?}", &offsets[12..]);
    assert!(offsets[14] > 1.0, "{:?}", &offsets[12..]);
}

/// Re-fitting the same chain keeps its params and says which bindings it
/// rewrote, because a rigger who had hand-edited one has just lost that edit.
#[test]
fn a_refit_reuses_the_params_and_names_what_it_replaced() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = strip(&ed, session, "Hair", &node("root"));
    let (chain, first) = match body(
        &ed,
        200,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: None,
            chain: None,
            node: None,
        },
    ) {
        ResponseBody::ChainFit { node, params, .. } => (node, params),
        other => panic!("{other:?}"),
    };

    let (again, second, replaced) = match body(
        &ed,
        201,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: None,
            chain: Some(chain.clone()),
            node: None,
        },
    ) {
        ResponseBody::ChainFit {
            node,
            params,
            replaced,
            ..
        } => (node, params, replaced),
        other => panic!("{other:?}"),
    };
    assert_eq!(again, chain, "the same chain, not another one");
    assert_eq!(second, first, "its params were kept");
    assert_eq!(replaced, first, "and every binding under them rewritten");
    assert_eq!(params(&ed, session).len(), 3, "no second set of params");

    // A longer chain keeps the params it had and mints the rest.
    let (_, longer, replaced) = match body(
        &ed,
        202,
        Command::ChainFit {
            session,
            part,
            links: 5,
            axis: None,
            on: None,
            chain: Some(chain.clone()),
            node: None,
        },
    ) {
        ResponseBody::ChainFit {
            node,
            params,
            replaced,
            ..
        } => (node, params, replaced),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        longer[..3],
        first[..],
        "the first three are the same params"
    );
    assert_eq!(longer.len(), 5);
    assert_eq!(replaced, first, "only the three that had a binding");
    let held = info(&ed, session, &chain).chain.unwrap();
    assert_eq!(held.links.len(), 5);
    let total: f32 = held.links.iter().map(|l| l.length.unwrap()).sum();
    assert!(close(total, 100.0), "five links still divide the strip");
}

/// A re-fit measures a strand and sets each link's length from it. Every
/// other knob is what a rigger tuned by hand, so a re-fit leaves it alone —
/// the bend spring included, which is the one a re-fit would be most annoying
/// to lose.
#[test]
fn a_refit_keeps_the_feel_a_rigger_tuned() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = strip(&ed, session, "Hair", &node("root"));
    let chain = match body(
        &ed,
        200,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: None,
            chain: None,
            node: None,
        },
    ) {
        ResponseBody::ChainFit { node, .. } => node,
        other => panic!("{other:?}"),
    };

    // Tune the strand: stiffer toward the root, and a slower clock all over.
    let mut links = info(&ed, session, &chain).chain.unwrap().links;
    for (i, link) in links.iter_mut().enumerate() {
        link.stiffness = Some(6.0 - i as f32);
        link.time_scale = Some(0.5);
    }
    body(
        &ed,
        201,
        Command::ChainSet {
            session,
            node: chain.clone(),
            links: Some(links),
            local_only: None,
            gravity: None,
            outputs: None,
        },
    );

    // Re-fit at a different link count: the lengths are the fit's, everything
    // else is the rigger's, and a link past the old end starts from the
    // core's defaults rather than from nothing.
    body(
        &ed,
        202,
        Command::ChainFit {
            session,
            part,
            links: 4,
            axis: None,
            on: None,
            chain: Some(chain.clone()),
            node: None,
        },
    );
    let held = info(&ed, session, &chain).chain.unwrap();
    assert_eq!(held.links.len(), 4);
    let total: f32 = held.links.iter().map(|l| l.length.unwrap()).sum();
    assert!(close(total, 100.0), "four links divide the strip: {total}");
    for (i, link) in held.links.iter().take(3).enumerate() {
        assert!(
            close(link.stiffness.unwrap(), 6.0 - i as f32),
            "link {i} kept its spring: {:?}",
            link.stiffness,
        );
        assert!(close(link.time_scale.unwrap(), 0.5));
    }
    assert_eq!(
        held.links[3].stiffness,
        Some(ChainLink::default().stiffness),
        "the link the re-fit added is a default one",
    );
}

/// The strip's geometry again, on a mesh group placed so the art lands in
/// exactly the same world position — the case `on` exists for.
///
/// A vertex draws at `translation + (v - origin)`, so a group at (130, 40)
/// with a mesh origin at the vertex origin holds the same art the part at
/// (100, 20) with origin (5, -5) does, once every vertex moves by (-35, -15).
fn twin_group(ed: &Editor, session: SessionId, at: [f32; 3]) -> NodeId {
    let group = match body(
        ed,
        140,
        Command::NodeAdd {
            session,
            parent: node("root"),
            kind: NodeKindArg::MeshGroup,
            name: Some("Twin".into()),
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };

    let mut verts = Vec::new();
    let mut uvs = Vec::new();
    for row in 0..4 {
        let y = 60.0 - 100.0 * (row as f32) / 3.0;
        let v = (row as f32) / 3.0;
        verts.push([-10.0 - 35.0, y - 15.0]);
        verts.push([10.0 - 35.0, y - 15.0]);
        uvs.push([0.0, v]);
        uvs.push([1.0, v]);
    }
    let mut indices = Vec::new();
    for row in 0..3u32 {
        let a = row * 2;
        indices.push([a, a + 1, a + 2]);
        indices.push([a + 1, a + 3, a + 2]);
    }
    body(
        ed,
        141,
        Command::MeshSet {
            session,
            node: group.clone(),
            verts,
            uvs,
            indices,
            origin: [0.0, 0.0],
        },
    );
    body(
        ed,
        142,
        Command::NodeSet {
            session,
            node: group.clone(),
            patch: catchlight_editor_protocol::NodePatch {
                translate: Some(at),
                ..Default::default()
            },
        },
    );
    group
}

/// Every deform cell one param authored on one node, in cell order.
fn cells(ed: &Editor, session: SessionId, param: &ParamId, on: &NodeId) -> Vec<Vec<f32>> {
    ed.with_model(session, |model| {
        let key = catchlight_core::BindingKey::new(
            param.clone(),
            on.clone(),
            catchlight_core::BindingTarget::Deform,
        );
        let binding = model.binding(&key).expect("the fit authored this");
        let mut out: Vec<_> = catchlight_core::deform_cells(binding.values())
            .expect("a deform binding")
            .iter()
            .map(|c| (c.x, c.y, c.value.clone()))
            .collect();
        out.sort_by_key(|(x, y, _)| (*y, *x));
        out.into_iter().map(|(_, _, value)| value).collect()
    })
    .unwrap()
}

/// Binding somewhere other than the part is allowed exactly where the two
/// rest frames differ by a translation, and the shift is corrected for: the
/// same art in the same world place gets the same keyforms either way.
#[test]
fn a_fit_bound_on_another_node_authors_the_same_deform() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = strip(&ed, session, "Hair", &node("root"));
    let twin = twin_group(&ed, session, [130.0, 40.0, 0.0]);

    let on_part = match body(
        &ed,
        200,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: None,
            chain: None,
            node: None,
        },
    ) {
        ResponseBody::ChainFit { params, .. } => params,
        other => panic!("{other:?}"),
    };
    let (bound, on_twin) = match body(
        &ed,
        201,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: Some(twin.clone()),
            chain: None,
            node: None,
        },
    ) {
        ResponseBody::ChainFit { bound, params, .. } => (bound, params),
        other => panic!("{other:?}"),
    };
    assert_eq!(bound, twin, "the reply names the node it bound");
    assert!(
        on_part.iter().zip(&on_twin).all(|(a, b)| a != b),
        "a second fit made its own params",
    );

    for (a, b) in on_part.iter().zip(&on_twin) {
        assert_eq!(
            cells(&ed, session, a, &part),
            cells(&ed, session, b, &twin),
            "the same art in the same place bends the same way",
        );
    }
}

/// A rotation between the two frames is not a translation, and offsets
/// measured in one frame would quietly move art in the other. So it is
/// refused, naming the node that carries it, rather than guessed at.
#[test]
fn a_fit_bound_across_a_rotation_is_refused() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = strip(&ed, session, "Hair", &node("root"));
    let twin = twin_group(&ed, session, [130.0, 40.0, 0.0]);
    body(
        &ed,
        150,
        Command::NodeSet {
            session,
            node: twin.clone(),
            patch: catchlight_editor_protocol::NodePatch {
                rotate: Some([0.0, 0.0, 0.5]),
                ..Default::default()
            },
        },
    );

    match reply(
        &ed,
        200,
        Command::ChainFit {
            session,
            part: part.clone(),
            links: 3,
            axis: None,
            on: Some(twin.clone()),
            chain: None,
            node: None,
        },
    ) {
        Reply::Err { code, message, .. } => {
            assert_eq!(code, ErrorCode::BadTarget);
            assert!(message.contains(&twin.to_string()), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(params(&ed, session).is_empty(), "and nothing was authored");

    // The part's own rotation is never looked at: a fit that binds where it
    // measured has no second frame to reconcile.
    body(
        &ed,
        201,
        Command::NodeSet {
            session,
            node: part.clone(),
            patch: catchlight_editor_protocol::NodePatch {
                rotate: Some([0.0, 0.0, 0.5]),
                ..Default::default()
            },
        },
    );
    body(
        &ed,
        202,
        Command::ChainFit {
            session,
            part,
            links: 3,
            axis: None,
            on: None,
            chain: None,
            node: None,
        },
    );
    assert_eq!(params(&ed, session).len(), 3);
}

/// A deform binding's cells are offsets into a mesh, so the node they go on
/// has to have one.
#[test]
fn binding_on_a_node_with_no_mesh_is_refused() {
    let ed = Editor::new();
    let session = session(&ed);
    let part = strip(&ed, session, "Hair", &node("root"));
    let group = match body(
        &ed,
        150,
        Command::NodeAdd {
            session,
            parent: node("root"),
            kind: NodeKindArg::Group,
            name: Some("Head".into()),
            node: None,
        },
    ) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };

    assert_eq!(
        code(
            &ed,
            200,
            Command::ChainFit {
                session,
                part: part.clone(),
                links: 3,
                axis: None,
                on: Some(group),
                chain: None,
                node: None,
            },
        ),
        ErrorCode::BadTarget,
    );
    // The refused fit left nothing behind: no params, and no chain beside
    // the two nodes the fixture made.
    assert!(params(&ed, session).is_empty());
    assert_eq!(children(&ed, session).len(), 2);

    // Zero links is refused the same way, and for the same reason a chain of
    // no links is.
    assert_eq!(
        code(
            &ed,
            201,
            Command::ChainFit {
                session,
                part,
                links: 0,
                axis: None,
                on: None,
                chain: None,
                node: None,
            },
        ),
        ErrorCode::BadTarget,
    );
}
