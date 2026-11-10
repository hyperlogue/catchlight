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
    succeeded(&dir.run(&["inx", "input.inx"]));
    let saved = std::fs::read(dir.0.join("input.clm")).unwrap();
    let model = catchlight_core::Model::from_clm_bytes(&saved).unwrap();
    assert_eq!(model.node_count(), 1);
    assert!(!dir.run(&["inx", "input.inx"]).status.success());
    assert_eq!(std::fs::read(dir.0.join("input.clm")).unwrap(), saved);
    assert!(!dir.run(&["inx", "input.inx", "--invalid"]).status.success());
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
    succeeded(&dir.run(&["livliv", "source", "--inspect"]));
    assert!(!dir.0.join("source.clm").exists());
    assert!(!dir.run(&["livliv", "source"]).status.success());
    succeeded(&dir.run(&[
        "livliv",
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
        .run(&["livliv", "source", "--allow-lossy", "-o", "model.clm"])
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
    assert!(help.contains("inx") && help.contains("livliv"));
    for args in [
        &[][..],
        &["inx"][..],
        &["livliv"][..],
        &["livliv", "--report"][..],
    ] {
        assert!(!dir.run(args).status.success());
    }
}
