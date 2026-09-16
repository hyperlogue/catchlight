#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Synthetic end-to-end artifacts exercise shared GPU state, selection, clocks,
//! discovery and failure handling without private models or imported fixtures.
mod common;
use catchlight_cli::render::{
    artifacts::InputIdentity,
    execute::{self, Cancellation, Destination},
    spec::Document,
};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding};
use catchlight_core::{
    BindingKey, BindingTarget, MaskMode, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart,
    ModelTexture, Name, NodeId, ParamId, ScalarTarget, TexId,
};
use image::ImageEncoder;
use serde_json::{json, Value};
use std::path::Path;

fn model() -> Model {
    let mut m = Model::new();
    let root = m.root().unwrap().clone();
    for (name, parent, x, color) in [
        ("parent", root, 0.0, [255, 0, 0, 255]),
        (
            "child",
            NodeId::new("parent").unwrap(),
            20.0,
            [0, 255, 0, 255],
        ),
        (
            "mask",
            NodeId::new("root").unwrap(),
            20.0,
            [255, 255, 255, 255],
        ),
    ] {
        let id = NodeId::new(name).unwrap();
        let mesh = ClmMesh {
            verts: vec![x - 8.0, -8.0, x + 8.0, -8.0, x + 8.0, 8.0, x - 8.0, 8.0],
            uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        };
        m.add_node_with_id(
            id.clone(),
            &parent,
            ModelNode::new(name, ModelNodeKind::Part(ModelPart::new(mesh))),
        )
        .unwrap();
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&color.repeat(64), 8, 8, image::ExtendedColorType::Rgba8)
            .unwrap();
        m.add_texture_with_id(
            TexId::new(format!("{name}-tex")).unwrap(),
            &id,
            ModelTexture {
                encoding: TextureEncoding::Png,
                alpha: TextureAlpha::Straight,
                data: bytes.into(),
            },
        )
        .unwrap();
    }
    let param = ParamId::new("drive").unwrap();
    m.add_param_with_id(
        param.clone(),
        ModelParam::new(Name::new("Drive").unwrap(), 0.0, 1.0, 0.0),
    )
    .unwrap();
    let key = BindingKey::new(
        param,
        NodeId::new("child").unwrap(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    m.set_binding_key(&key, [0, 0], 0.0).unwrap();
    m.set_binding_key(&key, [1, 0], 8.0).unwrap();
    m.mask_add(
        &NodeId::new("child").unwrap(),
        &NodeId::new("mask").unwrap(),
        MaskMode::Mask,
    )
    .unwrap();
    m.update_node(&NodeId::new("mask").unwrap(), |n| n.enabled = false)
        .unwrap();
    m
}
fn write_model(path: &Path) {
    std::fs::write(path, model().to_clm_bytes().unwrap()).unwrap();
}

#[test]
fn render_cannot_replace_its_input_model() {
    let dir = common::tmp("render-execution-input-protection");
    let path = dir.join("model.clm");
    write_model(&path);
    let before = std::fs::read(&path).unwrap();
    let (code, _, stderr) = common::run(&[
        "render",
        path.to_str().unwrap(),
        "--out",
        path.to_str().unwrap(),
    ]);
    assert_eq!(code, 2);
    assert!(stderr.contains("input"), "{stderr}");
    assert_eq!(std::fs::read(path).unwrap(), before);
}
fn spec() -> Value {
    json!({"schema":1,"requests":{"a":{"rect":[-16,-16,64,32],"scale":1,"physics":{"mode":"off"},"background":{"srgb":[0,0,0],"alpha":0},"only_parts":["child"]}}})
}
fn invoke(dir: &Path, value: &Value, out: &str) -> Value {
    let model = dir.join("model.clm");
    if !model.exists() {
        write_model(&model);
    }
    let path = dir.join(format!("{out}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    let destination = dir.join(out);
    let (code, stdout, stderr) = common::run(&[
        "render",
        model.to_str().unwrap(),
        "--spec",
        path.to_str().unwrap(),
        "--out-dir",
        destination.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    serde_json::from_slice(&std::fs::read(destination.join("run.json")).unwrap()).unwrap()
}
fn rgba(path: &Path) -> image::RgbaImage {
    image::open(path).unwrap().to_rgba8()
}

#[test]
fn retained_child_survives_excluded_parent_and_disabled_mask_contributes() {
    let dir = common::tmp("render-execution-selection");
    let mut s = spec();
    s["requests"]["b"] = json!({"extends":"a","hide_color":["child"]});
    s["requests"]["c"] =
        json!({"extends":"a","pose":{"drive":1},"strip_masks":[{"node":"child","source":"mask"}]});
    s["requests"]["d"] = json!({"extends":"a","pose":{"drive":1}});
    let run = invoke(&dir, &s, "images");
    assert_eq!(run["complete"], true);
    let a = rgba(&dir.join("images/a.png"));
    assert_eq!(a.get_pixel(36, 16).0, [0, 255, 0, 255]);
    assert_eq!(a.get_pixel(16, 16).0, [0, 0, 0, 0]);
    assert!(rgba(&dir.join("images/b.png"))
        .pixels()
        .all(|p| p.0 == [0, 0, 0, 0]));
    let c = rgba(&dir.join("images/c.png"));
    let d = rgba(&dir.join("images/d.png"));
    assert_eq!(c.get_pixel(48, 16).0, [0, 255, 0, 255]);
    assert_eq!(d.get_pixel(48, 16).0, [0, 0, 0, 0]);
    assert_eq!(
        run["input"]["model_sha256"],
        execute::hash(&std::fs::read(dir.join("model.clm")).unwrap())
    );
    assert!(run["renderer"]["backend"].is_string());
    assert_eq!(run["requests"]["a"]["outputs"][0]["file"], "a.png");
    assert_eq!(
        run["plan"]["requests"]["c"]["strip_masks"][0]["source"],
        "mask"
    );
}

#[test]
fn sidecars_match_captured_world_frame_and_clean_image_stays_separate() {
    let dir = common::tmp("render-execution-geometry");
    let mut s = spec();
    s["requests"]["a"]["pose"] = json!({"drive":1});
    s["requests"]["a"]["geometry"] = json!(["child"]);
    s["requests"]["a"]["overlay"] =
        json!({"mesh":{"parts":["child"],"positions":"posed","color":[0,0.8,1,1],"width_px":1}});
    invoke(&dir, &s, "images");
    let geometry: Value =
        serde_json::from_slice(&std::fs::read(dir.join("images/a--geometry.json")).unwrap())
            .unwrap();
    assert_eq!(geometry["nodes"]["child"]["rest"][0], json!([12.0, -8.0]));
    assert_eq!(
        geometry["nodes"]["child"]["world"][0],
        json!([20.0, -8.0, 0.0])
    );
    assert_eq!(
        geometry["nodes"]["child"]["triangles"],
        json!([[0, 1, 2], [0, 2, 3]])
    );
    assert_eq!(geometry["framing"]["size"], json!([64, 32]));
    assert_ne!(
        std::fs::read(dir.join("images/a.png")).unwrap(),
        std::fs::read(dir.join("images/a--mesh.png")).unwrap()
    );
}

#[test]
fn animation_decimation_reuses_native_clock_and_does_not_change_physics_or_images() {
    let dir = common::tmp("render-execution-animation");
    let mut m = model();
    let response = ParamId::new("response").unwrap();
    m.add_param_with_id(
        response.clone(),
        ModelParam::new(Name::new("Response").unwrap(), -1.0, 1.0, 0.0),
    )
    .unwrap();
    let driver = NodeId::new("pendulum").unwrap();
    let mut physics =
        catchlight_core::ModelPhysics::new(catchlight_core::physics::PendulumKind::RigidPendulum);
    physics.map_mode = catchlight_core::physics::PhysicsParamMapMode::XY;
    physics.length = 20.0;
    m.add_node_with_id(
        driver.clone(),
        &NodeId::new("parent").unwrap(),
        ModelNode::new("Pendulum", ModelNodeKind::SimplePhysics(physics)),
    )
    .unwrap();
    m.set_physics_targets(&driver, [Some(response.clone()), None])
        .unwrap();
    let drive_anchor = BindingKey::new(
        ParamId::new("drive").unwrap(),
        driver,
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    m.set_binding_key(&drive_anchor, [0, 0], 0.0).unwrap();
    m.set_binding_key(&drive_anchor, [1, 0], 30.0).unwrap();
    let response_mesh = BindingKey::new(
        response,
        NodeId::new("child").unwrap(),
        BindingTarget::Scalar(ScalarTarget::Ty),
    );
    m.set_binding_key(&response_mesh, [0, 0], -8.0).unwrap();
    m.set_binding_key(&response_mesh, [1, 0], 8.0).unwrap();
    std::fs::write(dir.join("model.clm"), m.to_clm_bytes().unwrap()).unwrap();
    let mut full = spec();
    full["animations"] = json!({"pulse":{"name":"Pulse","timestep":0.125,"length":8,"lead_in":2,"lead_out":6,
        "lanes":[{"param":"drive","interpolation":"Linear","keyframes":[{"frame":0,"value":0},{"frame":7,"value":1}]}]}});
    full["requests"]["a"]["animation"] = json!({"source":"spec","name":"pulse"});
    full["requests"]["a"]["frames"] = json!({"count":10,"every":1});
    full["requests"]["a"]["trace_params"] = json!(["drive", "response"]);
    full["requests"]["a"]["physics"] =
        json!({"mode":"simulate","initial":"settled","warmup_frames":3});
    let first = invoke(&dir, &full, "full");
    let mut decimated = full.clone();
    decimated["requests"]["a"]["frames"]["every"] = json!(3);
    let second = invoke(&dir, &decimated, "decimated");
    assert_eq!(
        first["plan"]["simulation_updates"],
        second["plan"]["simulation_updates"]
    );
    assert_eq!(
        std::fs::read(dir.join("full/a--trace.jsonl")).unwrap(),
        std::fs::read(dir.join("decimated/a--trace.jsonl")).unwrap()
    );
    let traces = std::fs::read_to_string(dir.join("full/a--trace.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str::<Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(traces.len(), 10);
    assert_eq!(traces[0]["clip"]["frame"], 0.0);
    assert_eq!(traces[6]["clip"]["frame"], 2.0);
    assert_eq!(traces[6]["elapsed"], 0.75);
    assert!(traces
        .iter()
        .any(|t| t["effective"]["response"].as_f64().unwrap().abs() > 0.001));
    assert!(traces.iter().all(|t| t["requested"]["response"] == 0.0));
    for frame in [0, 3, 6, 9] {
        let file = format!("a--{frame:06}.png");
        assert_eq!(
            std::fs::read(dir.join("full").join(&file)).unwrap(),
            std::fs::read(dir.join("decimated").join(&file)).unwrap()
        );
    }
    assert!(!dir.join("decimated/a--000001.png").exists());
}

#[test]
fn bounds_is_gpu_free_and_lists_excluded_parts_without_adding_them_to_overall() {
    let dir = common::tmp("render-execution-bounds");
    let path = dir.join("model.clm");
    write_model(&path);
    let (code, stdout, stderr) = common::run(&[
        "bounds",
        path.to_str().unwrap(),
        "--keep",
        "child",
        "--physics",
        "off",
        "--set",
        "drive=1",
    ]);
    assert_eq!(code, 0, "{stderr}");
    let bounds: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        bounds["overall"],
        json!({"min":[20.0,-8.0],"max":[36.0,8.0]})
    );
    assert_eq!(bounds["parts"]["parent"]["included"], false);
    assert_eq!(bounds["parts"]["mask"]["included"], false);
    assert_eq!(bounds["parts"]["child"]["included"], true);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn redirected_terminal_mode_emits_listing_only_and_never_creates_implicit_files() {
    let dir = common::tmp("render-execution-redirected");
    let path = dir.join("model.clm");
    write_model(&path);
    let (code, stdout, stderr) = common::run(&[
        "render",
        path.to_str().unwrap(),
        "--rect=-16,-16,64,32",
        "--scale",
        "1",
    ]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("Render list:"));
    assert!(stdout.contains("--out"));
    assert!(!stdout.contains('\u{1b}'));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn existing_output_and_cancelled_run_keep_an_honest_incomplete_manifest() {
    let dir = common::tmp("render-execution-cancel");
    let destination = dir.join("output");
    let m = model();
    let document = Document::parse(&serde_json::to_vec(&spec()).unwrap()).unwrap();
    let plan = document.resolve(&m).unwrap();
    let cancellation = Cancellation::default();
    cancellation.cancel();
    let error = execute::run(
        &m,
        plan,
        Destination::Directory(destination.clone()),
        Some(InputIdentity {
            model_sha256: execute::hash(b"model"),
            spec_sha256: None,
        }),
        &cancellation,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("cancelled"));
    let before = std::fs::read(destination.join("run.json")).unwrap();
    let run: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(run["complete"], false);
    assert_eq!(run["failure"]["kind"], "cancelled");
    assert!(execute::run(
        &m,
        document.resolve(&m).unwrap(),
        Destination::Directory(destination.clone()),
        Some(InputIdentity {
            model_sha256: execute::hash(b"model"),
            spec_sha256: None
        }),
        &Cancellation::default()
    )
    .is_err());
    assert_eq!(std::fs::read(destination.join("run.json")).unwrap(), before);
}

#[test]
fn schemas_are_exposed_for_every_emitted_record() {
    for kind in ["spec", "resolved", "geometry", "trace", "run"] {
        let (code, stdout, stderr) = common::run(&["render", "--schema", kind]);
        assert_eq!(code, 0, "{stderr}");
        let schema: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(schema["additionalProperties"], false);
    }
    let (code, stdout, stderr) = common::run(&["bounds", "--schema"]);
    assert_eq!(code, 0, "{stderr}");
    let schema: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        schema["properties"]["overall"]["anyOf"][0]["$ref"],
        "#/$defs/Bounds"
    );
}

#[cfg(unix)]
#[test]
fn interrupt_after_a_frame_preserves_valid_artifacts_and_marks_manifest_incomplete() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let dir = common::tmp("render-execution-interrupt");
    let model_path = dir.join("model.clm");
    write_model(&model_path);
    let mut value = spec();
    value["animations"] =
        json!({"pulse":{"timestep":0.016666667,"length":2,"lead_in":-1,"lead_out":-1,"lanes":[]}});
    value["requests"]["a"]["animation"] = json!({"source":"spec","name":"pulse"});
    value["requests"]["a"]["frames"] = json!({"count":1000});
    value["requests"]["a"]["scale"] = json!(8);
    let spec_path = dir.join("spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&value).unwrap()).unwrap();
    let destination = dir.join("images");
    let mut child = Command::new(common::bin())
        .args([
            "render",
            model_path.to_str().unwrap(),
            "--spec",
            spec_path.to_str().unwrap(),
            "--out-dir",
            destination.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !destination.join("a--000000.png").exists() && Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("render ended before the first frame: {status}");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if !destination.join("a--000000.png").exists() {
        let _ = child.kill();
        panic!("first frame did not appear");
    }
    assert!(Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(destination.join("run.json")).unwrap()).unwrap();
    assert_eq!(manifest["complete"], false);
    assert_eq!(manifest["failure"]["kind"], "cancelled");
    let outputs = manifest["requests"]["a"]["outputs"].as_array().unwrap();
    assert!(!outputs.is_empty());
    assert!(outputs.len() < 1000);
    for item in outputs {
        assert!(image::open(destination.join(item["file"].as_str().unwrap())).is_ok());
    }
    assert!(!destination.join("a--trace.jsonl").exists());
}
