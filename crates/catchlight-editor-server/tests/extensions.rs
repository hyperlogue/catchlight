#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Extensions on the wire: carried, never interpreted.
//!
//! A vendor files a value under a dotted key and catchlight reads none of it.
//! What is checked here is that the two kinds keep their shapes — JSON whole
//! and inline, bytes by hash with the payload beside the reply — that undo
//! covers a set the way it covers any other edit, and that a replica answers
//! the listing identically, because that is what keeps a tab's panel off the
//! wire.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::{
    Command, ErrorCode, ExtensionKey, ExtensionSet, ExtensionValueInfo, Reply, Request,
    ResponseBody, SessionId,
};
use catchlight_editor_server::{replica_query, Attachments, Editor, Payload, Storage};
use serde_json::json;

/// A store that is only a map: nothing here needs a filesystem.
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

// ------------------------------------------------------------------ harness

struct Fixture {
    editor: Editor,
    session: SessionId,
    next: u64,
}

impl Fixture {
    fn new() -> Self {
        let editor = Editor::with_storage(Arc::new(MemStorage::default()));
        let mut f = Self {
            editor,
            session: SessionId(0),
            next: 1,
        };
        f.session = match f.ok(Command::SessionNew { name: None }, Attachments::none()) {
            (ResponseBody::Session { session }, _) => session,
            (other, _) => panic!("expected Session, got {other:?}"),
        };
        f
    }

    fn reply(&mut self, command: Command, attachments: Attachments) -> (Reply, Option<Payload>) {
        self.next += 1;
        self.editor.handle_with(
            Request {
                id: self.next,
                command,
            },
            attachments,
        )
    }

    fn ok(
        &mut self,
        command: Command,
        attachments: Attachments,
    ) -> (ResponseBody, Option<Payload>) {
        match self.reply(command, attachments) {
            (Reply::Ok { body, .. }, payload) => (body, payload),
            (other, _) => panic!("expected Ok, got {other:?}"),
        }
    }

    fn code(&mut self, command: Command, attachments: Attachments) -> ErrorCode {
        match self.reply(command, attachments) {
            (Reply::Err { code, .. }, _) => code,
            (other, _) => panic!("expected Err, got {other:?}"),
        }
    }

    /// `extension_set` with a JSON value.
    fn set_json(&mut self, key: &str, value: serde_json::Value) -> (ResponseBody, Option<Payload>) {
        self.ok(
            Command::ExtensionSet {
                session: self.session,
                key: key.parse().unwrap(),
                value: ExtensionSet::Json { value },
            },
            Attachments::none(),
        )
    }

    /// `extension_set` with bytes attached.
    fn set_bytes(&mut self, key: &str, bytes: &[u8]) -> (ResponseBody, Option<Payload>) {
        let mut attachments = Attachments::none();
        attachments.insert("value", bytes.to_vec());
        self.ok(
            Command::ExtensionSet {
                session: self.session,
                key: key.parse().unwrap(),
                value: ExtensionSet::Bytes,
            },
            attachments,
        )
    }

    fn get(&mut self, key: &str) -> (ResponseBody, Option<Payload>) {
        self.ok(
            Command::ExtensionGet {
                session: self.session,
                key: key.parse().unwrap(),
            },
            Attachments::none(),
        )
    }

    fn list(&mut self) -> Vec<(String, ExtensionValueInfo)> {
        match self.ok(
            Command::Extensions {
                session: self.session,
            },
            Attachments::none(),
        ) {
            (ResponseBody::Extensions { extensions }, _) => extensions
                .into_iter()
                .map(|e| (e.key.to_string(), e.value))
                .collect(),
            (other, _) => panic!("expected Extensions, got {other:?}"),
        }
    }
}

fn key(s: &str) -> ExtensionKey {
    s.parse().unwrap()
}

// ------------------------------------------------------------------- tests

#[test]
fn a_json_extension_goes_in_whole_and_comes_back_whole() {
    let mut f = Fixture::new();
    let value = json!({"palette": ["#fff", "#000"], "version": 2});
    f.set_json("molan.caster", value.clone());

    match f.get("molan.caster") {
        (
            ResponseBody::Extension {
                key,
                value: ExtensionValueInfo::Json { value: got },
            },
            payload,
        ) => {
            assert_eq!(key, key_of("molan.caster"));
            assert_eq!(got, value, "the value is carried, not interpreted");
            assert!(payload.is_none(), "a json value is inline, not a payload");
        }
        (other, _) => panic!("expected a json extension, got {other:?}"),
    }
}

#[test]
fn a_byte_extension_arrives_attached_and_leaves_as_the_payload() {
    let mut f = Fixture::new();
    let bytes = b"a thumbnail, more or less".to_vec();
    f.set_bytes("molan.thumb", &bytes);

    match f.get("molan.thumb") {
        (
            ResponseBody::Extension {
                value: ExtensionValueInfo::Bytes { size, hash },
                ..
            },
            payload,
        ) => {
            let payload = payload.expect("the bytes are the payload");
            assert_eq!(payload.content_type, "application/octet-stream");
            assert_eq!(payload.bytes, bytes);
            assert_eq!(size as usize, bytes.len());
            // The hash a client compares against a marker in a structure feed.
            assert_eq!(
                hash,
                catchlight_core::formats::clm::extension_hash(&bytes),
                "the reply's hash is the marker's",
            );
        }
        (other, _) => panic!("expected a bytes extension, got {other:?}"),
    }
}

/// Exactly one of the two says what the value is: the kind decides, and a
/// command that says one and does the other is refused rather than guessed at.
#[test]
fn the_kind_and_the_attachment_have_to_agree() {
    let mut f = Fixture::new();

    // Bytes with nothing attached.
    let code = f.code(
        Command::ExtensionSet {
            session: f.session,
            key: key("molan.thumb"),
            value: ExtensionSet::Bytes,
        },
        Attachments::none(),
    );
    assert_eq!(code, ErrorCode::BadRequest);

    // JSON with bytes attached.
    let mut attachments = Attachments::none();
    attachments.insert("value", b"surprise".to_vec());
    let code = f.code(
        Command::ExtensionSet {
            session: f.session,
            key: key("molan.caster"),
            value: ExtensionSet::Json { value: json!(1) },
        },
        attachments,
    );
    assert_eq!(code, ErrorCode::BadRequest);

    // Neither landed.
    assert!(f.list().is_empty());
}

#[test]
fn an_attachment_under_another_name_never_reaches_the_command() {
    let mut f = Fixture::new();
    let mut attachments = Attachments::none();
    attachments.insert("bytes", b"wrong name".to_vec());
    let code = f.code(
        Command::ExtensionSet {
            session: f.session,
            key: key("molan.thumb"),
            value: ExtensionSet::Bytes,
        },
        attachments,
    );
    assert_eq!(code, ErrorCode::BadRequest);
}

#[test]
fn the_formats_own_prefix_is_not_a_vendors_to_author() {
    let mut f = Fixture::new();
    let code = f.code(
        Command::ExtensionSet {
            session: f.session,
            key: key("catchlight.thumb"),
            value: ExtensionSet::Json { value: json!(1) },
        },
        Attachments::none(),
    );
    assert_eq!(code, ErrorCode::ReservedExtension);
}

#[test]
fn a_key_the_model_does_not_carry_says_so() {
    let mut f = Fixture::new();
    f.set_json("molan.caster", json!(1));
    f.ok(
        Command::ExtensionDelete {
            session: f.session,
            key: key("molan.caster"),
        },
        Attachments::none(),
    );

    assert_eq!(
        f.code(
            Command::ExtensionDelete {
                session: f.session,
                key: key("molan.caster"),
            },
            Attachments::none(),
        ),
        ErrorCode::NoExtension,
        "deleting twice is an error, not a quiet no-op",
    );
    assert_eq!(
        f.code(
            Command::ExtensionGet {
                session: f.session,
                key: key("molan.caster"),
            },
            Attachments::none(),
        ),
        ErrorCode::NoExtension,
    );
}

#[test]
fn a_set_is_an_edit_and_undo_covers_it() {
    let mut f = Fixture::new();
    f.set_json("molan.caster", json!("first"));
    f.set_json("molan.caster", json!("second"));

    f.ok(Command::Undo { session: f.session }, Attachments::none());
    match f.get("molan.caster") {
        (
            ResponseBody::Extension {
                value: ExtensionValueInfo::Json { value },
                ..
            },
            _,
        ) => assert_eq!(value, json!("first"), "undo restored the previous value"),
        (other, _) => panic!("expected a json extension, got {other:?}"),
    }

    // And once more takes the key away entirely.
    f.ok(Command::Undo { session: f.session }, Attachments::none());
    assert!(f.list().is_empty());
}

#[test]
fn a_delete_is_undoable_too() {
    let mut f = Fixture::new();
    f.set_bytes("molan.thumb", b"held");
    f.ok(
        Command::ExtensionDelete {
            session: f.session,
            key: key("molan.thumb"),
        },
        Attachments::none(),
    );
    assert!(f.list().is_empty());

    f.ok(Command::Undo { session: f.session }, Attachments::none());
    assert_eq!(f.get("molan.thumb").1.expect("bytes").bytes, b"held");
}

#[test]
fn the_listing_reports_both_kinds_in_key_order() {
    let mut f = Fixture::new();
    f.set_bytes("molan.thumb", b"bytes here");
    f.set_json("molan.caster", json!({"v": 1}));

    let listed = f.list();
    let keys: Vec<&str> = listed.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["molan.caster", "molan.thumb"], "key order");
    assert!(matches!(listed[0].1, ExtensionValueInfo::Json { .. }));
    match &listed[1].1 {
        ExtensionValueInfo::Bytes { size, hash } => {
            assert_eq!(*size as usize, b"bytes here".len());
            assert_eq!(
                *hash,
                catchlight_core::formats::clm::extension_hash(b"bytes here")
            );
        }
        other => panic!("expected bytes, got {other:?}"),
    }
}

/// The listing is a pure read of the model, so a tab holding a replica answers
/// it without asking — and has to answer it the same.
#[test]
fn a_replica_lists_the_extensions_the_editor_lists() {
    let mut f = Fixture::new();
    f.set_bytes("molan.thumb", b"bytes here");
    f.set_json("molan.caster", json!({"v": 1}));

    let from_editor = f.list();
    let command = Command::Extensions { session: f.session };
    let from_replica = f
        .editor
        .with_model(f.session, |model| replica_query(model, &command))
        .unwrap()
        .unwrap();
    match from_replica {
        ResponseBody::Extensions { extensions } => {
            let mine: Vec<(String, ExtensionValueInfo)> = extensions
                .into_iter()
                .map(|e| (e.key.to_string(), e.value))
                .collect();
            assert_eq!(
                format!("{mine:?}"),
                format!("{from_editor:?}"),
                "one implementation, one answer",
            );
        }
        other => panic!("expected Extensions, got {other:?}"),
    }
}

fn key_of(s: &str) -> ExtensionKey {
    s.parse().unwrap()
}
