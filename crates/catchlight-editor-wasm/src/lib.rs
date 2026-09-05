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
//! - **Bytes cross once, inside the command that uses them.** `handleWith`
//!   takes the blobs beside the request and hands a payload back as bytes, so
//!   nothing is parked under a key for a later command to name and nothing has
//!   to be drained afterwards. It is the same [`Editor::handle_with`] every
//!   other transport funnels into; this crate only marshals.
//!
//! - **Staging is where the browser's asynchrony stops.** [`Storage`] is
//!   synchronous because [`Editor::handle`] is, and everything a browser reads
//!   bytes from — a file picker, OPFS, `fetch` — is not. So JS resolves its
//!   own promises first, [`put_bytes`] the result under a key, and only then
//!   sends the command naming that key. Nothing on the command path awaits,
//!   and the async lives in the TypeScript layer where the platform APIs are.
//!   This crate holds exactly two futures, and neither is a command:
//!   `Gpu::acquire`, once per tab, and `Viewport::readback`, which only the
//!   browser smoke test calls.
//!
//! - **Staged bytes are not a cache.** [`StagedStorage`] holds what was put
//!   there and nothing else; it never falls back to a network or a disk. A key
//!   that was not staged is a plain `NotFound`, which is the honest answer —
//!   the layer that knows how to fetch it is above this one.
//!
//! - **The tab holds a replica, and Rust never calls JavaScript.** What the
//!   page draws and reads per frame is a [`ReplicaState`] of one session, fed
//!   only by the two paths that type documents; commands are still the
//!   protocol. Nothing here takes a JS callback: an answer is a return value,
//!   and an [`Event`] the editor emitted waits in a queue until
//!   [`drain_events`] pulls it. A push would have to cross the boundary from
//!   inside a lock the editor holds, and would put the browser's scheduler in
//!   the middle of a command.
//!
//! [`handle`]: CatchlightEditor::handle
//! [`drain_events`]: CatchlightEditor::drain_events
//! [`put_bytes`]: CatchlightEditor::put_bytes
//! [`Event`]: catchlight_editor_protocol::Event
//! [`Request`]: catchlight_editor_protocol::Request
//! [`Reply`]: catchlight_editor_protocol::Reply

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId};
use catchlight_editor_server::{Attachments, Editor, Storage};

mod replica;
#[cfg(target_arch = "wasm32")]
pub use replica::Replica;
pub use replica::ReplicaState;

#[cfg(target_arch = "wasm32")]
mod gpu;
#[cfg(target_arch = "wasm32")]
pub use gpu::Gpu;

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

    /// Nothing is released here, and the `false` is load-bearing.
    ///
    /// A staged key in the tab is not a transient upload: it is a real key in
    /// the store above, staged from it, and a session opened from one is saved
    /// back under it — so `false` is what keeps that `file` on the session.
    /// The TypeScript layer drains the staging map itself once its command's
    /// reply is good (see `consumesKey` in `packages/core/src/in-tab.ts`),
    /// which is where the knowledge of which keys are the store's lives.
    fn release(&self, _key: &str) -> bool {
        false
    }
}

/// A poisoned staging map means a panic already happened somewhere else; the
/// bytes themselves are still whatever they were, so keep going rather than
/// cascading a second panic out through the wasm boundary.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The editor, its sessions, the bytes staged for them, and the events it has
/// emitted since the page last looked.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct CatchlightEditor {
    editor: Editor,
    staged: Arc<StagedStorage>,
    /// Serialized [`Event`]s in emission order, waiting to be pulled.
    ///
    /// [`Event`]: catchlight_editor_protocol::Event
    events: Arc<Mutex<Vec<String>>>,
}

impl Default for CatchlightEditor {
    fn default() -> Self {
        Self::new_inner()
    }
}

impl CatchlightEditor {
    fn new_inner() -> Self {
        let staged = Arc::new(StagedStorage::default());
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let editor = Editor::with_storage(staged.clone());
        let sink = events.clone();
        // The observer holds the queue and nothing else — never the editor,
        // which owns it, and never anything from JavaScript. The handle is
        // dropped on the floor: this subscription lasts exactly as long as the
        // editor it is attached to.
        editor.subscribe(Box::new(move |event| {
            if let Ok(json) = serde_json::to_string(event) {
                lock(&sink).push(json);
            }
        }));
        Self {
            editor,
            staged,
            events,
        }
    }

    /// The editor underneath, for a [`ReplicaState`] taking its session's
    /// model in-tab. Not on the JS surface: JavaScript reaches it through the
    /// protocol, and a replica names it by passing this whole object back.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The protocol round trip, as Rust. [`CatchlightEditor::handle`] is this
    /// with the string marshalling wasm-bindgen needs.
    fn dispatch(&self, request_json: &str) -> String {
        self.dispatch_with(request_json, Attachments::none()).0
    }

    /// The whole round trip: a request, the bytes that came with it, the reply
    /// and the bytes that go back. Everything on this crate's JS surface is
    /// this call plus marshalling.
    fn dispatch_with(
        &self,
        request_json: &str,
        attachments: Attachments,
    ) -> (String, Option<Vec<u8>>) {
        let (reply, payload) = match serde_json::from_str::<Request>(request_json) {
            Ok(request) => self.editor.handle_with(request, attachments),
            // Answer against the id the caller is waiting on, not against 0.
            Err(e) => (
                Reply::Err {
                    id: serde_json::from_str::<RequestId>(request_json)
                        .map(|r| r.id)
                        .unwrap_or(0),
                    code: ErrorCode::BadRequest,
                    message: e.to_string(),
                },
                None,
            ),
        };
        // A Reply is plain data and always serializes. If it somehow does not,
        // answer in the shape the caller is already parsing rather than
        // returning something it will fail to read.
        let json = serde_json::to_string(&reply).unwrap_or_else(|_| {
            r#"{"reply":"err","id":0,"code":"bad_request","message":"reply could not be serialized"}"#
                .to_string()
        });
        (json, payload.map(|p| p.bytes))
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

    /// The same round trip with bytes on both sides — the in-tab half of the
    /// editor's one `handle_with`.
    ///
    /// ```js
    /// const { reply, payload } = editor.handleWith(
    ///   JSON.stringify(request),
    ///   [["texture", new Uint8Array(bytes)]],
    /// );
    /// ```
    ///
    /// `attachments` is an array of `[name, Uint8Array]` pairs, and the answer
    /// is `{ reply: string, payload?: Uint8Array }` — `payload` present only
    /// when the command answers with bytes. Each blob crosses the boundary
    /// exactly once, into the command that uses it, and a payload comes back
    /// as bytes rather than through a key JS has to remember to drain. An
    /// entry that is not a `[string, Uint8Array]` pair throws, because that is
    /// a bug in the caller rather than something the editor refused.
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(js_name = handleWith)]
    pub fn handle_with(
        &self,
        request_json: &str,
        attachments: js_sys::Array,
    ) -> Result<js_sys::Object, JsValue> {
        let mut taken = Attachments::none();
        for entry in attachments.iter() {
            let pair: js_sys::Array = entry
                .dyn_into()
                .map_err(|_| JsValue::from_str("an attachment is a [name, Uint8Array] pair"))?;
            let name = pair
                .get(0)
                .as_string()
                .ok_or_else(|| JsValue::from_str("an attachment's name is a string"))?;
            let bytes: js_sys::Uint8Array = pair
                .get(1)
                .dyn_into()
                .map_err(|_| JsValue::from_str("an attachment's bytes are a Uint8Array"))?;
            taken.insert(name, bytes.to_vec());
        }
        let (reply, payload) = self.dispatch_with(request_json, taken);
        let out = js_sys::Object::new();
        js_sys::Reflect::set(
            &out,
            &JsValue::from_str("reply"),
            &JsValue::from_str(&reply),
        )?;
        if let Some(payload) = payload {
            js_sys::Reflect::set(
                &out,
                &JsValue::from_str("payload"),
                &js_sys::Uint8Array::from(payload.as_slice()),
            )?;
        }
        Ok(out)
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

    /// Every [`Event`] emitted since the last drain, each serialized, in the
    /// order they happened — and the queue is empty afterwards.
    ///
    /// A pull rather than a callback: an observer runs on the thread that ran
    /// the command, which for the in-tab editor is the middle of
    /// [`Self::handle`], and calling into JavaScript from there would let a
    /// listener re-enter the editor before the command it is hearing about has
    /// finished returning.
    ///
    /// [`Event`]: catchlight_editor_protocol::Event
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = drainEvents))]
    pub fn drain_events(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.events))
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

    /// The Rust half of `handleWith`: bytes in beside the command, bytes out
    /// beside the reply. What the JS surface adds to this is marshalling.
    #[test]
    fn attachments_reach_the_command_and_a_payload_comes_back() {
        let editor = CatchlightEditor::new();
        let session = call(&editor, json!({"id": 1, "cmd": "session_new"}))["body"]["session"]
            .as_u64()
            .unwrap();
        let root = call(
            &editor,
            json!({"id": 2, "cmd": "node_tree", "session": session}),
        )["body"]["root"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let part = call(
            &editor,
            json!({"id": 3, "cmd": "node_add", "session": session,
                   "parent": root, "kind": "part"}),
        )["body"]["node"]
            .as_str()
            .unwrap()
            .to_string();

        let mut attachments = Attachments::none();
        attachments.insert("texture", one_pixel_png());
        let (reply, payload) = editor.dispatch_with(
            &json!({"id": 4, "cmd": "texture_add", "session": session,
                    "node": part, "encoding": "png"})
            .to_string(),
            attachments,
        );
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
        assert!(reply["body"]["texture"].is_string(), "reply was {reply}");
        assert!(payload.is_none(), "texture_add answers with no bytes");
        // And nothing was parked under a key on the way through.
        assert!(editor.staged_keys().is_empty());
    }

    /// A command that carries bytes and is handed none is refused, so the
    /// no-attachment door cannot be used as a back way in.
    #[test]
    fn the_plain_door_still_refuses_a_byte_bearing_command() {
        let editor = CatchlightEditor::new();
        let session = call(&editor, json!({"id": 1, "cmd": "session_new"}))["body"]["session"]
            .as_u64()
            .unwrap();
        let reply = call(
            &editor,
            json!({"id": 2, "cmd": "import_manifest", "session": session}),
        );
        assert_eq!(reply["code"], "bad_request", "reply was {reply}");
    }

    fn one_pixel_png() -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([7, 7, 7, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
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
