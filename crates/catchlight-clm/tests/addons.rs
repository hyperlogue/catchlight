#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `extract`, `merge` and `requirements` against the committed fixtures.
//!
//! The centrepiece is the round trip: cut a subtree out of a model, delete it
//! from a copy of that model, install the cut back, and get the model that was
//! there before — checked with `diff`, which the `diff` suite has already
//! shown to be exact.
//!
//! `requirements` is checked two ways on the same file every time: the
//! file-level scan this crate does and `Model::requirements`, which walks the
//! same fields with a Model's tables. They must always agree.

mod common;

use catchlight_clm::diff::diff;
use catchlight_clm::{fragment, Error};
use catchlight_core::id::{NodeId, SeamId, TexId};
use catchlight_core::{InstallError, Model, Required, Requirement};

use common::{decode, read, tmp};

/// The base a `merge` puts an addon back into: the model with the given nodes
/// gone, and with the textures only they used gone too. `Model::delete_node`
/// does not garbage-collect a texture, so leaving one behind would collide
/// with the addon's own copy of it.
fn base_without(
    nodes: &[&str],
    textures: &[&str],
    dir: &std::path::Path,
    name: &str,
) -> std::path::PathBuf {
    let mut model = Model::from_clm_bytes(&read(&common::fixture("composite_masks"))).unwrap();
    for node in nodes {
        model.delete_node(&NodeId::new(node).unwrap()).unwrap();
    }
    for texture in textures {
        model.delete_texture(&TexId::new(texture).unwrap()).unwrap();
    }
    let path = dir.join(format!("{name}.clm"));
    std::fs::write(&path, model.to_clm_bytes().unwrap()).unwrap();
    path
}

#[test]
fn an_extracted_subtree_is_an_addon_and_not_a_complete_model() {
    let dir = tmp("addons-extract-shape");
    let addon = dir.join("hood.clm");

    let extracted = fragment::extract(
        &common::fixture("composite_masks"),
        &["node-9".into()],
        &addon,
    )
    .unwrap();
    assert_eq!(extracted.roots, vec!["node-9".to_string()]);
    assert_eq!(extracted.nodes, 2, "node-9 and its child");
    assert_eq!(extracted.textures, 1, "tex-4, which only node-10 uses");
    assert_eq!(extracted.requirements, 2);

    let bytes = read(&addon);
    let fragment = Model::from_clm_bytes_fragment(&bytes).unwrap();
    assert!(fragment.is_fragment());
    assert_eq!(fragment.node_count(), 2);
    assert_eq!(
        fragment.texture_ids(),
        &[TexId::new("tex-4").unwrap()][..],
        "a texture the rest of the model also uses would stay behind"
    );

    let as_model = Model::from_clm_bytes(&bytes);
    assert!(
        as_model.is_err(),
        "an addon must not read as a complete model"
    );
}

/// Delete a subtree from a model, install the extract of it back, and the
/// model is the one it was — install appends, and these roots were last
/// already.
#[test]
fn installing_an_extract_back_restores_the_model() {
    let dir = tmp("addons-round-trip");
    let original = common::fixture("composite_masks");

    let base = base_without(&["node-9"], &["tex-4"], &dir, "base");
    assert!(!diff(&decode(&original), &decode(&base)).is_empty());

    let addon = dir.join("addon.clm");
    fragment::extract(&original, &["node-9".into()], &addon).unwrap();

    let merged = dir.join("merged.clm");
    let installed = fragment::merge(&base, &addon, &merged).unwrap();
    assert_eq!(installed.nodes, 2);
    assert_eq!(installed.roots, vec!["node-9".to_string()]);
    assert_eq!(installed.textures, 1);

    assert_eq!(
        diff(&decode(&original), &decode(&merged)),
        Vec::<String>::new(),
        "the round trip did not restore the model"
    );
    assert_eq!(
        read(&merged),
        read(&original),
        "the round trip restored the model but not its bytes"
    );
}

#[test]
fn extracting_a_node_with_no_parent_is_refused() {
    let dir = tmp("addons-extract-root");
    let error = fragment::extract(
        &common::fixture("composite_masks"),
        &["root".into()],
        &dir.join("nope.clm"),
    )
    .unwrap_err();

    assert!(
        matches!(error, Error::ExtractingARoot { .. }),
        "expected ExtractingARoot, got {error}"
    );
    assert!(
        error.to_string().contains("extract its children"),
        "{error}"
    );
    assert!(
        !dir.join("nope.clm").exists(),
        "a refused extract wrote a file"
    );
}

#[test]
fn extracting_an_id_the_file_does_not_carry_is_refused() {
    let dir = tmp("addons-extract-unknown");

    let error = fragment::extract(
        &common::fixture("composite_masks"),
        &["node-9".into(), "node-99".into()],
        &dir.join("nope.clm"),
    )
    .unwrap_err();
    let Error::NoSuchId { kind, id, .. } = &error else {
        panic!("expected NoSuchId, got {error}");
    };
    assert_eq!(*kind, "node");
    assert_eq!(id, "node-99");

    let error = fragment::extract(
        &common::fixture("composite_masks"),
        &["not a valid id".into()],
        &dir.join("nope.clm"),
    )
    .unwrap_err();
    assert!(
        matches!(error, Error::BadId { .. }),
        "expected BadId, got {error}"
    );
}

/// The file-level scan and `Model::requirements` walk the same eight fields;
/// if they ever disagree, one of them is wrong.
#[test]
fn the_file_level_scan_agrees_with_the_model() {
    let dir = tmp("addons-requirements-agree");

    for (fixture, ids) in [
        ("composite_masks", vec!["node-9"]),
        ("composite_masks", vec!["node-7"]),
        ("composite_masks", vec!["node-2"]),
        ("composite_masks", vec!["node-6", "node-7"]),
        ("welded_seam", vec!["node-1"]),
        ("welded_seam", vec!["node-2"]),
        ("welded_seam", vec!["node-1", "node-2"]),
        ("mip_checker", vec!["node-1"]),
    ] {
        let addon = dir.join(format!("{fixture}-{}.clm", ids.join("+")));
        let owned: Vec<String> = ids.iter().map(ToString::to_string).collect();
        fragment::extract(&common::fixture(fixture), &owned, &addon).unwrap();

        let clm = decode(&addon);
        let by_file = fragment::scan(&clm);
        let by_model: Vec<Requirement> = Model::from_clm_bytes_fragment(&read(&addon))
            .unwrap()
            .requirements()
            .iter()
            .cloned()
            .collect();
        assert_eq!(by_file, by_model, "{fixture} {ids:?}");
        assert!(!by_file.is_empty(), "{fixture} {ids:?} required nothing");
    }
}

#[test]
fn the_requirements_are_exactly_the_cut_edges() {
    let dir = tmp("addons-requirements-lines");
    let addon = dir.join("hood.clm");
    fragment::extract(
        &common::fixture("composite_masks"),
        &["node-9".into()],
        &addon,
    )
    .unwrap();

    let requirements = fragment::requirements(&addon).unwrap();
    assert_eq!(
        requirements,
        vec![
            Requirement {
                id: Required::Node(NodeId::new("root").unwrap()),
                field: "parent",
                owner: "node-9".to_string(),
            },
            Requirement {
                id: Required::Part(NodeId::new("node-5").unwrap()),
                field: "mask source",
                owner: "node-9".to_string(),
            },
        ]
    );
    let lines: Vec<String> = requirements.iter().map(fragment::render_line).collect();
    assert_eq!(
        lines,
        vec![
            "node\troot\t\tparent\tnode-9".to_string(),
            "part\tnode-5\t\tmask source\tnode-9".to_string(),
        ]
    );
}

/// A weld that reaches into a base part needs the seam, not just the node —
/// the one requirement that carries two Ids.
#[test]
fn a_weld_reaching_into_the_base_requires_the_seam() {
    let dir = tmp("addons-requirements-seam");
    let addon = dir.join("lower.clm");
    fragment::extract(&common::fixture("welded_seam"), &["node-2".into()], &addon).unwrap();

    let requirements = fragment::requirements(&addon).unwrap();
    assert_eq!(
        requirements,
        vec![
            Requirement {
                id: Required::Node(NodeId::new("root").unwrap()),
                field: "parent",
                owner: "node-2".to_string(),
            },
            Requirement {
                id: Required::Seam(
                    NodeId::new("node-1").unwrap(),
                    SeamId::new("weld-0-a").unwrap(),
                ),
                field: "weld end",
                owner: "node-2".to_string(),
            },
        ]
    );
    assert_eq!(
        fragment::render_line(&requirements[1]),
        "seam\tnode-1\tweld-0-a\tweld end\tnode-2"
    );
}

#[test]
fn an_install_error_is_reported_verbatim() {
    let dir = tmp("addons-install-error");
    let original = common::fixture("composite_masks");

    let addon = dir.join("addon.clm");
    fragment::extract(&original, &["node-9".into()], &addon).unwrap();

    // A base that lacks the mask source the addon reaches for.
    let base = base_without(&["node-9", "node-5"], &["tex-4"], &dir, "base");

    let out = dir.join("merged.clm");
    let error = fragment::merge(&base, &addon, &out).unwrap_err();
    assert_eq!(
        error.to_string(),
        InstallError::Missing {
            id: Required::Part(NodeId::new("node-5").unwrap()),
            field: "mask source",
            owner: "node-9".to_string(),
        }
        .to_string()
    );
    assert!(!out.exists(), "a refused merge wrote a file");
}

/// Two addons that provide one Id are alternatives, so installing one into a
/// base that already has it is refused rather than renamed around.
#[test]
fn an_id_the_base_already_has_is_a_collision() {
    let dir = tmp("addons-collision");
    let original = common::fixture("composite_masks");
    let addon = dir.join("addon.clm");
    fragment::extract(&original, &["node-9".into()], &addon).unwrap();

    let out = dir.join("merged.clm");
    let error = fragment::merge(&original, &addon, &out).unwrap_err();
    assert_eq!(
        error.to_string(),
        InstallError::Collision {
            kind: "node",
            id: "node-9".to_string(),
        }
        .to_string()
    );
}

/// The two wire shapes are disjoint and `merge` never guesses: its addon
/// argument goes to the fragment reader, so a complete model says so.
#[test]
fn a_complete_model_handed_in_as_an_addon_says_so() {
    let dir = tmp("addons-not-a-fragment");
    let original = common::fixture("composite_masks");

    let error = fragment::merge(&original, &original, &dir.join("merged.clm")).unwrap_err();
    assert!(
        matches!(error, Error::NotAFragment { .. }),
        "expected NotAFragment, got {error}"
    );
    assert!(
        error
            .to_string()
            .contains("has no parent, so this file is a complete model"),
        "{error}"
    );
}

#[test]
fn the_binary_extracts_merges_and_lists_requirements() {
    let dir = tmp("addons-cli");
    let original = common::fixture("composite_masks");
    let addon = dir.join("addon.clm");

    let (code, out, err) = common::run(&[
        "extract",
        original.to_str().unwrap(),
        "--out",
        addon.to_str().unwrap(),
        "--",
        "node-9",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("extracted 2 node(s)"), "{out}");

    let (code, out, err) = common::run(&["requirements", addon.to_str().unwrap()]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec![
            "node\troot\t\tparent\tnode-9",
            "part\tnode-5\t\tmask source\tnode-9",
        ]
    );

    let (code, out, err) = common::run(&["requirements", addon.to_str().unwrap(), "--json"]);
    assert_eq!(code, 0, "{err}");
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(parsed[0]["kind"], "node");
    assert_eq!(parsed[0]["id"], "root");
    assert_eq!(parsed[0]["seam"], serde_json::Value::Null);
    assert_eq!(parsed[1]["kind"], "part");
    assert_eq!(parsed[1]["field"], "mask source");

    let base = base_without(&["node-9"], &["tex-4"], &dir, "base");

    let merged = dir.join("merged.clm");
    let (code, out, err) = common::run(&[
        "merge",
        base.to_str().unwrap(),
        addon.to_str().unwrap(),
        "-o",
        merged.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("installed 2 node(s)"), "{out}");

    let (code, out, _) =
        common::run(&["diff", original.to_str().unwrap(), merged.to_str().unwrap()]);
    assert_eq!(code, 0, "the merged model should match the original: {out}");
}

/// A requirements scan is a scan of the wire, so it works on a file the
/// complete-model reader would refuse.
#[test]
fn requirements_reads_a_file_no_model_reader_would_take() {
    let dir = tmp("addons-requirements-hostile");
    let mut clm = decode(&common::fixture("mip_checker"));
    clm.textures.clear();

    let file = common::write_clm(&dir, "no-textures", &clm);
    assert!(Model::from_clm_bytes(&read(&file)).is_err());

    let requirements = fragment::requirements(&file).unwrap();
    assert_eq!(
        requirements
            .iter()
            .map(fragment::render_line)
            .collect::<Vec<_>>(),
        vec![
            "texture\ttex-0\t\talbedo\tnode-1".to_string(),
            "texture\ttex-0\t\talbedo\tnode-2".to_string(),
        ]
    );
}
