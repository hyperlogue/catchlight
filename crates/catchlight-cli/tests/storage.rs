#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Accounting is additive, read-only, and independent of image decoding.

mod common;

use std::path::Path;

use catchlight_cli::storage;
use catchlight_core::formats::{clm, container};

#[test]
fn canonical_categories_partition_every_byte_without_decoding_textures() {
    let mut model = common::decode(&common::fixture("welded_seam"));
    // A complete-looking PNG header with no pixels or CRCs. A decoder would
    // reject it; storage accounting still reports its bytes and dimensions.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(100u32.to_be_bytes());
    png.extend(50u32.to_be_bytes());
    model.textures[0].data = png;
    let key = catchlight_core::ExtensionKey::new("example.bytes").unwrap();
    let data = vec![42; 200];
    model.doc.extensions.insert(
        key.clone(),
        clm::ClmExtension::Bytes(clm::ClmExtensionMarker {
            size: data.len() as u64,
            hash: clm::extension_hash(&data),
        }),
    );
    model.extensions.push(clm::ClmExtensionBlob { key, data });
    let bytes = clm::encode(&model.doc, &model.textures, &model.extensions).unwrap();
    let report = storage::inspect(&bytes, Path::new("example.clm")).unwrap();
    assert!(report.canonical_matches_input);
    assert_eq!(report.stored_sections.len(), 3);
    assert_eq!(report.canonical_bytes, bytes.len());
    assert_eq!(
        report
            .canonical_categories
            .iter()
            .map(|s| s.bytes)
            .sum::<usize>(),
        bytes.len()
    );
    let texture = report
        .textures
        .iter()
        .find(|t| t.id == model.textures[0].id.as_str())
        .unwrap();
    assert_eq!(texture.bytes, 24);
    assert_eq!(texture.dimensions, Some([100, 50]));
    assert!(!texture.parts.is_empty());
    let mesh_total: usize = report.meshes.iter().map(|m| m.bytes).sum();
    assert_eq!(
        mesh_total,
        report.elements.iter().map(|e| e.mesh_bytes).sum::<usize>()
    );
    let binding_total: usize = report.bindings.iter().map(|b| b.bytes).sum();
    assert_eq!(
        binding_total,
        report
            .elements
            .iter()
            .map(|e| e.binding_bytes)
            .sum::<usize>()
    );
    assert!(report
        .elements
        .windows(2)
        .all(|w| w[0].total_bytes >= w[1].total_bytes));
}

#[test]
fn authored_deform_values_are_counted_without_generating_unset_cells() {
    let model = common::decode(&common::fixture("two_param_grid"));
    let bytes = clm::encode(&model.doc, &model.textures, &model.extensions).unwrap();
    let report = storage::inspect(&bytes, Path::new("grid.clm")).unwrap();
    let authored = model
        .doc
        .bindings
        .iter()
        .filter_map(|binding| catchlight_core::deform_cells(&binding.values));
    let expected_cells: usize = authored.clone().map(<[_]>::len).sum();
    let expected_vectors: usize = authored.flatten().map(|cell| cell.value.len() / 2).sum();
    assert!(expected_cells > 0);
    assert_eq!(
        report
            .bindings
            .iter()
            .map(|b| b.deform_vectors)
            .sum::<usize>(),
        expected_vectors
    );
    assert_eq!(
        report
            .bindings
            .iter()
            .filter(|b| b.target == "deform")
            .map(|b| b.cells)
            .sum::<usize>(),
        expected_cells
    );
}

#[test]
fn unknown_fields_are_not_misreported_as_exact_element_sizes() {
    let base = common::decode(&common::fixture("welded_seam"));
    let encoded = clm::encode(&base.doc, &base.textures, &base.extensions).unwrap();
    let original = container::read(&encoded, &clm::MAGIC).unwrap();
    let mut value: ciborium::value::Value =
        ciborium::from_reader(original.section(0).unwrap()).unwrap();
    let ciborium::value::Value::Map(fields) = &mut value else {
        panic!("structure must be a map")
    };
    fields.push(("future_field".into(), "opaque annotation".into()));
    let mut structure = Vec::new();
    ciborium::into_writer(&value, &mut structure).unwrap();
    let input = container::write(
        &clm::MAGIC,
        clm::FORMAT_VERSION,
        &[
            container::Section {
                kind: 0,
                data: &structure,
            },
            container::Section {
                kind: 1,
                data: original.section(1).unwrap(),
            },
        ],
    );
    let report = storage::inspect(&input, Path::new("future.clm")).unwrap();
    assert!(!report.canonical_matches_input);
    assert_eq!(report.stored_bytes, input.len());
    assert_eq!(report.stored_sections[0].bytes, structure.len());
    assert_eq!(report.canonical_bytes, encoded.len());
    assert_eq!(
        report
            .canonical_categories
            .iter()
            .map(|s| s.bytes)
            .sum::<usize>(),
        encoded.len()
    );
}

#[test]
fn command_reports_json_and_leaves_the_input_unchanged() {
    let dir = common::tmp("storage-command");
    let path = common::copy_fixture("welded_seam", &dir);
    let before = common::read(&path);
    let (code, stdout, stderr) = common::run(&["storage", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["stored_bytes"], before.len());
    assert!(report["textures"].as_array().unwrap().len() > 1);
    assert_eq!(common::read(&path), before);
}
