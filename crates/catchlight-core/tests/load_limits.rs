#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! What the load budget charges for a `.clm`, and when.
//!
//! Two things are pinned here that the unit tests beside the budget cannot
//! see, because both are about the order the readers call it in: a file's
//! bytes are charged once, and its structure is charged before it is parsed.

use std::path::{Path, PathBuf};

use catchlight_core::formats::clm::{
    self, ClmAnimation, ClmError, ClmExtension, ClmKeyframe, ClmLane, ClmStructure,
};
use catchlight_core::formats::container::{self, Section};
use catchlight_core::id::{ExtensionKey, ParamId};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::load_budget::charge_clm_structure;
use catchlight_core::{LoadBudget, LoadLimitError, LoadLimits, Model, ModelError};

fn fixture(name: &str) -> Vec<u8> {
    // crates/catchlight-core/ -> crates/ -> workspace root.
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("tests/models")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn budget(set: impl FnOnce(&mut LoadLimits)) -> LoadBudget {
    let mut limits = LoadLimits::default();
    set(&mut limits);
    LoadBudget::new(limits)
}

fn limit_error(err: ModelError) -> LoadLimitError {
    match err {
        ModelError::LoadLimit(e) | ModelError::Clm(ClmError::LoadLimit(e)) => e,
        other => panic!("expected a load limit, got {other:?}"),
    }
}

/// A file's bytes are charged once. Decoding charged the whole input and
/// building the model then charged every texture payload again, so a file
/// loaded only while it was under about half of `encoded_bytes`.
#[test]
fn a_file_exactly_at_the_encoded_byte_limit_loads() {
    let bytes = fixture("quad_over_bg.clm");
    let size = bytes.len() as u64;

    let model = Model::from_clm_bytes_with_budget(&bytes, &mut budget(|l| l.encoded_bytes = size))
        .expect("a file exactly at the limit");
    assert!(
        !model.texture_ids().is_empty(),
        "the fixture has to carry payload bytes for this to mean anything"
    );

    let err =
        Model::from_clm_bytes_with_budget(&bytes, &mut budget(|l| l.encoded_bytes = size - 1))
            .unwrap_err();
    assert_eq!(
        limit_error(err),
        LoadLimitError {
            resource: "encoded bytes",
            limit: size - 1,
            got: size,
        }
    );
}

/// A `ClmFile` that never was bytes has had nothing charged, so its reader
/// still charges the payloads.
#[test]
fn a_hand_built_file_still_has_its_payloads_charged() {
    let file = clm::decode(&fixture("quad_over_bg.clm")).unwrap();
    let payloads: u64 = file.textures.iter().map(|t| t.data.len() as u64).sum();
    assert!(payloads > 0);

    Model::from_clm_file_with_budget(&file, &mut budget(|l| l.encoded_bytes = payloads))
        .expect("exactly the payloads");
    let err =
        Model::from_clm_file_with_budget(&file, &mut budget(|l| l.encoded_bytes = payloads - 1))
            .unwrap_err();
    assert_eq!(limit_error(err).resource, "encoded bytes");
}

/// The structure section is charged by its size before it is parsed. The
/// section here is not CBOR at all, so which error comes back says which
/// happened first.
#[test]
fn the_structure_section_is_charged_before_it_is_parsed() {
    let garbage = [0xFFu8; 64];
    let complete = container::write(
        &clm::MAGIC,
        clm::FORMAT_VERSION,
        &[Section {
            kind: 0,
            data: &garbage,
        }],
    )
    .unwrap();
    // The same section in a structure-only container: kind 2 is the manifest.
    let structure_only = container::write(
        &clm::MAGIC,
        clm::FORMAT_VERSION,
        &[
            Section {
                kind: 0,
                data: &garbage,
            },
            Section {
                kind: 2,
                data: &[0x80],
            },
        ],
    )
    .unwrap();

    let over = |l: &mut LoadLimits| l.structure_bytes = 63;
    let exact = |l: &mut LoadLimits| l.structure_bytes = 64;

    let err = clm::decode_with_budget(&complete, &mut budget(over)).unwrap_err();
    assert!(
        matches!(
            err,
            ClmError::LoadLimit(LoadLimitError {
                resource: "structure bytes",
                limit: 63,
                got: 64,
            })
        ),
        "{err:?}"
    );
    let err = clm::decode_with_budget(&complete, &mut budget(exact)).unwrap_err();
    assert!(matches!(err, ClmError::Decode(_)), "{err:?}");

    let err = clm::decode_structure_with_budget(&structure_only, &mut budget(over)).unwrap_err();
    assert!(
        matches!(
            err,
            ClmError::LoadLimit(LoadLimitError {
                resource: "structure bytes",
                ..
            })
        ),
        "{err:?}"
    );
    let err = clm::decode_structure_with_budget(&structure_only, &mut budget(exact)).unwrap_err();
    assert!(matches!(err, ClmError::Decode(_)), "{err:?}");
}

/// The default structure limit leaves the committed models a wide margin.
#[test]
fn the_largest_committed_structure_is_far_under_the_default_limit() {
    let bytes = fixture("lan.clm");
    let file = container::read(&bytes, &clm::MAGIC).unwrap();
    let structure = file.section(0).unwrap().len() as u64;
    assert!(
        structure * 8 <= LoadLimits::default().structure_bytes,
        "lan's structure is {structure} bytes"
    );
    Model::from_clm_bytes(&bytes).expect("lan loads under the defaults");
}

fn charge(doc: &ClmStructure, set: impl FnOnce(&mut LoadLimits)) -> Result<(), LoadLimitError> {
    charge_clm_structure(doc, &mut budget(set))
}

/// A spine's joints and bend targets, both of them.
#[test]
fn spine_joints_and_targets_are_charged() {
    let doc = clm::decode(&fixture("strand_response.clm")).unwrap().doc;

    assert_eq!(charge(&doc, |l| l.spine_joints = 6), Ok(()));
    let err = charge(&doc, |l| l.spine_joints = 5).unwrap_err();
    assert_eq!((err.resource, err.got), ("spine joints", 6));
}

/// A weld and each pair it holds.
#[test]
fn welds_and_their_pairs_are_charged() {
    let doc = clm::decode(&fixture("welded_seam.clm")).unwrap().doc;

    assert_eq!(charge(&doc, |l| l.weld_pairs = 4), Ok(()));
    let err = charge(&doc, |l| l.weld_pairs = 3).unwrap_err();
    assert_eq!((err.resource, err.got), ("weld pairs", 4));
}

/// An animation, each lane, each keyframe.
#[test]
fn animations_lanes_and_keyframes_are_charged() {
    let lane = |keys: i32| ClmLane {
        param: ParamId::new("p").unwrap(),
        interpolation: InterpolateMode::Linear,
        keyframes: (0..keys)
            .map(|frame| ClmKeyframe { frame, value: 0.0 })
            .collect(),
    };
    let doc = ClmStructure {
        animations: vec![ClmAnimation {
            lanes: vec![lane(2), lane(3)],
            ..ClmAnimation::default()
        }],
        ..ClmStructure::default()
    };

    // One animation, two lanes, five keyframes.
    assert_eq!(charge(&doc, |l| l.keyframes = 8), Ok(()));
    let err = charge(&doc, |l| l.keyframes = 7).unwrap_err();
    assert_eq!((err.resource, err.got), ("animation keyframes", 8));
}

/// A JSON extension by its JSON text, a byte extension by what its marker
/// states.
#[test]
fn extension_values_are_charged() {
    let mut doc = ClmStructure::default();
    doc.extensions.insert(
        ExtensionKey::new("acme.note").unwrap(),
        ClmExtension::Json(serde_json::json!({ "a": [1, 2, 3] })),
    );
    doc.extensions.insert(
        ExtensionKey::new("acme.thumb").unwrap(),
        ClmExtension::Bytes(clm::ClmExtensionMarker {
            size: 100,
            hash: clm::extension_hash(&[0; 100]),
        }),
    );
    let json = r#"{"a":[1,2,3]}"#.len() as u64;

    assert_eq!(charge(&doc, |l| l.extension_bytes = json + 100), Ok(()));
    let err = charge(&doc, |l| l.extension_bytes = json + 99).unwrap_err();
    assert_eq!((err.resource, err.got), ("extension bytes", json + 100));
}
