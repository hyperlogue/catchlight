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
//! - **The browser's asynchrony stops in TypeScript.** Everything a browser
//!   reads bytes from — a file picker, OPFS, `fetch` — is asynchronous, and
//!   [`Editor::handle_with`] is not. So JS awaits its own promises first and
//!   then calls in with the bytes in hand; nothing on the command path awaits.
//!   This crate holds exactly two futures, and neither is a command:
//!   `Gpu::acquire`, once per tab, and `Viewport::readback`, which only the
//!   browser smoke test calls.
//!
//! - **The tab's store is outward only.** [`WrittenBytes`] is where a `save`
//!   or an `export_manifest` leaves what it wrote, until [`take_bytes`] drains
//!   it into the browser's own storage or into a download. Nothing goes *in*
//!   through it — a command that needs bytes is handed them — so it is not a
//!   cache and a key nothing wrote is a plain `NotFound`.
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
//! [`take_bytes`]: CatchlightEditor::take_bytes
//! [`Event`]: catchlight_editor_protocol::Event
//! [`Request`]: catchlight_editor_protocol::Request
//! [`Reply`]: catchlight_editor_protocol::Reply

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_core::Manifest;
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

/// Every texture a manifest names, as it spells the reference.
///
/// A pure function of the JSON, with no session and no editor: the tab reads
/// a manifest it is about to import, resolves each reference against the
/// manifest's own location, and attaches the bytes under `texture:<ref>`. It
/// is here rather than in TypeScript so one reader decides what a manifest
/// says — the same [`Manifest`] the import itself parses.
///
/// The references come back verbatim, because that is the string the import
/// matches an attachment against. A manifest that does not parse is an error
/// carrying the reason.
pub fn manifest_requirements(json: &str) -> Result<Vec<String>, String> {
    let manifest = Manifest::from_json(json).map_err(|e| e.to_string())?;
    Ok(manifest.textures.into_iter().map(|t| t.path).collect())
}

/// [`manifest_requirements`] on the JS surface.
///
/// ```js
/// const refs = manifestRequirements(await file.text()); // string[]
/// ```
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = manifestRequirements)]
pub fn manifest_requirements_js(json: &str) -> Result<Vec<String>, JsValue> {
    manifest_requirements(json).map_err(|e| JsValue::from_str(&e))
}

/// The tab's own store: where a `save` or an `export_manifest` puts what it
/// wrote, until TypeScript drains it out into the browser's storage.
///
/// **Outward only.** A command that *reads* a key never reaches here — bytes
/// come in as attachments on the command that uses them — so a read is a plain
/// `NotFound` naming the key, and there is nothing a caller could put in.
#[derive(Debug, Default)]
struct WrittenBytes(Mutex<HashMap<String, Vec<u8>>>);

impl WrittenBytes {
    fn take(&self, key: &str) -> Option<Vec<u8>> {
        lock(&self.0).remove(key)
    }

    fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = lock(&self.0).keys().cloned().collect();
        keys.sort();
        keys
    }
}

impl Storage for WrittenBytes {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        lock(&self.0).get(key).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{key:?} is not in this tab's store"),
            )
        })
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        lock(&self.0).insert(key.to_string(), bytes.to_vec());
        Ok(())
    }
}

/// A poisoned map means a panic already happened somewhere else; the bytes
/// themselves are still whatever they were, so keep going rather than
/// cascading a second panic out through the wasm boundary.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The editor, its sessions, the bytes it has written, and the events it has
/// emitted since the page last looked.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct CatchlightEditor {
    editor: Editor,
    written: Arc<WrittenBytes>,
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
        let written = Arc::new(WrittenBytes::default());
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let editor = Editor::with_storage(written.clone());
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
            written,
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
    #[wasm_bindgen(
        js_name = handleWith,
        unchecked_return_type = "{ reply: string; payload?: Uint8Array }"
    )]
    pub fn handle_with(
        &self,
        request_json: &str,
        #[wasm_bindgen(unchecked_param_type = "Array<[string, Uint8Array]>")]
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

    /// Removes and returns what a command wrote under `key` — how a save's
    /// bytes leave for the browser's own storage or a download. Taking rather
    /// than copying is deliberate: a model's textures are the bulk of it, and
    /// a map that is never drained is a leak the size of the document.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = takeBytes))]
    pub fn take_bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.written.take(key)
    }

    /// Every key a command has written and nothing has drained, sorted.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = writtenKeys))]
    pub fn written_keys(&self) -> Vec<String> {
        self.written.keys()
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

    /// The store is outward only: a save leaves its bytes there, TypeScript
    /// takes them, and they come back into a document as an attachment.
    #[test]
    fn a_save_leaves_bytes_the_tab_can_take_and_import_again() {
        let editor = CatchlightEditor::new();
        let session = call(&editor, json!({"id": 1, "cmd": "session_new"}))["body"]["session"]
            .as_u64()
            .unwrap();

        let saved = call(
            &editor,
            json!({"id": 2, "cmd": "save", "session": session, "path": "akari.clm"}),
        );
        assert_eq!(saved["body"]["path"], "akari.clm");
        assert_eq!(editor.written_keys(), vec!["akari.clm".to_string()]);

        // Taking drains the map, so nothing is held twice.
        let bytes = editor.take_bytes("akari.clm").expect("save wrote bytes");
        assert!(!bytes.is_empty());
        assert!(editor.written_keys().is_empty());

        // And back in they go, attached to the command that reads them.
        let fresh = call(&editor, json!({"id": 3, "cmd": "session_new"}))["body"]["session"]
            .as_u64()
            .unwrap();
        let mut attachments = Attachments::none();
        attachments.insert("model", bytes);
        let (reply, _) = editor.dispatch_with(
            &json!({"id": 4, "cmd": "import_file", "session": fresh}).to_string(),
            attachments,
        );
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
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
        assert!(editor.written_keys().is_empty());
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
    fn a_key_nothing_wrote_says_so_rather_than_reading_nothing() {
        let editor = CatchlightEditor::new();
        let reply = call(
            &editor,
            json!({"id": 9, "cmd": "session_open", "path": "never-written.clm"}),
        );
        assert_eq!(reply["id"], 9);
        assert!(
            reply["message"]
                .as_str()
                .unwrap_or_default()
                .contains("never-written.clm"),
            "reply was {reply}"
        );
    }

    /// The pure reader the tab uses to know what a manifest needs, before it
    /// has a session to import into.
    #[test]
    fn a_manifests_references_come_back_verbatim() {
        let refs = manifest_requirements(
            r#"{"textures":[{"id":"face","path":"images/face.png"}],
                "nodes":[{"id":"face","kind":"part","texture":"face"}]}"#,
        )
        .expect("a manifest reads");
        assert_eq!(refs, vec!["images/face.png".to_string()]);
        assert!(manifest_requirements("not json").is_err());
    }
}
