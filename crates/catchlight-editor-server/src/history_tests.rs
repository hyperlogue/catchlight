//! Session publication, branching navigation and lock ordering contracts.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::{
    io,
    sync::{mpsc, Barrier},
    thread,
    time::Duration,
};

use super::*;

fn new_session(editor: &Editor) -> SessionId {
    let id = editor.alloc_id();
    editor.insert_session(
        id,
        Session::new(id, Model::new(), "history test".into(), None),
    );
    id
}

fn rename_model(model: &mut Model, name: &str) {
    let root = model.root().unwrap().clone();
    model
        .update_node(&root, |node| node.name = Name::truncated(name))
        .unwrap();
}

fn rename(editor: &Editor, session: SessionId, name: &str) -> Captured<ResponseBody> {
    editor
        .edit_session_captured(session, |s| {
            rename_model(&mut s.model, name);
            Ok(ResponseBody::Empty)
        })
        .unwrap()
}

fn name(editor: &Editor, session: SessionId) -> String {
    editor
        .with_model(session, |model| {
            model.node(model.root().unwrap()).unwrap().name.to_string()
        })
        .unwrap()
}

fn command(editor: &Editor, command: Command) -> Reply {
    editor.handle(Request { id: 1, command })
}

fn ok_revision(reply: Reply) -> u64 {
    match reply {
        Reply::Ok { rev: Some(rev), .. } => rev,
        other => panic!("expected a captured revision: {other:?}"),
    }
}

fn history(editor: &Editor, session: SessionId) -> catchlight_editor_core::HistoryMetadata {
    editor
        .with_session(session, |s| Ok(s.history.metadata()))
        .unwrap()
}

#[test]
fn history_wire_navigation_preserves_branches_and_uses_fresh_guards() {
    let editor = Editor::new();
    let session = new_session(&editor);
    assert_eq!(rename(&editor, session, "A").rev, Some(1));
    assert_eq!(rename(&editor, session, "B").rev, Some(2));
    assert_eq!(
        ok_revision(command(&editor, Command::Undo { session, if_rev: 2 })),
        3
    );
    assert_eq!(name(&editor, session), "A");
    assert_eq!(rename(&editor, session, "C").rev, Some(4));
    assert_eq!(
        ok_revision(command(
            &editor,
            Command::EditGoto {
                session,
                if_rev: 4,
                revision: 2
            }
        )),
        5
    );
    assert_eq!(name(&editor, session), "B");
    let before = history(&editor, session);
    assert_eq!(before.current, 2);
    assert_eq!(
        before
            .entries
            .iter()
            .map(|entry| entry.revision)
            .collect::<Vec<_>>(),
        [0, 1, 2, 4]
    );
    assert_eq!(before.entries[1].revisions, [1, 3]);
    assert_eq!(before.entries[2].revisions, [2, 5]);
    assert_eq!(editor.history(session).unwrap(), (2, 0));
    assert!(matches!(
        command(
            &editor,
            Command::EditGoto {
                session,
                if_rev: 2,
                revision: 4
            }
        ),
        Reply::Err {
            code: ErrorCode::RevisionConflict,
            ..
        }
    ));
    assert_eq!(history(&editor, session), before);
    assert!(matches!(
        command(
            &editor,
            Command::EditHistoryGet {
                session,
                if_rev: Some(4)
            }
        ),
        Reply::Err {
            code: ErrorCode::RevisionConflict,
            ..
        }
    ));
    match command(
        &editor,
        Command::EditHistoryGet {
            session,
            if_rev: Some(5),
        },
    ) {
        Reply::Ok {
            rev: Some(5),
            body:
                ResponseBody::EditHistory {
                    root,
                    current,
                    pruned,
                    entries,
                },
            ..
        } => {
            assert_eq!((root, current, pruned), (0, 2, false));
            assert_eq!(entries[1].revisions, [1, 3]);
            assert_eq!(entries[1].redo, Some(2));
        }
        other => panic!("unexpected history reply: {other:?}"),
    }
    assert_eq!(
        ok_revision(command(
            &editor,
            Command::EditGoto {
                session,
                if_rev: 5,
                revision: 3
            }
        )),
        6
    );
    assert_eq!(name(&editor, session), "A");
    assert_eq!(
        ok_revision(command(&editor, Command::Redo { session, if_rev: 6 })),
        7
    );
    assert_eq!(name(&editor, session), "B");
}

#[test]
fn noops_and_failures_preserve_revision_history_notifications_and_model() {
    let editor = Editor::new();
    let session = new_session(&editor);
    rename(&editor, session, "A");
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    editor.subscribe(Box::new(move |event| lock(&seen).push(event.clone())));
    let before = history(&editor, session);
    let identity = editor.with_model(session, Model::identity).unwrap();
    assert_eq!(rename(&editor, session, "A").rev, Some(1));
    let no_op = editor
        .edit_session_captured(session, |s| {
            rename_model(&mut s.model, "temporary");
            rename_model(&mut s.model, "A");
            s.touch();
            Ok(())
        })
        .unwrap();
    assert_eq!(no_op.rev, Some(1));
    assert!(editor
        .edit_session_captured(session, |s| -> Result<(), EditorError> {
            rename_model(&mut s.model, "partial");
            s.touch();
            Err(EditorError::BadTarget("refused second operation".into()))
        })
        .is_err());
    assert_eq!(
        ok_revision(command(
            &editor,
            Command::EditGoto {
                session,
                if_rev: 1,
                revision: 1
            }
        )),
        1
    );
    assert!(matches!(
        command(
            &editor,
            Command::EditGoto {
                session,
                if_rev: 1,
                revision: 99
            }
        ),
        Reply::Err {
            code: ErrorCode::RevisionUnavailable,
            ..
        }
    ));
    assert_eq!(history(&editor, session), before);
    assert_eq!(editor.revision(session), Some(1));
    assert_eq!(name(&editor, session), "A");
    assert_eq!(
        editor.with_model(session, Model::identity).unwrap(),
        identity
    );
    assert!(lock(&events).is_empty());
}

#[test]
fn changed_publication_is_stamped_before_reentrant_observer_edits() {
    let editor = Arc::new(Editor::new());
    let session = new_session(&editor);
    let weak = Arc::downgrade(&editor);
    editor.subscribe(Box::new(move |event| {
        if matches!(event, Event::ModelChanged { rev: 1, .. }) {
            let editor = weak.upgrade().unwrap();
            rename(&editor, session, "observer edit");
        }
    }));
    let root = editor
        .with_model(session, |model| model.root().unwrap().clone())
        .unwrap();
    let reply = command(
        &editor,
        Command::NodeSet {
            session,
            node: root,
            patch: NodePatch {
                name: Some("first edit".into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(ok_revision(reply), 1);
    assert_eq!(editor.revision(session), Some(2));
    assert_eq!(name(&editor, session), "observer edit");
    assert_eq!(history(&editor, session).entries.len(), 3);
}

#[derive(Debug)]
struct BlockingExport {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl Storage for BlockingExport {
    fn read(&self, _key: &str) -> io::Result<Vec<u8>> {
        Err(io::ErrorKind::NotFound.into())
    }
    fn write(&self, _key: &str, _bytes: &[u8]) -> io::Result<()> {
        self.entered.send(()).unwrap();
        lock(&self.release).recv().unwrap();
        Ok(())
    }
}

#[test]
fn output_work_keeps_its_capture_revision_while_later_edits_continue() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let editor = Arc::new(Editor::with_storage(Arc::new(BlockingExport {
        entered: entered_tx,
        release: Mutex::new(release_rx),
    })));
    let session = new_session(&editor);
    rename(&editor, session, "captured");
    let export_editor = editor.clone();
    let exporting = thread::spawn(move || {
        command(
            &export_editor,
            Command::ExportManifest {
                session,
                path: "model.json".into(),
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    rename(&editor, session, "newer");
    release_tx.send(()).unwrap();
    assert_eq!(ok_revision(exporting.join().unwrap()), 1);
    assert_eq!(editor.revision(session), Some(2));
}

#[test]
fn competing_write_waits_for_atomic_publication_and_rechecks_its_guard() {
    let editor = Arc::new(Editor::new());
    let session = new_session(&editor);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first_editor = editor.clone();
    let first = thread::spawn(move || {
        first_editor
            .edit_session_captured(session, |s| {
                s.check_revision(Some(0))?;
                rename_model(&mut s.model, "partial");
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                rename_model(&mut s.model, "complete");
                Ok(())
            })
            .unwrap()
            .rev
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let ready = Arc::new(Barrier::new(2));
    let second_ready = ready.clone();
    let second_editor = editor.clone();
    let (finished_tx, finished_rx) = mpsc::channel();
    let second = thread::spawn(move || {
        second_ready.wait();
        let result = second_editor.edit_session_captured(session, |s| {
            s.check_revision(Some(0))?;
            rename_model(&mut s.model, "stale writer");
            Ok(())
        });
        finished_tx.send(()).unwrap();
        result
    });
    ready.wait();
    let finished_before_release = finished_rx.recv_timeout(Duration::from_millis(30));
    release_tx.send(()).unwrap();
    assert_eq!(first.join().unwrap(), Some(1));
    assert_eq!(
        finished_before_release,
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    assert!(matches!(
        second.join().unwrap(),
        Err(EditorError::RevisionConflict)
    ));
    assert_eq!(name(&editor, session), "complete");
    assert_eq!(history(&editor, session).entries.len(), 2);
}

#[test]
fn navigation_waits_for_publication_and_refuses_its_now_stale_guard() {
    for goto in [false, true] {
        let editor = Arc::new(Editor::new());
        let session = new_session(&editor);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let editing_editor = editor.clone();
        let editing = thread::spawn(move || {
            editing_editor
                .edit_session_captured(session, |s| {
                    rename_model(&mut s.model, "partial");
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    rename_model(&mut s.model, "complete");
                    Ok(())
                })
                .unwrap()
                .rev
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let ready = Arc::new(Barrier::new(2));
        let navigating_ready = ready.clone();
        let navigating_editor = editor.clone();
        let (finished_tx, finished_rx) = mpsc::channel();
        let navigating = thread::spawn(move || {
            navigating_ready.wait();
            let action = if goto {
                Command::EditGoto {
                    session,
                    if_rev: 0,
                    revision: 0,
                }
            } else {
                Command::Undo { session, if_rev: 0 }
            };
            let reply = command(&navigating_editor, action);
            finished_tx.send(()).unwrap();
            reply
        });
        ready.wait();
        let finished_before_release = finished_rx.recv_timeout(Duration::from_millis(30));
        release_tx.send(()).unwrap();
        assert_eq!(editing.join().unwrap(), Some(1));
        assert!(matches!(
            navigating.join().unwrap(),
            Reply::Err {
                code: ErrorCode::RevisionConflict,
                ..
            }
        ));
        assert_eq!(
            finished_before_release,
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert_eq!(history(&editor, session).entries.len(), 2);
        assert_eq!(name(&editor, session), "complete");
    }
}

#[test]
fn close_waits_for_session_work_without_blocking_the_registry() {
    let editor = Arc::new(Editor::new());
    let session = new_session(&editor);
    let other = new_session(&editor);
    let stale_handle = editor.session(session).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let editing_editor = editor.clone();
    let editing = thread::spawn(move || {
        editing_editor
            .edit_session_captured(session, |s| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                rename_model(&mut s.model, "completed before close");
                Ok(())
            })
            .unwrap()
            .rev
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let ready = Arc::new(Barrier::new(2));
    let closing_ready = ready.clone();
    let closing_editor = editor.clone();
    let (closed_tx, closed_rx) = mpsc::channel();
    let closing = thread::spawn(move || {
        closing_ready.wait();
        let reply = command(&closing_editor, Command::SessionClose { session });
        closed_tx.send(()).unwrap();
        reply
    });
    ready.wait();
    let close_before_release = closed_rx.recv_timeout(Duration::from_millis(30));
    let independent_editor = editor.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let independent = thread::spawn(move || {
        rename(&independent_editor, other, "independent");
        done_tx.send(()).unwrap();
    });
    let independent_result = done_rx.recv_timeout(Duration::from_secs(5));
    // Release before asserting so a failed locking contract cannot strand a worker.
    release_tx.send(()).unwrap();
    assert_eq!(editing.join().unwrap(), Some(1));
    assert!(matches!(
        closing.join().unwrap(),
        Reply::Ok { rev: None, .. }
    ));
    independent.join().unwrap();
    assert_eq!(close_before_release, Err(mpsc::RecvTimeoutError::Timeout));
    assert!(independent_result.is_ok());
    assert!(matches!(
        lock(&stale_handle).ensure_open(session),
        Err(EditorError::NoSession(_))
    ));
    assert_eq!(editor.revision(session), None);
    assert!(matches!(
        command(&editor, Command::Undo { session, if_rev: 1 }),
        Reply::Err {
            code: ErrorCode::NoSession,
            ..
        }
    ));
}

#[test]
fn navigation_replaces_model_generation_and_clears_retained_runtime() {
    let editor = Editor::new();
    let session = new_session(&editor);
    rename(&editor, session, "A");
    let initial = editor
        .with_model_revision(session, |model, rev| {
            (model.identity(), model.generation(), rev)
        })
        .unwrap();
    editor.with_puppet(session, |_, _| ()).unwrap();
    assert!(editor
        .with_session(session, |s| Ok(s.puppet.is_some()))
        .unwrap());
    assert_eq!(
        ok_revision(command(&editor, Command::Undo { session, if_rev: 1 })),
        2
    );
    let after = editor
        .with_model_revision(session, |model, rev| {
            (model.identity(), model.generation(), rev)
        })
        .unwrap();
    assert_eq!(after.0, initial.0);
    assert!(after.1 > initial.1);
    assert_eq!(after.2, 2);
    assert!(editor
        .with_session(session, |s| Ok(s.puppet.is_none()))
        .unwrap());
    assert!(editor.is_dirty(session).unwrap());
}

#[test]
fn navigation_keeps_preview_pose_but_restarts_scratch_and_physics() {
    let editor = Editor::new();
    let session = editor.alloc_id();
    let mut model = Model::new();
    let root = model.root().unwrap().clone();
    let param = ParamId::new("drive").unwrap();
    model
        .add_param_with_id(
            param.clone(),
            ModelParam::new(Name::truncated("Drive"), 0.0, 1.0, 0.0),
        )
        .unwrap();
    let part = NodeId::new("panel").unwrap();
    model
        .add_node_with_id(
            part.clone(),
            &root,
            ModelNode::new(
                "Panel",
                ModelNodeKind::Part(ModelPart::new(ClmMesh {
                    verts: vec![0.0, 0.0],
                    uvs: vec![],
                    indices: ClmIndices::U16(vec![]),
                    origin: [0.0; 2],
                })),
            ),
        )
        .unwrap();
    let driver = NodeId::new("driver").unwrap();
    model
        .add_node_with_id(
            driver.clone(),
            &root,
            ModelNode::new(
                "Driver",
                ModelNodeKind::SimplePhysics(ModelPhysics::new(
                    catchlight_core::physics::PendulumKind::RigidPendulum,
                )),
            ),
        )
        .unwrap();
    editor.insert_session(
        session,
        Session::new(session, model, "preview history".into(), None),
    );
    rename(&editor, session, "Changed");
    editor
        .with_puppet(session, |model, puppet| {
            puppet.set_param_value(&param, 0.75);
            puppet.tick(model, 0.0);
            let part = puppet.node_idx(&part).unwrap();
            assert!(puppet.set_scratch_transform(
                part,
                catchlight_core::ScratchTransform {
                    translation: Some(Vec3::new(3.0, 4.0, 0.0)),
                    ..Default::default()
                },
            ));
            assert!(puppet.set_scratch_deform(part, &[Vec2::new(5.0, 6.0)]));
            puppet.combine_deforms();
            assert_eq!(
                puppet.combined_deform(part),
                Some(&[Vec2::new(5.0, 6.0)][..])
            );
            puppet.set_physics_enabled(true);
            assert!(puppet.place_driver(puppet.node_idx(&driver).unwrap(), Vec2::new(80.0, 20.0),));
        })
        .unwrap();

    assert_eq!(
        ok_revision(command(&editor, Command::Undo { session, if_rev: 1 })),
        2
    );
    // A second navigation before repaint must also retain the pending pose.
    assert_eq!(
        ok_revision(command(&editor, Command::Redo { session, if_rev: 2 })),
        3
    );
    editor
        .with_puppet(session, |model, puppet| {
            assert_eq!(puppet.param_value(&param), Some(0.75));
            assert!(!puppet.physics_enabled());
            let driver = puppet.node_idx(&driver).unwrap();
            let catchlight_core::NodeKind::SimplePhysics(physics) =
                &puppet.get(driver).unwrap().kind
            else {
                panic!("driver kind changed");
            };
            assert!(
                !physics.anchor_initialized,
                "history must not carry simulated state"
            );
            puppet.tick(model, 0.0);
            let part = puppet.node_idx(&part).unwrap();
            assert_eq!(puppet.scratch_transform(part), None);
            assert_eq!(puppet.combined_deform(part), Some(&[Vec2::ZERO][..]));
        })
        .unwrap();
}

#[test]
fn navigation_keeps_pose_while_a_preview_renderer_holds_the_runtime() {
    let editor = Editor::new();
    let session = new_session(&editor);
    let param = ParamId::new("drive").unwrap();
    editor
        .edit_session(session, |s| {
            s.model.add_param_with_id(
                param.clone(),
                ModelParam::new(Name::truncated("Drive"), 0.0, 1.0, 0.0),
            )?;
            Ok(())
        })
        .unwrap();
    rename(&editor, session, "Changed");
    // Exercise the real runtime handoff without needing a GPU.
    let detached = editor
        .with_session(session, |s| {
            s.puppet().1.set_param_value(&param, 0.625);
            s.take_preview_puppet()
        })
        .unwrap();
    assert_eq!(
        ok_revision(command(&editor, Command::Undo { session, if_rev: 2 })),
        3
    );
    editor
        .with_puppet(session, |_, puppet| {
            assert_eq!(puppet.param_value(&param), Some(0.625));
        })
        .unwrap();
    drop(detached);
}
