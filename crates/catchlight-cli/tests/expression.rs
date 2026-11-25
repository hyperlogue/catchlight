#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `expression` over copies of the committed fixtures. `two_param_grid` has
//! `param-0` in 0..1, `param-1` in -2..2 and `param-2` in 0..10.

mod common;

use std::path::{Path, PathBuf};

use catchlight_core::extension::Extension;
use catchlight_core::Model;
use catchlight_extension_expressions::{standard_visemes, Expressions, EMOTIONS, VISEMES};
use catchlight_extension_humanoid::Humanoid;
use serde_json::{json, Value};

fn grid(tag: &str) -> PathBuf {
    common::copy_fixture("two_param_grid", &common::tmp(tag))
}

fn expression(args: &[&str], path: &Path) -> (i32, String, String) {
    let mut all = vec!["expression", args[0], path.to_str().unwrap()];
    all.extend(&args[1..]);
    common::run(&all)
}

fn ok(args: &[&str], path: &Path) -> (Value, String) {
    let (code, stdout, stderr) = expression(args, path);
    assert_eq!(code, 0, "{args:?}: {stderr}");
    (
        serde_json::from_str(&stdout).expect("json on stdout"),
        stderr,
    )
}

fn refused(args: &[&str], path: &Path, want: &str) {
    let before = common::read(path);
    let (code, stdout, stderr) = expression(args, path);
    assert_eq!(code, 2, "{args:?} should be refused");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(
        stderr.contains(want),
        "{args:?}: wanted {want:?} in {stderr}"
    );
    assert_eq!(common::read(path), before, "a refusal writes nothing");
}

fn loaded(path: &Path) -> Model {
    Model::from_clm_bytes(&common::read(path)).unwrap()
}

#[test]
fn names_lists_the_standard_emotions_and_visemes_without_a_file() {
    let (code, stdout, stderr) = common::run(&["expression", "names"]);
    assert_eq!(code, 0, "{stderr}");
    let names: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(names, json!({"emotions": EMOTIONS, "visemes": VISEMES}));
}

#[test]
fn set_keeps_settings_in_the_order_given_and_list_reads_them_back() {
    let path = grid("expression-set");
    let (set, stderr) = ok(
        &[
            "set",
            "happy",
            "--param",
            "param-2=3",
            "--control",
            "mouth_smile=0.8",
            "--param",
            "param-1=-1.5",
        ],
        &path,
    );
    let happy = json!({"name": "happy", "settings": [
        {"param": "param-2", "value": 3.0},
        {"control": "mouth_smile", "value": 0.8},
        {"param": "param-1", "value": -1.5},
    ]});
    assert_eq!(set, json!({"expression": happy, "replaced": null}));
    assert!(
        stderr.contains("warning: catchlight.expressions/unmapped-control"),
        "a control setting with no mapping warns: {stderr}"
    );

    let (replaced, _) = ok(&["set", "happy", "--control", "mouth_smile=1"], &path);
    assert_eq!(replaced["replaced"], happy);
    ok(&["set", "happy", "--control", "mouth_smile=0.8"], &path);
    ok(&["set", "wink", "--control", "eye_left_open=-1"], &path);

    let (listing, _) = ok(&["list"], &path);
    assert_eq!(
        listing["expressions"],
        json!([
            {"name": "happy", "settings": [{"control": "mouth_smile", "value": 0.8}]},
            {"name": "wink", "settings": [{"control": "eye_left_open", "value": -1.0}]},
        ]),
        "replacing keeps the author order"
    );
    assert_eq!(
        listing["warnings"][0]["code"],
        "catchlight.expressions/unmapped-control"
    );

    assert_eq!(
        common::run(&[
            "humanoid",
            "map",
            path.to_str().unwrap(),
            "mouth_smile",
            "param-1"
        ])
        .0,
        0
    );
    let (_, stderr) = ok(&["set", "calm", "--param", "param-1=0"], &path);
    assert!(
        stderr.contains("catchlight.expressions/mapped-param"),
        "a param a control maps warns: {stderr}"
    );
}

#[test]
fn set_refuses_unknown_params_typos_out_of_range_and_non_finite_values() {
    let path = grid("expression-refusals");
    for (args, want) in [
        (
            vec!["set", "happy", "--param", "nope=1"],
            "no param with the id \"nope\"",
        ),
        (
            vec!["set", "happy", "--control", "mouth_smil=1"],
            "did you mean \"mouth_smile\"",
        ),
        (
            vec!["set", "happy", "--control", "mouth_smile=1.5"],
            "a finite value in -1..1",
        ),
        (
            vec!["set", "happy", "--control", "mouth_open=-0.5"],
            "a finite value in 0..1",
        ),
        (
            vec!["set", "happy", "--control", "mouth_smile=nan"],
            "finite value",
        ),
        (
            vec!["set", "happy", "--param", "param-0=inf"],
            "finite value",
        ),
        (vec!["set", "happy", "--param", "param-0"], "name=value"),
        (vec!["set", "happy"], "at least one --control"),
        (vec!["remove", "happy"], "has no expression \"happy\""),
    ] {
        refused(&args, &path, want);
    }
    let (kept, stderr) = ok(
        &["set", "pout", "--control", "lip_pout=0.5", "--nonstandard"],
        &path,
    );
    assert_eq!(kept["expression"]["settings"][0]["control"], "lip_pout");
    assert!(
        stderr.contains("catchlight.expressions/nonstandard-control"),
        "{stderr}"
    );
}

#[test]
fn add_visemes_skips_existing_names_unless_told_to_replace_them() {
    let path = grid("expression-visemes");
    ok(&["set", "aa", "--param", "param-0=1"], &path);
    let (report, _) = ok(&["add-visemes"], &path);
    assert_eq!(report["skipped"], json!(["aa"]));
    assert_eq!(report["replaced"], json!([]));
    assert_eq!(report["added"].as_array().unwrap().len(), VISEMES.len() - 1);
    let expressions = loaded(&path).extension::<Expressions>().unwrap().clone();
    assert_eq!(expressions.len(), VISEMES.len());
    assert_eq!(
        expressions.names().next(),
        Some("aa"),
        "existing expressions stay first"
    );
    assert_eq!(expressions.get("aa").unwrap().settings.len(), 1, "kept");
    let ae = standard_visemes().into_iter().find(|v| v.name == "ae");
    assert_eq!(expressions.get("ae"), ae);

    let (report, _) = ok(&["add-visemes"], &path);
    assert_eq!(report["added"], json!([]));
    assert_eq!(report["skipped"].as_array().unwrap().len(), VISEMES.len());

    let (report, _) = ok(&["add-visemes", "--replace"], &path);
    assert_eq!(report["replaced"].as_array().unwrap().len(), VISEMES.len());
    assert_eq!(report["skipped"], json!([]));
    let expressions = loaded(&path).extension::<Expressions>().unwrap().clone();
    let aa = standard_visemes().into_iter().find(|v| v.name == "aa");
    assert_eq!(expressions.get("aa"), aa);
}

#[test]
fn removing_the_last_expression_drops_the_extension() {
    let path = grid("expression-remove");
    let original = common::read(&path);
    ok(&["set", "happy", "--control", "mouth_smile=1"], &path);
    ok(&["set", "sad", "--control", "mouth_smile=-1"], &path);
    let (removed, _) = ok(&["remove", "happy"], &path);
    assert_eq!(
        removed,
        json!({"name": "happy", "settings": [{"control": "mouth_smile", "value": 1.0}]})
    );
    assert!(loaded(&path).extensions().contains_key(Expressions::KEY));
    ok(&["remove", "sad"], &path);
    assert!(!loaded(&path).extensions().contains_key(Expressions::KEY));
    assert_eq!(common::read(&path), original);
}

#[test]
fn an_edit_keeps_textures_and_every_other_extension_byte_for_byte() {
    let path = grid("expression-bytes");
    let blob = path.with_file_name("thumb.bin");
    std::fs::write(&blob, (0u8..=255).cycle().take(2000).collect::<Vec<_>>()).unwrap();
    let p = path.to_str().unwrap();
    for args in [
        vec![
            "extension",
            "set",
            p,
            "acme.thumb",
            "--bytes",
            blob.to_str().unwrap(),
        ],
        vec!["extension", "set", p, "acme.camera", "--json", "[1,2]"],
        vec!["humanoid", "map", p, "mouth_open", "param-0"],
    ] {
        assert_eq!(common::run(&args).0, 0, "{args:?}");
    }
    let before = common::decode(&path);

    ok(&["add-visemes"], &path);
    let after = common::decode(&path);
    assert_eq!(after.textures, before.textures);
    assert_eq!(after.extensions, before.extensions);
    assert_eq!(after.unknown_sections, before.unknown_sections);
    let mut stripped = after.doc.clone();
    stripped.extensions.remove(Expressions::KEY);
    assert_eq!(stripped, before.doc, "only the expressions key changed");
    assert_eq!(
        after.doc.extensions[Humanoid::KEY],
        before.doc.extensions[Humanoid::KEY]
    );
}
