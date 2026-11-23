#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A section this build does not recognise is carried, not dropped.
//!
//! The container is a table of numbered sections and a newer writer may add
//! one. A file that passes through an older catchlight — opened, edited,
//! saved — has to come out with that section intact, or every tool built on
//! the older one is quietly destructive.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use catchlight_core::formats::clm::{self, ClmError, ClmUnknownSection};
use catchlight_core::formats::container::{self, Section};
use catchlight_core::id::SeededHex;
use catchlight_core::{LoadBudget, LoadLimits, Model, ModelError, ModelNode, ModelNodeKind};

const FUTURE: u32 = 77;
const FURTHER: u32 = 1000;

fn fixture() -> Vec<u8> {
    // crates/catchlight-core/ -> crates/ -> workspace root.
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("tests/models/quad_over_bg.clm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The fixture as a newer writer might have left it: its own two sections
/// with three this build has no name for threaded between them, two of them
/// under one kind.
fn file_from_the_future() -> Vec<u8> {
    let bytes = fixture();
    let original = container::read(&bytes, &clm::MAGIC).unwrap();
    let (structure, textures) = (original.section(0).unwrap(), original.section(1).unwrap());
    container::write(
        &clm::MAGIC,
        clm::FORMAT_VERSION,
        &[
            Section {
                kind: FUTURE,
                data: b"first",
            },
            Section {
                kind: 0,
                data: structure,
            },
            Section {
                kind: FURTHER,
                data: &[0, 159, 146, 150, 255],
            },
            Section {
                kind: 1,
                data: textures,
            },
            Section {
                kind: FUTURE,
                data: b"",
            },
        ],
    )
    .unwrap()
}

fn carried() -> Vec<ClmUnknownSection> {
    let section = |kind, data: &[u8]| ClmUnknownSection {
        kind,
        data: Arc::from(data),
    };
    vec![
        section(FUTURE, b"first"),
        section(FURTHER, &[0, 159, 146, 150, 255]),
        section(FUTURE, b""),
    ]
}

fn kinds(bytes: &[u8]) -> Vec<(u32, Vec<u8>)> {
    container::read(bytes, &clm::MAGIC)
        .unwrap()
        .sections
        .iter()
        .map(|s| (s.kind, s.data.to_vec()))
        .collect()
}

/// Decode, encode: every unknown section is still there, byte for byte and
/// in the order the file had them, after the sections this build writes.
#[test]
fn a_decoded_file_writes_its_unknown_sections_back() {
    let bytes = file_from_the_future();
    let file = clm::decode(&bytes).unwrap();
    assert_eq!(file.unknown_sections, carried());

    let saved = file.encode().unwrap();
    let original = container::read(&bytes, &clm::MAGIC).unwrap();
    assert_eq!(
        kinds(&saved),
        vec![
            (0, original.section(0).unwrap().to_vec()),
            (1, original.section(1).unwrap().to_vec()),
            (FUTURE, b"first".to_vec()),
            (FURTHER, vec![0, 159, 146, 150, 255]),
            (FUTURE, Vec::new()),
        ]
    );

    // The first save moved them behind the known sections; from then on a
    // save changes nothing.
    let again = clm::decode(&saved).unwrap();
    assert_eq!(again, file);
    assert_eq!(again.encode().unwrap(), saved);
}

/// The same through a `Model`, which is the path an editor saves by — and
/// across an edit, which is the point of opening the file at all.
#[test]
fn a_model_carries_unknown_sections_through_an_edit_and_a_save() {
    let bytes = file_from_the_future();
    let expected = clm::decode(&bytes).unwrap().encode().unwrap();

    let mut model = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(model.unknown_sections(), carried());
    assert_eq!(model.to_clm_bytes().unwrap(), expected);
    assert_eq!(model.to_clm_file().unwrap().unknown_sections, carried());

    // Read from a decoded file rather than from bytes.
    let through_file = Model::from_clm_file(&clm::decode(&bytes).unwrap()).unwrap();
    assert_eq!(through_file.unknown_sections(), carried());
    assert_eq!(through_file.to_clm_bytes().unwrap(), expected);

    let root = model.root().unwrap().clone();
    model
        .add_node(
            &root,
            ModelNode::new("added", ModelNodeKind::Group),
            &mut SeededHex::new(5),
        )
        .unwrap();
    let saved = model.to_clm_bytes().unwrap();
    assert_ne!(saved, expected, "the edit reached the file");
    assert_eq!(
        kinds(&saved)[2..],
        kinds(&expected)[2..],
        "and the unknown sections did not move"
    );
    assert_eq!(
        Model::from_clm_bytes(&saved).unwrap().unknown_sections(),
        carried()
    );
}

/// A file with nothing unknown in it is written exactly as before.
#[test]
fn a_file_with_no_unknown_section_is_unchanged() {
    let bytes = fixture();
    let file = clm::decode(&bytes).unwrap();
    assert!(file.unknown_sections.is_empty());
    assert_eq!(file.encode().unwrap(), bytes);
    assert_eq!(
        clm::encode(&file.doc, &file.textures, &file.extensions).unwrap(),
        bytes
    );
}

/// Two models that differ only in what they carry are two different files.
#[test]
fn authored_equality_sees_unknown_sections() {
    let with = Model::from_clm_bytes(&file_from_the_future()).unwrap();
    let without = Model::from_clm_bytes(&fixture()).unwrap();

    assert!(!with.authored_eq(&without).unwrap());
    assert!(!without.authored_eq(&with).unwrap());
    assert!(with
        .authored_eq(&Model::from_clm_bytes(&file_from_the_future()).unwrap())
        .unwrap());
}

/// A structure push is not a file: it carries none of them, and applying one
/// leaves a replica the ones it was loaded with.
#[test]
fn a_structure_push_neither_carries_nor_clears_them() {
    let bytes = file_from_the_future();
    let mut server = Model::from_clm_bytes(&bytes).unwrap();
    let mut replica = Model::from_clm_bytes(&bytes).unwrap();

    let root = server.root().unwrap().clone();
    server
        .add_node(
            &root,
            ModelNode::new("added", ModelNodeKind::Group),
            &mut SeededHex::new(5),
        )
        .unwrap();
    let push = server.to_structure_bytes().unwrap();
    assert_eq!(
        kinds(&push)
            .iter()
            .map(|(kind, _)| *kind)
            .collect::<Vec<_>>(),
        vec![0, 2],
        "a structure and a texture manifest, nothing else"
    );

    let held: Vec<_> = replica
        .texture_ids()
        .iter()
        .map(|id| (id.clone(), replica.texture(id).unwrap().clone()))
        .collect();
    replica
        .replace_structure(&push, |id| {
            held.iter()
                .find(|(have, _)| have == id)
                .map(|(_, t)| t.clone())
        })
        .unwrap();

    assert_eq!(replica.unknown_sections(), carried());
    assert!(replica.authored_eq(&server).unwrap());
    assert_eq!(
        replica.to_clm_bytes().unwrap(),
        server.to_clm_bytes().unwrap()
    );
}

/// An addon is a piece cut out of a model, and nothing says which piece an
/// unreadable section is about.
#[test]
fn an_extracted_addon_carries_none() {
    let mut model = Model::from_clm_bytes(&file_from_the_future()).unwrap();
    let root = model.root().unwrap().clone();
    let child = model
        .add_node(
            &root,
            ModelNode::new("child", ModelNodeKind::Group),
            &mut SeededHex::new(5),
        )
        .unwrap();

    assert!(model.extract(&[child]).unknown_sections().is_empty());
    assert_eq!(
        model.unknown_sections(),
        carried(),
        "the base keeps its own"
    );
}

/// A carried section under a kind the format defines would be a second
/// structure, or turn the file into a structure-only container. A reader
/// never produces one; a hand-built file that holds one is refused.
#[test]
fn a_section_of_a_known_kind_cannot_be_carried_as_unknown() {
    for kind in 0..=3 {
        let mut file = clm::decode(&fixture()).unwrap();
        file.unknown_sections.push(ClmUnknownSection {
            kind,
            data: Arc::from(&b"x"[..]),
        });
        assert!(
            matches!(file.encode(), Err(ClmError::ReservedSectionKind(k)) if k == kind),
            "kind {kind}"
        );
    }
}

/// A hand-built file's unknown sections count toward its encoded bytes, as
/// they do when the same file arrives as bytes.
#[test]
fn unknown_sections_are_charged_as_encoded_bytes() {
    let mut file = clm::decode(&fixture()).unwrap();
    let payloads: u64 = file.textures.iter().map(|t| t.data.len() as u64).sum();
    file.unknown_sections.push(ClmUnknownSection {
        kind: FUTURE,
        data: Arc::from(vec![0u8; 100]),
    });

    let budget = |encoded_bytes| {
        let mut limits = LoadLimits::default();
        limits.encoded_bytes = encoded_bytes;
        LoadBudget::new(limits)
    };
    Model::from_clm_file_with_budget(&file, &mut budget(payloads + 100)).unwrap();
    let err = Model::from_clm_file_with_budget(&file, &mut budget(payloads + 99)).unwrap_err();
    assert!(
        matches!(&err, ModelError::LoadLimit(e) if e.resource == "encoded bytes"),
        "{err}"
    );
}
