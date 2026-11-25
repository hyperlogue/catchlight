#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `extension` over a real file, and how `patch`, `diff` and `merge` treat it.

mod common;

use catchlight_cli::{extension, fragment};
use catchlight_core::formats::clm::ClmExtension;

fn payload() -> Vec<u8> {
    (0u8..=255).cycle().take(3000).collect()
}

#[test]
fn both_kinds_round_trip_through_the_binary() {
    let dir = common::tmp("extension-roundtrip");
    let model = common::copy_fixture("welded_seam", &dir);
    let source = dir.join("thumb.bin");
    std::fs::write(&source, payload()).unwrap();

    let (code, stdout, stderr) = common::run(&[
        "extension",
        "set",
        model.to_str().unwrap(),
        "acme.camera",
        "--json",
        r#"{"rig":"v2","count":3}"#,
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("set"), "{stdout}");

    let (code, _, stderr) = common::run(&[
        "extension",
        "set",
        model.to_str().unwrap(),
        "acme.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");

    let (code, stdout, _) = common::run(&["extension", "list", model.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec!["acme.camera\tjson", "acme.thumb\tbytes\t3000"]
    );

    let (code, stdout, _) =
        common::run(&["extension", "get", model.to_str().unwrap(), "acme.camera"]);
    assert_eq!(code, 0);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(json, serde_json::json!({"rig": "v2", "count": 3}));

    let out = dir.join("back.bin");
    let (code, _, stderr) = common::run(&[
        "extension",
        "get",
        model.to_str().unwrap(),
        "acme.thumb",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(common::read(&out), payload());

    let loaded = catchlight_core::Model::from_clm_bytes(&common::read(&model)).expect("loads");
    assert_eq!(loaded.extensions().len(), 2);

    let (code, _, _) = common::run(&["extension", "delete", model.to_str().unwrap(), "acme.thumb"]);
    assert_eq!(code, 0);
    let (code, stdout, _) = common::run(&["extension", "list", model.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(stdout.lines().count(), 1);
    let (code, _, stderr) =
        common::run(&["extension", "delete", model.to_str().unwrap(), "acme.thumb"]);
    assert_eq!(code, 2, "deleting twice is an error");
    assert!(stderr.contains("acme.thumb"), "{stderr}");
}

#[test]
fn a_byte_value_will_not_be_printed() {
    let dir = common::tmp("extension-no-out");
    let model = common::copy_fixture("welded_seam", &dir);
    let source = dir.join("thumb.bin");
    std::fs::write(&source, payload()).unwrap();
    common::run(&[
        "extension",
        "set",
        model.to_str().unwrap(),
        "acme.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);

    let (code, stdout, stderr) =
        common::run(&["extension", "get", model.to_str().unwrap(), "acme.thumb"]);
    assert_eq!(code, 2, "an error exits 2");
    assert!(stdout.is_empty(), "nothing was printed: {stdout:?}");
    assert!(stderr.contains("--out"), "{stderr}");
}

#[test]
fn a_key_that_is_not_a_vendors_is_refused() {
    let dir = common::tmp("extension-bad-keys");
    let model = common::copy_fixture("welded_seam", &dir);
    for (key, expected) in [
        ("camera", "dot"),
        ("catchlight.thing", "catchlight."),
        ("acme camera", "camera"),
    ] {
        let (code, _, stderr) = common::run(&[
            "extension",
            "set",
            model.to_str().unwrap(),
            key,
            "--json",
            "1",
        ]);
        assert_eq!(code, 2, "{key} should be refused");
        assert!(stderr.contains(expected), "{key}: {stderr}");
    }
}

#[test]
fn a_patch_leaves_extensions_byte_identical() {
    let dir = common::tmp("extension-patch");
    let model = common::copy_fixture("welded_seam", &dir);
    let source = dir.join("thumb.bin");
    std::fs::write(&source, payload()).unwrap();
    common::run(&[
        "extension",
        "set",
        model.to_str().unwrap(),
        "acme.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);
    common::run(&[
        "extension",
        "set",
        model.to_str().unwrap(),
        "acme.camera",
        "--json",
        r#"{"rig":"v2"}"#,
    ]);
    let before = common::decode(&model);

    let (code, _, stderr) =
        common::run(&["patch", model.to_str().unwrap(), "node-1", "z_order", "3.5"]);
    assert_eq!(code, 0, "stderr: {stderr}");

    let after = common::decode(&model);
    assert_eq!(after.doc.extensions, before.doc.extensions);
    assert_eq!(after.extensions, before.extensions);
}

#[test]
fn diff_renders_a_changed_key_as_one_line() {
    let dir = common::tmp("extension-diff");
    let base = common::copy_fixture("welded_seam", &dir);
    let changed = dir.join("changed.clm");
    std::fs::copy(&base, &changed).unwrap();
    common::run(&[
        "extension",
        "set",
        changed.to_str().unwrap(),
        "acme.camera",
        "--json",
        r#"{"rig":"v2"}"#,
    ]);

    let (code, stdout, _) =
        common::run(&["diff", base.to_str().unwrap(), changed.to_str().unwrap()]);
    assert_eq!(code, 1, "the files differ");
    let lines: Vec<&str> = stdout.lines().filter(|l| l.contains("extension")).collect();
    assert_eq!(lines, vec![r#"+ extension acme.camera: {"rig":"v2"}"#]);

    common::run(&[
        "extension",
        "set",
        base.to_str().unwrap(),
        "acme.camera",
        "--json",
        r#"{"rig":"v1"}"#,
    ]);
    let (_, stdout, _) = common::run(&["diff", base.to_str().unwrap(), changed.to_str().unwrap()]);
    assert_eq!(
        stdout
            .lines()
            .filter(|l| l.contains("extension"))
            .collect::<Vec<_>>(),
        vec![r#"~ extension acme.camera: {"rig":"v1"} -> {"rig":"v2"}"#]
    );

    let source = dir.join("thumb.bin");
    std::fs::write(&source, payload()).unwrap();
    common::run(&[
        "extension",
        "set",
        changed.to_str().unwrap(),
        "acme.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);
    let (_, stdout, _) = common::run(&["diff", base.to_str().unwrap(), changed.to_str().unwrap()]);
    let line = stdout
        .lines()
        .find(|l| l.contains("acme.thumb"))
        .expect("a line for the byte value");
    assert!(line.contains("3000 bytes"), "{line}");
    assert!(line.contains("blake3"), "{line}");
}

/// `extract` drops them and `merge` refuses an addon that carries one.
#[test]
fn an_addon_carries_no_extensions() {
    let dir = common::tmp("extension-addon");
    let base = common::copy_fixture("welded_seam", &dir);
    let addon = dir.join("addon.clm");
    common::run(&[
        "extension",
        "set",
        base.to_str().unwrap(),
        "acme.camera",
        "--json",
        r#"{"rig":"v2"}"#,
    ]);

    let (code, _, stderr) = common::run(&[
        "extract",
        base.to_str().unwrap(),
        "node-1",
        "--out",
        addon.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let cut = common::decode(&addon);
    assert!(cut.doc.extensions.is_empty(), "extract carries none");

    // Give the fragment one by hand, and merging it is refused by key.
    let mut carrier = cut;
    carrier.doc.extensions.insert(
        catchlight_core::id::ExtensionKey::new("acme.camera").unwrap(),
        ClmExtension::Json(serde_json::json!(1)),
    );
    let carrier_path = common::write_clm(&dir, "carrier", &carrier);
    let err = fragment::merge(&base, &carrier_path, &dir.join("merged.clm")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("acme.camera"), "{message}");
    assert!(message.contains("extension"), "{message}");
}

#[test]
fn render_shows_json_whole_and_bytes_as_their_marker() {
    let json = ClmExtension::Json(serde_json::json!({"a": [1, 2]}));
    assert_eq!(extension::render(&json), r#"{"a":[1,2]}"#);
}

#[test]
fn a_maintained_extension_lists_and_prints_its_envelope() {
    use catchlight_core::extension::{IdRef, Payload, RawExtension};
    use catchlight_core::id::{ExtensionKey, ParamId};
    use catchlight_core::{ExtensionValue, Model};

    let dir = common::tmp("extension-maintained");
    let path = common::copy_fixture("welded_seam", &dir);
    let mut model = Model::from_clm_bytes(&std::fs::read(&path).unwrap()).unwrap();
    model
        .set_extension(
            ExtensionKey::new("acme.pins").unwrap(),
            ExtensionValue::Maintained(RawExtension {
                version: 3,
                refs: vec![IdRef::Param(ParamId::new("param-a").unwrap())],
                payload: Payload::Json(serde_json::json!({"pins": [0]})),
            }),
        )
        .unwrap();
    std::fs::write(&path, model.to_clm_bytes().unwrap()).unwrap();

    let (code, stdout, _) = common::run(&["extension", "list", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "acme.pins\tmaintained");

    let (code, stdout, stderr) =
        common::run(&["extension", "get", path.to_str().unwrap(), "acme.pins"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(
        json,
        serde_json::json!({
            "version": 3,
            "refs": [{"param": "param-a"}],
            "payload": {"pins": [0]},
        })
    );
}

#[test]
fn get_prints_the_animations_byte_payload_as_its_decoded_clips() {
    use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane, LaneTarget};

    let dir = common::tmp("extension-animations");
    let path = common::copy_fixture("welded_seam", &dir);
    let mut model = catchlight_core::Model::from_clm_bytes(&common::read(&path)).unwrap();
    let param = model.param_ids()[0].clone();
    let clip = Clip {
        length: 4,
        lanes: vec![
            Lane {
                target: LaneTarget::Param(param.clone()),
                interpolation: catchlight_core::InterpolateMode::Linear,
                keyframes: vec![Keyframe {
                    frame: 2,
                    value: 0.5,
                }],
            },
            Lane {
                target: LaneTarget::Viseme("aa".into()),
                interpolation: catchlight_core::InterpolateMode::Linear,
                keyframes: vec![Keyframe {
                    frame: 1,
                    value: 1.0,
                }],
            },
        ],
        ..Clip::new("nod")
    };
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([clip.clone()]);
    std::fs::write(&path, model.to_clm_bytes().unwrap()).unwrap();

    let (code, stdout, stderr) = common::run(&[
        "extension",
        "get",
        path.to_str().unwrap(),
        "catchlight.animations",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let clips: Vec<Clip> = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(clips, [clip]);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json[0]["lanes"][0]["param"], param.as_str(), "{stdout}");
    assert_eq!(json[0]["lanes"][1]["viseme"], "aa", "{stdout}");
}

#[test]
fn get_prints_the_humanoid_mappings_in_their_envelope() {
    use catchlight_core::extension::Extension;
    use catchlight_extension_humanoid::{ControlMapping, Humanoid};

    let dir = common::tmp("extension-humanoid");
    let path = common::copy_fixture("welded_seam", &dir);
    let mut model = catchlight_core::Model::from_clm_bytes(&common::read(&path)).unwrap();
    let param = model.param_ids()[0].clone();
    model.extension_or_default::<Humanoid>().unwrap().set(
        "mouth_open",
        ControlMapping {
            param: param.clone(),
            stops: [0.0, 0.25, 1.0],
        },
    );
    std::fs::write(&path, model.to_clm_bytes().unwrap()).unwrap();

    let (code, stdout, stderr) =
        common::run(&["extension", "get", path.to_str().unwrap(), Humanoid::KEY]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(
        json,
        serde_json::json!({
            "version": 1,
            "refs": [{"param": param.as_str()}],
            "payload": {"controls": {"mouth_open": {"param": 0, "stops": [0.0, 0.25, 1.0]}}},
        })
    );
}

#[test]
fn a_maintained_json_value_takes_a_version_and_references_and_round_trips() {
    let dir = common::tmp("extension-maintained-json");
    let path = common::copy_fixture("welded_seam", &dir);
    let model = catchlight_core::Model::from_clm_bytes(&common::read(&path)).unwrap();
    let param = model.param_ids()[0].to_string();
    let node = model.root().unwrap().to_string();
    let refs = serde_json::json!([{"param": param}, {"node": node}]);

    let (code, stdout, stderr) = common::run(&[
        "extension",
        "set",
        path.to_str().unwrap(),
        "acme.framing",
        "--version",
        "3",
        "--refs",
        &refs.to_string(),
        "--json",
        r#"{"subject": 1}"#,
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("set"), "{stdout}");
    assert!(stderr.is_empty(), "every reference resolves: {stderr}");

    let (code, stdout, _) = common::run(&["extension", "list", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "acme.framing\tmaintained");

    let (code, stdout, stderr) =
        common::run(&["extension", "get", path.to_str().unwrap(), "acme.framing"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(
        json,
        serde_json::json!({"version": 3, "refs": refs, "payload": {"subject": 1}})
    );
    let loaded = catchlight_core::Model::from_clm_bytes(&common::read(&path)).unwrap();
    assert_eq!(loaded.extensions().refs("acme.framing").unwrap().len(), 2);
}

#[test]
fn a_maintained_byte_payload_goes_to_out_and_its_envelope_is_printed() {
    let dir = common::tmp("extension-maintained-bytes");
    let path = common::copy_fixture("welded_seam", &dir);
    let source = dir.join("payload.bin");
    std::fs::write(&source, payload()).unwrap();

    let (code, _, stderr) = common::run(&[
        "extension",
        "set",
        path.to_str().unwrap(),
        "acme.cache",
        "--version",
        "1",
        "--bytes",
        source.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let (code, stdout, _) = common::run(&["extension", "list", path.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "acme.cache\tmaintained\t3000");

    let (code, _, stderr) =
        common::run(&["extension", "get", path.to_str().unwrap(), "acme.cache"]);
    assert_eq!(code, 2, "bytes need --out");
    assert!(stderr.contains("--out"), "{stderr}");

    let out = dir.join("back.bin");
    let (code, stdout, stderr) = common::run(&[
        "extension",
        "get",
        path.to_str().unwrap(),
        "acme.cache",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(common::read(&out), payload());
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(json["version"], 1);
    assert_eq!(json["refs"], serde_json::json!([]));
    assert_eq!(json["size"], 3000);
    assert_eq!(json["hash"].as_str().unwrap().len(), 64);
}

#[test]
fn an_unresolved_reference_is_kept_and_warned_about() {
    let dir = common::tmp("extension-maintained-dangling");
    let path = common::copy_fixture("welded_seam", &dir);
    let refs = r#"[{"param": "ghost"}, {"slot": ["nowhere", "slot-1"]}]"#;

    let (code, _, stderr) = common::run(&[
        "extension",
        "set",
        path.to_str().unwrap(),
        "acme.framing",
        "--version",
        "1",
        "--refs",
        refs,
        "--json",
        "{}",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stderr.contains("catchlight/dangling-reference") && stderr.contains("ghost"),
        "{stderr}"
    );
    assert!(stderr.contains("nowhere"), "{stderr}");
    let loaded = catchlight_core::Model::from_clm_bytes(&common::read(&path)).unwrap();
    assert_eq!(loaded.extensions().refs("acme.framing").unwrap().len(), 2);
}

#[test]
fn a_maintained_value_is_refused_a_reserved_key_and_malformed_references() {
    let dir = common::tmp("extension-maintained-refused");
    let path = common::copy_fixture("welded_seam", &dir);
    let before = common::read(&path);
    for (key, refs, want) in [
        ("catchlight.humanoid", "[]", "catchlight."),
        ("acme.framing", r#"[{"bone": "x"}]"#, "--refs"),
        ("acme.framing", r#"{"param": "x"}"#, "--refs"),
    ] {
        let (code, _, stderr) = common::run(&[
            "extension",
            "set",
            path.to_str().unwrap(),
            key,
            "--version",
            "1",
            "--refs",
            refs,
            "--json",
            "{}",
        ]);
        assert_eq!(code, 2, "{key} {refs} should be refused");
        assert!(stderr.contains(want), "{key} {refs}: {stderr}");
    }
    let (code, _, stderr) = common::run(&[
        "extension",
        "set",
        path.to_str().unwrap(),
        "acme.framing",
        "--refs",
        "[]",
        "--json",
        "{}",
    ]);
    assert_eq!(code, 2, "--refs needs --version");
    assert!(stderr.contains("--version"), "{stderr}");
    assert_eq!(common::read(&path), before, "a refusal writes nothing");
}
