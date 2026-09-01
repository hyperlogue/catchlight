//! The browser's one door into the editor.
//!
//! `@catchlight/wasm` is this crate through `wasm-bindgen`. It adds no editing
//! logic of its own: it owns the [`Editor`] and hands JavaScript the protocol.
//!
//! Invariants this module carries:
//!
//! - **The JSON protocol is the base, and it is the same protocol.** [`handle`]
//!   takes one serialized [`Request`] and returns one serialized [`Reply`] —
//!   the messages the Unix socket and the CLI already speak. A command is
//!   added in one place and every client gets it. Hot paths (pointer events,
//!   per-frame work) get typed scalar entry points beside this one, never a
//!   second command vocabulary.
//!
//! - **A malformed request is still answered.** A body that does not parse is
//!   answered against the `id` the caller is waiting on, the way the socket
//!   transport does it, so a JS caller's promise never hangs.
//!
//! - **Staging is where the browser's asynchrony stops.** [`Storage`] is
//!   synchronous because [`Editor::handle`] is, and everything a browser reads
//!   bytes from — a file picker, OPFS, `fetch` — is not. So JS resolves its
//!   own promises first, [`put_bytes`] the result under a key, and only then
//!   sends the command naming that key. Nothing in Rust ever awaits, and the
//!   async lives entirely in the TypeScript layer where the platform APIs are.
//!
//! - **Staged bytes are not a cache.** [`StagedStorage`] holds what was put
//!   there and nothing else; it never falls back to a network or a disk. A key
//!   that was not staged is a plain `NotFound`, which is the honest answer —
//!   the layer that knows how to fetch it is above this one.
//!
//! [`handle`]: CatchlightEditor::handle
//! [`put_bytes`]: CatchlightEditor::put_bytes
//! [`Request`]: catchlight_editor_protocol::Request
//! [`Reply`]: catchlight_editor_protocol::Reply

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

#[cfg(target_arch = "wasm32")]
use catchlight_editor_protocol::SessionId;
use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId};
use catchlight_editor_server::{Editor, Storage};

#[cfg(target_arch = "wasm32")]
mod viewport;
#[cfg(target_arch = "wasm32")]
pub use viewport::Viewport;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Bytes JavaScript has already resolved, keyed by the same string the
/// protocol's `path` fields carry.
#[derive(Debug, Default)]
struct StagedStorage(Mutex<HashMap<String, Vec<u8>>>);

impl StagedStorage {
    fn put(&self, key: &str, bytes: Vec<u8>) {
        lock(&self.0).insert(key.to_string(), bytes);
    }

    fn take(&self, key: &str) -> Option<Vec<u8>> {
        lock(&self.0).remove(key)
    }

    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = lock(&self.0).keys().cloned().collect();
        keys.sort();
        keys
    }
}

impl Storage for StagedStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        lock(&self.0).get(key).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{key:?} was not staged; put its bytes before naming it"),
            )
        })
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        lock(&self.0).insert(key.to_string(), bytes.to_vec());
        Ok(())
    }
}

/// A poisoned staging map means a panic already happened somewhere else; the
/// bytes themselves are still whatever they were, so keep going rather than
/// cascading a second panic out through the wasm boundary.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The editor, its sessions, and the bytes staged for them.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct CatchlightEditor {
    /// Shared because a [`Viewport`] outlives any one call and draws the same
    /// sessions this answers commands about.
    editor: Arc<Editor>,
    staged: Arc<StagedStorage>,
}

impl Default for CatchlightEditor {
    fn default() -> Self {
        Self::new_inner()
    }
}

impl CatchlightEditor {
    fn new_inner() -> Self {
        let staged = Arc::new(StagedStorage::default());
        Self {
            editor: Arc::new(Editor::with_storage(staged.clone())),
            staged,
        }
    }

    /// The protocol round trip, as Rust. [`CatchlightEditor::handle`] is this
    /// with the string marshalling wasm-bindgen needs.
    fn dispatch(&self, request_json: &str) -> String {
        let reply = match serde_json::from_str::<Request>(request_json) {
            Ok(request) => self.editor.handle(request),
            // Answer against the id the caller is waiting on, not against 0.
            Err(e) => Reply::Err {
                id: serde_json::from_str::<RequestId>(request_json)
                    .map(|r| r.id)
                    .unwrap_or(0),
                code: ErrorCode::BadRequest,
                message: e.to_string(),
            },
        };
        // A Reply is plain data and always serializes. If it somehow does not,
        // answer in the shape the caller is already parsing rather than
        // returning something it will fail to read.
        serde_json::to_string(&reply).unwrap_or_else(|_| {
            r#"{"reply":"err","id":0,"code":"bad_request","message":"reply could not be serialized"}"#
                .to_string()
        })
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl CatchlightEditor {
    /// A fresh editor with no sessions and nothing staged.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new() -> Self {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        Self::new_inner()
    }

    /// Applies one JSON [`Request`] and returns its JSON [`Reply`].
    ///
    /// [`Request`]: catchlight_editor_protocol::Request
    /// [`Reply`]: catchlight_editor_protocol::Reply
    pub fn handle(&self, request_json: &str) -> String {
        self.dispatch(request_json)
    }

    /// Stages bytes under `key`, for a command that is about to name it.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = putBytes))]
    pub fn put_bytes(&self, key: &str, bytes: Vec<u8>) {
        self.staged.put(key, bytes);
    }

    /// Removes and returns what is staged under `key` — how a save's bytes
    /// leave for a download or an upload. Taking rather than copying is
    /// deliberate: a model's textures are the bulk of it, and a staging map
    /// that is never drained is a leak the size of the document.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = takeBytes))]
    pub fn take_bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.staged.take(key)
    }

    /// Every staged key, sorted. For diagnostics; nothing should need it to
    /// decide what to do next.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = stagedKeys))]
    pub fn staged_keys(&self) -> Vec<String> {
        self.staged.keys()
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl CatchlightEditor {
    /// Draws `session` on `canvas`, from now until the viewport is stopped.
    ///
    /// Asynchronous because WebGPU is: asking for an adapter and a device are
    /// both promises, and they are the only two. Everything after this — a
    /// resize, a camera move, a frame — is synchronous, which is what keeps the
    /// frame loop off the microtask queue.
    ///
    /// `session` is an `f64` because a session id crosses the JSON protocol as
    /// a `number` and has to be the same value here. A `u64` parameter would
    /// reach JavaScript as a `bigint`, so the one id would have two spellings
    /// and every call site would convert.
    pub async fn attach(
        &self,
        canvas: web_sys::HtmlCanvasElement,
        session: f64,
    ) -> Result<Viewport, JsValue> {
        Viewport::attach(self.editor.clone(), SessionId(session as u64), canvas)
            .await
            .map_err(|message| JsValue::from_str(&message))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use serde_json::json;

    fn call(editor: &CatchlightEditor, value: serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&editor.dispatch(&value.to_string())).unwrap()
    }

    #[test]
    fn a_command_round_trips_as_json() {
        let editor = CatchlightEditor::new();
        let reply = call(&editor, json!({"id": 1, "cmd": "session_new"}));
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["reply"], "ok");
        assert!(reply["body"]["session"].is_number(), "reply was {reply}");
    }

    #[test]
    fn a_malformed_body_is_answered_against_its_own_id() {
        let editor = CatchlightEditor::new();
        // A real id, an unparsable command.
        let reply = call(&editor, json!({"id": 42, "cmd": "no_such_command"}));
        assert_eq!(reply["id"], 42, "reply was {reply}");
        assert_eq!(reply["code"], "bad_request");
    }

    #[test]
    fn a_body_with_no_id_at_all_still_answers() {
        let editor = CatchlightEditor::new();
        let reply: serde_json::Value = serde_json::from_str(&editor.dispatch("{")).unwrap();
        assert_eq!(reply["id"], 0);
        assert_eq!(reply["code"], "bad_request");
    }

    #[test]
    fn staged_bytes_are_what_a_path_command_reads() {
        let editor = CatchlightEditor::new();
        let session = call(&editor, json!({"id": 1, "cmd": "session_new"}))["body"]["session"]
            .as_u64()
            .unwrap();

        // Save stages the document's bytes under the key it was given...
        let saved = call(
            &editor,
            json!({"id": 2, "cmd": "save", "session": session, "path": "akari.clm"}),
        );
        assert_eq!(saved["body"]["path"], "akari.clm");
        assert_eq!(editor.staged_keys(), vec!["akari.clm".to_string()]);

        // ...and taking them drains the staging map, so nothing is held twice.
        let bytes = editor.take_bytes("akari.clm").expect("save staged bytes");
        assert!(!bytes.is_empty());
        assert!(editor.staged_keys().is_empty());

        // Put them back and a fresh session opens from them.
        editor.put_bytes("akari.clm", bytes);
        let opened = call(
            &editor,
            json!({"id": 3, "cmd": "session_open", "path": "akari.clm"}),
        );
        assert!(opened["body"]["session"].is_number(), "reply was {opened}");
    }

    #[test]
    fn an_unstaged_key_says_so_rather_than_reading_nothing() {
        let editor = CatchlightEditor::new();
        let reply = call(
            &editor,
            json!({"id": 9, "cmd": "session_open", "path": "never-staged.clm"}),
        );
        assert_eq!(reply["id"], 9);
        assert!(
            reply["message"]
                .as_str()
                .unwrap_or_default()
                .contains("never-staged.clm"),
            "reply was {reply}"
        );
    }
}
