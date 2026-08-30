//! Headless tests of the app's command paths, driven against an in-process
//! [`Editor`] — no window, no event loop. What they pin is which command a
//! gesture or a panel action sends, and what the document looks like
//! afterwards; the drawing is `viewport::tests`' job.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn welded_seam() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clm"
    ))
    .expect("welded_seam.clm")
}

/// An app on a session over the given `.clm` bytes, with no window behind it.
fn app_on(bytes: &[u8]) -> (Arc<Editor>, SessionId, App) {
    let editor = Arc::new(Editor::new());
    let session = editor.open_bytes("model", bytes).expect("open");
    let app = App::with_session(
        editor.clone(),
        egui::Context::default(),
        session,
        "model".into(),
    );
    (editor, session, app)
}

fn param_named(editor: &Editor, session: SessionId, name: &str) -> ParamId {
    editor
        .with_model(session, |m| {
            m.param_ids()
                .iter()
                .find(|id| m.param(id).is_some_and(|p| p.name.as_str() == name))
                .cloned()
        })
        .unwrap()
        .unwrap_or_else(|| panic!("model has a {name:?} param"))
}

/// The lowest-Id meshed node. Sorted, because `node_ids` is a hash order and
/// a test that picked a different part per run would assert against a
/// different weld.
fn first_meshed_node(editor: &Editor, session: SessionId) -> NodeId {
    editor
        .with_model(session, |m| {
            let mut ids: Vec<NodeId> = m
                .node_ids()
                .filter(|id| m.node_mesh(id).is_some_and(|mesh| !mesh.verts.is_empty()))
                .cloned()
                .collect();
            ids.sort();
            ids.into_iter().next()
        })
        .unwrap()
        .expect("model has a meshed node")
}

/// The gesture split, end to end through the app: every pointer move sends a
/// scratch deform (presence — no revision, no snapshot), the release sends one
/// `DeformVertices`. A drag that snapshotted per move would bury every earlier
/// edit under a hundred indistinguishable undo entries.
#[test]
fn a_drag_of_any_length_and_its_release_leave_one_undo_entry() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    let core = app.core_of_ref(&node).expect("node is baked");
    app.armed = Some(Armed::One(param_named(&editor, session, "pull")));

    let rev_before = editor.doc_snapshot(session).expect("snapshot").rev;
    for i in 0..100u32 {
        let deltas = HashMap::from([(0usize, glam::vec2(i as f32 * 0.1, 1.0))]);
        app.set_scratch_deform(session, &node, &deltas);
    }
    assert_eq!(
        editor.history(session).unwrap(),
        (0, 0),
        "a drag must not snapshot the model",
    );
    assert_eq!(
        editor.doc_snapshot(session).expect("snapshot").rev,
        rev_before,
        "a drag must not bump the revision",
    );

    // ...and it is on the puppet, where the viewport will draw it — including
    // after the tick every render runs. (welded_seam's two parts are welded,
    // and `node-1` is the side the weld does not pull, so what the drag says
    // is what the frame shows.)
    let idx = catchlight_core::NodeIdx(core);
    let drawn = editor
        .with_puppet(session, |model, p| {
            p.tick(model, 0.0);
            p.combined_deform(idx).map(<[_]>::to_vec)
        })
        .unwrap()
        .expect("a part carries a deform");
    assert!(
        (drawn[0] - catchlight_core::Vec2::new(9.9, 1.0)).length() < 1e-4,
        "the drag must survive the render's tick, got {:?}",
        drawn[0],
    );

    let snapshot = editor.doc_snapshot(session);
    app.clear_scratch_deform();
    let deltas = HashMap::from([(0usize, glam::vec2(9.9, 1.0))]);
    app.commit_deform_deltas(session, core, &deltas, &snapshot);

    assert_eq!(
        editor.history(session).unwrap(),
        (1, 0),
        "the release is the one undo entry the gesture costs",
    );
    // One Undo takes the whole gesture back.
    app.send(Command::Undo { session });
    assert_eq!(editor.history(session).unwrap(), (0, 1));
}

/// Clearing is the other half of the presence path: the drag has to leave the
/// puppet when the gesture ends, or the next frame draws a deform nobody is
/// holding any more.
#[test]
fn clearing_the_scratch_deform_takes_it_off_the_puppet() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    let idx = catchlight_core::NodeIdx(app.core_of_ref(&node).expect("baked"));

    app.set_scratch_deform(
        session,
        &node,
        &HashMap::from([(0usize, glam::vec2(5.0, 5.0))]),
    );
    let moved = app.scratch_rev;
    app.clear_scratch_deform();
    assert!(app.scratch.is_none());
    assert_ne!(app.scratch_rev, moved, "the render signature has to move");

    let drawn = editor
        .with_puppet(session, |_model, p| {
            p.combine_deforms();
            p.combined_deform(idx).map(<[_]>::to_vec)
        })
        .unwrap()
        .expect("a part carries a deform");
    assert!(
        drawn.iter().all(|v| *v == catchlight_core::Vec2::ZERO),
        "{drawn:?}",
    );
}

/// Editing a Name is relabelling: it is free, it repeats, and it must leave
/// the Id — what the file, an addon and every binding name the node by —
/// exactly where it was.
#[test]
fn renaming_a_node_leaves_its_id_alone() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    app.selection = vec![node.clone()];

    app.apply_inspector_action(InspectorAction::Commit(NodePatch {
        name: Some("Shoulder".into()),
        ..Default::default()
    }));

    let (id_still_there, name) = editor
        .with_model(session, |m| {
            (
                m.node(&node).is_some(),
                m.node(&node).map(|n| n.name.to_string()),
            )
        })
        .unwrap();
    assert!(id_still_there, "a name edit must not move the Id");
    assert_eq!(name.as_deref(), Some("Shoulder"));

    // The same name on a second node is fine: nothing is addressed by it.
    let other = editor
        .with_model(session, |m| {
            let mut ids: Vec<NodeId> = m.node_ids().cloned().collect();
            ids.sort();
            ids.into_iter().find(|id| *id != node)
        })
        .unwrap()
        .expect("welded_seam has more than one node");
    app.selection = vec![other.clone()];
    app.apply_inspector_action(InspectorAction::Commit(NodePatch {
        name: Some("Shoulder".into()),
        ..Default::default()
    }));
    let both = editor
        .with_model(session, |m| {
            [&node, &other]
                .map(|id| m.node(id).map(|n| n.name.to_string()))
                .to_vec()
        })
        .unwrap();
    assert_eq!(both, vec![Some("Shoulder".into()), Some("Shoulder".into())]);
}

/// Renaming an Id is the deliberate one: it goes through a confirmation, it
/// answers to the new Id afterwards, and the tree — which reads Names —
/// carries it.
#[test]
fn renaming_an_id_is_confirmed_and_shows_up_in_the_tree() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    app.selection = vec![node.clone()];
    app.isolated = Some(node.clone());

    app.apply_inspector_action(InspectorAction::RenameId);
    assert!(
        app.id_rename.is_some(),
        "an Id rename must ask before it breaks anything",
    );
    // Nothing has happened yet.
    assert!(editor
        .with_model(session, |m| m.node(&node).is_some())
        .unwrap());

    let renamed = NodeId::new("shoulder").unwrap();
    app.id_rename.as_mut().expect("prompt").to = "shoulder".into();
    app.confirm_id_rename();
    assert!(app.id_rename.is_none(), "the prompt closes on success");

    let tree = editor.doc_snapshot(session).expect("snapshot");
    assert!(
        find_subtree(&tree.root, &renamed).is_some(),
        "the tree must name the node by its new Id",
    );
    assert!(
        find_subtree(&tree.root, &node).is_none(),
        "and not by the old one",
    );
    // The client-side state that named it follows, or the inspector would go
    // blank on the node the author just renamed.
    assert_eq!(app.selection, vec![renamed.clone()]);
    assert_eq!(app.isolated, Some(renamed));
}

/// A string outside the Id charset is refused where the author can see it,
/// not swallowed into a status line.
#[test]
fn an_id_outside_the_charset_keeps_the_prompt_open() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    app.begin_id_rename(RenameSubject::Node(node.clone()));
    app.id_rename.as_mut().expect("prompt").to = "not an id".into();
    app.confirm_id_rename();

    let pending = app.id_rename.as_ref().expect("the prompt stays open");
    assert!(
        pending
            .error
            .as_deref()
            .is_some_and(|e| e.contains("invalid byte")),
        "{:?}",
        pending.error,
    );
    assert!(editor
        .with_model(session, |m| m.node(&node).is_some())
        .unwrap());
}

/// Renaming a param's Id has to carry the recording state with it: the armed
/// param and the live pose both name it by an Id that no longer exists.
#[test]
fn renaming_a_param_id_carries_the_recording_state() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let pull = param_named(&editor, session, "pull");
    app.armed = Some(Armed::One(pull.clone()));
    app.pose.insert(pull.clone(), 0.75);

    app.apply_param_action(ParamAction::RenameId(pull.clone()), &None);
    app.id_rename.as_mut().expect("prompt").to = "tug".into();
    app.confirm_id_rename();

    let tug = ParamId::new("tug").unwrap();
    assert_eq!(app.armed, Some(Armed::One(tug.clone())));
    assert_eq!(app.pose.get(&tug), Some(&0.75));
    assert!(!app.pose.contains_key(&pull));
    assert!(editor
        .with_model(session, |m| m.param(&tug).is_some())
        .unwrap());
}

fn add_param(app: &mut App, session: SessionId, name: &str) -> ParamId {
    match app.send(Command::ParamAdd {
        session,
        name: name.into(),
        min: 0.0,
        max: 1.0,
        default: 0.0,
        key_positions: Vec::new(),
    }) {
        Reply::Ok {
            body: ResponseBody::Param { param },
            ..
        } => param,
        other => panic!("{other:?}"),
    }
}

/// A model whose `head.x` / `head.y` pair already drives `tx` on a node
/// through one two-param binding — the shape an inochi2d 2-D param imports as.
fn split_pair(app: &mut App, session: SessionId, node: &NodeId) -> (ParamId, ParamId) {
    let x = add_param(app, session, "head.x");
    let y = add_param(app, session, "head.y");
    app.send(Command::BindingAdd {
        session,
        params: BindingParams::two(x.clone(), y.clone()),
        node: node.clone(),
        target: "tx".into(),
    });
    (x, y)
}

/// Arming one param of an imported pair and recording must join the pair's
/// binding, filling the partner's cell from the pose. A one-param binding
/// beside the two-param one is what the v0 flatten refuses as unpairable, and
/// nothing in the GUI would tell the author why their save broke.
#[test]
fn recording_on_half_a_pair_joins_its_binding_instead_of_starting_a_rival() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    let (x, y) = split_pair(&mut app, session, &node);

    app.armed = Some(Armed::One(x.clone()));
    app.selection = vec![node.clone()];
    // Pose both: the partner's cell comes from the pose, which only the GUI
    // holds — the server has none.
    app.pose.insert(x.clone(), 1.0);
    app.pose.insert(y.clone(), 1.0);
    app.commit_patch(
        node.clone(),
        NodePatch {
            translate: Some([5.0, 0.0, 0.0]),
            ..Default::default()
        },
    );

    let (bindings, authored) = editor
        .with_model(session, |m| {
            let bindings: Vec<String> = m
                .bindings_of_node(&node)
                .filter(|b| b.target().name() == "tx")
                .map(|b| match b.params() {
                    catchlight_core::BindingParams::One(p) => format!("one({p})"),
                    catchlight_core::BindingParams::Two(a, b) => format!("two({a},{b})"),
                })
                .collect();
            let key = catchlight_core::BindingKey::pair(
                x.clone(),
                y.clone(),
                node.clone(),
                BindingTarget::Scalar(catchlight_core::ScalarTarget::Tx),
            );
            let authored: Vec<[u32; 2]> = m
                .binding(&key)
                .and_then(|b| catchlight_core::scalar_cells(b.values()))
                .map(|cells| cells.iter().map(|c| [c.x, c.y]).collect())
                .unwrap_or_default();
            (bindings, authored)
        })
        .unwrap();

    assert_eq!(
        bindings,
        vec![format!("two({x},{y})")],
        "the pair's binding is the only one driving tx",
    );
    // (a new binding authors its origin cell, so the pose's cell is the
    // second one — what matters is that it is *in the pair's grid*.)
    assert!(
        authored.contains(&[1, 1]),
        "the key lands at the pose's cell in the pair's grid, got {authored:?}",
    );
}

/// The same rule reaches a target the pair does not drive yet: a param that
/// is half of a pair anywhere in the model is half of a pair everywhere, or
/// the model stops being flattenable the moment the second key is recorded.
#[test]
fn a_paired_param_records_as_a_pair_even_on_a_target_it_does_not_drive_yet() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let mut nodes: Vec<NodeId> = editor
        .with_model(session, |m| {
            let mut ids: Vec<NodeId> = m
                .node_ids()
                .filter(|id| m.node_mesh(id).is_some())
                .cloned()
                .collect();
            ids.sort();
            ids
        })
        .unwrap();
    let elsewhere = nodes.pop().expect("two meshed nodes");
    let node = nodes.pop().expect("two meshed nodes");
    let (x, y) = split_pair(&mut app, session, &node);

    app.armed = Some(Armed::One(x.clone()));
    app.pose.insert(y.clone(), 1.0);
    let snap = editor.doc_snapshot(session).expect("snapshot");
    let (params, cell) = app
        .record_target(&snap, &elsewhere)
        .expect("a target to record into");

    assert_eq!(params, BindingParams::two(x, y));
    assert_eq!(cell, [0, 1], "the partner's cell comes from the pose");
}

/// The pad is a view over a two-param binding: arming a pair records into a
/// grid that spans both params' key positions.
#[test]
fn arming_a_pair_records_into_the_pairs_grid() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    let x = add_param(&mut app, session, "look.x");
    let y = add_param(&mut app, session, "look.y");

    app.apply_param_action(
        ParamAction::Arm(Some(Armed::Two(x.clone(), y.clone()))),
        &None,
    );
    app.selection = vec![node.clone()];
    app.pose.insert(x.clone(), 1.0);
    app.pose.insert(y.clone(), 0.0);
    app.commit_patch(
        node.clone(),
        NodePatch {
            translate: Some([0.0, 3.0, 0.0]),
            ..Default::default()
        },
    );

    let authored = editor
        .with_model(session, |m| {
            let key = catchlight_core::BindingKey::pair(
                x.clone(),
                y.clone(),
                node.clone(),
                BindingTarget::Scalar(catchlight_core::ScalarTarget::Ty),
            );
            m.binding(&key)
                .and_then(|b| catchlight_core::scalar_cells(b.values()))
                .map(|cells| cells.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .unwrap();
    assert!(authored.contains(&[1, 0]), "{authored:?}");

    // And the panel data reads that grid, not one param's row.
    let snap = editor.doc_snapshot(session).expect("snapshot");
    let info = app.armed_info(&snap).expect("armed info");
    assert_eq!(info.grid, (2, 2));
    assert_eq!(info.cell, [1, 0]);
    assert_eq!(info.cell_states.len(), 4);
    assert!(
        info.bindings
            .iter()
            .any(|r| r.cell == [1, 0] && r.authored_at_cell),
        "the row has to read its own binding's cell",
    );
}

/// "Add two" makes two ordinary scalar params and opens them on the pad —
/// there is no two-axis param for it to make.
#[test]
fn adding_two_params_makes_two_scalars_on_one_pad() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let before = editor.with_model(session, |m| m.param_ids().len()).unwrap();

    app.apply_param_action(
        ParamAction::AddParamPair {
            name: "head".into(),
        },
        &None,
    );

    let names = editor
        .with_model(session, |m| {
            m.param_ids()
                .iter()
                .filter_map(|id| m.param(id).map(|p| p.name.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(names.len(), before + 2);
    assert!(names.contains(&"head.x".to_string()), "{names:?}");
    assert!(names.contains(&"head.y".to_string()), "{names:?}");
    match app.armed.as_ref().expect("the pad opens on the new pair") {
        Armed::Two(x, y) => assert_ne!(x, y),
        other => panic!("{other:?}"),
    }
}

/// A driver writes two params, and the inspector edits both. Setting the
/// second must not clear the first: the pair travels as one list, so a panel
/// that sent only what changed would detach the other every time.
#[test]
fn the_physics_inspector_aims_a_driver_at_both_of_its_params() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let root = editor
        .with_model(session, |m| m.root().cloned())
        .unwrap()
        .expect("a root");
    let angle = add_param(&mut app, session, "swing.angle");
    let length = add_param(&mut app, session, "swing.length");
    let node = match app.send(Command::PhysicsAdd {
        session,
        parent: root,
        name: Some("Pendulum".into()),
        kind: "rigid".into(),
        target_params: Vec::new(),
        length: None,
        gravity: None,
        frequency: None,
        angle_damping: None,
        length_damping: None,
    }) {
        Reply::Ok {
            body: ResponseBody::Node { node },
            ..
        } => node,
        other => panic!("{other:?}"),
    };
    app.selection = vec![node.clone()];

    let targets = |editor: &Editor| {
        editor
            .with_model(session, |m| match m.node(&node).map(|n| &n.kind) {
                Some(catchlight_core::ModelNodeKind::SimplePhysics(ph)) => {
                    ph.target_params().clone()
                }
                _ => panic!("not a physics node"),
            })
            .unwrap()
    };

    app.apply_inspector_action(InspectorAction::PhysicsCommit(PhysicsPatch {
        target_params: Some(vec![angle.clone()]),
        ..Default::default()
    }));
    assert_eq!(targets(&editor), [Some(angle.clone()), None]);

    app.apply_inspector_action(InspectorAction::PhysicsCommit(PhysicsPatch {
        target_params: Some(vec![angle.clone(), length.clone()]),
        ..Default::default()
    }));
    assert_eq!(
        targets(&editor),
        [Some(angle), Some(length)],
        "the second target must not cost the first",
    );

    app.apply_inspector_action(InspectorAction::PhysicsCommit(PhysicsPatch {
        clear_target_params: true,
        ..Default::default()
    }));
    assert_eq!(targets(&editor), [None, None]);
}
