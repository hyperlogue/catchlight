#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Extensions: what a file carries for a vendor, and what the reader refuses.
//!
//! The property that matters most is the boring one — an extension survives a
//! save unchanged, and a save→save is a no-op — because everything a vendor
//! builds on top of the format rests on it. The rest is the reader refusing
//! every shape it cannot vouch for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use catchlight_core::formats::clm::{
    self as clm, ClmExtension, ClmExtensionBlob, ClmExtensionMarker, ClmFile, ClmStructure,
    MAX_EXTENSION_BYTES,
};
use catchlight_core::id::ExtensionKey;
use catchlight_core::{ExtensionValue, InstallError, Model, ModelError};

fn models_dir() -> PathBuf {
    // crates/catchlight-core/ -> crates/ -> workspace root.
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

fn key(text: &str) -> ExtensionKey {
    ExtensionKey::new(text).expect("a valid extension key")
}

fn json() -> ExtensionValue {
    ExtensionValue::Json(serde_json::json!({
        "rig": "v2",
        "count": 3,
        "ratio": 0.5,
        "flags": [true, false, null],
    }))
}

fn bytes() -> ExtensionValue {
    // Not an image and not text: an extension's bytes are opaque.
    ExtensionValue::Bytes(Arc::from(
        (0u8..=255).cycle().take(4096).collect::<Vec<u8>>(),
    ))
}

/// The one that everything else rests on: a model carrying both kinds writes,
/// reads back the same, and writes the same bytes the second time.
#[test]
fn every_fixture_carries_both_kinds_through_a_save() {
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let mut model = Model::from_clm_bytes(&std::fs::read(&path).unwrap())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        model.set_extension(key("molan.caster"), json()).unwrap();
        model.set_extension(key("molan.thumb"), bytes()).unwrap();

        let first = model
            .to_clm_bytes()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let read = Model::from_clm_bytes(&first).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            read.extensions(),
            model.extensions(),
            "{name}: extensions changed across a save"
        );
        let second = read.to_clm_bytes().unwrap();
        assert_eq!(first, second, "{name}: save->save is not a no-op");
    }
}

/// A model with no extensions writes what it always wrote: the field is
/// skipped when empty and the section is absent, so no committed fixture
/// needs regenerating.
#[test]
fn a_model_without_extensions_writes_the_bytes_it_always_did() {
    for path in fixtures() {
        let committed = std::fs::read(&path).unwrap();
        let model = Model::from_clm_bytes(&committed).unwrap();
        assert!(model.extensions().is_empty());
        assert_eq!(
            model.to_clm_bytes().unwrap(),
            committed,
            "{} would be rewritten",
            path.display()
        );
    }
}

#[test]
fn setting_and_deleting_moves_the_generation() {
    let mut model = Model::new();
    let before = model.generation();
    model.set_extension(key("molan.caster"), json()).unwrap();
    let after_set = model.generation();
    assert!(after_set > before, "a set is a model edit");
    model.delete_extension(&key("molan.caster")).unwrap();
    assert!(model.generation() > after_set, "so is a delete");
    assert!(model.extensions().is_empty());
}

#[test]
fn deleting_a_key_the_model_does_not_carry_is_an_error() {
    let mut model = Model::new();
    let err = model.delete_extension(&key("molan.absent")).unwrap_err();
    assert!(
        matches!(&err, ModelError::UnknownExtension(k) if k == "molan.absent"),
        "{err}"
    );
}

/// `catchlight.` is the format's own prefix. A reader takes such a key,
/// because the format may write one some day; nothing else may author one.
#[test]
fn a_reserved_key_is_refused_to_an_author_and_accepted_from_a_file() {
    let mut model = Model::new();
    let reserved = key("catchlight.thumbnail");
    assert!(reserved.is_reserved());
    let err = model.set_extension(reserved.clone(), json()).unwrap_err();
    assert!(
        matches!(&err, ModelError::ReservedExtension(k) if k == "catchlight.thumbnail"),
        "{err}"
    );
    assert!(model.delete_extension(&reserved).is_err());

    // The same key straight off the wire loads.
    let doc = ClmStructure {
        extensions: [(reserved.clone(), ClmExtension::Json(serde_json::json!(1)))]
            .into_iter()
            .collect(),
        ..root_structure()
    };
    let read = Model::from_clm_file(&ClmFile {
        doc,
        textures: Vec::new(),
        extensions: Vec::new(),
    })
    .expect("a reserved key loads");
    assert!(read.extensions().contains_key(&reserved));
}

#[test]
fn a_byte_value_over_the_cap_is_refused() {
    let mut model = Model::new();
    let too_big = vec![0u8; MAX_EXTENSION_BYTES + 1];
    let err = model
        .set_extension(key("molan.huge"), ExtensionValue::Bytes(Arc::from(too_big)))
        .unwrap_err();
    assert!(
        matches!(&err, ModelError::ExtensionTooLarge { key, .. } if key == "molan.huge"),
        "{err}"
    );
}

// ---- what the reader refuses --------------------------------------------

/// A one-node structure, the smallest complete model.
fn root_structure() -> ClmStructure {
    use catchlight_core::formats::clm::{ClmNode, ClmNodeKind, ClmTransform};
    ClmStructure {
        nodes: vec![ClmNode {
            id: catchlight_core::NodeId::new("root").unwrap(),
            parent: None,
            name: "root".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform {
                translation: [0.0; 3],
                rotation: [0.0; 3],
                scale: [1.0, 1.0],
            },
            lock_to_root: false,
            kind: ClmNodeKind::Group,
        }],
        ..ClmStructure::default()
    }
}

fn with_extensions(entries: Vec<(ExtensionKey, ClmExtension)>) -> ClmStructure {
    ClmStructure {
        extensions: entries.into_iter().collect::<BTreeMap<_, _>>(),
        ..root_structure()
    }
}

fn marker(data: &[u8]) -> ClmExtension {
    ClmExtension::Bytes(ClmExtensionMarker {
        size: data.len() as u64,
        hash: clm::extension_hash(data),
    })
}

fn blob(k: &str, data: &[u8]) -> ClmExtensionBlob {
    ClmExtensionBlob {
        key: key(k),
        data: data.to_vec(),
    }
}

/// `check_extensions` is what both the reader and the writer run, so driving
/// it directly is driving both.
fn refusal(doc: &ClmStructure, blobs: &[ClmExtensionBlob]) -> clm::ClmError {
    clm::check_extensions(doc, blobs).expect_err("this pairing must be refused")
}

#[test]
fn a_marker_without_its_bytes_is_refused_by_key() {
    let doc = with_extensions(vec![(key("molan.thumb"), marker(b"abc"))]);
    assert!(
        matches!(refusal(&doc, &[]), clm::ClmError::ExtensionBytesMissing { key } if key == "molan.thumb")
    );
}

#[test]
fn bytes_nothing_names_are_refused_by_key() {
    // No entry at all, and an entry of the wrong kind, are both orphans.
    let empty = root_structure();
    assert!(
        matches!(refusal(&empty, &[blob("molan.thumb", b"abc")]), clm::ClmError::ExtensionBytesOrphan { key } if key == "molan.thumb")
    );
    let as_json = with_extensions(vec![(
        key("molan.thumb"),
        ClmExtension::Json(serde_json::json!(1)),
    )]);
    assert!(matches!(
        refusal(&as_json, &[blob("molan.thumb", b"abc")]),
        clm::ClmError::ExtensionBytesOrphan { .. }
    ));
}

#[test]
fn a_size_or_a_hash_that_disagrees_with_the_marker_is_refused() {
    let doc = with_extensions(vec![(key("molan.thumb"), marker(b"abc"))]);
    assert!(matches!(
        refusal(&doc, &[blob("molan.thumb", b"abcd")]),
        clm::ClmError::ExtensionSizeMismatch { .. }
    ));
    // Same length, different bytes: only the hash catches it.
    assert!(matches!(
        refusal(&doc, &[blob("molan.thumb", b"abd")]),
        clm::ClmError::ExtensionHashMismatch { key } if key == "molan.thumb"
    ));
}

#[test]
fn a_byte_value_over_the_cap_is_refused_on_the_way_in() {
    let big = vec![7u8; MAX_EXTENSION_BYTES + 1];
    let doc = with_extensions(vec![(key("molan.huge"), marker(&big))]);
    assert!(matches!(
        refusal(&doc, &[blob("molan.huge", &big)]),
        clm::ClmError::ExtensionTooLarge { key, .. } if key == "molan.huge"
    ));
}

#[test]
fn the_same_key_carried_twice_is_refused() {
    let doc = with_extensions(vec![(key("molan.thumb"), marker(b"abc"))]);
    assert!(matches!(
        refusal(
            &doc,
            &[blob("molan.thumb", b"abc"), blob("molan.thumb", b"abc")]
        ),
        clm::ClmError::ExtensionBytesDuplicate { .. }
    ));
}

/// A writer runs the same check, so it can never leave a marker whose bytes
/// are not in the file.
#[test]
fn writing_a_marker_without_its_bytes_is_refused_too() {
    let doc = with_extensions(vec![(key("molan.thumb"), marker(b"abc"))]);
    assert!(matches!(
        clm::encode(&doc, &[], &[]).unwrap_err(),
        clm::ClmError::ExtensionBytesMissing { .. }
    ));
}

/// Every value shape the format does not carry is refused by the
/// deserializer, before any of the checks above run.
#[test]
fn a_value_that_is_not_json_or_a_marker_is_refused() {
    for (what, value) in [
        ("inline bytes", ciborium::value::Value::Bytes(vec![1, 2, 3])),
        (
            "a tagged value",
            ciborium::value::Value::Tag(0, Box::new(ciborium::value::Value::Text("x".into()))),
        ),
        (
            "a map with a non-string key",
            ciborium::value::Value::Map(vec![(
                ciborium::value::Value::Integer(1.into()),
                ciborium::value::Value::Integer(2.into()),
            )]),
        ),
    ] {
        // `{"json": <value>}` — the arm a JSON value arrives under.
        let wrapped =
            ciborium::value::Value::Map(vec![(ciborium::value::Value::Text("json".into()), value)]);
        let mut buf = Vec::new();
        ciborium::into_writer(&wrapped, &mut buf).unwrap();
        let decoded: Result<ClmExtension, _> = ciborium::from_reader(&buf[..]);
        assert!(decoded.is_err(), "{what} should not be a value");
    }
}

#[test]
fn a_key_off_the_charset_or_without_a_dot_is_refused_and_named() {
    for bad in ["caster", "molan.", "molan caster", ".molan"] {
        assert!(
            ExtensionKey::new(bad).is_err(),
            "{bad:?} should not be a key"
        );
    }
    // And on the wire, where the message has to say which key.
    let mut buf = Vec::new();
    ciborium::into_writer(
        &ciborium::value::Value::Map(vec![(
            ciborium::value::Value::Text("caster".into()),
            ciborium::value::Value::Map(vec![(
                ciborium::value::Value::Text("json".into()),
                ciborium::value::Value::Integer(1.into()),
            )]),
        )]),
        &mut buf,
    )
    .unwrap();
    let decoded: Result<BTreeMap<ExtensionKey, ClmExtension>, _> = ciborium::from_reader(&buf[..]);
    let message = decoded.expect_err("a bad key is refused").to_string();
    assert!(message.contains("caster"), "{message}");
}

// ---- addons --------------------------------------------------------------

#[test]
fn extract_leaves_extensions_behind_and_install_refuses_one() {
    use catchlight_core::id::SeededHex;
    use catchlight_core::{ModelNode, ModelNodeKind};

    let mut hex = SeededHex::new(3);
    let mut base = Model::new();
    let root = base.root().unwrap().clone();
    let child = base
        .add_node(
            &root,
            ModelNode::new("child", ModelNodeKind::Group),
            &mut hex,
        )
        .unwrap();
    base.set_extension(key("molan.caster"), json()).unwrap();

    let addon = base.extract(&[child]);
    assert!(
        addon.extensions().is_empty(),
        "an addon carries no extensions"
    );

    // One that does is refused, by key.
    let mut carrier = addon.clone();
    carrier.set_extension(key("molan.caster"), json()).unwrap();
    let mut target = Model::new();
    let err = target.install(&carrier).unwrap_err();
    assert!(
        matches!(&err, InstallError::CarriesExtension { key } if key == "molan.caster"),
        "{err}"
    );
}
