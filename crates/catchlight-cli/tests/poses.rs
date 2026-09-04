#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What `poses` promises a rig evaluator: the tables it keys against, the
//! value a key maps to, what a diff leaves out, and the grid a pair covers.
//!
//! CPU only, so unlike the render suite this needs no GPU.

mod common;

use catchlight_cli::poses::{self, Cap, Poses};
use catchlight_core::{Model, NodeKind, Puppet};

fn dump(name: &str) -> Poses {
    let model = Model::from_clm_bytes(&common::read(&common::fixture(name))).expect("load");
    poses::build(&model)
}

/// Every Part of `name`, in tree order, as `(id, name)`.
fn parts_of(name: &str) -> Vec<(String, String)> {
    let model = Model::from_clm_bytes(&common::read(&common::fixture(name))).expect("load");
    let puppet = Puppet::new(&model);
    puppet
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Part(_)))
        .map(|(idx, node)| {
            (
                puppet.node_id(idx).expect("a part has an id").to_string(),
                node.name.clone(),
            )
        })
        .collect()
}

fn keys(map: &std::collections::BTreeMap<u32, impl Sized>) -> Vec<u32> {
    map.keys().copied().collect()
}

fn is_empty(cap: &Cap) -> bool {
    cap.verts.is_empty()
        && cap.opacity.is_empty()
        && cap.z_order.is_empty()
        && cap.anchors.is_empty()
}

#[test]
fn the_parts_table_is_every_part_in_tree_order() {
    let dumped = dump("welded_seam");
    let listed: Vec<(String, String)> = dumped
        .parts
        .iter()
        .map(|part| (part.id.to_string(), part.name.clone()))
        .collect();
    assert_eq!(listed, parts_of("welded_seam"));
    assert_eq!(
        dumped.parts.iter().map(|p| &p.name).collect::<Vec<_>>(),
        vec!["upper", "lower"]
    );
    // No physics node in this fixture, so no anchors anywhere.
    assert!(dumped.physics.is_empty());
    assert!(dumped.rest.anchors.is_empty());
}

#[test]
fn rest_holds_every_part_in_every_map_that_applies() {
    let dumped = dump("welded_seam");
    let every: Vec<u32> = (0..dumped.parts.len() as u32).collect();
    assert_eq!(keys(&dumped.rest.verts), every, "verts");
    assert_eq!(keys(&dumped.rest.opacity), every, "opacity");
    assert_eq!(keys(&dumped.rest.z_order), every, "z_order");
    for bytes in dumped.rest.verts.values() {
        assert_eq!(bytes.len() % 8, 0, "two little-endian f32 per vertex");
        assert!(!bytes.is_empty(), "a part's rest verts are never empty");
    }
}

#[test]
fn a_key_pose_is_posed_at_the_value_its_position_maps_to() {
    for name in ["welded_seam", "two_param_grid"] {
        for param in &dump(name).params {
            assert_eq!(
                param.poses.len(),
                param.key_positions.len(),
                "{name}: {} has one pose per key",
                param.id
            );
            for (pose, position) in param.poses.iter().zip(&param.key_positions) {
                let expected = param.min + position * (param.max - param.min);
                assert_eq!(pose.value, expected, "{name}: {}", param.id);
            }
        }
    }
}

#[test]
fn the_key_at_a_params_default_changes_nothing() {
    for name in ["welded_seam", "two_param_grid"] {
        for param in &dump(name).params {
            let at_default = param
                .poses
                .iter()
                .find(|pose| pose.value == param.default)
                .unwrap_or_else(|| panic!("{name}: {} has a key at its default", param.id));
            assert!(
                is_empty(&at_default.cap),
                "{name}: {} at its default differs from rest",
                param.id
            );
        }
    }
}

/// The far end of a sweep has to actually move something, or every other
/// assertion here would hold just as well for a dump that recorded nothing.
#[test]
fn a_non_default_key_moves_a_part() {
    let dumped = dump("welded_seam");
    let pull = param(&dumped, "pull");
    let far = pull.poses.last().expect("a far key");
    assert_ne!(far.value, pull.default);
    assert!(
        !far.cap.verts.is_empty(),
        "pulling the seam moved no vertices"
    );
    // A diff names parts by index into `parts`, so every key is one.
    for index in far.cap.verts.keys() {
        assert!((*index as usize) < dumped.parts.len());
    }
}

/// A sweep records what moved and, just as importantly, leaves out what did
/// not: the diff is what makes a hundred-part dump small.
#[test]
fn a_sweep_names_the_parts_it_moved_and_no_others() {
    let dumped = dump("two_param_grid");
    let driven = index_of(&dumped, "driven");
    let swept = index_of(&dumped, "swept");

    let grid_x = param(&dumped, "grid_x");
    let far = grid_x.poses.last().expect("a far key");
    assert_ne!(far.value, grid_x.default);
    assert_eq!(
        keys(&far.cap.verts),
        vec![driven],
        "the joint grid moves only the part it is bound to"
    );
    assert!(far.cap.opacity.is_empty(), "no opacity binding on grid_x");
    assert!(far.cap.z_order.is_empty(), "no z binding on grid_x");
    assert!(far.cap.anchors.is_empty(), "grid_x moves no transform");

    // The sweep param drives one binding of every other kind, so its far key
    // is the one pose that fills all four maps.
    let sweep = param(&dumped, "sweep").poses.last().expect("a far key");
    assert_eq!(keys(&sweep.cap.verts), vec![swept], "the swept part moved");
    assert_eq!(
        keys(&sweep.cap.opacity),
        vec![driven],
        "the driven part faded"
    );
    assert_eq!(keys(&sweep.cap.z_order), vec![swept], "the swept part rose");
    assert_eq!(
        keys(&sweep.cap.anchors),
        vec![0],
        "the pendulum moved with it"
    );
}

#[test]
fn a_pair_covers_the_product_of_both_key_lists_b_outer_a_inner() {
    let dumped = dump("two_param_grid");
    let [pair] = &dumped.pairs[..] else {
        panic!("one two-param binding, so one pair");
    };
    assert!(
        pair.a < pair.b,
        "a pair is ordered by id: {} {}",
        pair.a,
        pair.b
    );

    let a = param(&dumped, "grid_x");
    let b = param(&dumped, "grid_y");
    assert_eq!(pair.a, a.id);
    assert_eq!(pair.b, b.id);
    assert_eq!(
        pair.poses.len(),
        a.key_positions.len() * b.key_positions.len()
    );

    // `b` outer, `a` inner: b holds while a runs the row.
    let expected: Vec<[f32; 2]> = b
        .key_positions
        .iter()
        .flat_map(|bp| {
            a.key_positions
                .iter()
                .map(move |ap| [a.min + ap * (a.max - a.min), b.min + bp * (b.max - b.min)])
        })
        .collect();
    let actual: Vec<[f32; 2]> = pair.poses.iter().map(|pose| pose.value).collect();
    assert_eq!(actual, expected);

    // The grid point where both params sit at their defaults is the rest pose.
    let both_default = pair
        .poses
        .iter()
        .find(|pose| pose.value == [a.default, b.default])
        .expect("the grid holds the all-defaults cell");
    assert!(is_empty(&both_default.cap));
    // And the rest of the grid is not: a pair exists because a sweep of
    // either param alone reaches only one row of it.
    assert!(
        pair.poses.iter().filter(|p| !is_empty(&p.cap)).count() > a.key_positions.len(),
        "the joint grid moves more than one row"
    );
}

#[test]
fn two_runs_write_the_same_bytes() {
    let dir = common::tmp("poses-deterministic");
    let model = common::fixture("two_param_grid");
    let mut written = Vec::new();
    for run in 0..2 {
        let out = dir.join(format!("run{run}.cbor"));
        let (code, _, stderr) = common::run(&[
            "poses",
            model.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "stderr: {stderr}");
        written.push(common::read(&out));
    }
    assert_eq!(written[0], written[1], "two runs differ");
    assert!(!written[0].is_empty());

    // And what the binary wrote decodes to what the library builds.
    let decoded: Poses = ciborium::from_reader(&written[0][..]).expect("decode the cbor");
    assert_eq!(decoded, dump("two_param_grid"));
}

#[test]
fn a_file_that_is_not_a_clm_is_refused() {
    let dir = common::tmp("poses-not-a-clm");
    let input = dir.join("model.inx");
    std::fs::write(&input, b"not a model").unwrap();
    let (code, _, stderr) = common::run(&[
        "poses",
        input.to_str().unwrap(),
        "--out",
        dir.join("out.cbor").to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "an error exits 2");
    assert!(stderr.contains("is not a .clm"), "{stderr}");
}

fn index_of(dumped: &Poses, name: &str) -> u32 {
    dumped
        .parts
        .iter()
        .position(|part| part.name == name)
        .unwrap_or_else(|| panic!("a part named {name}")) as u32
}

fn param<'a>(dumped: &'a Poses, name: &str) -> &'a poses::ParamPoses {
    dumped
        .params
        .iter()
        .find(|param| param.name == name)
        .unwrap_or_else(|| panic!("a param named {name}"))
}
