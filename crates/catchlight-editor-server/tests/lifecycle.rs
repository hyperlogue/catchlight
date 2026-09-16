//! Atomic session construction, independent forks and captured model transfer.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::{
    collections::HashMap,
    io,
    sync::{Arc, Mutex},
};

use catchlight_core::{
    formats::clm::{self, ClmMesh},
    BindingKey, BindingTarget as CoreBindingTarget, ExtensionValue, Model, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelTexture, Name, ScalarTarget, SeededHex,
};
use catchlight_editor_protocol::*;
use catchlight_editor_server::{Attachments, Editor, Payload, Storage};

fn request(command: Command) -> Request {
    Request { id: 1, command }
}

fn send(editor: &Editor, command: Command) -> Reply {
    editor.handle(request(command))
}

fn ok(reply: Reply) -> (u64, ResponseBody) {
    match reply {
        Reply::Ok {
            rev: Some(rev),
            body,
            ..
        } => (rev, body),
        other => panic!("unexpected reply: {other:?}"),
    }
}

fn create(
    editor: &Editor,
    name: Option<&str>,
    source: Option<SessionSource>,
    attachments: Attachments,
) -> SessionId {
    let (reply, payload) = editor.handle_with(
        request(Command::SessionNew {
            name: name.map(str::to_owned),
            source,
        }),
        attachments,
    );
    assert!(payload.is_none());
    let (rev, body) = ok(reply);
    assert_eq!(rev, 0);
    match body {
        ResponseBody::Session { session } => session,
        other => panic!("unexpected creation: {other:?}"),
    }
}

fn authored_model() -> Model {
    let mut model = Model::new();
    let root = model.root().unwrap().clone();
    model
        .update_node(&root, |node| {
            node.name = Name::truncated("Source character")
        })
        .unwrap();
    let mut hex = SeededHex::new(25);
    let part = model
        .add_node(
            &root,
            ModelNode::new(
                "art",
                ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
            ),
            &mut hex,
        )
        .unwrap();
    let mut png = io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(1, 1, image::Rgba([21, 37, 89, 255]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    model
        .add_texture(
            &part,
            ModelTexture {
                encoding: clm::TextureEncoding::Png,
                alpha: clm::TextureAlpha::Straight,
                data: png.into_inner().into(),
            },
            &mut hex,
        )
        .unwrap();
    let param = model
        .add_param(
            ModelParam::new(Name::truncated("drive"), 0.0, 1.0, 0.0),
            &mut hex,
        )
        .unwrap();
    let key = BindingKey::new(param, part, CoreBindingTarget::Scalar(ScalarTarget::Tx));
    model
        .add_binding_with_positions(&key, vec![vec![0.0, 0.25, 1.0]])
        .unwrap();
    model.set_binding_key(&key, [2, 0], 12.0).unwrap();
    model
        .set_extension(
            ExtensionKey::new("test.metadata").unwrap(),
            ExtensionValue::Json(serde_json::json!({"shape": "test"})),
        )
        .unwrap();
    model
        .set_extension(
            ExtensionKey::new("test.binary").unwrap(),
            ExtensionValue::Bytes(vec![5, 4, 3, 2, 1].into()),
        )
        .unwrap();
    model
}

fn clm_source(model: &Model) -> Attachments {
    let mut attachments = Attachments::none();
    attachments.insert("model", model.to_clm_bytes().unwrap());
    attachments
}

fn json_source(model: &Model) -> (Vec<ImportTexture>, Attachments) {
    let mut attachments = Attachments::none();
    attachments.insert(
        "structure",
        serde_json::to_vec(&model.to_clm_structure().unwrap()).unwrap(),
    );
    let textures = model
        .texture_ids()
        .iter()
        .map(|id| {
            let texture = model.texture(id).unwrap();
            attachments.insert(format!("texture:{id}"), texture.data.to_vec());
            ImportTexture {
                texture: id.clone(),
                encoding: texture.encoding.into(),
                alpha: texture.alpha.into(),
            }
        })
        .collect();
    (textures, attachments)
}

fn session_count(editor: &Editor) -> usize {
    match send(editor, Command::SessionList) {
        Reply::Ok {
            body: ResponseBody::Sessions { sessions },
            rev: None,
            ..
        } => sessions.len(),
        other => panic!("unexpected session list: {other:?}"),
    }
}

#[test]
fn empty_and_clm_creation_publish_finished_clean_roots_once() {
    let editor = Editor::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let received = events.clone();
    editor.subscribe(Box::new(move |event| {
        received.lock().unwrap().push(event.clone())
    }));
    let empty = create(&editor, None, None, Attachments::none());
    assert_eq!(editor.history(empty).unwrap(), (0, 0));
    let model = authored_model();
    let loaded = create(
        &editor,
        Some("Explicit title"),
        Some(SessionSource::Clm {}),
        clm_source(&model),
    );
    assert!(editor
        .with_model(loaded, |loaded| loaded.authored_eq(&model).unwrap())
        .unwrap());
    assert_eq!(editor.history(loaded).unwrap(), (0, 0));
    assert!(!editor.is_dirty(loaded).unwrap());
    assert!(matches!(
        send(
            &editor,
            Command::Undo {
                session: loaded,
                if_rev: 0
            }
        ),
        Reply::Err {
            code: ErrorCode::NothingToUndo,
            ..
        }
    ));
    assert!(matches!(
        send(
            &editor,
            Command::Save {
                session: loaded,
                path: None
            }
        ),
        Reply::Err {
            code: ErrorCode::NoSavePath,
            ..
        }
    ));
    match ok(send(&editor, Command::Status { session: loaded })).1 {
        ResponseBody::Status { status } => assert_eq!(status.title, "Explicit title"),
        other => panic!("unexpected status: {other:?}"),
    }
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| matches!(event, Event::SessionsChanged)));
}

#[test]
fn all_source_failures_leave_no_visible_session_or_event() {
    let editor = Editor::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let received = events.clone();
    editor.subscribe(Box::new(move |event| {
        received.lock().unwrap().push(event.clone())
    }));
    let mut extra = Attachments::none();
    extra.insert("model", vec![]);
    let mut wrong_kind = Attachments::none();
    wrong_kind.insert("structure", b"{}".to_vec());
    let mut malformed = Attachments::none();
    malformed.insert("model", vec![1, 2, 3]);
    let model = authored_model();
    let (textures, missing_binary) = json_source(&model);
    let cases = [
        (None, extra),
        (Some(SessionSource::Clm {}), Attachments::none()),
        (Some(SessionSource::Clm {}), wrong_kind),
        (Some(SessionSource::Clm {}), malformed),
        (Some(SessionSource::Json { textures }), missing_binary),
    ];
    for (source, attachments) in cases {
        assert!(matches!(
            editor
                .handle_with(
                    request(Command::SessionNew { name: None, source }),
                    attachments
                )
                .0,
            Reply::Err { .. }
        ));
        assert_eq!(session_count(&editor), 0);
        assert!(events.lock().unwrap().is_empty());
    }
    assert!(matches!(
        send(
            &editor,
            Command::SessionNew {
                name: Some("x".repeat(catchlight_core::id::MAX_NAME_BYTES + 1)),
                source: None
            }
        ),
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    assert_eq!(session_count(&editor), 0);
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn json_creation_preserves_authored_data_and_pairs_images_exactly() {
    let editor = Editor::new();
    let mut model = authored_model();
    model
        .delete_extension(&ExtensionKey::new("test.binary").unwrap())
        .unwrap();
    let (textures, attachments) = json_source(&model);
    let loaded = create(
        &editor,
        None,
        Some(SessionSource::Json { textures }),
        attachments,
    );
    assert!(editor
        .with_model(loaded, |loaded| loaded.authored_eq(&model).unwrap())
        .unwrap());
    let (textures, mut extra) = json_source(&model);
    extra.insert("texture:unused", vec![1]);
    assert!(matches!(
        editor
            .handle_with(
                request(Command::SessionNew {
                    name: None,
                    source: Some(SessionSource::Json { textures })
                }),
                extra
            )
            .0,
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    let (mut textures, attachments) = json_source(&model);
    textures.push(textures[0].clone());
    assert!(matches!(
        editor
            .handle_with(
                request(Command::SessionNew {
                    name: None,
                    source: Some(SessionSource::Json { textures })
                }),
                attachments
            )
            .0,
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    assert_eq!(session_count(&editor), 1);
}

#[test]
fn manifest_creation_names_the_session_and_refuses_unreferenced_files() {
    let editor = Editor::new();
    let mut attachments = Attachments::none();
    attachments.insert(
        "manifest",
        br#"{"name":"Manifest title","nodes":[{"id":"group","name":"Body"}]}"#.to_vec(),
    );
    let session = create(&editor, None, Some(SessionSource::Manifest {}), attachments);
    match ok(send(&editor, Command::Status { session })).1 {
        ResponseBody::Status { status } => {
            assert_eq!(status.title, "Manifest title");
            assert_eq!(status.node_count, 2);
        }
        other => panic!("unexpected status: {other:?}"),
    }
    let mut extra = Attachments::none();
    extra.insert("manifest", b"{}".to_vec());
    extra.insert("texture:unused.png", vec![]);
    assert!(matches!(
        editor
            .handle_with(
                request(Command::SessionNew {
                    name: None,
                    source: Some(SessionSource::Manifest {})
                }),
                extra
            )
            .0,
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    assert_eq!(session_count(&editor), 1);
}

#[test]
fn source_variants_reject_foreign_fields_and_classify_bytes_per_request() {
    for value in [
        serde_json::json!({"format":"clm", "textures":[]}),
        serde_json::json!({"format":"manifest", "textures":[]}),
        serde_json::json!({"format":"json", "path":"model.json"}),
    ] {
        assert!(serde_json::from_value::<SessionSource>(value).is_err());
    }
    assert!(Command::SessionNew {
        name: None,
        source: None
    }
    .carries_bytes()
    .is_none());
    assert!(Command::SessionNew {
        name: None,
        source: Some(SessionSource::Clm {})
    }
    .carries_bytes()
    .is_some());
    assert!(serde_json::from_value::<Command>(
        serde_json::json!({"cmd":"structure_json_import", "session":1, "if_rev":0})
    )
    .is_err());
    assert!(
        serde_json::from_value::<Command>(serde_json::json!({"cmd":"edit_undo", "session":1}))
            .is_err()
    );
}

#[test]
fn forks_are_independent_models_with_clean_history_and_source_metadata() {
    let editor = Editor::new();
    let model = authored_model();
    let source = create(
        &editor,
        None,
        Some(SessionSource::Clm {}),
        clm_source(&model),
    );
    let before = editor.with_model(source, Model::identity).unwrap();
    let (rev, body) = ok(send(
        &editor,
        Command::SessionFork {
            session: source,
            if_rev: 0,
            name: Some("Candidate".into()),
        },
    ));
    assert_eq!(rev, 0);
    let fork = match body {
        ResponseBody::SessionFork {
            session,
            source: origin,
        } => {
            assert_eq!(origin.session, source);
            assert_eq!(origin.rev, 0);
            session
        }
        other => panic!("unexpected fork: {other:?}"),
    };
    assert_ne!(editor.with_model(fork, Model::identity).unwrap(), before);
    assert!(editor
        .with_model(fork, |fork| fork.authored_eq(&model).unwrap())
        .unwrap());
    assert_eq!(editor.history(fork).unwrap(), (0, 0));
    assert!(!editor.is_dirty(fork).unwrap());
    let key = ExtensionKey::new("test.binary").unwrap();
    let mut bytes = Attachments::none();
    bytes.insert("value", vec![9, 8, 7]);
    ok(editor
        .handle_with(
            request(Command::ExtensionSet {
                session: fork,
                key: key.clone(),
                value: ExtensionSet::Bytes,
            }),
            bytes,
        )
        .0);
    assert_eq!(
        editor
            .with_model(source, |model| model
                .extension(&key)
                .unwrap()
                .bytes()
                .unwrap()
                .to_vec())
            .unwrap(),
        [5, 4, 3, 2, 1]
    );
    assert_eq!(editor.revision(source), Some(0));
    assert!(matches!(
        send(
            &editor,
            Command::SessionFork {
                session: source,
                if_rev: 1,
                name: None
            }
        ),
        Reply::Err {
            code: ErrorCode::RevisionConflict,
            ..
        }
    ));
    assert_eq!(session_count(&editor), 2);
    assert!(matches!(
        send(
            &editor,
            Command::Save {
                session: fork,
                path: None
            }
        ),
        Reply::Err {
            code: ErrorCode::NoSavePath,
            ..
        }
    ));
}

fn exported(editor: &Editor, session: SessionId, rev: u64) -> (ResponseBody, Payload) {
    let (reply, payload) = editor.handle_with(
        request(Command::ModelExport {
            session,
            if_rev: rev,
        }),
        Attachments::none(),
    );
    let (captured, body) = ok(reply);
    assert_eq!(captured, rev);
    (body, payload.unwrap())
}

#[test]
fn export_returns_the_complete_guarded_model_without_save_side_effects() {
    let editor = Editor::new();
    let model = authored_model();
    let session = create(
        &editor,
        None,
        Some(SessionSource::Clm {}),
        clm_source(&model),
    );
    ok(send(
        &editor,
        Command::ExtensionSet {
            session,
            key: ExtensionKey::new("test.new").unwrap(),
            value: ExtensionSet::Json {
                value: serde_json::json!({"changed": true}),
            },
        },
    ));
    let history = editor.history(session).unwrap();
    let (body, payload) = exported(&editor, session, 1);
    match body {
        ResponseBody::ModelExport {
            format,
            byte_length,
            sha256,
        } => {
            assert_eq!(format, ModelFormat::Clm);
            assert_eq!(byte_length, payload.bytes.len() as u64);
            use sha2::{Digest, Sha256};
            assert_eq!(
                sha256,
                Sha256::digest(&payload.bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
        }
        other => panic!("unexpected export: {other:?}"),
    }
    let exported = Model::from_clm_bytes(&payload.bytes).unwrap();
    assert!(editor
        .with_model(session, |model| model.authored_eq(&exported).unwrap())
        .unwrap());
    assert_eq!(editor.history(session).unwrap(), history);
    assert_eq!(editor.revision(session), Some(1));
    assert!(editor.is_dirty(session).unwrap());
    let (reply, payload) = editor.handle_with(
        request(Command::ModelExport { session, if_rev: 0 }),
        Attachments::none(),
    );
    assert!(matches!(
        reply,
        Reply::Err {
            code: ErrorCode::RevisionConflict,
            ..
        }
    ));
    assert!(payload.is_none());
}

#[derive(Debug, Default)]
struct MemoryStore(Mutex<HashMap<String, Vec<u8>>>);
impl Storage for MemoryStore {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or(io::ErrorKind::NotFound.into())
    }
    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().insert(key.into(), bytes.to_vec());
        Ok(())
    }
}

#[test]
fn store_open_and_fork_share_initialization_but_only_open_keeps_a_save_path() {
    let store = Arc::new(MemoryStore::default());
    store
        .write(
            "models/character.clm",
            &authored_model().to_clm_bytes().unwrap(),
        )
        .unwrap();
    let editor = Editor::with_storage(store);
    let (rev, body) = ok(send(
        &editor,
        Command::SessionOpen {
            path: "models/character.clm".into(),
        },
    ));
    assert_eq!(rev, 0);
    let session = match body {
        ResponseBody::Session { session } => session,
        other => panic!("unexpected open: {other:?}"),
    };
    assert!(!editor.is_dirty(session).unwrap());
    assert_eq!(editor.history(session).unwrap(), (0, 0));
    assert!(matches!(
        send(
            &editor,
            Command::Save {
                session,
                path: None
            }
        ),
        Reply::Ok { .. }
    ));
    let fork = match ok(send(
        &editor,
        Command::SessionFork {
            session,
            if_rev: 0,
            name: None,
        },
    ))
    .1
    {
        ResponseBody::SessionFork { session, .. } => session,
        other => panic!("unexpected fork: {other:?}"),
    };
    assert!(matches!(
        send(
            &editor,
            Command::Save {
                session: fork,
                path: None
            }
        ),
        Reply::Err {
            code: ErrorCode::NoSavePath,
            ..
        }
    ));
}

#[test]
fn guarded_structure_installation_is_atomic_undoable_and_preserves_destination() {
    let editor = Editor::new();
    let session = create(&editor, None, None, Attachments::none());
    let parent = editor
        .with_model(session, |model| model.root().unwrap().clone())
        .unwrap();
    let mut incoming = Model::new();
    let root = incoming.root().unwrap().clone();
    incoming
        .rename_node_id(&root, NodeId::new("installed").unwrap())
        .unwrap();
    let structure = serde_json::to_vec(&incoming.to_clm_structure().unwrap()).unwrap();
    let install = |if_rev| {
        let mut attachments = Attachments::none();
        attachments.insert("structure", structure.clone());
        editor
            .handle_with(
                request(Command::ImportJson {
                    session,
                    parent: parent.clone(),
                    if_rev,
                    textures: vec![],
                }),
                attachments,
            )
            .0
    };
    assert_eq!(ok(install(0)).0, 1);
    assert_eq!(
        editor
            .with_model(session, |model| model.node_count())
            .unwrap(),
        2
    );
    assert_eq!(editor.history(session).unwrap(), (1, 0));
    assert!(matches!(
        install(0),
        Reply::Err {
            code: ErrorCode::RevisionConflict,
            ..
        }
    ));
    assert!(matches!(
        install(1),
        Reply::Err {
            code: ErrorCode::DuplicateId,
            ..
        }
    ));
    assert_eq!(editor.revision(session), Some(1));
    assert_eq!(editor.history(session).unwrap(), (1, 0));
    assert_eq!(ok(send(&editor, Command::Undo { session, if_rev: 1 })).0, 2);
    assert_eq!(
        editor
            .with_model(session, |model| model.node_count())
            .unwrap(),
        1
    );
}
