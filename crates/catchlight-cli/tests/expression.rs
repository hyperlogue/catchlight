#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test aborts on a broken fixture"
)]

//! `expression` and `viseme` over copies of the committed fixtures. `two_param_grid` has
//! `param-0` in 0..1, `param-1` in -2..2 and `param-2` in 0..10.

mod common;

use std::path::{Path, PathBuf};

use catchlight_core::Model;
use catchlight_core::extension::Extension;
use catchlight_extension_expressions::{EMOTIONS, Expressions, Group, VISEMES, standard_visemes};
use catchlight_extension_humanoid::Humanoid;
use serde_json::{Value, json};

fn grid(tag: &str) -> PathBuf {
    common::copy_fixture("two_param_grid", &common::tmp(tag))
}

fn in_group(command: &str, args: &[&str], path: &Path) -> (i32, String, String) {
    let mut all = vec![command, args[0], path.to_str().unwrap()];
    all.extend(&args[1..]);
    common::run(&all)
}

fn ok(args: &[&str], path: &Path) -> (Value, String) {
    ok_in("expression", args, path)
}

fn viseme(args: &[&str], path: &Path) -> (Value, String) {
    ok_in("viseme", args, path)
}

fn ok_in(command: &str, args: &[&str], path: &Path) -> (Value, String) {
    let (code, stdout, stderr) = in_group(command, args, path);
    assert_eq!(code, 0, "{command} {args:?}: {stderr}");
    (
        serde_json::from_str(&stdout).expect("json on stdout"),
        stderr,
    )
}

fn refused(args: &[&str], path: &Path, want: &str) {
    refused_in("expression", args, path, want);
}

fn refused_in(command: &str, args: &[&str], path: &Path, want: &str) {
    let before = common::read(path);
    let (code, stdout, stderr) = in_group(command, args, path);
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
fn names_lists_each_groups_standard_names_without_a_file() {
    for (command, want) in [("expression", &EMOTIONS[..]), ("viseme", &VISEMES[..])] {
        let (code, stdout, stderr) = common::run(&[command, "names"]);
        assert_eq!(code, 0, "{stderr}");
        let names: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(names, json!(want), "{command}");
    }
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
fn add_standard_skips_existing_visemes_unless_told_to_replace_them() {
    let path = grid("viseme-standard");
    // An expression of a viseme's name is no clash.
    ok(&["set", "ae", "--param", "param-0=1"], &path);
    viseme(&["set", "aa", "--param", "param-0=1"], &path);
    let (report, _) = viseme(&["add-standard"], &path);
    assert_eq!(report["skipped"], json!(["aa"]));
    assert_eq!(report["replaced"], json!([]));
    assert_eq!(report["added"].as_array().unwrap().len(), VISEMES.len() - 1);
    let expressions = loaded(&path).extension::<Expressions>().unwrap().clone();
    assert_eq!(expressions.len(Group::Viseme), VISEMES.len());
    assert_eq!(
        expressions.names(Group::Expression).collect::<Vec<_>>(),
        ["ae"]
    );
    assert_eq!(
        expressions.names(Group::Viseme).next(),
        Some("aa"),
        "existing visemes stay first"
    );
    assert_eq!(
        expressions.get(Group::Viseme, "aa").unwrap().settings.len(),
        1,
        "kept"
    );
    let ae = standard_visemes().into_iter().find(|v| v.name == "ae");
    assert_eq!(expressions.get(Group::Viseme, "ae"), ae);
    assert_ne!(expressions.get(Group::Expression, "ae"), ae);

    let (report, _) = viseme(&["add-standard"], &path);
    assert_eq!(report["added"], json!([]));
    assert_eq!(report["skipped"].as_array().unwrap().len(), VISEMES.len());

    let (report, _) = viseme(&["add-standard", "--replace"], &path);
    assert_eq!(report["replaced"].as_array().unwrap().len(), VISEMES.len());
    assert_eq!(report["skipped"], json!([]));
    let expressions = loaded(&path).extension::<Expressions>().unwrap().clone();
    let aa = standard_visemes().into_iter().find(|v| v.name == "aa");
    assert_eq!(expressions.get(Group::Viseme, "aa"), aa);
}

#[test]
fn viseme_commands_edit_only_the_visemes_and_list_only_their_warnings() {
    let path = grid("viseme-set");
    let p = path.to_str().unwrap();
    assert_eq!(
        common::run(&["humanoid", "map", p, "mouth_smile", "param-1"]).0,
        0
    );
    ok(&["set", "oh", "--control", "mouth_open=0.5"], &path);
    let (set, stderr) = viseme(
        &[
            "set",
            "oh",
            "--param",
            "param-2=3",
            "--control",
            "mouth_funnel=1",
        ],
        &path,
    );
    let oh = json!({"name": "oh", "settings": [
        {"param": "param-2", "value": 3.0},
        {"control": "mouth_funnel", "value": 1.0},
    ]});
    assert_eq!(set, json!({"viseme": oh, "replaced": null}));
    assert!(stderr.contains("viseme \"oh\""), "{stderr}");
    assert!(!stderr.contains("expression \"oh\""), "{stderr}");

    let (listing, _) = viseme(&["list"], &path);
    assert_eq!(listing["visemes"], json!([oh]));
    assert!(listing.get("expressions").is_none());
    let messages: Vec<&str> = listing["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["message"].as_str().unwrap())
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].starts_with("viseme \"oh\""), "{messages:?}");
    let (listing, _) = ok(&["list"], &path);
    assert_eq!(listing["expressions"][0]["name"], "oh");
    assert!(listing.get("visemes").is_none());
    assert!(
        listing["warnings"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("expression \"oh\"")
    );

    let (removed, _) = viseme(&["remove", "oh"], &path);
    assert_eq!(removed, oh);
    let expressions = loaded(&path).extension::<Expressions>().unwrap().clone();
    assert!(expressions.contains(Group::Expression, "oh"));
    assert_eq!(expressions.len(Group::Viseme), 0);
}

#[test]
fn viseme_commands_refuse_what_expression_commands_refuse() {
    let path = grid("viseme-refusals");
    for (args, want) in [
        (
            vec!["set", "aa", "--param", "nope=1"],
            "no param with the id \"nope\"",
        ),
        (
            vec!["set", "aa", "--control", "mouth_open=-0.5"],
            "a finite value in 0..1",
        ),
        (vec!["set", "aa"], "at least one --control"),
        (
            vec!["remove", "aa"],
            "has no viseme \"aa\"; `catchlight-cli viseme list`",
        ),
    ] {
        refused_in("viseme", &args, &path, want);
    }
    ok(&["set", "aa", "--control", "mouth_open=1"], &path);
    refused_in("viseme", &["remove", "aa"], &path, "has no viseme \"aa\"");
    viseme(&["set", "aa", "--control", "mouth_open=1"], &path);
    refused(&["remove", "ee"], &path, "has no expression \"ee\"");
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

    viseme(&["add-standard"], &path);
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
