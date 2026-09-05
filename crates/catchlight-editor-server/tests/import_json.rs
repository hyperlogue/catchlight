#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `import_json`: a structure document as JSON, its images beside it.
//!
//! The property that matters is that this is not a second importer. It is the
//! same two paths `import_file` takes, reached from a different envelope, so
//! the tests here mostly assert that the two agree — on the model they build,
//! on the pristine rule, and on what installing under a parent leaves behind.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use catchlight_core::formats::clm::{self, ClmFile};
use catchlight_editor_protocol::{
    Command, ErrorCode, ImportTexture, NodeId, Reply, Request, ResponseBody, SessionId,
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

fn new_session(ed: &Editor) -> SessionId {
    match ed.handle(req(1, Command::SessionNew { name: None })) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("expected a session, got {other:?}"),
    }
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

/// The JSON document, the `textures` list and the attachments one file turns
/// into — what a client authoring a model would put together itself.
struct AsJson {
    document: Vec<u8>,
    textures: Vec<ImportTexture>,
    attachments: Vec<(String, Vec<u8>)>,
}

fn as_json(file: &ClmFile) -> AsJson {
    AsJson {
        document: serde_json::to_vec(&file.doc).expect("the document serializes as json"),
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

fn send_json(ed: &Editor, session: SessionId, parent: Option<NodeId>, sent: &AsJson) -> Reply {
    let mut attachments = Attachments::none();
    attachments.insert("document", sent.document.clone());
    for (name, bytes) in &sent.attachments {
        attachments.insert(name.clone(), bytes.clone());
    }
    ed.handle_with(
        req(
            2,
            Command::ImportJson {
                session,
                parent,
                textures: sent.textures.clone(),
            },
        ),
        attachments,
    )
    .0
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

/// The two imports are one operation in two envelopes, so they have to land
/// on the same model, byte for byte.
#[test]
fn a_json_import_builds_what_the_same_file_would() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();

    let ed = editor();
    let from_file = new_session(&ed);
    let mut attachments = Attachments::none();
    attachments.insert("model", bytes.clone());
    ok(ed
        .handle_with(
            req(
                2,
                Command::ImportFile {
                    session: from_file,
                    parent: None,
                },
            ),
            attachments,
        )
        .0);

    let from_json = new_session(&ed);
    ok(send_json(&ed, from_json, None, &as_json(&file)));

    assert_eq!(
        session_bytes(&ed, from_json),
        session_bytes(&ed, from_file),
        "the two imports disagree about the model"
    );
}

/// Every committed fixture survives the trip out through JSON and back, which
/// is the whole promise: a client may hold a model as a document and its
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
        let session = new_session(&ed);
        let reply = send_json(&ed, session, None, &as_json(&file));
        if let Reply::Err { code, message, .. } = &reply {
            panic!("{name}: {code:?}: {message}");
        }
        assert_eq!(
            session_bytes(&ed, session),
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
    let session = new_session(&ed);
    let (code, message) = err(send_json(&ed, session, None, &sent));
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
    let session = new_session(&ed);
    let (code, message) = err(send_json(&ed, session, None, &sent));
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(message.contains("tex-stray"), "{message}");
}

/// A texture the document references but nobody declared is the reader's
/// refusal, not a second check here.
#[test]
fn a_texture_the_document_references_but_nobody_sent_is_refused_by_the_reader() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();
    let mut sent = as_json(&file);
    let gone = sent.textures.remove(0).texture;
    sent.attachments
        .retain(|(name, _)| name != &format!("texture:{gone}"));

    let ed = editor();
    let session = new_session(&ed);
    let (_, message) = err(send_json(&ed, session, None, &sent));
    assert!(message.contains(gone.as_str()), "{message}");
}

/// `parent` present installs the document's roots under that node, the same
/// way a file does, and it is an ordinary edit: dirty, and undoable.
#[test]
fn a_document_installs_under_the_parent_the_command_names() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();

    let ed = editor();
    let session = new_session(&ed);
    // A base to hang it off: the same model, imported whole first.
    ok(send_json(&ed, session, None, &as_json(&file)));
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
    ok(send_json(&ed, session, Some(root), &as_json(&addon)));

    let after = ed
        .with_model(session, |m| m.node_count())
        .expect("the session is open");
    assert!(after > before, "the subtree landed: {before} -> {after}");

    // An install is an ordinary edit, unlike a pristine replace.
    ok(ed.handle(req(9, Command::Undo { session })));
    assert_eq!(
        ed.with_model(session, |m| m.node_count()).unwrap(),
        before,
        "one undo takes the install back"
    );
}

/// The pristine rule is the same rule, whichever envelope the model arrived
/// in: a session that already holds a model refuses a whole-model import.
#[test]
fn importing_over_a_session_that_holds_a_model_is_refused() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let file = clm::decode(&bytes).unwrap();
    let sent = as_json(&file);

    let ed = editor();
    let session = new_session(&ed);
    ok(send_json(&ed, session, None, &sent));
    let (code, _) = err(send_json(&ed, session, None, &sent));
    assert_eq!(code, ErrorCode::NotEmpty);
}

/// A byte extension's payload has no room in a JSON document, so a marker in
/// one is refused by key rather than imported as a model missing its bytes.
#[test]
fn a_byte_extension_marker_is_refused_by_key() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).unwrap();
    let mut file = clm::decode(&bytes).unwrap();
    // A marker for bytes nothing carries: legal in a document, unreadable
    // without the section a JSON import has no room for.
    file.doc.extensions.insert(
        catchlight_core::id::ExtensionKey::new("molan.thumb").unwrap(),
        clm::ClmExtension::Bytes(clm::ClmExtensionMarker {
            size: 3,
            hash: clm::extension_hash(b"abc"),
        }),
    );

    let ed = editor();
    let session = new_session(&ed);
    let (_, message) = err(send_json(&ed, session, None, &as_json(&file)));
    assert!(message.contains("molan.thumb"), "{message}");
}

/// The same document with every node and texture Id prefixed, so it can be
/// installed beside itself.
fn renamed_fragment(file: &ClmFile) -> ClmFile {
    let mut out = file.clone();
    let rename = |id: &str| NodeId::new(format!("copy{id}")).expect("a valid id");
    for node in &mut out.doc.nodes {
        node.id = rename(node.id.as_str());
        node.parent = node.parent.as_ref().map(|p| rename(p.as_str()));
    }
    // The roots' parents now name nodes the document does not carry, which is
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
