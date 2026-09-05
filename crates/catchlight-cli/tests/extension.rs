#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `extension` over a real file: both kinds round-trip, and the operations
//! beside it — `patch`, `diff`, `merge` — behave the way an extension needs
//! them to.

mod common;

use catchlight_cli::{extension, fragment};
use catchlight_core::formats::clm::ClmExtension;

/// Not text and not an image: an extension's bytes are opaque.
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
        "molan.caster",
        "--json",
        r#"{"rig":"v2","count":3}"#,
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("set"), "{stdout}");

    let (code, _, stderr) = common::run(&[
        "extension",
        "set",
        model.to_str().unwrap(),
        "molan.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");

    // list: key, kind, and a size for the byte value only.
    let (code, stdout, _) = common::run(&["extension", "list", model.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        vec!["molan.caster\tjson", "molan.thumb\tbytes\t3000"]
    );

    // get: JSON to stdout, bytes to a file, byte-for-byte.
    let (code, stdout, _) =
        common::run(&["extension", "get", model.to_str().unwrap(), "molan.caster"]);
    assert_eq!(code, 0);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("json on stdout");
    assert_eq!(json, serde_json::json!({"rig": "v2", "count": 3}));

    let out = dir.join("back.bin");
    let (code, _, stderr) = common::run(&[
        "extension",
        "get",
        model.to_str().unwrap(),
        "molan.thumb",
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(common::read(&out), payload());

    // The file still loads as a model, which is the real test of the pairing.
    let loaded = catchlight_core::Model::from_clm_bytes(&common::read(&model)).expect("loads");
    assert_eq!(loaded.extensions().len(), 2);

    // delete takes one away and errors on a key that is not there.
    let (code, _, _) = common::run(&[
        "extension",
        "delete",
        model.to_str().unwrap(),
        "molan.thumb",
    ]);
    assert_eq!(code, 0);
    let (code, stdout, _) = common::run(&["extension", "list", model.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(stdout.lines().count(), 1);
    let (code, _, stderr) = common::run(&[
        "extension",
        "delete",
        model.to_str().unwrap(),
        "molan.thumb",
    ]);
    assert_eq!(code, 2, "deleting twice is an error");
    assert!(stderr.contains("molan.thumb"), "{stderr}");
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
        "molan.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);

    let (code, stdout, stderr) =
        common::run(&["extension", "get", model.to_str().unwrap(), "molan.thumb"]);
    assert_eq!(code, 2, "an error exits 2");
    assert!(stdout.is_empty(), "nothing was printed: {stdout:?}");
    assert!(stderr.contains("--out"), "{stderr}");
}

#[test]
fn a_key_that_is_not_a_vendors_is_refused() {
    let dir = common::tmp("extension-bad-keys");
    let model = common::copy_fixture("welded_seam", &dir);
    for (key, expected) in [
        ("caster", "dot"),
        ("catchlight.thing", "catchlight."),
        ("molan caster", "caster"),
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

/// A patch of an unrelated field carries extensions through untouched, which
/// is what "the file ops get them for free" has to mean.
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
        "molan.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);
    common::run(&[
        "extension",
        "set",
        model.to_str().unwrap(),
        "molan.caster",
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

/// `diff` renders a changed key as one line: JSON whole, bytes as what the
/// marker carries.
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
        "molan.caster",
        "--json",
        r#"{"rig":"v2"}"#,
    ]);

    let (code, stdout, _) =
        common::run(&["diff", base.to_str().unwrap(), changed.to_str().unwrap()]);
    assert_eq!(code, 1, "the files differ");
    let lines: Vec<&str> = stdout.lines().filter(|l| l.contains("extension")).collect();
    assert_eq!(lines, vec![r#"+ extension molan.caster: {"rig":"v2"}"#]);

    // Change it, and the one line names both sides.
    common::run(&[
        "extension",
        "set",
        base.to_str().unwrap(),
        "molan.caster",
        "--json",
        r#"{"rig":"v1"}"#,
    ]);
    let (_, stdout, _) = common::run(&["diff", base.to_str().unwrap(), changed.to_str().unwrap()]);
    assert_eq!(
        stdout
            .lines()
            .filter(|l| l.contains("extension"))
            .collect::<Vec<_>>(),
        vec![r#"~ extension molan.caster: {"rig":"v1"} -> {"rig":"v2"}"#]
    );

    // A byte value shows its size and its hash, never its bytes.
    let source = dir.join("thumb.bin");
    std::fs::write(&source, payload()).unwrap();
    common::run(&[
        "extension",
        "set",
        changed.to_str().unwrap(),
        "molan.thumb",
        "--bytes",
        source.to_str().unwrap(),
    ]);
    let (_, stdout, _) = common::run(&["diff", base.to_str().unwrap(), changed.to_str().unwrap()]);
    let line = stdout
        .lines()
        .find(|l| l.contains("molan.thumb"))
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
        "molan.caster",
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
        catchlight_core::id::ExtensionKey::new("molan.caster").unwrap(),
        ClmExtension::Json(serde_json::json!(1)),
    );
    let carrier_path = common::write_clm(&dir, "carrier", &carrier);
    let err = fragment::merge(&base, &carrier_path, &dir.join("merged.clm")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("molan.caster"), "{message}");
    assert!(message.contains("extension"), "{message}");
}

/// The renderer `diff` uses, on the two shapes.
#[test]
fn render_shows_json_whole_and_bytes_as_their_marker() {
    let json = ClmExtension::Json(serde_json::json!({"a": [1, 2]}));
    assert_eq!(extension::render(&json), r#"{"a":[1,2]}"#);
}
