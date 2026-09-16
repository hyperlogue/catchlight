#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Render planning runs entirely on synthetic model structure, with no GPU,
//! texture decoding, editor dependency or private/LFS fixtures.

mod common;

use catchlight_cli::render::{args::ImageArgs, spec::*};
use catchlight_core::formats::clm::{ClmAnimation, ClmMesh};
use catchlight_core::{
    MaskMode, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart, Name, NodeId, ParamId,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn model() -> Model {
    let mut m = Model::new();
    let root = m.root().unwrap().clone();
    for name in ["panel-a", "panel-b", "mask-a", "interior-a", "interior-b"] {
        m.add_node_with_id(
            NodeId::new(name).unwrap(),
            &root,
            ModelNode::new(
                name,
                ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
            ),
        )
        .unwrap();
    }
    for name in ["drive", "response", "closure"] {
        m.add_param_with_id(
            ParamId::new(name).unwrap(),
            ModelParam::new(Name::new(name).unwrap(), 0.0, 1.0, 0.25),
        )
        .unwrap();
    }
    m.mask_add(
        &NodeId::new("panel-a").unwrap(),
        &NodeId::new("mask-a").unwrap(),
        MaskMode::Mask,
    )
    .unwrap();
    m
}
fn parse(value: Value) -> Document {
    Document::parse(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn resolve(value: Value) -> ResolvedSpec {
    parse(value).resolve(&model()).unwrap()
}
fn request(value: Value) -> ResolvedRequest {
    resolve(json!({"schema":1,"requests":{"default":value}}))
        .requests
        .remove("default")
        .unwrap()
}
fn assert_bad(value: Value, want: &str) {
    let error = Document::parse(&serde_json::to_vec(&value).unwrap())
        .and_then(|d| d.resolve(&model()))
        .unwrap_err()
        .to_string();
    assert!(error.contains(want), "wanted {want}, got {error}");
}
fn animation() -> Value {
    json!({"name":"Pulse","timestep":0.016666667,"length":6,"lead_in":-1,"lead_out":-1,
        "lanes":[{"param":"drive","interpolation":"Linear","keyframes":[{"frame":0,"value":0},{"frame":5,"value":1}]}]})
}
fn animated(settings: Value) -> Value {
    let mut req = json!({"animation":{"source":"spec","name":"pulse"},"rect":[0,0,1,1],"scale":1});
    req.as_object_mut()
        .unwrap()
        .extend(settings.as_object().unwrap().clone());
    json!({"schema":1,"animations":{"pulse":animation()},"requests":{"pulse":req}})
}

#[test]
fn defaults_resolve_independently_and_materialize_all_base_controls() {
    let r = request(json!({}));
    assert_eq!(r.framing.rect, DEFAULT_RECT);
    assert_eq!(r.framing.scale, DEFAULT_SCALE);
    assert_eq!(r.framing.size, [960, 1600]);
    assert_eq!(r.physics, Physics::Settled {});
    assert_eq!(r.background, Background::default());
    assert_eq!(r.pose.len(), 3);
    assert!(r.pose.values().all(|v| *v == 0.25));
    assert_eq!(r.outputs, ["default.png"]);
    assert_eq!(
        request(json!({"rect":[0,0,100,100]})).framing.size,
        [32, 32]
    );
    assert_eq!(request(json!({"scale":1})).framing.size, [3000, 5000]);
}

#[test]
fn inheritance_is_shallow_null_resets_and_all_names_execute_in_sorted_order() {
    let r = resolve(json!({"schema":1,"requests":{
        "z-base":{"rect":[10,20,100,100],"scale":2,"pose":{"drive":0.5,"closure":1},
            "background":{"srgb":[0,0,0],"alpha":0},"physics":{"mode":"off"},"only_parts":["panel-a"]},
        "a-child":{"extends":"z-base","scale":null,"pose":{"drive":1},"only_parts":[],"background":null},
        "b-reset":{"extends":"a-child","rect":null,"physics":null,"pose":null,"only_parts":null}
    }}));
    assert_eq!(
        r.requests.keys().map(String::as_str).collect::<Vec<_>>(),
        ["a-child", "b-reset", "z-base"]
    );
    let child = &r.requests["a-child"];
    assert_eq!(child.framing.size, [32, 32]);
    assert_eq!(child.framing.rect, [10.0, 20.0, 100.0, 100.0]);
    assert_eq!(child.pose[&ParamId::new("drive").unwrap()], 1.0);
    assert_eq!(child.pose[&ParamId::new("closure").unwrap()], 0.25);
    assert_eq!(child.only_parts, Some(vec![]));
    assert_eq!(child.background, Background::default());
    let reset = &r.requests["b-reset"];
    assert_eq!(reset.framing.rect, DEFAULT_RECT);
    assert_eq!(reset.physics, Physics::Settled {});
    assert_eq!(reset.only_parts, None);
    assert!(reset.pose.values().all(|v| *v == 0.25));
    assert_eq!(r.requests["z-base"].framing.scale, 2.0);
}

#[test]
fn inheritance_reports_missing_bases_cycles_and_does_not_use_call_stack() {
    for requests in [
        json!({"a":{"extends":"missing"}}),
        json!({"a":{"extends":"a"}}),
        json!({"a":{"extends":"b"},"b":{"extends":"a"}}),
    ] {
        assert!(parse(json!({"schema":1,"requests":requests}))
            .resolve(&model())
            .is_err());
    }
    let mut requests = BTreeMap::new();
    requests.insert("r0000".to_owned(), json!({"rect":[0,0,1,1],"scale":1}));
    for i in 1..MAX_REQUESTS {
        requests.insert(format!("r{i:04}"), json!({"extends":format!("r{:04}",i-1)}));
    }
    let doc = parse(json!({"schema":1,"requests":requests}));
    assert_eq!(doc.resolve(&model()).unwrap().requests.len(), MAX_REQUESTS);
}

#[test]
fn single_request_discovery_uses_inheritance_without_charging_other_render_work() {
    let document = parse(json!({"schema":1,"requests":{
        "base":{"rect":[0,0,4096,4096],"scale":1,"physics":{"mode":"off"}},
        "detail":{"extends":"base","rect":[0,0,32,32]},
        "unrelated":{"rect":[0,0,8192,8192],"scale":1}
    }}));
    assert!(document.resolve(&model()).is_err());
    let detail = document.resolve_one(&model(), "detail").unwrap();
    assert_eq!(detail.framing.size, [32, 32]);
    assert_eq!(detail.physics, Physics::Off {});
    assert!(document.resolve_one(&model(), "missing").is_err());
}

#[test]
fn framing_rounds_positive_halves_up_and_preserves_scale_and_center() {
    let f = Framing::resolve([1.0, 2.0, 2.5, 3.5], 1.0).unwrap();
    assert_eq!(f.size, [3, 4]);
    assert_eq!(f.center, [2.25, 3.75]);
    assert_eq!(f.effective_rect, [0.75, 1.75, 3.0, 4.0]);
    assert_eq!(f.world_to_pixel, [[1.0, 0.0, -0.75], [0.0, -1.0, 5.75]]);
    assert_eq!(
        Framing::resolve([0.0, 0.0, 0.01, 0.01], 1.0).unwrap().size,
        [1, 1]
    );
    let f = Framing::legacy(64, 96, 5000.0).unwrap();
    assert_eq!(f.size, [64, 96]);
    assert_eq!(f.center, [0.0, 0.0]);
    assert!((f.height - 5000.0).abs() < 0.001);
    for (rect, scale) in [
        ([0.0, 0.0, 0.0, 1.0], 1.0),
        ([0.0, 0.0, 1.0, 1.0], 0.0),
        ([0.0, 0.0, 1.0, 1.0], f32::INFINITY),
        ([f32::MAX, 0.0, f32::MAX, 1.0], f32::MIN_POSITIVE),
    ] {
        assert!(Framing::resolve(rect, scale).is_err());
    }
}

#[test]
fn filenames_and_animation_decimation_include_zero_without_changing_updates() {
    let r = resolve(animated(
        json!({"frames":{"every":2},"geometry":["panel-a"],
        "overlay":{"mesh":{"parts":["panel-a"],"positions":"rest","color":[0,0.8,1,1],"width_px":1}}}),
    ));
    let req = &r.requests["pulse"];
    assert_eq!(
        req.animation.as_ref().unwrap().frames,
        ResolvedFrames { count: 6, every: 2 }
    );
    assert_eq!(
        req.physics,
        Physics::Simulate {
            initial: Initial::Fresh,
            warmup_frames: 0
        }
    );
    assert_eq!(r.simulation_updates, 5);
    assert_eq!(r.output_pixels, 6);
    assert_eq!(r.output_files, 11);
    assert_eq!(
        req.outputs,
        [
            "pulse--000000.png",
            "pulse--000000--mesh.png",
            "pulse--000000--geometry.json",
            "pulse--000002.png",
            "pulse--000002--mesh.png",
            "pulse--000002--geometry.json",
            "pulse--000004.png",
            "pulse--000004--mesh.png",
            "pulse--000004--geometry.json",
            "pulse--trace.jsonl"
        ]
    );
    let r = resolve(animated(
        json!({"physics":{"mode":"simulate","initial":"settled","warmup_frames":7},"frames":{"count":1}}),
    ));
    assert_eq!(r.simulation_updates, 7);
    assert_eq!(
        r.requests["pulse"].outputs,
        ["pulse--000000.png", "pulse--trace.jsonl"]
    );
}

#[test]
fn static_and_animated_physics_have_distinct_valid_defaults() {
    assert_bad(
        json!({"schema":1,"requests":{"a":{"physics":{"mode":"simulate"}}}}),
        "requires an animation",
    );
    for field in [json!({"frames":{}}), json!({"trace_params":[]})] {
        assert_bad(
            json!({"schema":1,"requests":{"a":field}}),
            "require an animation",
        );
    }
    assert_bad(
        animated(json!({"physics":{"mode":"settled"}})),
        "animated requests",
    );
    let r = resolve(animated(json!({"physics":{"mode":"off"}})));
    assert_eq!(r.requests["pulse"].physics, Physics::Off {});
    assert_bad(animated(json!({"frames":{"count":0}})), "must be positive");
    assert_bad(animated(json!({"frames":{"every":0}})), "must be positive");
    assert_bad(
        animated(json!({"physics":{"mode":"off","warmup_frames":10}})),
        "unknown field",
    );
}

#[test]
fn duplicate_keys_unknown_fields_and_non_native_motion_are_refused() {
    for text in [
        r#"{"schema":1,"schema":1,"requests":{"a":{}}}"#,
        r#"{"schema":1,"requests":{"a":{},"a":{}}}"#,
        r#"{"schema":1,"requests":{"a":{"pose":{"drive":0,"drive":1}}}}"#,
    ] {
        assert!(Document::parse(text.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("duplicate key"));
    }
    for value in [
        json!({"schema":1,"requests":{"a":{"typo":1}}}),
        json!({"schema":1,"requests":{"a":{"background":{"srgb":[1,1,1],"alpha":1,"typo":1}}}}),
        json!({"schema":1,"requests":{"a":{"motion":{"inputs":"elsewhere.json"}}}}),
        json!({"schema":1,"requests":{"a":{}},"unknown":0}),
    ] {
        assert_bad(value, "unknown field");
    }
    for path in ["top", "lane", "keyframe"] {
        let mut doc = animated(json!({}));
        let clip = &mut doc["animations"]["pulse"];
        match path {
            "lane" => clip["lanes"][0]["typo"] = json!(1),
            "keyframe" => clip["lanes"][0]["keyframes"][0]["typo"] = json!(1),
            _ => clip["typo"] = json!(1),
        }
        assert_bad(doc, "unknown field");
    }
}

#[test]
fn native_animation_shape_matches_core_and_reuses_its_invariants() {
    let bytes = serde_json::to_vec(&animation()).unwrap();
    let wrapped: NativeAnimation = decode_json(&bytes).unwrap();
    let native: ClmAnimation = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wrapped.0, native);
    assert_eq!(
        serde_json::to_value(&wrapped).unwrap(),
        serde_json::to_value(&native).unwrap()
    );
    let mut unsorted = animated(json!({}));
    unsorted["animations"]["pulse"]["lanes"][0]["keyframes"][0]["frame"] = json!(9);
    assert!(parse(unsorted).resolve(&model()).is_err());
    let mut missing = animated(json!({}));
    missing["animations"]["pulse"]["lanes"][0]["param"] = json!("missing");
    assert!(parse(missing).resolve(&model()).is_err());
    for invalid in [0.0, -1.0] {
        let mut value = animated(json!({}));
        value["animations"]["pulse"]["timestep"] = json!(invalid);
        assert_bad(value, "timestep and length");
    }
    let mut model = model();
    model.set_animations(vec![native.clone(), native]).unwrap();
    let doc =
        parse(json!({"schema":1,"requests":{"a":{"animation":{"source":"model","name":"Pulse"}}}}));
    assert!(doc
        .resolve(&model)
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
    assert_bad(
        animated(json!({"animation":{"source":"model","name":"pulse"}})),
        "unknown model animation",
    );
}

#[test]
fn model_references_exact_edges_and_collections_are_validated() {
    for (field, want) in [
        (json!({"only_parts":["root"]}), "not a Part"),
        (json!({"geometry":["root"]}), "not a meshed node"),
        (json!({"hide_color":["missing"]}), "not a Part"),
        (json!({"pose":{"missing":0}}), "unknown parameter"),
        (json!({"pose":{"drive":2}}), "must be finite"),
        (
            json!({"strip_masks":[{"node":"panel-b","source":"mask-a"}]}),
            "no mask edge",
        ),
        (
            json!({"only_parts":["panel-a","panel-a"]}),
            "duplicate selection",
        ),
        (
            json!({"background":{"srgb":[2,0,0],"alpha":1}}),
            "components",
        ),
        (
            json!({"overlay":{"mesh":{"parts":[],"positions":"posed","color":[0,0,0,1],"width_px":1}}}),
            "nonempty meshes",
        ),
    ] {
        assert_bad(json!({"schema":1,"requests":{"a":field}}), want);
    }
    assert_bad(
        animated(json!({"trace_params":["missing"]})),
        "unknown trace parameter",
    );
}

#[test]
fn names_are_portable_and_case_insensitive_collisions_are_rejected() {
    for name in [
        "",
        "../escape",
        "/absolute",
        "-flag",
        "_lead",
        "frame--mesh",
        "snow☃",
        "a.png",
    ] {
        assert_bad(json!({"schema":1,"requests":{name:{}}}), "must use");
    }
    assert_bad(json!({"schema":1,"requests":{"A":{},"a":{}}}), "collision");
    assert_bad(json!({"schema":1,"requests":{}}), "at least one");
    assert_bad(json!({"schema":2,"requests":{"a":{}}}), "unsupported");
}

#[test]
fn budgets_fail_before_output_allocation_with_named_limits() {
    assert_bad(
        json!({"schema":1,"requests":{"a":{"rect":[0,0,8193,1],"scale":1}}}),
        "image_dimension",
    );
    assert_bad(
        json!({"schema":1,"requests":{"a":{"rect":[0,0,8192,8192],"scale":1}}}),
        "image_pixels",
    );
    assert_bad(
        animated(json!({"frames":{"count":100002,"every":100000}})),
        "simulation_updates",
    );
    let r = resolve(animated(json!({"frames":{"count":100001,"every":100000}})));
    assert_eq!(r.simulation_updates, MAX_UPDATES);
    assert_bad(
        animated(
            json!({"frames":{"count":100001,"every":100000},"physics":{"mode":"simulate","warmup_frames":1}}),
        ),
        "simulation_updates",
    );
    assert_bad(animated(json!({"frames":{"count":100000}})), "output_files");
    assert_bad(
        animated(json!({"rect":[0,0,4096,4096],"scale":1,"frames":{"count":120}})),
        "run_pixels",
    );
    let mut requests = BTreeMap::new();
    for i in 0..=MAX_REQUESTS {
        requests.insert(format!("r{i}"), json!({}));
    }
    assert_bad(json!({"schema":1,"requests":requests}), "budget requests");
    let error = Document::parse(&vec![b' '; MAX_JSON_BYTES as usize + 1]).unwrap_err();
    assert!(matches!(
        error,
        catchlight_cli::Error::RenderLimit {
            budget: "spec_bytes",
            ..
        }
    ));
}

#[test]
fn documented_shortcuts_and_specs_normalize_identically_without_mutation() {
    let m = model();
    let original = m.to_clm_bytes().unwrap();
    let doc = Document::parse(include_bytes!("fixtures/render-spec.json")).unwrap();
    let normalized = doc.resolve(&m).unwrap();
    let flags = ImageArgs {
        keep: Some(vec!["panel-a".into()]),
        strip_masks: vec!["mask-a".into()],
        rect: Some("-500,-500,1000,1000".into()),
        scale: Some(0.512),
        set: vec!["drive=0.5".into()],
        background: Some(catchlight_cli::render::args::BackgroundArg::Transparent),
        physics: Some(catchlight_cli::render::args::PhysicsArg::Off),
    };
    let flag_request = Document::single(flags.request(&m).unwrap())
        .resolve(&m)
        .unwrap();
    let mut from_flags = serde_json::to_value(&flag_request.requests["default"]).unwrap();
    let mut from_spec = serde_json::to_value(&normalized.requests["part"]).unwrap();
    from_flags.as_object_mut().unwrap().remove("outputs");
    from_spec.as_object_mut().unwrap().remove("outputs");
    assert_eq!(from_flags, from_spec);
    assert_eq!(m.to_clm_bytes().unwrap(), original);
    assert_eq!(
        normalized.requests["detail"].outputs,
        ["detail.png", "detail--mesh.png", "detail--geometry.json"]
    );
    assert_eq!(normalized.requests["pulse"].outputs.len(), 62);
}

#[test]
fn cli_schema_and_validate_do_not_need_a_gpu_or_create_artifacts() {
    let dir = common::tmp("render-spec-validate");
    let model_path = dir.join("model.clm");
    std::fs::write(&model_path, model().to_clm_bytes().unwrap()).unwrap();
    let spec_path = dir.join("spec.json");
    std::fs::write(&spec_path, include_bytes!("fixtures/render-spec.json")).unwrap();
    for args in [
        vec!["render", "--schema"],
        vec!["render", "--schema", "resolved"],
    ] {
        let (code, stdout, stderr) = common::run(&args);
        assert_eq!(code, 0, "{stderr}");
        let schema: Value = serde_json::from_str(&stdout).unwrap();
        assert!(schema["$schema"]
            .as_str()
            .unwrap()
            .contains("json-schema.org"));
        assert_eq!(schema["additionalProperties"], false);
    }
    let (code, stdout, stderr) = common::run(&[
        "render",
        model_path.to_str().unwrap(),
        "--spec",
        spec_path.to_str().unwrap(),
        "--validate",
    ]);
    assert_eq!(code, 0, "{stderr}");
    let result: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        result["requests"]["part"]["framing"]["size"],
        json!([512, 512])
    );
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    for args in [
        vec!["render", "--schema", "--validate"],
        vec![
            "render",
            model_path.to_str().unwrap(),
            "--spec",
            spec_path.to_str().unwrap(),
            "--validate",
            "--scale",
            "1",
        ],
        vec![
            "render",
            model_path.to_str().unwrap(),
            "out.png",
            "--scale",
            "1",
        ],
        vec!["render", "--schema", "unknown"],
    ] {
        let (code, _, _) = common::run(&args);
        assert_eq!(code, 2);
    }
}
