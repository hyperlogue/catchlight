#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! JSON session construction and guarded subtree import share core validation.
//! Models round-trip through structures and exact texture attachments; subtree
//! installation is one undoable edit against an explicitly selected parent.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use catchlight_core::formats::clm::{self, ClmFile};
use catchlight_editor_protocol::{
    Command, ErrorCode, ImportTexture, NodeId, Reply, Request, ResponseBody, SessionId,
    SessionSource,
};
use catchlight_editor_server::{Attachments, Editor, Storage};

// ------------------------------------------------------------------ harness

#[derive(Debug, Default)]
struct MemStorage(Mutex<HashMap<String, Vec<u8>>>);

impl Storage for MemStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, key.to_string()))
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }
}

fn editor() -> Editor {
    Editor::with_storage(Arc::new(MemStorage::default()))
}

fn req(id: u64, command: Command) -> Request {
    Request { id, command }
}

fn models_dir() -> PathBuf {
    // crates/catchlight-editor-server/ -> crates/ -> workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("tests/models")
}

fn fixtures() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(models_dir())
        .expect("tests/models")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "clm"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no .clm fixtures found");
    paths
}

/// The JSON structure, the `textures` list and the attachments one file turns
/// into — what a client authoring a model would put together itself.
struct AsJson {
    structure: Vec<u8>,
    textures: Vec<ImportTexture>,
    attachments: Vec<(String, Vec<u8>)>,
}

fn as_json(file: &ClmFile) -> AsJson {
    AsJson {
        structure: serde_json::to_vec(&file.doc).expect("the structure serializes as json"),
        textures: file
            .textures
            .iter()
            .map(|t| ImportTexture {
                texture: t.id.clone(),
                encoding: t.encoding.into(),
                alpha: t.alpha.into(),
            })
            .collect(),
        attachments: file
            .textures
            .iter()
            .map(|t| (format!("texture:{}", t.id), t.data.clone()))
            .collect(),
    }
}

fn send_json(ed: &Editor, command: Command, sent: &AsJson) -> Reply {
    let mut attachments = Attachments::none();
    attachments.insert("structure", sent.structure.clone());
    for (name, bytes) in &sent.attachments {
        attachments.insert(name.clone(), bytes.clone());
    }
    ed.handle_with(req(2, command), attachments).0
}

fn create_json(ed: &Editor, sent: &AsJson) -> Reply {
    send_json(
        ed,
        Command::SessionNew {
            name: None,
            source: Some(SessionSource::Json {
                textures: sent.textures.clone(),
            }),
        },
        sent,
    )
}

fn session(reply: Reply) -> SessionId {
    match reply {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("expected a session, got {other:?}"),
    }
}

/// The session's model, written back out.
fn session_bytes(ed: &Editor, session: SessionId) -> Vec<u8> {
    ed.with_model(session, |model| model.to_clm_bytes().expect("write"))
        .expect("the session is open")
}

fn err(reply: Reply) -> (ErrorCode, String) {
    match reply {
        Reply::Err { code, message, .. } => (code, message),
        other => panic!("expected Err, got {other:?}"),
    }
}

fn ok(reply: Reply) {
    if let Reply::Err { code, message, .. } = reply {
        panic!("expected Ok, got {code:?}: {message}");
    }
}

// -------------------------------------------------------------------- tests

#[test]
fn json_and_clm_sources_construct_the_same_model() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();
    let ed = editor();
    let mut attachments = Attachments::none();
    attachments.insert("model", bytes);
    let from_file = session(
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
    );
    let from_json = session(create_json(&ed, &as_json(&file)));
    assert_eq!(session_bytes(&ed, from_json), session_bytes(&ed, from_file));
}

/// Every committed fixture survives the trip out through JSON and back, which
/// is the whole promise: a client may hold a model as a structure and its
/// images and lose nothing by it.
#[test]
fn every_fixture_round_trips_through_json() {
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).unwrap();
        let file = clm::decode(&bytes).unwrap();
        let want = catchlight_core::Model::from_clm_bytes(&bytes)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .to_clm_bytes()
            .unwrap();

        let ed = editor();
        let reply = create_json(&ed, &as_json(&file));
        if let Reply::Err { code, message, .. } = &reply {
            panic!("{name}: {code:?}: {message}");
        }
        assert_eq!(
            session_bytes(&ed, session(reply)),
            want,
            "{name} did not survive the json round trip"
        );
    }
}

#[test]
fn a_declared_texture_with_no_attachment_is_refused_naming_it() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();
    let mut sent = as_json(&file);
    let dropped = sent.attachments.remove(0).0;

    let ed = editor();
    let (code, message) = err(create_json(&ed, &sent));
    assert_eq!(code, ErrorCode::BadRequest);
    let id = dropped.strip_prefix("texture:").unwrap();
    assert!(
        message.contains(id),
        "the refusal names the texture: {message}"
    );
}

#[test]
fn an_attachment_no_texture_declares_is_refused_naming_it() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();
    let mut sent = as_json(&file);
    sent.attachments
        .push(("texture:tex-stray".into(), vec![1, 2, 3]));

    let ed = editor();
    let (code, message) = err(create_json(&ed, &sent));
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(message.contains("tex-stray"), "{message}");
}

/// A texture the structure references but nobody declared is the reader's
/// refusal, not a second check here.
#[test]
fn a_texture_the_structure_references_but_nobody_sent_is_refused_by_the_reader() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();
    let mut sent = as_json(&file);
    let gone = sent.textures.remove(0).texture;
    sent.attachments
        .retain(|(name, _)| name != &format!("texture:{gone}"));

    let ed = editor();
    let (_, message) = err(create_json(&ed, &sent));
    assert!(message.contains(gone.as_str()), "{message}");
}

/// The structure's roots install under the named parent as one undoable edit.
#[test]
fn a_structure_installs_under_the_parent_the_command_names() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();

    let ed = editor();
    let session = session(create_json(&ed, &as_json(&file)));
    let before = ed
        .with_model(session, |m| m.node_count())
        .expect("the session is open");

    // Cut a subtree out and re-import it under the root, with fresh Ids so it
    // cannot collide with what is already there.
    let addon = renamed_fragment(&file);
    let root = ed
        .with_model(session, |m| m.root().cloned())
        .expect("the session is open")
        .expect("a complete model has a root");
    let sent = as_json(&addon);
    ok(send_json(
        &ed,
        Command::ImportJson {
            session,
            parent: root,
            if_rev: ed.revision(session).unwrap(),
            textures: sent.textures.clone(),
        },
        &sent,
    ));

    let after = ed
        .with_model(session, |m| m.node_count())
        .expect("the session is open");
    assert!(after > before, "the subtree landed: {before} -> {after}");

    // One Undo restores the model before the installation.
    ok(ed.handle(req(
        9,
        Command::Undo {
            session,
            if_rev: ed.revision(session).unwrap(),
        },
    )));
    assert_eq!(
        ed.with_model(session, |m| m.node_count()).unwrap(),
        before,
        "one undo takes the install back"
    );
}

#[test]
fn repeated_factory_calls_publish_independent_sessions() {
    let file = clm::decode(&std::fs::read(models_dir().join("welded_seam.clm")).unwrap()).unwrap();
    let ed = editor();
    let sent = as_json(&file);
    let a = session(create_json(&ed, &sent));
    let b = session(create_json(&ed, &sent));
    assert_ne!(a, b);
    assert_eq!(session_bytes(&ed, a), session_bytes(&ed, b));
    assert_eq!(ed.revision(a), Some(0));
    assert_eq!(ed.revision(b), Some(0));
}

/// A byte extension's payload has no room in a JSON structure, so a marker in
/// one is refused by key rather than imported as a model missing its bytes.
#[test]
fn a_byte_extension_marker_is_refused_by_key() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let mut file = clm::decode(&bytes).unwrap();
    // A marker for bytes nothing carries: legal in a structure, unreadable
    // without the section a JSON import has no room for.
    file.doc.extensions.insert(
        catchlight_core::id::ExtensionKey::new("molan.thumb").unwrap(),
        clm::ClmExtension::Bytes(clm::ClmExtensionMarker {
            size: 3,
            hash: clm::extension_hash(b"abc"),
        }),
    );

    let ed = editor();
    let (_, message) = err(create_json(&ed, &as_json(&file)));
    assert!(message.contains("molan.thumb"), "{message}");
}

/// The same structure with every node and texture Id prefixed, so it can be
/// installed beside itself.
fn renamed_fragment(file: &ClmFile) -> ClmFile {
    let mut out = file.clone();
    let rename = |id: &str| NodeId::new(format!("copy{id}")).expect("a valid id");
    for node in &mut out.doc.nodes {
        node.id = rename(node.id.as_str());
        node.parent = node.parent.as_ref().map(|p| rename(p.as_str()));
    }
    // The roots' parents now name nodes the structure does not carry, which is
    // what makes it a fragment; the command's `parent` overrides them anyway.
    for texture in &mut out.textures {
        texture.id = catchlight_core::TexId::new(format!("copy{}", texture.id)).unwrap();
    }
    for node in &mut out.doc.nodes {
        if let clm::ClmNodeKind::Part(part) = &mut node.kind {
            part.albedo = part
                .albedo
                .as_ref()
                .map(|t| catchlight_core::TexId::new(format!("copy{t}")).unwrap());
        }
    }
    // An addon carries no params, no bindings on them and no animations;
    // `Model::extract` drops all three and install refuses them.
    out.doc.params.clear();
    out.doc.bindings.clear();
    out.doc.welds.clear();
    out.doc.animations.clear();
    out
}
