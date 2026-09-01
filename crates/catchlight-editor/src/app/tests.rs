//! Headless tests of the app's command paths, driven against an in-process
//! [`Editor`] — no window, no event loop. What they pin is which command a
//! gesture or a panel action sends, and what the document looks like
//! afterwards; the drawing is `viewport::tests`' job.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::inspector::PhysicsPatch;

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

/// Nudge one vertex of the open mesh editor's working mesh — what makes an
/// Apply an actual mesh edit rather than a no-op leave.
fn edit_working_mesh(app: &mut App) {
    let mesh = app.mesh_edit.as_mut().expect("the mode is open");
    let mut working = mesh.working.clone();
    let p = working.pos(0);
    working
        .move_vertex(0, [p[0] + 1.0, p[1]])
        .expect("move a vertex");
    mesh.replace_working(working);
    assert!(!mesh.matches_document(), "the working mesh has moved");
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
            body: ResponseBody::Node { node, .. },
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

/// The seam repair round trip. Re-meshing a part empties every slot on it,
/// because which vertex fills a slot is a claim about the mesh that just went
/// away. The mode stays open on the seam tool, the model will not save while
/// a slot is empty, and refilling every one of them clears the gate.
#[test]
fn a_mesh_edit_empties_a_seam_and_the_gate_holds_until_it_is_refilled() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    let seams = editor
        .with_model(session, |m| {
            m.seams(&node)
                .map(|s| {
                    s.iter()
                        .map(|seam| {
                            (
                                seam.id().clone(),
                                seam.slots()
                                    .iter()
                                    .map(|slot| slot.id().clone())
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .unwrap();
    assert!(!seams.is_empty(), "welded_seam's parts carry seams");
    assert!(app.commit_block().is_empty(), "nothing to repair yet");

    app.selection = vec![node.clone()];
    app.enter_mesh_edit();
    assert!(app.mesh_edit.is_some(), "the mode opened");
    edit_working_mesh(&mut app);
    app.apply_mesh_edit();

    let blocked = app.commit_block();
    assert_eq!(
        blocked.len(),
        seams.iter().map(|(_, slots)| slots.len()).sum::<usize>(),
        "every slot on the re-meshed part is emptied",
    );
    assert!(blocked.iter().all(|a| a.node == node));
    assert!(
        app.blocked_from_saving(),
        "the model must not be saved half-repaired",
    );
    assert!(app.status.contains("cannot save"), "{}", app.status);

    // The mode stayed open on the seam tool, on the document's own mesh.
    let mesh = app
        .mesh_edit
        .as_ref()
        .expect("the mode stays open to repair");
    assert!(mesh.matches_document());
    assert_eq!(mesh.emptied.len(), blocked.len());

    // Refill each slot with a vertex of the new mesh — what clicking one does.
    for (seam, slots) in &seams {
        for (i, slot) in slots.iter().enumerate() {
            app.apply_seam_action(SeamAction::FillSlot {
                seam: seam.clone(),
                slot: slot.clone(),
                vertex: i as u32,
            });
        }
    }
    assert!(app.commit_block().is_empty(), "the gate clears on refill");
    assert!(!app.blocked_from_saving());

    // Leaving the mode must not re-mesh the part: the working mesh is the
    // document's again, and applying it would empty every slot just refilled.
    app.apply_mesh_edit();
    assert!(
        app.commit_block().is_empty(),
        "a second Apply must not undo the repair",
    );
}

/// The other way out: deleting the seam. A weld that named it goes with it,
/// which is the point — the author has decided that seam no longer exists.
#[test]
fn deleting_the_seam_is_the_other_way_past_the_commit_gate() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    app.selection = vec![node.clone()];
    app.enter_mesh_edit();
    edit_working_mesh(&mut app);
    app.apply_mesh_edit();
    assert!(!app.commit_block().is_empty());

    let seams: Vec<catchlight_core::SeamId> = editor
        .with_model(session, |m| {
            m.seams(&node)
                .map(|s| s.iter().map(|seam| seam.id().clone()).collect())
                .unwrap_or_default()
        })
        .unwrap();
    for seam in seams {
        app.apply_seam_action(SeamAction::DeleteSeam(seam));
    }
    assert!(app.commit_block().is_empty());
    assert!(
        editor
            .with_model(session, |m| m.welds().is_empty())
            .unwrap(),
        "deleting a seam takes the welds that named it",
    );
}

/// The seam tool builds a weld: two seams, slot by slot. `slot_add` reaches
/// every seam welded to the one it is called on, so the two slot sets are one
/// set and the weld can never pair mismatched seams.
#[test]
fn welding_two_seams_keeps_their_slot_sets_one_set() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let mut parts: Vec<NodeId> = editor
        .with_model(session, |m| {
            let mut ids: Vec<NodeId> = m
                .node_ids()
                .filter(|id| m.seams(id).is_some())
                .cloned()
                .collect();
            ids.sort();
            ids
        })
        .unwrap();
    let b = parts.pop().expect("two parts");
    let a = parts.pop().expect("two parts");

    // A fresh seam on each part, welded, then a slot added on one side only.
    let seam = catchlight_core::SeamId::new("hem").unwrap();
    for node in [&a, &b] {
        app.send(Command::SeamAdd {
            session,
            node: node.clone(),
            seam: seam.clone(),
        });
    }
    app.selection = vec![a.clone()];
    app.enter_mesh_edit();
    app.apply_seam_action(SeamAction::Weld {
        seam: seam.clone(),
        other: SeamAddr {
            node: b.clone(),
            seam: seam.clone(),
        },
    });
    app.apply_seam_action(SeamAction::AddSlot {
        seam: seam.clone(),
        slot: catchlight_core::SlotId::new("left").unwrap(),
    });

    let (slots_a, slots_b, weights) = editor
        .with_model(session, |m| {
            let slots = |node: &NodeId| {
                m.seam(node, &seam)
                    .map(|s| {
                        s.slots()
                            .iter()
                            .map(|slot| slot.id().to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            };
            let weights = m
                .welds()
                .iter()
                .find(|w| w.a().1 == seam || w.b().1 == seam)
                .map(|w| w.weights().to_vec())
                .unwrap_or_default();
            (slots(&a), slots(&b), weights)
        })
        .unwrap();
    assert_eq!(slots_a, vec!["left".to_string()]);
    assert_eq!(
        slots_b, slots_a,
        "a slot added to one welded seam reaches the other",
    );
    assert_eq!(weights.len(), 1);
    assert_eq!(
        weights[0].1,
        catchlight_core::DEFAULT_SLOT_WEIGHT,
        "a slot that arrives through a weld arrives at the default weight",
    );

    // ...and the weight slider replaces the weld, keeping every other slot.
    app.apply_seam_action(SeamAction::SetWeight {
        seam: seam.clone(),
        other: SeamAddr {
            node: b.clone(),
            seam: seam.clone(),
        },
        slot: catchlight_core::SlotId::new("left").unwrap(),
        weight: 0.25,
    });
    let weights = editor
        .with_model(session, |m| {
            m.welds()
                .iter()
                .find(|w| w.a().1 == seam || w.b().1 == seam)
                .map(|w| w.weights().to_vec())
                .unwrap_or_default()
        })
        .unwrap();
    assert_eq!(weights.len(), 1);
    assert!((weights[0].1 - 0.25).abs() < 1e-6, "{weights:?}");
}

/// check()'s findings reach the panel, and the seam ones are the reason it
/// exists: an unfilled slot is a weld that silently no longer closes.
#[test]
fn the_warnings_panel_reads_the_models_own_check() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let node = first_meshed_node(&editor, session);
    let (seam, slot) = editor
        .with_model(session, |m| {
            m.seams(&node)
                .and_then(|s| s.first())
                .map(|s| (s.id().clone(), s.slots()[0].id().clone()))
        })
        .unwrap()
        .expect("a seam with a slot");

    let warnings = |app: &mut App| {
        let rev = editor.doc_snapshot(session).expect("snapshot").rev;
        app.warnings = None;
        match app.send(Command::Check { session }) {
            Reply::Ok {
                body: ResponseBody::Warnings { warnings },
                ..
            } => {
                app.warnings = Some((rev, warnings.clone()));
                warnings
            }
            other => panic!("{other:?}"),
        }
    };
    assert!(!warnings(&mut app).iter().any(|w| w.contains("unfilled")));

    app.selection = vec![node.clone()];
    app.enter_mesh_edit();
    app.apply_seam_action(SeamAction::ClearSlot { seam, slot });

    assert!(
        warnings(&mut app).iter().any(|w| w.contains("unfilled")),
        "{:?}",
        warnings(&mut app),
    );
}

/// A texture with no part drawing it is deleted by the edit that took its last
/// user. Undo brings it back here; nothing brings it back for an addon that
/// named it by Id, so the app holds the edit until the author has seen what it
/// costs — the same reason an Id rename is held.
#[test]
fn an_edit_that_would_delete_a_texture_waits_to_be_confirmed() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let part = first_meshed_node(&editor, session);
    let tex = editor
        .with_model(session, |m| match m.node(&part).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(p)) => p.albedo().cloned(),
            _ => None,
        })
        .unwrap()
        .expect("the part draws a texture");

    app.guard_texture_drop(DroppingEdit::Send(Box::new(Command::NodeDelete {
        session,
        node: part.clone(),
    })));

    assert_eq!(
        app.texture_drop.as_ref().map(|held| held.dropped.clone()),
        Some(vec![tex.clone()]),
        "the delete is held, naming what it would take"
    );
    assert!(
        editor
            .with_model(session, |m| m.node(&part).is_some())
            .unwrap(),
        "and nothing has happened to the document yet"
    );

    let held = app.texture_drop.take().expect("a held edit");
    app.run_dropping_edit(held.edit);

    assert!(editor
        .with_model(session, |m| m.node(&part).is_none())
        .unwrap());
    assert!(editor
        .with_model(session, |m| m.texture(&tex).is_none())
        .unwrap());
}

/// An edit that leaves every texture with a part drawing it costs nothing to
/// undo, so it is not worth a prompt and does not get one.
#[test]
fn an_edit_that_deletes_no_texture_is_not_held() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let parent = editor
        .with_model(session, |m| m.root().cloned())
        .unwrap()
        .expect("a complete model has a root");
    let group = match app.send(Command::NodeAdd {
        session,
        parent,
        kind: catchlight_editor_protocol::NodeKindArg::Group,
        name: None,
    }) {
        Reply::Ok {
            body: ResponseBody::Node { node, .. },
            ..
        } => node,
        other => panic!("{other:?}"),
    };

    app.guard_texture_drop(DroppingEdit::Send(Box::new(Command::NodeDelete {
        session,
        node: group.clone(),
    })));

    assert!(app.texture_drop.is_none(), "nothing to warn about");
    assert!(
        editor
            .with_model(session, |m| m.node(&group).is_none())
            .unwrap(),
        "so it ran instead of waiting"
    );
}

/// A held texture-drop confirm was computed against the session it was built
/// on. Adopting another session must drop it — otherwise "Delete and
/// continue" applies the old model's edit to the new one, without a valid
/// confirmation.
#[test]
fn a_held_texture_drop_does_not_survive_a_session_change() {
    let (editor, session, mut app) = app_on(&welded_seam());
    let part = first_meshed_node(&editor, session);

    app.guard_texture_drop(DroppingEdit::Send(Box::new(Command::NodeDelete {
        session,
        node: part.clone(),
    })));
    assert!(app.texture_drop.is_some(), "the delete is held");

    let other = editor.open_bytes("other", &welded_seam()).expect("open");
    app.adopt_session(other, "other".into());

    assert!(
        app.texture_drop.is_none(),
        "a session change drops the held edit"
    );
    assert!(
        editor
            .with_model(session, |m| m.node(&part).is_some())
            .unwrap(),
        "and the old session's document is untouched"
    );
}
