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

use catchlight_cli::diff::diff;
use catchlight_cli::{fragment, Error};
use catchlight_core::id::{NodeId, SlotId, TexId};
use catchlight_core::{InstallError, Model, Required, Requirement};

use common::{decode, read, tmp};

/// The base a `merge` puts an addon back into: the model with the given nodes
/// gone. The textures only they drew go with them — `delete_node` takes them,
/// because a texture no part draws is not a thing a model holds — and leaving
/// one behind would collide with the addon's own copy of it.
fn base_without(nodes: &[&str], dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let mut model = Model::from_clm_bytes(&read(&common::fixture("composite_masks"))).unwrap();
    for node in nodes {
        model.delete_node(&NodeId::new(node).unwrap()).unwrap();
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
        "an addon carries every texture its parts draw"
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

    let base = base_without(&["node-9"], &dir, "base");
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

/// A texture is never a requirement: an addon carries the textures its own
/// parts draw, so `extract` copies one the base still draws instead of asking
/// the base for it. The addon then provides that Id, and installing it back
/// into the base that still has it is the collision it now is.
#[test]
fn a_shared_texture_is_copied_into_the_addon_and_never_required() {
    let dir = tmp("addons-shared-texture");
    let original = common::fixture("composite_masks");

    // Give `node-10` the texture `node-6` draws, and drop the one that
    // leaves behind, so the cut and the base share a texture.
    let mut clm = decode(&original);
    let keep = part_albedo(&clm, "node-6");
    let freed = part_albedo(&clm, "node-10");
    set_part_albedo(&mut clm, "node-10", &keep);
    clm.textures.retain(|t| t.id != freed);
    let shared = common::write_clm(&dir, "shared", &clm);

    let addon = dir.join("hood.clm");
    let extracted = fragment::extract(&shared, &["node-9".into()], &addon).unwrap();
    assert_eq!(extracted.textures, 1, "the shared texture came along");

    let requirements = fragment::requirements(&addon).unwrap();
    assert!(
        requirements
            .iter()
            .all(|r| !matches!(r.id, Required::Node(_) if r.field == "albedo")),
        "{requirements:?}"
    );
    assert_eq!(
        requirements
            .iter()
            .map(fragment::render_line)
            .collect::<Vec<_>>(),
        vec![
            "node\troot\t\tparent\tnode-9".to_string(),
            "part\tnode-5\t\tmask source\tnode-9".to_string(),
        ],
        "the albedo is carried, not required"
    );
    assert_eq!(
        Model::from_clm_bytes_fragment(&read(&addon))
            .unwrap()
            .texture_ids(),
        &[keep.clone()][..]
    );

    // Cut the subtree out of the base and the shared texture stays — `node-6`
    // still draws it — so the Id the addon provides is taken and the merge
    // says so instead of installing a second copy.
    let mut base = Model::from_clm_bytes(&read(&shared)).unwrap();
    base.delete_node(&NodeId::new("node-9").unwrap()).unwrap();
    assert!(base.texture(&keep).is_some(), "node-6 still draws it");
    let cut = dir.join("cut.clm");
    std::fs::write(&cut, base.to_clm_bytes().unwrap()).unwrap();

    let merged = dir.join("merged.clm");
    let error = fragment::merge(&cut, &addon, &merged).unwrap_err();
    assert!(
        matches!(
            &error,
            Error::Install(InstallError::Collision { kind: "texture", id }) if id == keep.as_str()
        ),
        "{error}"
    );
}

fn part_albedo(clm: &catchlight_core::formats::clm::ClmFile, node: &str) -> TexId {
    clm.doc
        .nodes
        .iter()
        .find(|n| n.id.as_str() == node)
        .and_then(|n| match &n.kind {
            catchlight_core::formats::clm::ClmNodeKind::Part(p) => p.albedo.clone(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{node} draws a texture"))
}

fn set_part_albedo(clm: &mut catchlight_core::formats::clm::ClmFile, node: &str, tex: &TexId) {
    for n in &mut clm.doc.nodes {
        if n.id.as_str() == node {
            if let catchlight_core::formats::clm::ClmNodeKind::Part(p) = &mut n.kind {
                p.albedo = Some(tex.clone());
            }
        }
    }
}

/// A weld that reaches into a base part needs each slot it pairs there, not
/// just the node — the one requirement kind that carries two Ids.
#[test]
fn a_weld_reaching_into_the_base_requires_its_slots() {
    let dir = tmp("addons-requirements-slot");
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
                id: Required::Slot(NodeId::new("node-1").unwrap(), SlotId::new("s0").unwrap(),),
                field: "weld end",
                owner: "node-2".to_string(),
            },
            Requirement {
                id: Required::Slot(NodeId::new("node-1").unwrap(), SlotId::new("s1").unwrap(),),
                field: "weld end",
                owner: "node-2".to_string(),
            },
            Requirement {
                id: Required::Slot(NodeId::new("node-1").unwrap(), SlotId::new("s2").unwrap(),),
                field: "weld end",
                owner: "node-2".to_string(),
            },
        ]
    );
    assert_eq!(
        fragment::render_line(&requirements[1]),
        "slot\tnode-1\ts0\tweld end\tnode-2"
    );
}

/// A weld whose pairs are all gone still reaches the base part, so the file
/// scan names the part — and agrees with the model's own scan.
#[test]
fn a_weld_with_no_pairs_still_requires_the_part() {
    let dir = tmp("addons-requirements-empty-weld");
    let extracted = dir.join("lower.clm");
    fragment::extract(
        &common::fixture("welded_seam"),
        &["node-2".into()],
        &extracted,
    )
    .unwrap();

    let mut clm = decode(&extracted);
    for weld in &mut clm.doc.welds {
        weld.pairs.clear();
    }
    let addon = common::write_clm(&dir, "lower-no-pairs.clm", &clm);

    let requirements = fragment::requirements(&addon).unwrap();
    assert!(
        requirements.contains(&Requirement {
            id: Required::Part(NodeId::new("node-1").unwrap()),
            field: "weld end",
            owner: "node-2".to_string(),
        }),
        "{requirements:?}"
    );
    let by_model: Vec<Requirement> = Model::from_clm_bytes_fragment(&read(&addon))
        .unwrap()
        .requirements()
        .iter()
        .cloned()
        .collect();
    assert_eq!(requirements, by_model);
}

#[test]
fn an_install_error_is_reported_verbatim() {
    let dir = tmp("addons-install-error");
    let original = common::fixture("composite_masks");

    let addon = dir.join("addon.clm");
    fragment::extract(&original, &["node-9".into()], &addon).unwrap();

    // A base that lacks the mask source the addon reaches for.
    let base = base_without(&["node-9", "node-5"], &dir, "base");

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
    assert_eq!(parsed[0]["slot"], serde_json::Value::Null);
    assert_eq!(parsed[1]["kind"], "part");
    assert_eq!(parsed[1]["field"], "mask source");

    let base = base_without(&["node-9"], &dir, "base");

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
    // Cut the root out and take the textures with it. Neither reader takes
    // this — the complete one because the parents dangle, the fragment one
    // because the albedos do — and the scan answers anyway.
    clm.doc.nodes.retain(|n| n.parent.is_some());
    clm.textures.clear();

    let file = common::write_clm(&dir, "no-root-no-textures", &clm);
    assert!(Model::from_clm_bytes(&read(&file)).is_err());
    assert!(Model::from_clm_bytes_fragment(&read(&file)).is_err());

    let requirements = fragment::requirements(&file).unwrap();
    assert_eq!(
        requirements
            .iter()
            .map(fragment::render_line)
            .collect::<Vec<_>>(),
        vec![
            "node\troot\t\tparent\tnode-1".to_string(),
            "node\troot\t\tparent\tnode-2".to_string(),
        ]
    );
}
