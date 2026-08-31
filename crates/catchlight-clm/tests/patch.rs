#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `patch` on copies of the committed fixtures.
//!
//! Every case asserts three things: the file still loads, `diff` reports
//! exactly the one field that was asked for, and the texture bytes came
//! through untouched. The last two tests are the "no image is decoded" proof —
//! one on a file whose textures are not images at all, one on a file large
//! enough that a per-pixel pass would show.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use catchlight_clm::diff::diff;
use catchlight_clm::patch::{self, node_fields, Kind, PARAM_FIELDS};
use catchlight_clm::Error;
use catchlight_core::formats::clm::{
    ClmComposite, ClmFile, ClmNode, ClmNodeKind, ClmParam, ClmSimplePhysics, ClmTransform,
};
use catchlight_core::id::{NodeId, ParamId, TexId, MAX_NAME_BYTES};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::Model;

use common::{copy_fixture, decode, read, tmp, write_clm};

#[test]
fn one_field_changes_and_the_file_still_loads() {
    let dir = tmp("patch-one-field");
    let file = copy_fixture("composite_masks", &dir);
    let before = decode(&file);

    let change = patch::run(&file, "node-1", "z_order", "2.5", None, None).unwrap();
    assert_eq!(change.kind, Kind::Node);
    assert!(change.changed());
    assert_eq!(change.to_string(), "node \"node-1\" z_order: -10 -> 2.5");

    let after = decode(&file);
    assert_eq!(
        diff(&before, &after),
        vec!["~ node node-1 z_order: -10 -> 2.5".to_string()]
    );
    let model = Model::from_clm_bytes(&read(&file)).unwrap();
    assert_eq!(
        model.node(&NodeId::new("node-1").unwrap()).unwrap().z_order,
        2.5
    );
}

/// Every field the tool advertises for a kind can be set, on a file carrying
/// one node of every kind — so the list in `--help`'s errors and the fields
/// that actually resolve cannot drift apart.
#[test]
fn every_advertised_field_can_be_set() {
    let dir = tmp("patch-every-field");
    let file = write_clm(&dir, "kitchen-sink", &kitchen_sink());

    let mut cases: Vec<(String, &'static str, String)> = Vec::new();
    let base = decode(&file);
    for node in &base.doc.nodes {
        for field in node_fields(&node.kind) {
            cases.push((node.id.to_string(), field, value_for(field).to_string()));
        }
    }
    for field in PARAM_FIELDS {
        cases.push(("param-0".to_string(), field, value_for(field).to_string()));
    }
    assert!(cases.len() > 40, "only {} fields covered", cases.len());

    for (id, field, value) in cases {
        let target = dir.join(format!("{id}-{field}.clm"));
        let change = patch::run(&file, &id, field, &value, None, Some(&target)).unwrap();
        assert!(
            change.changed(),
            "{id}.{field} = {value} did not move the value ({change})"
        );

        let after = decode(&target);
        let lines = diff(&base, &after);
        assert_eq!(lines.len(), 1, "{id}.{field} = {value} gave {lines:?}");
        assert!(
            lines[0].contains(&format!(" {field}: ")),
            "{id}.{field} = {value} gave {lines:?}"
        );
        Model::from_clm_bytes(&read(&target))
            .unwrap_or_else(|e| panic!("{id}.{field} = {value} no longer loads: {e}"));
        std::fs::remove_file(&target).unwrap();
    }
}

/// A field that already holds the value it is being set to still rewrites the
/// file — byte for byte the file it was.
#[test]
fn setting_a_field_to_what_it_already_holds_rewrites_the_same_bytes() {
    let dir = tmp("patch-byte-stable");
    let file = copy_fixture("composite_masks", &dir);
    let before = read(&file);

    let change = patch::run(&file, "node-1", "z_order", "-10", None, None).unwrap();
    assert!(!change.changed(), "{change}");
    assert_eq!(read(&file), before, "an unchanged patch rewrote the file");
}

#[test]
fn writing_elsewhere_leaves_the_input_alone() {
    let dir = tmp("patch-out");
    let file = copy_fixture("mip_checker", &dir);
    let before = read(&file);
    let out = dir.join("patched.clm");

    patch::run(&file, "node-1", "opacity", "0.5", None, Some(&out)).unwrap();

    assert_eq!(read(&file), before, "the input was written to");
    assert_ne!(read(&out), before);
}

#[test]
fn an_unknown_field_lists_the_ones_the_node_has() {
    let dir = tmp("patch-unknown-field");
    let file = copy_fixture("mip_checker", &dir);

    // `dynamic` is a mesh group's field, and node-1 is a part.
    let error = patch::run(&file, "node-1", "dynamic", "true", None, None).unwrap_err();
    let Error::NoSuchField {
        owner,
        field,
        known,
    } = &error
    else {
        panic!("expected NoSuchField, got {error}");
    };
    assert_eq!(field, "dynamic");
    assert!(owner.contains("part"), "{owner}");
    assert!(known.contains(&"opacity"), "{known:?}");
    assert!(known.contains(&"z_order"), "{known:?}");
    assert!(!known.contains(&"dynamic"), "{known:?}");
    assert!(error.to_string().contains("blend_mode"), "{error}");
    assert_eq!(read(&file), read(&common::fixture("mip_checker")));
}

#[test]
fn a_value_of_the_wrong_type_names_the_field_and_what_it_takes() {
    let dir = tmp("patch-bad-value");
    let file = copy_fixture("mip_checker", &dir);

    for (field, value, expected) in [
        ("z_order", "high", "a number"),
        ("enabled", "yes", "true or false"),
        ("blend_mode", "multiply", "one of Normal, Multiply"),
    ] {
        let error = patch::run(&file, "node-1", field, value, None, None).unwrap_err();
        let Error::BadValue {
            field: named,
            expected: takes,
            value: given,
        } = &error
        else {
            panic!("expected BadValue for {field}={value}, got {error}");
        };
        assert_eq!(named, field);
        assert_eq!(given, value);
        assert!(takes.starts_with(expected), "{takes}");
    }
    assert_eq!(read(&file), read(&common::fixture("mip_checker")));
}

#[test]
fn an_id_that_names_nothing_is_refused() {
    let dir = tmp("patch-no-such-id");
    let file = copy_fixture("mip_checker", &dir);

    let error = patch::run(&file, "node-99", "z_order", "1", None, None).unwrap_err();
    let Error::NoSuchId { kind, id, .. } = &error else {
        panic!("expected NoSuchId, got {error}");
    };
    assert_eq!(*kind, "node or param");
    assert_eq!(id, "node-99");
}

/// A node Id and a param Id are separate namespaces, so one string can be
/// both. The tool refuses to pick for you.
#[test]
fn an_id_that_names_both_a_node_and_a_param_asks_which() {
    let dir = tmp("patch-ambiguous");
    let mut clm = decode(&common::fixture("welded_seam"));
    let shared = ParamId::new("node-1").unwrap();
    clm.doc.params[0].id = shared.clone();
    clm.doc.bindings[0].params = vec![shared];
    let file = write_clm(&dir, "shared-id", &clm);

    let error = patch::run(&file, "node-1", "name", "Both", None, None).unwrap_err();
    assert!(
        matches!(error, Error::AmbiguousId { .. }),
        "expected AmbiguousId, got {error}"
    );
    assert!(error.to_string().contains("--node or --param"), "{error}");

    let node = dir.join("as-node.clm");
    patch::run(
        &file,
        "node-1",
        "name",
        "Node",
        Some(Kind::Node),
        Some(&node),
    )
    .unwrap();
    assert_eq!(
        diff(&clm, &decode(&node)),
        vec!["~ node node-1 name: \"upper\" -> \"Node\"".to_string()]
    );

    let param = dir.join("as-param.clm");
    patch::run(
        &file,
        "node-1",
        "name",
        "Param",
        Some(Kind::Param),
        Some(&param),
    )
    .unwrap();
    assert_eq!(
        diff(&clm, &decode(&param)),
        vec!["~ param node-1 name: \"pull\" -> \"Param\"".to_string()]
    );
}

/// `albedo` is the only cross-reference `patch` will write, and it has to name
/// a texture the file carries — otherwise the patch would produce a file the
/// reader refuses.
#[test]
fn albedo_is_the_one_cross_reference_and_it_must_resolve() {
    let dir = tmp("patch-albedo");
    let file = copy_fixture("welded_seam", &dir);

    let error = patch::run(&file, "node-1", "albedo", "tex-nope", None, None).unwrap_err();
    assert!(
        matches!(error, Error::BadValue { .. }),
        "expected BadValue, got {error}"
    );
    assert!(
        error.to_string().contains("a texture this file carries"),
        "{error}"
    );
    assert_eq!(read(&file), read(&common::fixture("welded_seam")));

    // Every texture a model carries is drawn by a part. `patch` moves one
    // field and collects nothing, so pointing the only part drawing `tex-0`
    // away from it would write a file no reader takes — and `patch` refuses
    // with the reader's own error rather than saving it.
    let orphaned = dir.join("orphaned.clm");
    let error = patch::run(&file, "node-1", "albedo", "tex-1", None, Some(&orphaned)).unwrap_err();
    assert!(
        matches!(error, Error::PatchBreaksFile { .. }),
        "expected PatchBreaksFile, got {error}"
    );
    assert!(error.to_string().contains("tex-0"), "{error}");
    assert!(!orphaned.exists(), "a refused patch writes nothing");

    // Where the texture has a second part drawing it, the swap and the unmap
    // both go through.
    let (clm, [from, to], shared, other) = shared_albedo();
    let file = write_clm(&dir, "shared", &clm);
    let base = decode(&file);

    let swapped = dir.join("swapped.clm");
    patch::run(
        &file,
        from.as_str(),
        "albedo",
        other.as_str(),
        None,
        Some(&swapped),
    )
    .unwrap();
    assert_eq!(
        diff(&base, &decode(&swapped)),
        vec![format!(
            "~ node {from} albedo: {:?} -> {:?}",
            shared.as_str(),
            other.as_str()
        )]
    );
    Model::from_clm_bytes(&read(&swapped)).unwrap();

    let unmapped = dir.join("unmapped.clm");
    patch::run(&file, to.as_str(), "albedo", "none", None, Some(&unmapped)).unwrap();
    assert_eq!(
        diff(&base, &decode(&unmapped)),
        vec![format!(
            "~ node {to} albedo: {:?} -> (none)",
            shared.as_str()
        )]
    );
    Model::from_clm_bytes(&read(&unmapped)).unwrap();
}

/// `composite_masks` with its second part drawing the first's texture and the
/// texture that left behind dropped: one texture with two parts drawing it, so
/// an albedo can move off one of them and still leave every texture a user.
/// Hands back the two parts sharing it, the texture they share, and another
/// the file still carries to swap to.
fn shared_albedo() -> (ClmFile, [NodeId; 2], TexId, TexId) {
    fn drawn(clm: &ClmFile) -> Vec<(NodeId, TexId)> {
        clm.doc
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                ClmNodeKind::Part(p) => Some((n.id.clone(), p.albedo.clone()?)),
                _ => None,
            })
            .collect()
    }

    let mut clm = decode(&common::fixture("composite_masks"));
    let all = drawn(&clm);
    let (first, shared) = all.first().cloned().expect("a part drawing a texture");
    // A part drawing a texture nothing else draws: repointing it frees that
    // texture, and dropping it leaves every texture left with a user.
    let (second, freed) = all
        .iter()
        .find(|(id, t)| {
            id != &first && t != &shared && all.iter().filter(|(_, u)| u == t).count() == 1
        })
        .cloned()
        .expect("a part with a texture of its own");
    for node in &mut clm.doc.nodes {
        if node.id == second {
            if let ClmNodeKind::Part(p) = &mut node.kind {
                p.albedo = Some(shared.clone());
            }
        }
    }
    clm.textures.retain(|t| t.id != freed);
    let other = clm
        .textures
        .iter()
        .map(|t| t.id.clone())
        .find(|t| t != &shared)
        .expect("a second texture to swap to");
    (clm, [first, second], shared, other)
}

/// A `Name` is capped, and a Model truncates a longer one on load — so writing
/// one would give a file that is not what it reads back as.
#[test]
fn a_name_past_the_cap_is_refused() {
    let dir = tmp("patch-long-name");
    let file = copy_fixture("mip_checker", &dir);

    let long = "n".repeat(MAX_NAME_BYTES + 1);
    let error = patch::run(&file, "node-1", "name", &long, None, None).unwrap_err();
    assert!(
        matches!(error, Error::BadValue { .. }),
        "expected BadValue, got {error}"
    );
    assert!(error.to_string().contains("at most 256 bytes"), "{error}");

    let at_cap = "n".repeat(MAX_NAME_BYTES);
    patch::run(&file, "node-1", "name", &at_cap, None, None).unwrap();
    let model = Model::from_clm_bytes(&read(&file)).unwrap();
    assert_eq!(
        model
            .node(&NodeId::new("node-1").unwrap())
            .unwrap()
            .name
            .as_str(),
        at_cap
    );
}

/// A file that did not load before the patch says so rather than blaming the
/// patch — and is still not written to.
#[test]
fn a_file_that_did_not_load_before_says_so_and_is_left_alone() {
    let dir = tmp("patch-already-broken");
    let mut clm = decode(&common::fixture("mip_checker"));
    if let ClmNodeKind::Part(part) = &mut clm.doc.nodes[1].kind {
        part.albedo = Some(TexId::new("tex-gone").unwrap());
    } else {
        panic!("node-1 should be a part");
    }
    let file = write_clm(&dir, "broken", &clm);
    let before = read(&file);

    let error = patch::run(&file, "root", "name", "Root", None, None).unwrap_err();
    assert!(
        matches!(error, Error::AlreadyBroken { .. }),
        "expected AlreadyBroken, got {error}"
    );
    assert!(error.to_string().contains("tex-gone"), "{error}");
    assert_eq!(read(&file), before, "a refused patch wrote to the file");
}

// ---- the no-decode proof ---------------------------------------------------

/// The definitive proof: the texture payload is not a decodable image at all,
/// so anything that tried to decode it would fail here. It does not, and the
/// bytes come out the far side identical.
#[test]
fn patching_a_file_whose_textures_are_not_images() {
    let dir = tmp("patch-not-an-image");
    let mut clm = decode(&common::fixture("mip_checker"));
    let garbage: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
    clm.textures[0].data = garbage.clone();
    let file = write_clm(&dir, "not-an-image", &clm);

    let change = patch::run(&file, "node-1", "opacity", "0.5", None, None).unwrap();
    assert!(change.changed());

    let after = decode(&file);
    assert_eq!(
        after.textures[0].data, garbage,
        "the texture bytes did not survive the patch"
    );
    assert_eq!(
        diff(&clm, &after),
        vec!["~ node node-1 opacity: 1 -> 0.5".to_string()]
    );
}

/// The size bound: a file whose textures are a hundred megabytes patches in
/// the time it takes to copy it, because nothing looks inside them. A
/// per-pixel pass over this much data would not fit in the bound.
#[test]
fn patching_a_hundred_megabyte_texture_file_stays_linear_in_bytes() {
    const MEGABYTE: usize = 1024 * 1024;
    const TEXTURE_BYTES: usize = 100 * MEGABYTE;
    const BOUND: Duration = Duration::from_secs(20);

    let dir = tmp("patch-hundred-megabytes");
    let mut clm = decode(&common::fixture("mip_checker"));
    clm.textures[0].data = vec![0x5a; TEXTURE_BYTES];
    let file = write_clm(&dir, "heavy", &clm);
    assert!(
        std::fs::metadata(&file).unwrap().len() as usize > TEXTURE_BYTES,
        "the fixture should be dominated by its texture"
    );

    let started = Instant::now();
    patch::run(&file, "node-1", "z_order", "3", None, None).unwrap();
    let elapsed = started.elapsed();

    let after = decode(&file);
    assert_eq!(after.textures[0].data.len(), TEXTURE_BYTES);
    assert!(
        after.textures[0].data.iter().all(|b| *b == 0x5a),
        "the texture bytes were rewritten"
    );
    assert_eq!(after.doc.nodes[1].z_order, 3.0);
    assert!(
        elapsed < BOUND,
        "patching {TEXTURE_BYTES} bytes of texture took {elapsed:?}, over the {BOUND:?} bound"
    );

    // Do not leave a hundred megabytes behind for the rest of the suite.
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- the command line ------------------------------------------------------

/// An Id may not begin with `-`, so no command has to be reached through
/// `--`: an argument that opens with a dash is an option and nothing else.
#[test]
fn an_id_cannot_begin_with_a_dash() {
    assert!(NodeId::new("-odd").is_err());

    let dir = tmp("patch-leading-dash");
    let file = copy_fixture("mip_checker", &dir);
    let path = file.to_str().unwrap();
    let before = read(&file);

    let (code, _, err) = common::run(&["patch", path, "--", "-odd", "z_order", "1"]);
    assert_eq!(code, 2, "even after `--`, `-odd` is not an id: {err}");
    assert_eq!(read(&file), before, "a refused patch writes nothing");
}

#[test]
fn the_binary_says_when_a_patch_changed_nothing() {
    let dir = tmp("patch-cli-unchanged");
    let file = copy_fixture("mip_checker", &dir);
    let path = file.to_str().unwrap();

    let (code, out, err) = common::run(&["patch", path, "node-1", "opacity", "1"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("(unchanged)"), "{out}");

    let (code, _, err) = common::run(&["patch", path, "node-1", "nope", "1"]);
    assert_eq!(code, 2);
    assert!(err.contains("has no field \"nope\""), "{err}");
}

// ---- helpers ---------------------------------------------------------------

/// A value that is guaranteed to differ from the kitchen sink's current one.
fn value_for(field: &str) -> &'static str {
    match field {
        "enabled" => "false",
        "name" => "Renamed",
        "blend_mode" => "Multiply",
        "pendulum" => "SpringPendulum",
        "map_mode" => "XY",
        "albedo" => "none",
        "dynamic"
        | "lock_to_root"
        | "local_only"
        | "propagate_meshgroup"
        | "translate_children" => "true",
        _ => "0.25",
    }
}

/// `mip_checker` with one node of every kind and a param, so the field table
/// can be walked end to end on a file that loads.
fn kitchen_sink() -> ClmFile {
    let mut clm = decode(&Path::new(&common::models_dir()).join("mip_checker.clm"));
    let root = NodeId::new("root").unwrap();

    // node-2 is the second quad; make it the mesh group, reusing its mesh.
    let ClmNodeKind::Part(part) = clm.doc.nodes[2].kind.clone() else {
        panic!("node-2 should be a part");
    };
    clm.doc.nodes[2].kind = ClmNodeKind::MeshGroup(catchlight_core::formats::clm::ClmMeshGroup {
        mesh: part.mesh,
        dynamic: false,
        translate_children: false,
    });

    // A second part drawing `tex-0`, so unmapping either one still leaves the
    // texture with a part drawing it — a texture no part draws is a file no
    // reader takes.
    clm.doc.nodes.push(ClmNode {
        id: NodeId::new("node-5").unwrap(),
        parent: Some(root.clone()),
        name: "Twin".into(),
        enabled: true,
        z_order: 0.0,
        transform: ClmTransform::default(),
        lock_to_root: false,
        kind: clm.doc.nodes[1].kind.clone(),
    });
    clm.doc.nodes.push(ClmNode {
        id: NodeId::new("node-3").unwrap(),
        parent: Some(root.clone()),
        name: "Composite".into(),
        enabled: true,
        z_order: 0.0,
        transform: ClmTransform::default(),
        lock_to_root: false,
        kind: ClmNodeKind::Composite(ClmComposite {
            opacity: 1.0,
            blend_mode: catchlight_core::components::BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            masks: Vec::new(),
            mask_threshold: 0.5,
            propagate_meshgroup: false,
        }),
    });
    clm.doc.nodes.push(ClmNode {
        id: NodeId::new("node-4").unwrap(),
        parent: Some(root),
        name: "Pendulum".into(),
        enabled: true,
        z_order: 0.0,
        transform: ClmTransform::default(),
        lock_to_root: false,
        kind: ClmNodeKind::SimplePhysics(ClmSimplePhysics {
            kind: PendulumKind::RigidPendulum,
            map_mode: PhysicsParamMapMode::AngleLength,
            local_only: false,
            target_params: [None, None],
            gravity: 9.8,
            length: 100.0,
            frequency: 1.0,
            angle_damping: 0.5,
            length_damping: 0.5,
            output_scale: [1.0, 1.0],
        }),
    });
    clm.doc.params.push(ClmParam {
        id: ParamId::new("param-0").unwrap(),
        name: "Turn".into(),
        min: 0.0,
        max: 1.0,
        default: 0.0,
        key_positions: vec![0.0, 1.0],
    });
    clm
}
