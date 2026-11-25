#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `humanoid` over copies of the committed fixtures. `two_param_grid` has
//! `param-0` in 0..1 resting at 0 and `param-1` in -2..2 resting at -2.

mod common;

use std::path::{Path, PathBuf};

use catchlight_core::extension::Extension;
use catchlight_core::Model;
use catchlight_extension_humanoid::{Humanoid, CONTROLS};
use serde_json::{json, Value};

fn grid(tag: &str) -> PathBuf {
    common::copy_fixture("two_param_grid", &common::tmp(tag))
}

fn humanoid(args: &[&str], path: &Path) -> (i32, String, String) {
    let mut all = vec!["humanoid", args[0], path.to_str().unwrap()];
    all.extend(&args[1..]);
    common::run(&all)
}

fn ok(args: &[&str], path: &Path) -> (Value, String) {
    let (code, stdout, stderr) = humanoid(args, path);
    assert_eq!(code, 0, "{args:?}: {stderr}");
    (
        serde_json::from_str(&stdout).expect("json on stdout"),
        stderr,
    )
}

fn refused(args: &[&str], path: &Path, want: &str) {
    let before = common::read(path);
    let (code, stdout, stderr) = humanoid(args, path);
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
fn controls_lists_the_standard_controls_without_a_file() {
    let (code, stdout, stderr) = common::run(&["humanoid", "controls"]);
    assert_eq!(code, 0, "{stderr}");
    let listed: Vec<Value> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(listed.len(), CONTROLS.len());
    assert_eq!(listed[0], json!({"name": "head_yaw", "one_sided": false}));
    assert!(listed.contains(&json!({"name": "mouth_open", "one_sided": true})));
}

#[test]
fn default_stops_span_the_param_by_the_controls_sidedness() {
    let path = grid("humanoid-default-stops");
    let (two, stderr) = ok(&["map", "head_yaw", "param-1"], &path);
    assert_eq!(
        two,
        json!({"control": "head_yaw", "param": "param-1", "stops": [-2.0, -2.0, 2.0], "replaced": null})
    );
    assert!(stderr.is_empty(), "{stderr}");
    let (one, _) = ok(&["map", "mouth_open", "param-0"], &path);
    assert_eq!(one["stops"], json!([0.0, 0.0, 1.0]));

    let model = loaded(&path);
    let h = model.extension::<Humanoid>().unwrap();
    assert_eq!(h.mapping("head_yaw").unwrap().stops, [-2.0, -2.0, 2.0]);
    assert_eq!(h.mapping("mouth_open").unwrap().param.as_str(), "param-0");
}

#[test]
fn map_replaces_and_list_reports_mappings_unmapped_and_warnings() {
    let path = grid("humanoid-list");
    ok(&["map", "mouth_open", "param-0"], &path);
    let (replaced, _) = ok(
        &["map", "mouth_open", "param-0", "--stops", "-0.5,0,0.75"],
        &path,
    );
    assert_eq!(replaced["stops"], json!([-0.5, 0.0, 0.75]));
    assert_eq!(replaced["replaced"]["stops"], json!([0.0, 0.0, 1.0]));

    let (shared, stderr) = ok(&["map", "mouth_smile", "param-0"], &path);
    assert_eq!(shared["replaced"], Value::Null);
    assert!(
        stderr.contains("warning: catchlight.humanoid/shared-param"),
        "{stderr}"
    );

    let (listing, _) = ok(&["list"], &path);
    assert_eq!(
        listing["mappings"],
        json!([
            {"control": "mouth_open", "param": "param-0", "stops": [-0.5, 0.0, 0.75]},
            {"control": "mouth_smile", "param": "param-0", "stops": [0.0, 0.0, 1.0]},
        ])
    );
    let unmapped = listing["unmapped"].as_array().unwrap();
    assert_eq!(unmapped.len(), CONTROLS.len() - 2);
    assert!(!unmapped.contains(&json!("mouth_open")));
    assert_eq!(
        listing["warnings"][0]["code"],
        "catchlight.humanoid/shared-param"
    );
    assert!(listing["warnings"][0]["message"]
        .as_str()
        .unwrap()
        .contains("param-0"));
}

#[test]
fn map_refuses_unknown_params_typos_and_bad_stops() {
    let path = grid("humanoid-refusals");
    refused(
        &["map", "mouth_open", "no-such-param"],
        &path,
        "no param with the id \"no-such-param\"",
    );
    refused(
        &["map", "mouth_opn", "param-0"],
        &path,
        "did you mean \"mouth_open\"",
    );
    refused(&["map", "jaw", "param-0"], &path, "--nonstandard");
    for stops in ["0,1", "0,1,2,3", "0,x,1", "0,inf,1", "0,NaN,1"] {
        refused(
            &["map", "mouth_open", "param-0", "--stops", stops],
            &path,
            "three finite numbers",
        );
    }
    refused(
        &["unmap", "mouth_open"],
        &path,
        "maps no control \"mouth_open\"",
    );

    let (kept, stderr) = ok(&["map", "jaw", "param-0", "--nonstandard"], &path);
    assert_eq!(
        kept["stops"],
        json!([0.0, 0.0, 1.0]),
        "two-sided by default"
    );
    assert!(
        stderr.contains("catchlight.humanoid/nonstandard-control"),
        "{stderr}"
    );
}

#[test]
fn unmapping_the_last_mapping_drops_the_extension() {
    let path = grid("humanoid-unmap");
    let original = common::read(&path);
    ok(&["map", "head_yaw", "param-1"], &path);
    ok(&["map", "mouth_open", "param-0"], &path);

    let (removed, _) = ok(&["unmap", "head_yaw"], &path);
    assert_eq!(
        removed,
        json!({"control": "head_yaw", "param": "param-1", "stops": [-2.0, -2.0, 2.0]})
    );
    assert!(loaded(&path).extensions().contains_key(Humanoid::KEY));
    ok(&["unmap", "mouth_open"], &path);
    assert!(!loaded(&path).extensions().contains_key(Humanoid::KEY));
    assert_eq!(
        common::read(&path),
        original,
        "mapping and unmapping again restores the file byte for byte"
    );
}

#[test]
fn an_edit_keeps_textures_and_every_other_extension_byte_for_byte() {
    let path = grid("humanoid-bytes");
    let blob = path.with_file_name("thumb.bin");
    std::fs::write(&blob, (0u8..=255).cycle().take(2000).collect::<Vec<_>>()).unwrap();
    for args in [
        vec!["--json", r#"{"rig":"v2"}"#],
        vec!["--bytes", blob.to_str().unwrap()],
    ] {
        let key = if args[0] == "--json" {
            "acme.camera"
        } else {
            "acme.thumb"
        };
        let mut all = vec!["extension", "set", path.to_str().unwrap(), key];
        all.extend(args);
        assert_eq!(common::run(&all).0, 0);
    }
    let before = common::decode(&path);

    ok(&["map", "mouth_open", "param-0"], &path);
    let after = common::decode(&path);
    assert_eq!(after.textures, before.textures);
    assert_eq!(after.extensions, before.extensions);
    assert_eq!(after.unknown_sections, before.unknown_sections);
    for key in ["acme.camera", "acme.thumb"] {
        assert_eq!(after.doc.extensions[key], before.doc.extensions[key]);
    }
    let mut stripped = after.doc.clone();
    stripped.extensions.remove(Humanoid::KEY);
    assert_eq!(stripped, before.doc, "only the humanoid key changed");
}

#[test]
fn unreadable_humanoid_data_is_refused_rather_than_overwritten() {
    use catchlight_core::extension::{Payload, RawExtension};
    use catchlight_core::formats::clm::{ClmExtension, ClmPayload};
    use catchlight_core::id::ExtensionKey;

    let path = grid("humanoid-newer");
    let mut file = common::decode(&path);
    let newer = RawExtension {
        version: Humanoid::VERSION + 1,
        refs: Vec::new(),
        payload: Payload::Json(json!({"future": true})),
    };
    let Payload::Json(payload) = newer.payload else {
        unreachable!()
    };
    file.doc.extensions.insert(
        ExtensionKey::new(Humanoid::KEY).unwrap(),
        ClmExtension::Maintained {
            version: newer.version,
            refs: newer.refs,
            payload: ClmPayload::Json(payload),
        },
    );
    let path = common::write_clm(path.parent().unwrap(), "newer", &file);
    refused(
        &["map", "mouth_open", "param-0"],
        &path,
        "cannot be read by this build",
    );
}
