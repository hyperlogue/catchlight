#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a test aborts on a broken fixture"
)]

//! Extensions on the wire: carried, never interpreted.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::{
    Command, ErrorCode, ExtensionKey, ExtensionRef, ExtensionSet, ExtensionValueInfo, ParamId,
    Rename, Reply, Request, ResponseBody, SessionId,
};
use catchlight_editor_server::{Attachments, Editor, Payload, Storage, replica_query};
use serde_json::json;

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
        f.session = match f.ok(
            Command::SessionNew {
                source: None,
                name: None,
            },
            Attachments::none(),
        ) {
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

#[test]
fn a_json_extension_goes_in_whole_and_comes_back_whole() {
    let mut f = Fixture::new();
    let value = json!({"palette": ["#fff", "#000"], "version": 2});
    f.set_json("acme.camera", value.clone());

    match f.get("acme.camera") {
        (
            ResponseBody::Extension {
                key,
                value: ExtensionValueInfo::Json { value: got },
            },
            payload,
        ) => {
            assert_eq!(key, key_of("acme.camera"));
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
    f.set_bytes("acme.thumb", &bytes);

    match f.get("acme.thumb") {
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

    let code = f.code(
        Command::ExtensionSet {
            session: f.session,
            key: key("acme.thumb"),
            value: ExtensionSet::Bytes,
        },
        Attachments::none(),
    );
    assert_eq!(code, ErrorCode::BadRequest);

    let mut attachments = Attachments::none();
    attachments.insert("value", b"surprise".to_vec());
    let code = f.code(
        Command::ExtensionSet {
            session: f.session,
            key: key("acme.camera"),
            value: ExtensionSet::Json { value: json!(1) },
        },
        attachments,
    );
    assert_eq!(code, ErrorCode::BadRequest);

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
            key: key("acme.thumb"),
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
    f.set_json("acme.camera", json!(1));
    f.ok(
        Command::ExtensionDelete {
            session: f.session,
            key: key("acme.camera"),
        },
        Attachments::none(),
    );

    assert_eq!(
        f.code(
            Command::ExtensionDelete {
                session: f.session,
                key: key("acme.camera"),
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
                key: key("acme.camera"),
            },
            Attachments::none(),
        ),
        ErrorCode::NoExtension,
    );
}

#[test]
fn a_set_is_an_edit_and_undo_covers_it() {
    let mut f = Fixture::new();
    f.set_json("acme.camera", json!("first"));
    f.set_json("acme.camera", json!("second"));

    f.ok(
        Command::Undo {
            session: f.session,
            if_rev: f.editor.revision(f.session).unwrap(),
        },
        Attachments::none(),
    );
    match f.get("acme.camera") {
        (
            ResponseBody::Extension {
                value: ExtensionValueInfo::Json { value },
                ..
            },
            _,
        ) => assert_eq!(value, json!("first"), "undo restored the previous value"),
        (other, _) => panic!("expected a json extension, got {other:?}"),
    }

    f.ok(
        Command::Undo {
            session: f.session,
            if_rev: f.editor.revision(f.session).unwrap(),
        },
        Attachments::none(),
    );
    assert!(f.list().is_empty());
}

#[test]
fn a_delete_is_undoable_too() {
    let mut f = Fixture::new();
    f.set_bytes("acme.thumb", b"held");
    f.ok(
        Command::ExtensionDelete {
            session: f.session,
            key: key("acme.thumb"),
        },
        Attachments::none(),
    );
    assert!(f.list().is_empty());

    f.ok(
        Command::Undo {
            session: f.session,
            if_rev: f.editor.revision(f.session).unwrap(),
        },
        Attachments::none(),
    );
    assert_eq!(f.get("acme.thumb").1.expect("bytes").bytes, b"held");
}

#[test]
fn the_listing_reports_both_kinds_in_key_order() {
    let mut f = Fixture::new();
    f.set_bytes("acme.thumb", b"bytes here");
    f.set_json("acme.camera", json!({"v": 1}));

    let listed = f.list();
    let keys: Vec<&str> = listed.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["acme.camera", "acme.thumb"], "key order");
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
    f.set_bytes("acme.thumb", b"bytes here");
    f.set_json("acme.camera", json!({"v": 1}));

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

impl Fixture {
    fn add_param(&mut self, id: &str) -> ParamId {
        let param = ParamId::new(id).unwrap();
        self.ok(
            Command::ParamAdd {
                session: self.session,
                name: id.into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                param: Some(param.clone()),
            },
            Attachments::none(),
        );
        param
    }

    fn set_maintained(
        &mut self,
        key: &str,
        refs: Vec<ExtensionRef>,
        value: Option<serde_json::Value>,
        bytes: Option<&[u8]>,
    ) -> Reply {
        let mut attachments = Attachments::none();
        if let Some(bytes) = bytes {
            attachments.insert("value", bytes.to_vec());
        }
        self.reply(
            Command::ExtensionSet {
                session: self.session,
                key: key.parse().unwrap(),
                value: ExtensionSet::Maintained {
                    version: 2,
                    refs,
                    value,
                },
            },
            attachments,
        )
        .0
    }
}

#[test]
fn a_maintained_vendor_extension_is_set_listed_and_fetched_with_its_references() {
    let mut f = Fixture::new();
    let eye = f.add_param("eye");
    let refs = vec![
        ExtensionRef::Param(eye),
        ExtensionRef::Param(ParamId::new("ghost").unwrap()),
    ];
    let reply = f.set_maintained("acme.framing", refs.clone(), Some(json!({"shot": 0})), None);
    assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
    let reply = f.set_maintained("acme.cache", Vec::new(), None, Some(b"cached"));
    assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");

    let listed = f.list();
    match &listed[1] {
        (
            key,
            ExtensionValueInfo::Maintained {
                version,
                refs: listed_refs,
                value,
                size,
                ..
            },
        ) => {
            assert_eq!(key, "acme.framing");
            assert_eq!(*version, 2);
            assert_eq!(*listed_refs, refs, "an unresolved reference is kept");
            assert_eq!(*value, Some(json!({"shot": 0})));
            assert_eq!(*size, None);
        }
        other => panic!("expected a maintained value, got {other:?}"),
    }

    let (body, payload) = f.get("acme.cache");
    match body {
        ResponseBody::Extension {
            value:
                ExtensionValueInfo::Maintained {
                    version,
                    size,
                    value,
                    ..
                },
            ..
        } => {
            assert_eq!((version, size, value), (2, Some(6), None));
        }
        other => panic!("expected a maintained value, got {other:?}"),
    }
    assert_eq!(payload.unwrap().bytes, b"cached");

    // The table travels in the file's own form.
    let wire = serde_json::to_value(&listed[1].1).unwrap();
    assert_eq!(wire["refs"], json!([{"param": "eye"}, {"param": "ghost"}]));
}

#[test]
fn a_maintained_extensions_references_follow_a_rename_and_a_removal() {
    let mut f = Fixture::new();
    let eye = f.add_param("eye");
    let brow = f.add_param("brow");
    f.set_maintained(
        "acme.framing",
        vec![
            ExtensionRef::Param(eye.clone()),
            ExtensionRef::Param(brow.clone()),
        ],
        Some(json!({})),
        None,
    );
    let left = ParamId::new("eye_left").unwrap();
    f.ok(
        Command::RenameId {
            session: f.session,
            rename: Rename::Param {
                from: eye,
                to: left.clone(),
            },
        },
        Attachments::none(),
    );
    f.ok(
        Command::ParamDelete {
            session: f.session,
            param: brow.clone(),
        },
        Attachments::none(),
    );
    match &f.list()[0].1 {
        ExtensionValueInfo::Maintained { refs, .. } => assert_eq!(
            *refs,
            vec![
                ExtensionRef::Param(left),
                ExtensionRef::Removed(Box::new(ExtensionRef::Param(brow))),
            ]
        ),
        other => panic!("expected a maintained value, got {other:?}"),
    }
}

#[test]
fn a_maintained_value_needs_exactly_one_payload_and_a_vendor_key() {
    let mut f = Fixture::new();
    let code = |reply: Reply| match reply {
        Reply::Err { code, .. } => code,
        other => panic!("expected Err, got {other:?}"),
    };
    assert_eq!(
        code(f.set_maintained("acme.x", Vec::new(), None, None)),
        ErrorCode::BadRequest
    );
    assert_eq!(
        code(f.set_maintained("acme.x", Vec::new(), Some(json!(1)), Some(b"b"))),
        ErrorCode::BadRequest
    );
    let twice_removed = ExtensionRef::Removed(Box::new(ExtensionRef::Removed(Box::new(
        ExtensionRef::Param(ParamId::new("p").unwrap()),
    ))));
    assert_eq!(
        code(f.set_maintained("acme.x", vec![twice_removed], Some(json!(1)), None)),
        ErrorCode::BadRequest
    );
    assert_eq!(
        code(f.set_maintained("catchlight.humanoid", Vec::new(), Some(json!({})), None)),
        ErrorCode::ReservedExtension
    );
    assert!(f.list().is_empty());
}
