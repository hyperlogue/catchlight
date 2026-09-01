#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A `path` on the wire is a storage key, not a filesystem path.
//!
//! These drive the document commands against a store with no filesystem
//! behind it — the shape the browser and the cloud both use. If these pass,
//! the same commands the CLI sends over the Unix socket work unchanged in
//! wasm, which is the whole point of the seam.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::{Command, Reply, Request, ResponseBody, SessionId};
use catchlight_editor_server::{Editor, Storage};

/// A store that is only a map. No paths, no directories, no filesystem.
#[derive(Debug, Default)]
struct MemStorage(Mutex<HashMap<String, Vec<u8>>>);

impl MemStorage {
    fn with(key: &str, bytes: Vec<u8>) -> Arc<Self> {
        let s = Self::default();
        s.0.lock().unwrap().insert(key.to_string(), bytes);
        Arc::new(s)
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(key).cloned()
    }
}

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

/// An empty model's `.clm` bytes — enough to prove the path, and it needs no
/// LFS fixture.
fn clm_bytes() -> Vec<u8> {
    let store = Arc::new(MemStorage::default());
    let editor = Editor::with_storage(store.clone());
    let session = new_session(&editor);
    ok(&editor.handle(req(
        2,
        Command::Save {
            session,
            path: Some("scratch.clm".into()),
        },
    )));
    store.get("scratch.clm").expect("save wrote the key")
}

fn req(id: u64, command: Command) -> Request {
    Request { id, command }
}

fn ok(reply: &Reply) -> &ResponseBody {
    match reply {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn new_session(editor: &Editor) -> SessionId {
    match ok(&editor.handle(req(1, Command::SessionNew { name: None }))) {
        ResponseBody::Session { session } => *session,
        other => panic!("expected Session, got {other:?}"),
    }
}

#[test]
fn a_session_opens_from_a_store_with_no_filesystem() {
    let store = MemStorage::with("project/akari.clm", clm_bytes());
    let editor = Editor::with_storage(store);

    let session = match ok(&editor.handle(req(
        1,
        Command::SessionOpen {
            path: "project/akari.clm".into(),
        },
    ))) {
        ResponseBody::Session { session } => *session,
        other => panic!("expected Session, got {other:?}"),
    };

    // The title comes from the key's last segment, not from any path logic.
    match ok(&editor.handle(req(2, Command::Status { session }))) {
        ResponseBody::Status { status } => assert_eq!(status.title, "akari"),
        other => panic!("expected Status, got {other:?}"),
    }
}

#[test]
fn save_without_a_path_reuses_the_key_the_session_was_opened_from() {
    let store = MemStorage::with("project/akari.clm", clm_bytes());
    let editor = Editor::with_storage(store.clone());
    let session = match ok(&editor.handle(req(
        1,
        Command::SessionOpen {
            path: "project/akari.clm".into(),
        },
    ))) {
        ResponseBody::Session { session } => *session,
        other => panic!("expected Session, got {other:?}"),
    };

    match ok(&editor.handle(req(
        2,
        Command::Save {
            session,
            path: None,
        },
    ))) {
        ResponseBody::Saved { path } => assert_eq!(path, "project/akari.clm"),
        other => panic!("expected Saved, got {other:?}"),
    }
    assert!(store.get("project/akari.clm").is_some());
}

#[test]
fn a_missing_key_is_an_error_against_the_request_that_asked_for_it() {
    let editor = Editor::with_storage(Arc::new(MemStorage::default()));
    match editor.handle(req(
        7,
        Command::SessionOpen {
            path: "nope.clm".into(),
        },
    )) {
        Reply::Err { id, message, .. } => {
            assert_eq!(id, 7);
            assert!(message.contains("nope.clm"), "message was {message:?}");
        }
        other => panic!("expected Err, got {other:?}"),
    }
}

#[test]
fn no_storage_is_a_named_error_rather_than_a_panic() {
    let editor = Editor::with_storage(Arc::new(catchlight_editor_server::NoStorage));
    match editor.handle(req(
        1,
        Command::SessionOpen {
            path: "anything.clm".into(),
        },
    )) {
        Reply::Err { message, .. } => assert!(
            message.contains("no storage backend configured"),
            "message was {message:?}"
        ),
        other => panic!("expected Err, got {other:?}"),
    }
}
