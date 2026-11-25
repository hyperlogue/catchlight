//! The browser's one door into the editor, exported as `@catchlight/wasm`.
//!
//! JS calls in with bytes already in hand, so nothing on the command path
//! awaits. Rust never calls JavaScript: events queue until [`drain_events`]
//! pulls them, since a push would run inside the editor's lock and let a
//! listener re-enter it mid-command.
//!
//! [`drain_events`]: CatchlightEditor::drain_events

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_core::Manifest;
use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId};
use catchlight_editor_server::{Attachments, Editor, Storage};

pub mod authoring;
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

/// Every texture a manifest names, verbatim, since the import matches
/// attachments against those strings.
pub fn manifest_requirements(json: &str) -> Result<Vec<String>, String> {
    let manifest = Manifest::from_json(json).map_err(|e| e.to_string())?;
    Ok(manifest.textures.into_iter().map(|t| t.path).collect())
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(js_name = manifestRequirements)]
pub fn manifest_requirements_js(json: &str) -> Result<Vec<String>, JsValue> {
    manifest_requirements(json).map_err(|e| JsValue::from_str(&e))
}

/// Where a `save` or `export_manifest` leaves its bytes until TypeScript
/// drains them. Outward only: reading a key is a plain `NotFound`.
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

/// A poisoned map's bytes are still intact; keep going rather than cascade a
/// second panic across the wasm boundary.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct CatchlightEditor {
    editor: Editor,
    written: Arc<WrittenBytes>,
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
        // A version's time; the server cannot read the clock on wasm32.
        #[cfg(target_arch = "wasm32")]
        let editor = editor.with_clock(|| js_sys::Date::now() as u64);
        let sink = events.clone();
        // The observer holds only the queue; the subscription lives as long as the
        // editor.
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

    /// Not on the JS surface; a replica names it by being passed this object.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    fn dispatch(&self, request_json: &str) -> String {
        self.dispatch_with(request_json, Attachments::none()).0
    }

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
                    op_index: None,
                    limit: None,
                    id: serde_json::from_str::<RequestId>(request_json)
                        .map(|r| r.id)
                        .unwrap_or(0),
                    code: ErrorCode::BadRequest,
                    message: e.to_string(),
                },
                None,
            ),
        };
        // A Reply always serializes; if not, answer in the shape the caller parses.
        let json = serde_json::to_string(&reply).unwrap_or_else(|_| {
            r#"{"reply":"err","id":0,"code":"bad_request","message":"reply could not be serialized"}"#
                .to_string()
        });
        (json, payload.map(|p| p.bytes))
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl CatchlightEditor {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new() -> Self {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        Self::new_inner()
    }

    pub fn handle(&self, request_json: &str) -> String {
        self.dispatch(request_json)
    }

    /// `attachments` is an array of `[name, Uint8Array]` pairs; the answer is
    /// `{ reply: string, payload?: Uint8Array }`. A malformed entry throws.
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

    /// Removes and returns what a command wrote under `key`. Taking, not
    /// copying: an undrained map leaks the size of the model.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = takeBytes))]
    pub fn take_bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.written.take(key)
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(js_name = writtenKeys))]
    pub fn written_keys(&self) -> Vec<String> {
        self.written.keys()
    }

    /// Every serialized event since the last drain, in order. A pull, because a
    /// callback from inside `handle` could re-enter the editor.
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
        let reply = call(&editor, json!({"id": 1, "cmd": "session_create"}));
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["reply"], "ok");
        assert!(reply["body"]["session"].is_number(), "reply was {reply}");
    }

    #[test]
    fn a_malformed_body_is_answered_against_its_own_id() {
        let editor = CatchlightEditor::new();
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
    fn a_save_leaves_bytes_the_tab_can_take_and_import_again() {
        let editor = CatchlightEditor::new();
        let session = call(&editor, json!({"id": 1, "cmd": "session_create"}))["body"]["session"]
            .as_u64()
            .unwrap();

        let saved = call(
            &editor,
            json!({"id": 2, "cmd": "session_save", "session": session, "path": "akari.clm"}),
        );
        assert_eq!(saved["body"]["path"], "akari.clm");
        assert_eq!(editor.written_keys(), vec!["akari.clm".to_string()]);

        let bytes = editor.take_bytes("akari.clm").expect("save wrote bytes");
        assert!(!bytes.is_empty());
        assert!(editor.written_keys().is_empty());

        let mut attachments = Attachments::none();
        attachments.insert("model", bytes);
        let (reply, _) = editor.dispatch_with(
            &json!({"id": 4, "cmd": "session_create", "source": {"format":"clm"}}).to_string(),
            attachments,
        );
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
    }

    #[test]
    fn attachments_reach_the_command_and_a_payload_comes_back() {
        let editor = CatchlightEditor::new();
        let session = call(&editor, json!({"id": 1, "cmd": "session_create"}))["body"]["session"]
            .as_u64()
            .unwrap();
        let root = call(
            &editor,
            json!({"id": 2, "cmd": "node_tree_get", "session": session}),
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
        assert!(editor.written_keys().is_empty());
    }

    #[test]
    fn the_plain_door_still_refuses_a_byte_bearing_command() {
        let editor = CatchlightEditor::new();
        let reply = call(
            &editor,
            json!({"id": 2, "cmd": "session_create", "source": {"format":"manifest"}}),
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
