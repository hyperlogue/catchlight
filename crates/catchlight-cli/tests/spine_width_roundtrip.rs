#![allow(clippy::unwrap_used)]
//! File operations preserve a spine's paired width fields and companions.

#[path = "../../../tests/support/spine_width.rs"]
mod support;

use catchlight_cli::patch;
use catchlight_core::{Model, Name};

#[test]
fn patching_a_width_model_preserves_its_authored_fields() {
    let mut fixture = support::Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("width.clm");
    let output = dir.path().join("renamed.clm");
    std::fs::write(&source, fixture.model.to_clm_bytes().unwrap()).unwrap();
    let root = fixture.model.root().unwrap().clone();
    patch::run(
        &source,
        root.as_str(),
        "name",
        "renamed",
        Some(patch::Kind::Node),
        Some(&output),
    )
    .unwrap();
    fixture
        .model
        .update_node(&root, |node| node.name = Name::truncated("renamed"))
        .unwrap();
    let reopened = Model::from_clm_bytes(&std::fs::read(output).unwrap()).unwrap();
    assert!(fixture.model.authored_eq(&reopened).unwrap());
    assert_eq!(
        fixture.model.to_clm_bytes().unwrap(),
        reopened.to_clm_bytes().unwrap()
    );
}
