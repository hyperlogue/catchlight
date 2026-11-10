#![cfg(feature = "cli")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "catchlight-importer-cli-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_catchlight-importer"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn succeeded(result: &Output) {
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn inx_binary_writes_a_loadable_model_and_preserves_existing_outputs() {
    let dir = Workspace::new();
    let json = br#"{"nodes":{"name":"Root","type":"Node","children":[]},"param":[]}"#;
    let mut bytes = b"TRNSRTS\0".to_vec();
    bytes.extend((json.len() as u32).to_be_bytes());
    bytes.extend(json);
    bytes.extend(b"TEX_SECT");
    bytes.extend(0u32.to_be_bytes());
    std::fs::write(dir.0.join("input.inx"), bytes).unwrap();
    succeeded(&dir.run(&["input.inx", "-o", "input.clm"]));
    let saved = std::fs::read(dir.0.join("input.clm")).unwrap();
    let model = catchlight_core::Model::from_clm_bytes(&saved).unwrap();
    assert_eq!(model.node_count(), 1);
    assert!(!dir.run(&["input.inx"]).status.success());
    assert_eq!(std::fs::read(dir.0.join("input.clm")).unwrap(), saved);
    assert!(!dir.run(&["input.inx", "--invalid"]).status.success());
}

#[test]
fn livliv_binary_inspects_and_exports_the_final_baking_report() {
    let dir = Workspace::new();
    std::fs::create_dir_all(dir.0.join("source/textures")).unwrap();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 255, 255, 255]))
        .save(dir.0.join("source/textures/a.png"))
        .unwrap();
    let manifest = json!({"format":"livliv-model","version":4,"name":"Synthetic","canvas":[32,32],"meta":{"generator":{"name":"test"},"license":{"id":"CC0-1.0"},"watermark":{"kind":"none"}},
        "anchors":{"head":{"cx":16,"cy":12,"rx":8,"ry":8},"face":[8,4,24,20],"neck_y":22,"top_y":0,"bottom_y":32,"hair_root_y":4},
        "layers":[{"id":"face","group":"head","texture":"textures/a.png","x":12,"y":8,"w":4,"h":4}]});
    std::fs::write(
        dir.0.join("source/model.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    succeeded(&dir.run(&["source", "--inspect"]));
    succeeded(&dir.run(&["source/model.json", "--inspect"]));
    // ZIP magic selects LivLiv even with a misleading INX suffix.
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for path in ["model.json", "textures/a.png"] {
        zip.start_file(
            path,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        std::io::Write::write_all(
            &mut zip,
            &std::fs::read(dir.0.join("source").join(path)).unwrap(),
        )
        .unwrap();
    }
    std::fs::write(
        dir.0.join("package.inx"),
        zip.finish().unwrap().into_inner(),
    )
    .unwrap();
    succeeded(&dir.run(&["package.inx", "--inspect"]));
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "unrelated.txt",
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )
    .unwrap();
    std::io::Write::write_all(&mut zip, b"unrelated").unwrap();
    std::fs::write(
        dir.0.join("unrelated.zip"),
        zip.finish().unwrap().into_inner(),
    )
    .unwrap();
    let result = dir.run(&["unrelated.zip", "--inspect"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("model.json"));

    assert!(!dir.0.join("source.clm").exists());
    assert!(!dir.run(&["source"]).status.success());
    succeeded(&dir.run(&[
        "source",
        "--allow-lossy",
        "-o",
        "model.clm",
        "--report",
        "report.json",
    ]));
    let bytes = std::fs::read(dir.0.join("model.clm")).unwrap();
    let model = catchlight_core::Model::from_clm_bytes(&bytes).unwrap();
    assert!(model
        .param(&catchlight_core::ParamId::new("angleX").unwrap())
        .is_some());
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.0.join("report.json")).unwrap()).unwrap();
    assert!(report["baking"]["deform_bindings"].as_u64().unwrap() > 0);
    assert!(!dir
        .run(&["source", "--allow-lossy", "-o", "model.clm"])
        .status
        .success());
    assert_eq!(std::fs::read(dir.0.join("model.clm")).unwrap(), bytes);
}

#[test]
fn help_does_not_require_input_and_missing_arguments_fail() {
    let dir = Workspace::new();
    let result = dir.run(&["--help"]);
    succeeded(&result);
    let help = String::from_utf8(result.stdout).unwrap();
    assert!(help.contains("INX") && help.contains("LivLiv"));
    for args in [&[][..], &["--inspect"][..], &["-o"][..], &["--report"][..]] {
        assert!(!dir.run(args).status.success());
    }
}

#[test]
fn magic_overrides_extensions_and_recognizes_extensionless_inx() {
    let dir = Workspace::new();
    let payload = b"{}";
    let mut inx = b"TRNSRTS\0".to_vec();
    inx.extend((payload.len() as u32).to_be_bytes());
    inx.extend(payload);
    inx.extend(b"TEX_SECT");
    inx.extend(0u32.to_be_bytes());
    for (i, name) in ["model", "wrong.livliv", "model.INP", "inx", "-model.inx"]
        .into_iter()
        .enumerate()
    {
        std::fs::write(dir.0.join(name), &inx).unwrap();
        let out = format!("output-{i}.clm");
        succeeded(&dir.run(&["-o", &out, "--", name]));
        let bytes = std::fs::read(dir.0.join(&out)).unwrap();
        catchlight_core::Model::from_clm_bytes(&bytes).unwrap();
    }
    // Recognized CLM magic must not be sent back through the INX adapter.
    std::fs::copy(dir.0.join("output-0.clm"), dir.0.join("already.inx")).unwrap();
    let result = dir.run(&["already.inx"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already a Catchlight"));
    // LivLiv-only switches must not silently become no-ops for an INX input.
    assert!(!dir.run(&["model", "--inspect"]).status.success());
}

#[test]
fn unknown_content_fails_and_extension_hints_still_validate_the_format() {
    let dir = Workspace::new();
    std::fs::write(dir.0.join("unknown.bin"), b"not a model").unwrap();
    let result = dir.run(&["unknown.bin"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot detect the format"));
    std::fs::write(dir.0.join("broken.INP"), b"not a model").unwrap();
    let result = dir.run(&["broken.INP"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Invalid magic bytes"));
    std::fs::create_dir(dir.0.join("empty")).unwrap();
    let result = dir.run(&["empty"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("missing model.json"));
}

#[cfg(unix)]
#[test]
fn non_utf8_input_paths_are_detected_by_content() {
    use std::os::unix::ffi::OsStringExt;
    let dir = Workspace::new();
    let name = std::ffi::OsString::from_vec(vec![b'm', 0xff]);
    let mut bytes = b"TRNSRTS\0".to_vec();
    bytes.extend(2u32.to_be_bytes());
    bytes.extend(b"{}");
    bytes.extend(b"TEX_SECT");
    bytes.extend(0u32.to_be_bytes());
    std::fs::write(dir.0.join(&name), bytes).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_catchlight-importer"))
        .arg(name)
        .args(["-o", "result.clm"])
        .current_dir(&dir.0)
        .output()
        .unwrap();
    succeeded(&result);
    catchlight_core::Model::from_clm_bytes(&std::fs::read(dir.0.join("result.clm")).unwrap())
        .unwrap();
}
