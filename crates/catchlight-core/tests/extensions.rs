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
    MAX_EXTENSION_BYTES, MAX_EXTENSION_JSON_BYTES,
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
        model.set_extension(key("acme.camera"), json()).unwrap();
        model.set_extension(key("acme.thumb"), bytes()).unwrap();

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

/// A model with no extensions omits the field and section on save.
#[test]
fn saving_a_model_without_extensions_omits_the_extension_data() {
    for path in fixtures() {
        let committed = std::fs::read(&path).unwrap();
        let model = Model::from_clm_bytes(&committed).unwrap();
        assert!(model.extensions().is_empty());
        let encoded = model.to_clm_bytes().unwrap();
        let parsed = catchlight_core::formats::container::read(&encoded, &clm::MAGIC).unwrap();
        assert!(parsed.section(3).is_none());
        let structure: serde_json::Value =
            ciborium::from_reader(parsed.section(0).unwrap()).unwrap();
        assert!(structure.get("extensions").is_none());
        assert!(encoded == committed, "saving changed {}", path.display());
    }
}

#[test]
fn setting_and_deleting_moves_the_generation() {
    let mut model = Model::new();
    let before = model.generation();
    model.set_extension(key("acme.camera"), json()).unwrap();
    let after_set = model.generation();
    assert!(after_set > before, "a set is a model edit");
    model.delete_extension(&key("acme.camera")).unwrap();
    assert!(model.generation() > after_set, "so is a delete");
    assert!(model.extensions().is_empty());
}

#[test]
fn deleting_a_key_the_model_does_not_carry_is_an_error() {
    let mut model = Model::new();
    let err = model.delete_extension(&key("acme.absent")).unwrap_err();
    assert!(
        matches!(&err, ModelError::UnknownExtension(k) if k == "acme.absent"),
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
        .set_extension(key("acme.huge"), ExtensionValue::Bytes(Arc::from(too_big)))
        .unwrap_err();
    assert!(
        matches!(&err, ModelError::ExtensionTooLarge { key, .. } if key == "acme.huge"),
        "{err}"
    );
}

/// A JSON string whose compact JSON text is exactly `len` bytes: the two
/// quotes count.
fn json_of_len(len: usize) -> serde_json::Value {
    serde_json::Value::String("a".repeat(len - 2))
}

/// A JSON value rides inside the structure, so it is pushed whole with every
/// structure feed. It used to have no cap at all.
#[test]
fn a_json_value_over_the_cap_is_refused() {
    let mut model = Model::new();
    model
        .set_extension(
            key("acme.note"),
            ExtensionValue::Json(json_of_len(MAX_EXTENSION_JSON_BYTES)),
        )
        .expect("a value exactly at the cap");

    let err = model
        .set_extension(
            key("acme.huge"),
            ExtensionValue::Json(json_of_len(MAX_EXTENSION_JSON_BYTES + 1)),
        )
        .unwrap_err();
    assert!(
        matches!(
            &err,
            ModelError::ExtensionTooLarge { key, size, max }
                if key == "acme.huge"
                    && *size == MAX_EXTENSION_JSON_BYTES + 1
                    && *max == MAX_EXTENSION_JSON_BYTES
        ),
        "{err}"
    );
    assert!(
        model.extension(&key("acme.huge")).is_none(),
        "a refused value is not set"
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
    let doc = with_extensions(vec![(key("acme.thumb"), marker(b"abc"))]);
    assert!(
        matches!(refusal(&doc, &[]), clm::ClmError::ExtensionBytesMissing { key } if key == "acme.thumb")
    );
}

#[test]
fn bytes_nothing_names_are_refused_by_key() {
    // No entry at all, and an entry of the wrong kind, are both orphans.
    let empty = root_structure();
    assert!(
        matches!(refusal(&empty, &[blob("acme.thumb", b"abc")]), clm::ClmError::ExtensionBytesOrphan { key } if key == "acme.thumb")
    );
    let as_json = with_extensions(vec![(
        key("acme.thumb"),
        ClmExtension::Json(serde_json::json!(1)),
    )]);
    assert!(matches!(
        refusal(&as_json, &[blob("acme.thumb", b"abc")]),
        clm::ClmError::ExtensionBytesOrphan { .. }
    ));
}

#[test]
fn a_size_or_a_hash_that_disagrees_with_the_marker_is_refused() {
    let doc = with_extensions(vec![(key("acme.thumb"), marker(b"abc"))]);
    assert!(matches!(
        refusal(&doc, &[blob("acme.thumb", b"abcd")]),
        clm::ClmError::ExtensionSizeMismatch { .. }
    ));
    // Same length, different bytes: only the hash catches it.
    assert!(matches!(
        refusal(&doc, &[blob("acme.thumb", b"abd")]),
        clm::ClmError::ExtensionHashMismatch { key } if key == "acme.thumb"
    ));
}

#[test]
fn a_byte_value_over_the_cap_is_refused_on_the_way_in() {
    let big = vec![7u8; MAX_EXTENSION_BYTES + 1];
    let doc = with_extensions(vec![(key("acme.huge"), marker(&big))]);
    assert!(matches!(
        refusal(&doc, &[blob("acme.huge", &big)]),
        clm::ClmError::ExtensionTooLarge { key, .. } if key == "acme.huge"
    ));
}

/// The reader and the writer hold a JSON value to the same cap the model
/// does, measured the same way.
#[test]
fn a_json_value_over_the_cap_is_refused_on_the_way_in_and_out() {
    let at_cap = with_extensions(vec![(
        key("acme.note"),
        ClmExtension::Json(json_of_len(MAX_EXTENSION_JSON_BYTES)),
    )]);
    clm::check_extensions(&at_cap, &[]).expect("a value exactly at the cap");

    let over = with_extensions(vec![(
        key("acme.huge"),
        ClmExtension::Json(json_of_len(MAX_EXTENSION_JSON_BYTES + 1)),
    )]);
    assert!(matches!(
        refusal(&over, &[]),
        clm::ClmError::ExtensionTooLarge { key, size, max }
            if key == "acme.huge"
                && size == MAX_EXTENSION_JSON_BYTES + 1
                && max == MAX_EXTENSION_JSON_BYTES
    ));
    assert!(matches!(
        clm::encode(&over, &[], &[]).unwrap_err(),
        clm::ClmError::ExtensionTooLarge { .. }
    ));

    // A file some other writer produced: the structure section written
    // straight to CBOR, past `encode`'s own check.
    let mut structure = Vec::new();
    ciborium::into_writer(&over, &mut structure).unwrap();
    let bytes = catchlight_core::formats::container::write(
        &clm::MAGIC,
        clm::FORMAT_VERSION,
        &[
            catchlight_core::formats::container::Section {
                kind: 0,
                data: &structure,
            },
            // An empty texture table.
            catchlight_core::formats::container::Section {
                kind: 1,
                data: &[0x80],
            },
        ],
    )
    .unwrap();
    assert!(matches!(
        clm::decode(&bytes).unwrap_err(),
        clm::ClmError::ExtensionTooLarge { key, .. } if key == "acme.huge"
    ));
}

/// CBOR can spell NaN and the infinities; JSON cannot. `serde_json` reads
/// one as `null`, which would hand back a different value from the one the
/// file carries — so it is refused, wherever in the value it sits.
#[test]
fn a_float_that_is_not_finite_is_refused_rather_than_read_as_null() {
    use ciborium::value::Value;

    let wrap = |value: Value| {
        let mut buf = Vec::new();
        ciborium::into_writer(&Value::Map(vec![(Value::Text("json".into()), value)]), &mut buf)
            .unwrap();
        buf
    };
    for (what, value) in [
        ("NaN", Value::Float(f64::NAN)),
        ("infinity", Value::Float(f64::INFINITY)),
        ("-infinity", Value::Float(f64::NEG_INFINITY)),
        (
            "NaN in an array",
            Value::Array(vec![Value::Integer(1.into()), Value::Float(f64::NAN)]),
        ),
        (
            "infinity in a map",
            Value::Map(vec![(
                Value::Text("ratio".into()),
                Value::Float(f64::INFINITY),
            )]),
        ),
    ] {
        let decoded: Result<ClmExtension, _> = ciborium::from_reader(&wrap(value)[..]);
        let message = decoded
            .expect_err(&format!("{what} should not be a value"))
            .to_string();
        assert!(message.contains("not a JSON number"), "{what}: {message}");
    }

    // Everything JSON can spell still reads as itself, nested or not.
    let finite = Value::Map(vec![
        (Value::Text("ratio".into()), Value::Float(0.5)),
        (Value::Text("count".into()), Value::Integer(3.into())),
        (Value::Text("below".into()), Value::Integer((-7).into())),
        (Value::Text("name".into()), Value::Text("rig".into())),
        (Value::Text("none".into()), Value::Null),
        (
            "flags".to_string().into(),
            Value::Array(vec![Value::Bool(true), Value::Bool(false)]),
        ),
    ]);
    let decoded: ClmExtension = ciborium::from_reader(&wrap(finite)[..]).unwrap();
    assert_eq!(
        decoded,
        ClmExtension::Json(serde_json::json!({
            "ratio": 0.5,
            "count": 3,
            "below": -7,
            "name": "rig",
            "none": null,
            "flags": [true, false],
        }))
    );
}

#[test]
fn the_same_key_carried_twice_is_refused() {
    let doc = with_extensions(vec![(key("acme.thumb"), marker(b"abc"))]);
    assert!(matches!(
        refusal(
            &doc,
            &[blob("acme.thumb", b"abc"), blob("acme.thumb", b"abc")]
        ),
        clm::ClmError::ExtensionBytesDuplicate { .. }
    ));
}

/// A writer runs the same check, so it can never leave a marker whose bytes
/// are not in the file.
#[test]
fn writing_a_marker_without_its_bytes_is_refused_too() {
    let doc = with_extensions(vec![(key("acme.thumb"), marker(b"abc"))]);
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
    for bad in ["camera", "acme.", "acme camera", ".acme"] {
        assert!(
            ExtensionKey::new(bad).is_err(),
            "{bad:?} should not be a key"
        );
    }
    // And on the wire, where the message has to say which key.
    let mut buf = Vec::new();
    ciborium::into_writer(
        &ciborium::value::Value::Map(vec![(
            ciborium::value::Value::Text("camera".into()),
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
    assert!(message.contains("camera"), "{message}");
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
    base.set_extension(key("acme.camera"), json()).unwrap();

    let addon = base.extract(&[child]);
    assert!(
        addon.extensions().is_empty(),
        "an addon carries no extensions"
    );

    // One that does is refused, by key.
    let mut carrier = addon.clone();
    carrier.set_extension(key("acme.camera"), json()).unwrap();
    let mut target = Model::new();
    let err = target.install(&carrier).unwrap_err();
    assert!(
        matches!(&err, InstallError::CarriesExtension { key } if key == "acme.camera"),
        "{err}"
    );
}
