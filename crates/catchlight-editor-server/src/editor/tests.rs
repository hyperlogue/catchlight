use super::*;

#[test]
fn every_command_that_carries_bytes_is_held_to_the_attachment_budget() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let limit = LoadLimits::default().encoded_bytes;
    let mut attachments = Attachments::none();
    // Zeroed, so the pages are never touched.
    attachments.insert("texture", vec![0; limit as usize + 1]);
    let (reply, _) = ed.handle_with(
        req(
            2,
            Command::TextureAdd {
                session: s,
                node: "face".parse().unwrap(),
                encoding: TextureEncoding::Png,
                texture: None,
            },
        ),
        attachments,
    );
    match reply {
        Reply::Err {
            code: ErrorCode::LimitExceeded,
            limit: Some(info),
            ..
        } => {
            assert_eq!(info.resource, "encoded_bytes");
            assert_eq!(info.requested, limit + 1);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_well_formed_id_the_model_lacks_is_a_no_node_error() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    assert!(matches!(
        ed.handle(req(
            2,
            Command::NodeDelete {
                session: s,
                node: NodeId::new("root/part-deadbeef").unwrap(),
            },
        )),
        Reply::Err {
            id: 2,
            code: ErrorCode::NoNode,
            ..
        }
    ));
}

/// The observer re-enters the editor; an emission holding a lock would
/// deadlock here.
#[test]
fn an_observer_hears_every_change_until_its_subscription_drops() {
    let ed = Arc::new(Editor::new());
    let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let weak = Arc::downgrade(&ed);
    let subscription = ed.subscribe(Box::new(move |event| {
        if let (Event::ModelChanged { session, .. }, Some(ed)) = (event, weak.upgrade()) {
            ed.with_model(*session, Model::node_count).unwrap();
        }
        lock(&sink).push(event.clone());
    }));

    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    assert!(
        matches!(lock(&seen).as_slice(), [Event::SessionsChanged]),
        "a new session changes the set of open sessions",
    );
    lock(&seen).clear();

    body(ed.handle(req(
        2,
        Command::NodeAdd {
            session: s,
            parent: NodeId::new("root").unwrap(),
            kind: NodeKindArg::Group,
            name: None,
            node: None,
        },
    )));
    match lock(&seen).as_slice() {
        [Event::ModelChanged { session, rev }] => {
            assert_eq!(*session, s);
            assert_eq!(*rev, 1, "the event carries the revision after the edit");
        }
        other => panic!("expected one ModelChanged, got {other:?}"),
    }
    lock(&seen).clear();

    body(ed.handle(req(3, Command::SessionClose { session: s })));
    assert!(matches!(lock(&seen).as_slice(), [Event::SessionsChanged]));
    lock(&seen).clear();

    drop(subscription);
    let s = session_of(body(ed.handle(req(
        4,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    body(ed.handle(req(
        5,
        Command::NodeAdd {
            session: s,
            parent: NodeId::new("root").unwrap(),
            kind: NodeKindArg::Group,
            name: None,
            node: None,
        },
    )));
    assert!(
        lock(&seen).is_empty(),
        "an unsubscribed observer hears nothing",
    );
}

#[test]
fn a_reply_carries_the_revision_it_reflects() {
    let ed = Editor::new();
    let (s, rev) = match ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    )) {
        Reply::Ok { body, rev, .. } => (session_of(body), rev),
        other => panic!("{other:?}"),
    };
    assert_eq!(rev, Some(0), "a new session reports the rev it starts at");
    assert!(
        matches!(
            ed.handle(req(2, Command::SessionList)),
            Reply::Ok { rev: None, .. }
        ),
        "an editor-level read names no session and so no revision",
    );

    let add = |id| {
        ed.handle(req(
            id,
            Command::NodeAdd {
                session: s,
                parent: NodeId::new("root").unwrap(),
                kind: NodeKindArg::Group,
                name: None,
                node: None,
            },
        ))
    };
    assert!(matches!(add(3), Reply::Ok { rev: Some(1), .. }));
    assert!(matches!(
        ed.handle(req(4, Command::NodeTree { session: s })),
        Reply::Ok { rev: Some(1), .. }
    ));
    assert!(matches!(
        ed.handle(req(5, Command::SessionClose { session: s })),
        Reply::Ok { rev: None, .. }
    ));
}

fn body(reply: Reply) -> ResponseBody {
    match reply {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn req(id: u64, command: Command) -> Request {
    Request { id, command }
}

fn session_of(b: ResponseBody) -> SessionId {
    match b {
        ResponseBody::Session { session } => session,
        other => panic!("expected session, got {other:?}"),
    }
}

#[test]
fn a_session_an_edit_panicked_in_is_closed_and_refused_by_its_own_code() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ed.edit_session(s, |session| -> Result<(), EditorError> {
            let root = session.model.root().unwrap().clone();
            session
                .model
                .update_node(&root, |node| node.name = Name::truncated("half-applied"))
                .unwrap();
            panic!("an edit failed partway");
        })
    }));
    assert!(panicked.is_err());

    for id in [2, 3] {
        assert!(matches!(
            ed.handle(req(id, Command::Status { session: s })),
            Reply::Err {
                code: ErrorCode::SessionPoisoned,
                ..
            }
        ));
    }
    assert!(matches!(
        body(ed.handle(req(4, Command::SessionList))),
        ResponseBody::Sessions { sessions, .. } if sessions.is_empty()
    ));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn independent_sessions_do_not_share_a_command_lock() {
    let editor = Arc::new(Editor::new());
    let first = session_of(body(editor.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let second = session_of(body(editor.handle(req(
        2,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocking_editor = editor.clone();
    let blocking = std::thread::spawn(move || {
        blocking_editor
            .with_model(first, |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap();
    });
    entered_rx.recv().unwrap();

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let querying_editor = editor;
    let query = std::thread::spawn(move || {
        done_tx.send(querying_editor.is_dirty(second)).unwrap();
    });
    let result = done_rx.recv_timeout(std::time::Duration::from_secs(10));
    release_tx.send(()).unwrap();
    blocking.join().unwrap();
    query.join().unwrap();

    assert!(!result.unwrap().unwrap());
}

#[test]
fn session_node_save_reopen_lifecycle() {
    let ed = Editor::with_storage(Arc::new(FileStorage::new(std::env::temp_dir()).unwrap()))
        .with_kept_store(Arc::new(SidecarKeptStore::default()));
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: Some("t".into()),
            source: None,
        },
    ))));

    let root = match body(ed.handle(req(2, Command::NodeTree { session: s }))) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("{other:?}"),
    };
    let node = match body(ed.handle(req(
        3,
        Command::NodeAdd {
            session: s,
            parent: root,
            kind: NodeKindArg::Group,
            name: Some("A".into()),
            node: None,
        },
    ))) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };

    ed.handle(req(
        4,
        Command::NodeSet {
            session: s,
            node,
            patch: NodePatch {
                translate: Some([1.0, 2.0, 3.0]),
                ..Default::default()
            },
        },
    ));

    let status = match body(ed.handle(req(5, Command::Status { session: s }))) {
        ResponseBody::Status { status } => status,
        other => panic!("{other:?}"),
    };
    assert_eq!(status.node_count, 2);
    assert!(status.dirty);

    let tmp = std::env::temp_dir().join(format!("catchlight-srv-{}.clm", std::process::id()));
    assert!(matches!(
        body(ed.handle(req(
            6,
            Command::Save {
                session: s,
                path: Some(tmp.display().to_string()),
            },
        ))),
        ResponseBody::Saved { .. }
    ));

    let s2 = session_of(body(ed.handle(req(
        7,
        Command::SessionOpen {
            path: tmp.display().to_string(),
        },
    ))));
    let st2 = match body(ed.handle(req(8, Command::Status { session: s2 }))) {
        ResponseBody::Status { status } => status,
        other => panic!("{other:?}"),
    };
    assert_eq!(st2.node_count, 2);
    assert!(!st2.dirty, "freshly opened session is clean");
    let _ = std::fs::remove_file(&tmp);
}

fn one_pixel_png(rgba: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_raw(1, 1, rgba.to_vec()).expect("1x1");
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

fn new_part(ed: &Editor, s: SessionId, id: u64) -> NodeId {
    let root = match body(ed.handle(req(id, Command::NodeTree { session: s }))) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("{other:?}"),
    };
    match body(ed.handle(req(
        id + 1,
        Command::NodeAdd {
            session: s,
            parent: root,
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        },
    ))) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    }
}

fn texture_ids(ed: &Editor, s: SessionId, id: u64) -> Vec<TexId> {
    match body(ed.handle(req(id, Command::TextureList { session: s }))) {
        ResponseBody::Textures { textures } => textures.into_iter().map(|t| t.id).collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_upload_adds_the_texture_and_assigns_it_in_one_edit() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let part = new_part(&ed, s, 2);
    let rev = ed.revision(s).expect("an open session");

    let (tex, dropped) = ed
        .add_texture_bytes(s, &part, CoreTextureEncoding::Png, one_pixel_png([9; 4]))
        .expect("upload");

    assert!(dropped.is_empty(), "nothing was displaced");
    assert_eq!(texture_ids(&ed, s, 4), vec![tex.clone()]);
    assert_eq!(
        ed.with_model(s, |m| match m.node(&part).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(p)) => p.albedo().cloned(),
            _ => None,
        })
        .expect("session"),
        Some(tex),
    );
    assert_eq!(
        ed.revision(s).expect("an open session"),
        rev + 1,
        "one edit, one revision"
    );
}

#[test]
fn a_reply_names_the_textures_the_edit_deleted() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let part = new_part(&ed, s, 2);
    let (first, _) = ed
        .add_texture_bytes(s, &part, CoreTextureEncoding::Png, one_pixel_png([1; 4]))
        .expect("upload");

    let (second, dropped) = ed
        .add_texture_bytes(s, &part, CoreTextureEncoding::Png, one_pixel_png([2; 4]))
        .expect("upload");
    assert_eq!(dropped, vec![first]);
    assert_eq!(texture_ids(&ed, s, 4), vec![second.clone()]);

    let other = new_part(&ed, s, 5);
    let (third, _) = ed
        .add_texture_bytes(s, &other, CoreTextureEncoding::Png, one_pixel_png([3; 4]))
        .expect("upload");
    match body(ed.handle(req(
        7,
        Command::NodeSet {
            session: s,
            node: other,
            patch: NodePatch {
                texture: Some(Some(second.clone())),
                ..Default::default()
            },
        },
    ))) {
        ResponseBody::Node { dropped, .. } => assert_eq!(dropped, vec![third]),
        other => panic!("{other:?}"),
    }

    match body(ed.handle(req(
        8,
        Command::NodeDelete {
            session: s,
            node: part,
        },
    ))) {
        ResponseBody::Node { dropped, .. } => {
            assert!(dropped.is_empty(), "the other part still draws it");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(texture_ids(&ed, s, 9), vec![second]);
}

#[test]
fn delete_then_save_drops_the_node() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let root = match body(ed.handle(req(2, Command::NodeTree { session: s }))) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("{other:?}"),
    };
    let node = match body(ed.handle(req(
        3,
        Command::NodeAdd {
            session: s,
            parent: root,
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        },
    ))) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };
    assert!(matches!(
        body(ed.handle(req(4, Command::NodeDelete { session: s, node }))),
        ResponseBody::Node { dropped, .. } if dropped.is_empty()
    ));
    let status = match body(ed.handle(req(5, Command::Status { session: s }))) {
        ResponseBody::Status { status } => status,
        other => panic!("{other:?}"),
    };
    assert_eq!(status.node_count, 1);
}

#[test]
fn unknown_session_is_an_error_reply() {
    let ed = Editor::new();
    assert!(matches!(
        ed.handle(req(
            1,
            Command::Status {
                session: SessionId(999)
            }
        )),
        Reply::Err { .. }
    ));
}

fn node_count(ed: &Editor, s: SessionId, id: u64) -> u32 {
    match body(ed.handle(req(id, Command::Status { session: s }))) {
        ResponseBody::Status { status } => status.node_count,
        other => panic!("{other:?}"),
    }
}

#[test]
fn undo_redo_round_trips() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let root = match body(ed.handle(req(2, Command::NodeTree { session: s }))) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("{other:?}"),
    };
    for id in 3..=4 {
        ed.handle(req(
            id,
            Command::NodeAdd {
                session: s,
                parent: root.clone(),
                kind: NodeKindArg::Group,
                name: None,
                node: None,
            },
        ));
    }
    assert_eq!(node_count(&ed, s, 5), 3);
    assert!(matches!(
        ed.handle(req(
            6,
            Command::Undo {
                session: s,
                if_rev: 2
            }
        )),
        Reply::Ok { .. }
    ));
    assert!(matches!(
        ed.handle(req(
            7,
            Command::Undo {
                session: s,
                if_rev: 3
            }
        )),
        Reply::Ok { .. }
    ));
    assert_eq!(node_count(&ed, s, 8), 1);
    assert!(matches!(
        ed.handle(req(
            9,
            Command::Undo {
                session: s,
                if_rev: 4
            }
        )),
        Reply::Err { .. }
    ));
    assert!(matches!(
        ed.handle(req(
            10,
            Command::Redo {
                session: s,
                if_rev: 4
            }
        )),
        Reply::Ok { .. }
    ));
    assert_eq!(node_count(&ed, s, 11), 2);
}

#[test]
fn presence_is_off_the_model_path() {
    let ed = Editor::new();
    let s = session_of(body(ed.handle(req(
        1,
        Command::SessionNew {
            name: None,
            source: None,
        },
    ))));
    let presence = Presence {
        pose: vec![ParamPose {
            param: ParamId::new("x").unwrap(),
            value: 1.0,
        }],
        camera: None,
        selection: None,
    };
    assert!(matches!(
        ed.handle(req(
            2,
            Command::PresenceSet {
                session: s,
                presence
            }
        )),
        Reply::Ok { .. }
    ));
    match body(ed.handle(req(3, Command::PresenceGet { session: s }))) {
        ResponseBody::Presence { presence: Some(p) } => {
            assert_eq!(p.pose[0].param.as_str(), "x");
        }
        other => panic!("{other:?}"),
    }
    match body(ed.handle(req(4, Command::Status { session: s }))) {
        ResponseBody::Status { status } => {
            assert_eq!(status.rev, 0, "presence does not bump rev");
            assert!(!status.dirty, "presence does not dirty the model");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn preview_renders_png_smoke() {
    let mut img = image::RgbaImage::new(32, 32);
    for px in img.pixels_mut() {
        *px = image::Rgba([220, 40, 40, 255]);
    }
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();

    let ed = Editor::new();
    let mut attachments = Attachments::none();
    attachments.insert(
        "manifest",
        br#"{"textures":[{"id":"face","path":"face.png"}],
           "nodes":[{"id":"face","kind":"part","texture":"face","mesh":{"auto":"quad"}}]}"#
            .to_vec(),
    );
    attachments.insert("texture:face.png", png.into_inner());
    let s = session_of(body(
        ed.handle_with(
            req(
                2,
                Command::SessionNew {
                    name: None,
                    source: Some(SessionSource::Manifest {}),
                },
            ),
            attachments,
        )
        .0,
    ));
    let (reply, payload) = ed.handle_with(
        req(
            3,
            Command::Preview {
                session: s,
                pose: vec![],
                size: Some([128, 128]),
                camera: None,
            },
        ),
        Attachments::none(),
    );
    match reply {
        Reply::Ok {
            body: ResponseBody::Preview { preview },
            ..
        } => {
            assert_eq!(preview.width, 128);
            let payload = payload.expect("preview answers with the png");
            assert_eq!(payload.content_type, "image/png");
            assert!(!payload.bytes.is_empty(), "preview png is empty");
        }
        other => panic!("preview failed: {other:?}"),
    }
}

#[test]
fn a_preview_larger_than_the_renderer_allows_is_a_limit_error() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clm"
    ))
    .unwrap();
    let ed = Editor::new();
    let mut attachments = Attachments::none();
    attachments.insert("model", bytes);
    let s = session_of(body(
        ed.handle_with(
            req(
                1,
                Command::SessionNew {
                    name: None,
                    source: Some(SessionSource::Clm {}),
                },
            ),
            attachments,
        )
        .0,
    ));
    let preview = |id: u64, size: [u32; 2]| {
        ed.handle_with(
            req(
                id,
                Command::Preview {
                    session: s,
                    pose: vec![],
                    size: Some(size),
                    camera: None,
                },
            ),
            Attachments::none(),
        )
    };

    for (id, size, resource) in [
        (2, [9000, 9000], "preview_pixels"),
        (3, [65535, 1], "preview_dimension"),
    ] {
        match preview(id, size).0 {
            Reply::Err {
                code: ErrorCode::LimitExceeded,
                limit: Some(limit),
                ..
            } => assert_eq!(limit.resource, resource),
            other => panic!("{size:?} should exceed {resource}: {other:?}"),
        }
    }
    let (reply, payload) = preview(4, [64, 64]);
    assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
    assert!(
        payload.is_some(),
        "a refused preview leaves the session previewable"
    );
}

/// Pixels prove the pose landed: a preview rendered at rest would still
/// be a valid PNG.
#[test]
fn a_preview_honours_the_pose_it_is_given() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/welded_seam.clm"
    ))
    .unwrap();
    let ed = Editor::new();
    let mut attachments = Attachments::none();
    attachments.insert("model", bytes);
    let s = session_of(body(
        ed.handle_with(
            req(
                2,
                Command::SessionNew {
                    name: None,
                    source: Some(SessionSource::Clm {}),
                },
            ),
            attachments,
        )
        .0,
    ));
    let shot = |id: u64, pose: Vec<ParamPose>| -> Vec<u8> {
        let (reply, payload) = ed.handle_with(
            req(
                id,
                Command::Preview {
                    session: s,
                    pose,
                    // welded_seam spans 300x240 world units, well inside
                    // the default 2000-unit camera.
                    size: Some([256, 256]),
                    camera: None,
                },
            ),
            Attachments::none(),
        );
        match reply {
            Reply::Ok {
                body: ResponseBody::Preview { .. },
                ..
            } => payload.expect("preview answers with the png").bytes,
            other => panic!("preview failed: {other:?}"),
        }
    };

    let rest = shot(1, Vec::new());
    let pull = ed
        .with_model(s, |model| {
            model
                .param_ids()
                .iter()
                .find(|id| model.param(id).is_some_and(|p| p.name.as_str() == "pull"))
                .cloned()
        })
        .unwrap()
        .expect("welded_seam has a `pull` param");
    let posed = shot(
        2,
        vec![ParamPose {
            param: pull,
            value: 1.0,
        }],
    );
    let back = shot(3, Vec::new());

    assert_ne!(rest, posed, "posing pull must change the preview");
    assert_eq!(rest, back, "leaving pull out must render it at its default");
}
