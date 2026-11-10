#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use catchlight_core::formats::clm::{ClmExtension, ClmNodeKind, TextureAlpha};
use catchlight_core::{Model, NodeId, ParamId};
use serde_json::json;
use std::io::{Cursor, Write};

fn files() -> BTreeMap<String, Vec<u8>> {
    let manifest = json!({
        "format":"livliv-model", "version":4, "name":"Synthetic", "canvas":[32,32],
        "meta":{"generator":{"name":"catchlight tests"}, "author":{"name":"Test artist"}, "license":{"id":"CC0-1.0"}, "watermark":{"kind":"none"}},
        "anchors":{"head":{"cx":16,"cy":12,"rx":8,"ry":8},"face":[8,4,24,20],"neck_y":22,"top_y":0,"bottom_y":32,"hair_root_y":4},
        "expressions":[{"id":"smile","label":{"en":"Smile"}}],
        "layers":[
          {"id":"face","texture":"textures/a.png","x":2,"y":4,"w":4,"h":4,"group":"head"},
          {"id":"mouth","texture":"textures/a.png","frames":["textures/a.png","textures/b.png","textures/c.png"],"visemes":["closed","half","open"],"role":"mouth","x":10,"y":16,"w":4,"h":4,"group":"head"},
          {"id":"smile","texture":"textures/b.png","role":"expr","expression":"smile","x":2,"y":4,"w":4,"h":4,"group":"head"}
        ]
    });
    let mut files = BTreeMap::from([("model.json".into(), serde_json::to_vec(&manifest).unwrap())]);
    for (name, rgba) in [
        ("a", [255, 0, 0, 128]),
        ("b", [0, 255, 0, 255]),
        ("c", [0, 0, 255, 255]),
    ] {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(4, 4, image::Rgba(rgba))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        files.insert(format!("textures/{name}.png"), png.into_inner());
    }
    files
}
fn change(files: &mut BTreeMap<String, Vec<u8>>, f: impl FnOnce(&mut Value)) {
    let mut m: Value = serde_json::from_slice(&files["model.json"]).unwrap();
    f(&mut m);
    files.insert("model.json".into(), serde_json::to_vec(&m).unwrap());
}
fn validated() -> ValidatedPackage {
    Package::from_files(files()).unwrap().validate().unwrap()
}

#[test]
fn conversion_roundtrips_geometry_alpha_rights_and_drivable_parts() {
    let converted = validated().convert(Options { allow_lossy: true }).unwrap();
    let bytes = converted.model.to_clm_bytes().unwrap();
    let model = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(bytes, model.to_clm_bytes().unwrap());
    let file = model.to_clm_file().unwrap();
    let ClmNodeKind::Part(face) = &file.doc.nodes[1].kind else {
        panic!()
    };
    assert_eq!(
        face.mesh.verts,
        [-14., 12., -10., 12., -14., 10., -10., 10., -14., 8., -10., 8.]
    );
    assert_eq!(
        face.mesh.uvs,
        [0., 0., 1., 0., 0., 0.5, 1., 0.5, 0., 1., 1., 1.]
    );
    assert!(file.doc.nodes[4].z_order > file.doc.nodes[1].z_order);
    assert_eq!(file.textures[0].alpha, TextureAlpha::Straight);
    assert_eq!(file.textures[0].data, files()["textures/a.png"]);
    let ClmExtension::Json(info) =
        &file.doc.extensions[&catchlight_core::id::ExtensionKey::new("livliv.import").unwrap()]
    else {
        panic!()
    };
    assert_eq!(info["manifest"]["meta"]["author"]["name"], "Test artist");
    assert_eq!(info["manifest"]["meta"]["license"]["id"], "CC0-1.0");
    assert!(!info["report"]["losses"].as_array().unwrap().is_empty());
    let mut puppet = catchlight_core::puppet::Puppet::new(&model);
    for frame in 0..3 {
        puppet.set_param_value(&ParamId::new("mouthFrame/mouth").unwrap(), frame as f32);
        puppet.set_param_value(
            &ParamId::new("expression/smile").unwrap(),
            if frame == 2 { 1.0 } else { 0.0 },
        );
        puppet.tick(&model, 0.0);
        for part in 0..3 {
            let idx = puppet
                .node_idx(&NodeId::new(format!("layer-1/frame-{part}")).unwrap())
                .unwrap();
            assert_eq!(
                puppet.opacity(idx),
                Some(if part == frame { 1.0 } else { 0.0 })
            );
        }
        let idx = puppet
            .node_idx(&NodeId::new("layer-2/frame-0").unwrap())
            .unwrap();
        assert_eq!(
            puppet.opacity(idx),
            Some(if frame == 2 { 1.0 } else { 0.0 })
        );
    }
}

#[test]
fn watermark_is_a_blocker_even_with_lossy_opt_in() {
    for mark in [
        json!({"kind":"livliv","center":false}),
        json!({"kind":"text","text":"Credit"}),
    ] {
        let mut files = files();
        change(&mut files, |m| m["meta"]["watermark"] = mark);
        let package = Package::from_files(files).unwrap().validate().unwrap();
        assert!(package.report().blockers[0].contains("watermark"));
        assert!(package.convert(Options { allow_lossy: true }).is_err());
    }
    assert!(validated().convert(Options::default()).is_err());
}

#[test]
fn rejects_bad_versions_references_geometry_and_resources() {
    let mutations: [fn(&mut Value); 10] = [
        |m| m["version"] = json!(5),
        |m| m["requires"] = json!(["x_unknown"]),
        |m| m["layers"][1]["id"] = json!("face"),
        |m| m["layers"][0]["clip"] = json!("absent"),
        |m| m["layers"][0]["x"] = json!(31),
        |m| m["layers"][0]["group"] = json!("unknown"),
        |m| m["layers"][0]["texture"] = json!("../outside.png"),
        |m| m["layers"][0]["w"] = json!(5),
        |m| m["layers"][1]["visemes"] = json!(["closed"]),
        |m| m["layers"][0]["hide"] = json!({"missing":1}),
    ];
    for mutate in mutations {
        let mut files = files();
        change(&mut files, mutate);
        assert!(Package::from_files(files).unwrap().validate().is_err());
    }
    let mut files = files();
    files.remove("textures/c.png");
    assert!(Package::from_files(files).unwrap().validate().is_err());
}

fn zip_files(files: &BTreeMap<String, Vec<u8>>, method: zip::CompressionMethod) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in files {
        zip.start_file(
            name,
            zip::write::SimpleFileOptions::default().compression_method(method),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn zip_and_wrapped_directory_have_identical_outputs() {
    let plain = validated()
        .convert(Options { allow_lossy: true })
        .unwrap()
        .model
        .to_clm_bytes()
        .unwrap();
    let wrapped = files()
        .into_iter()
        .map(|(n, b)| (format!("character/{n}"), b))
        .collect();
    for method in [
        zip::CompressionMethod::Stored,
        zip::CompressionMethod::Deflated,
    ] {
        let bytes = zip_files(&wrapped, method);
        let imported = Package::from_zip(&bytes)
            .unwrap()
            .validate()
            .unwrap()
            .convert(Options { allow_lossy: true })
            .unwrap();
        assert_eq!(plain, imported.model.to_clm_bytes().unwrap());
    }
}

#[test]
fn rejects_traversal_corruption_and_unbounded_zip_directory() {
    for path in [
        "../model.json",
        "/model.json",
        "a/./b",
        "a\\b",
        "C:/file",
        "a//b",
    ] {
        assert!(Package::from_files(BTreeMap::from([(path.into(), vec![])])).is_err());
        assert!(Package::from_zip(&zip_files(
            &BTreeMap::from([(path.into(), vec![])]),
            zip::CompressionMethod::Stored
        ))
        .is_err());
    }
    let mut bytes = zip_files(&files(), zip::CompressionMethod::Stored);
    let n = bytes.len();
    bytes[n - 12..n - 10].copy_from_slice(&2049u16.to_le_bytes());
    assert!(Package::from_zip(&bytes).is_err());
    assert!(Package::from_zip(b"PK").is_err());
}

#[test]
fn keyforms_reject_wrong_decoded_lengths() {
    let mut files = files();
    change(&mut files, |m| {
        m["files"] = json!({"keyforms":"keyforms.json"})
    });
    files.insert("keyforms.json".into(), serde_json::to_vec(&json!({"version":1,"cell":8,"cols":2,"rows":2,"units":1,"turn":1,"nod":1,"keys":[{"param":"angleX","value":30,"fields":{"head":"AA=="}}]})).unwrap());
    assert!(Package::from_files(files)
        .unwrap()
        .validate()
        .unwrap_err()
        .to_string()
        .contains("incorrect field length"));
}

#[test]
fn webp_is_transcoded_and_clip_references_an_invisible_mask() {
    let mut files = files();
    let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([20, 30, 40, 128]));
    let mut webp = Cursor::new(Vec::new());
    image.write_to(&mut webp, image::ImageFormat::WebP).unwrap();
    files.insert("textures/mask.webp".into(), webp.into_inner());
    change(&mut files, |m| {
        m["layers"][0]["mask"] = json!("textures/mask.webp");
        m["layers"][1]["clip"] = json!("face");
    });
    let model = Package::from_files(files)
        .unwrap()
        .validate()
        .unwrap()
        .convert(Options { allow_lossy: true })
        .unwrap()
        .model;
    let file = model.to_clm_file().unwrap();
    let mask = file
        .doc
        .nodes
        .iter()
        .find(|n| n.id.as_str() == "layer-0/mask")
        .unwrap();
    let ClmNodeKind::Part(mask_part) = &mask.kind else {
        panic!()
    };
    assert_eq!(mask_part.opacity, 0.0);
    let texture = file
        .textures
        .iter()
        .find(|t| Some(&t.id) == mask_part.albedo.as_ref())
        .unwrap();
    assert_eq!(
        image::load_from_memory(&texture.data).unwrap().to_rgba8(),
        image
    );
    assert_eq!(
        texture.encoding,
        catchlight_core::formats::clm::TextureEncoding::Png
    );
    for node in file
        .doc
        .nodes
        .iter()
        .filter(|n| n.id.as_str().starts_with("layer-1/"))
    {
        let ClmNodeKind::Part(part) = &node.kind else {
            panic!()
        };
        assert_eq!(part.masks[0].source, mask.id);
    }
}

#[test]
fn duplicate_zip_names_are_rejected_before_manifest_selection() {
    let mut bytes = zip_files(
        &BTreeMap::from([("a".into(), vec![0]), ("b".into(), vec![0])]),
        zip::CompressionMethod::Stored,
    );
    for i in 0..bytes.len().saturating_sub(46) {
        let offset = if bytes.get(i..i + 4) == Some(b"PK\x03\x04") {
            30
        } else if bytes.get(i..i + 4) == Some(b"PK\x01\x02") {
            46
        } else {
            continue;
        };
        if bytes[i + offset] == b'b' {
            bytes[i + offset] = b'a';
        }
    }
    let error = Package::from_zip(&bytes).unwrap_err().to_string();
    assert!(error.contains("duplicate"), "{error}");
}

#[cfg(unix)]
#[test]
fn directory_resource_symlinks_are_rejected() {
    let dir = std::env::temp_dir().join(format!(
        "catchlight-livliv-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink("/dev/null", dir.join("model.json")).unwrap();
    let error = Package::from_directory(&dir).unwrap_err();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(error.to_string().contains("symlink"));
}

#[test]
#[ignore = "requires pinned official w01 sample; set LIVLIV_SAMPLE_DIR"]
fn official_sample_validates_without_losing_watermark_requirement() {
    let path = std::env::var_os("LIVLIV_SAMPLE_DIR")
        .expect("set LIVLIV_SAMPLE_DIR to the pinned official characters/w01 directory");
    let package = Package::from_directory(std::path::Path::new(&path))
        .unwrap()
        .validate()
        .unwrap();
    assert_eq!(package.name(), "Max");
    assert_eq!(package.layer_count(), 19);
    assert_eq!(
        package.manifest()["meta"]["license"]["id"],
        "LicenseRef-LivLiv-Samples"
    );
    assert!(package
        .report()
        .blockers
        .iter()
        .any(|b| b.contains("watermark")));
    assert!(package.convert(Options { allow_lossy: true }).is_err());
    let zip = zip_files(&package.package.files, zip::CompressionMethod::Deflated);
    let zipped = Package::from_zip(&zip).unwrap().validate().unwrap();
    assert_eq!(package.manifest(), zipped.manifest());
    assert_eq!(
        serde_json::to_value(package.report()).unwrap(),
        serde_json::to_value(zipped.report()).unwrap()
    );
}

fn motion_package() -> ValidatedPackage {
    let mut f = files();
    change(&mut f, |m| {
        m["canvas"] = json!([96, 96]);
        m["anchors"] = json!({"head":{"cx":48,"cy":36,"rx":24,"ry":24},"face":[24,12,72,60],"neck_y":66,"top_y":0,"bottom_y":96,"hair_root_y":12,
            "jaw":{"top":42,"chin":60,"half":2,"open":4},
            "chest":{"cx":48,"cy":78,"rx":24,"ry":12},
            "brows":[[24,24,36,30]],"brow_lines":[{"x":[24,30,36],"bottom":[28,29,28],"lid":[36,37,36],"cap":[8,8,8]}]});
        for l in m["layers"].as_array_mut().unwrap() {
            l["x"] = json!(12);
            l["y"] = json!(12);
            l["w"] = json!(48);
            l["h"] = json!(48);
        }
        let layers = m["layers"].as_array_mut().unwrap();
        for (id, group, role, depth) in [
            ("body", "body", "", 0.),
            ("hair", "hair_front", "", 0.2),
            ("eye_L", "head", "eye_L", 0.1),
            ("iris_L", "head", "iris_L", 0.12),
            ("lid_L", "head", "lid_L", 0.),
            ("shadow", "head", "neck_shadow", 0.),
        ] {
            let mut l = json!({"id":id,"group":group,"texture":"textures/a.png","x":12,"y":12,"w":48,"h":48,"depth":depth});
            if !role.is_empty() {
                l["role"] = json!(role);
            }
            if role == "eye_L" || role == "iris_L" {
                l["pivot"] = json!([36, 36]);
            }
            if role == "iris_L" {
                l["range"] = json!([5, 4]);
            }
            if group == "body" {
                l["y"] = json!(36);
            }
            layers.push(l);
        }
        m["rig"] =
            json!({"neck":{"twist":0.4},"bulge_shape":1.3,"bulge_extent":[1.1,1.2,1.3,0.9,0.8]});
    });
    for path in ["textures/a.png", "textures/b.png", "textures/c.png"] {
        let mut png = Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(48, 48, image::Rgba([255, 255, 255, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        f.insert(path.into(), png.into_inner());
    }
    Package::from_files(f).unwrap().validate().unwrap()
}

#[test]
fn baked_bindings_reproduce_key_poses_after_save_and_load() {
    use super::runtime::{Pose, Runtime, PARAMETERS};
    let package = motion_package();
    let runtime = Runtime::new(&package).unwrap();
    let converted = package.convert(Options { allow_lossy: true }).unwrap();
    let summary = converted.report.baking.as_ref().unwrap();
    assert!(summary.deform_bindings > 10);
    eprintln!("bake summary: {}", serde_json::to_string(summary).unwrap());
    assert!(summary.max_mixed_pose_error_pixels.is_finite());
    let model = Model::from_clm_bytes(&converted.model.to_clm_bytes().unwrap()).unwrap();
    let mut puppet = catchlight_core::puppet::Puppet::new(&model);
    for (layer, l) in package.model.layers.iter().enumerate() {
        let (_, points) = super::bake::mesh(l, package.model.canvas);
        let vertices = runtime.prepare(layer, &points).unwrap();
        let idx = puppet
            .node_idx(&NodeId::new(format!("layer-{layer}/frame-0")).unwrap())
            .unwrap();
        let mut poses = vec![Pose::default()];
        for x in [-30., -15., 0., 15., 30.] {
            for y in [-30., 0., 30.] {
                let mut p = Pose::default();
                p.0[0] = x;
                p.0[1] = y;
                poses.push(p);
            }
        }
        for (i, parameter) in PARAMETERS.iter().enumerate().skip(2) {
            let mut p = Pose::default();
            p.0[i] = parameter.2;
            poses.push(p);
        }
        let mut mouth = Pose {
            1: 2,
            ..Pose::default()
        };
        mouth.0[12] = 1.;
        poses.push(mouth);
        for pose in poses {
            for (i, (name, _, _, _)) in PARAMETERS.iter().enumerate() {
                puppet.set_param_value(&ParamId::new(*name).unwrap(), pose.0[i] as f32);
            }
            puppet.set_param_value(&ParamId::new("mouthFrame/mouth").unwrap(), pose.1 as f32);
            puppet.tick(&model, 0.);
            let deform = puppet.combined_deform(idx).unwrap();
            for (i, v) in vertices.iter().enumerate() {
                let expected = runtime.position(layer, v, pose);
                let actual = [
                    v.rest[0] + deform[i].x as f64,
                    v.rest[1] - deform[i].y as f64,
                ];
                assert!(
                    (expected[0] - actual[0]).hypot(expected[1] - actual[1]) < 0.0001,
                    "layer {layer}, pose {pose:?}, vertex {i}: {actual:?} != {expected:?}"
                );
            }
        }
    }
    // Closed lash opacity is driven, rather than permanently zero.
    puppet.set_param_value(&ParamId::new("eyeLOpen").unwrap(), 0.);
    puppet.tick(&model, 0.);
    let lid = puppet
        .node_idx(&NodeId::new("layer-7/frame-0").unwrap())
        .unwrap();
    assert_eq!(puppet.opacity(lid), Some(1.));
    let eye = puppet
        .node_idx(&NodeId::new("layer-5/frame-0").unwrap())
        .unwrap();
    assert_eq!(puppet.opacity(eye), Some(0.));
}

#[test]
fn keyform_fields_are_baked_at_source_poses_and_hermite_between_them() {
    use super::runtime::{Pose, Runtime};
    use base64::Engine;
    let mut f = files();
    let field = |dx: i16| {
        let bytes: Vec<_> = (0..4)
            .flat_map(|_| [dx.to_le_bytes(), 0i16.to_le_bytes()].concat())
            .collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    };
    change(&mut f, |m| m["files"] = json!({"keyforms":"keyforms.json"}));
    f.insert("keyforms.json".into(),serde_json::to_vec(&json!({"version":1,"cell":32,"cols":2,"rows":2,"units":1,"turn":1,"nod":1,"interp":"hermite","keys":[{"param":"angleX","value":30,"fields":{"head":field(6)}},{"param":"angleX","value":-30,"fields":{"head":field(-2)}}]})).unwrap());
    let package = Package::from_files(f).unwrap().validate().unwrap();
    let runtime = Runtime::new(&package).unwrap();
    let points = vec![[16., 12.]];
    let vertices = runtime.prepare(0, &points).unwrap();
    for (angle, dx) in [(30., 6.), (-30., -2.), (0., 0.)] {
        let mut p = Pose::default();
        p.0[0] = angle;
        let out = runtime.position(0, &vertices[0], p);
        assert!((out[0] - 16. - dx).abs() < 1e-5);
        assert!((out[1] - 12.).abs() < 1e-5);
    }
    let model = package
        .convert(Options { allow_lossy: true })
        .unwrap()
        .model;
    assert!(model
        .to_clm_file()
        .unwrap()
        .doc
        .bindings
        .iter()
        .any(|b| matches!(
            &b.values,
            catchlight_core::formats::clm::ClmBindingValues::Deform(_)
        )));
}

#[test]
#[ignore = "requires pinned LivLiv player checkout and Node 24; set LIVLIV_PLAYER_DIR"]
fn motion_evaluator_matches_upstream_cpu_runtime() {
    use super::runtime::{Pose, Runtime, PARAMETERS};
    use std::process::{Command, Stdio};
    let upstream = std::env::var("LIVLIV_PLAYER_DIR").expect("set LIVLIV_PLAYER_DIR");
    let revision = Command::new("git")
        .args(["-C", &upstream, "rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(revision.status.success());
    assert_eq!(
        String::from_utf8(revision.stdout).unwrap().trim(),
        UPSTREAM_REVISION
    );

    let mut poses = vec![Pose::default()];
    for (axis, parameter) in PARAMETERS.iter().enumerate() {
        for value in [parameter.1, parameter.2] {
            let mut p = Pose::default();
            p.0[axis] = value;
            poses.push(p);
        }
    }
    let mut mixed = Pose::default();
    mixed.0[0] = 19.;
    mixed.0[1] = -13.;
    mixed.0[2] = 21.;
    mixed.0[6] = 0.35;
    mixed.0[8] = 0.6;
    mixed.0[10] = -0.4;
    poses.push(mixed);
    let mut mouth = Pose {
        1: 2,
        ..Pose::default()
    };
    mouth.0[12] = 0.75;
    poses.push(mouth);
    let specs:Vec<_>=poses.iter().map(|p|json!({"params":PARAMETERS.iter().enumerate().map(|(i,(name,_,_,_))|((*name).into(),json!(p.0[i]))).collect::<serde_json::Map<_,_>>(),"frame":p.1})).collect();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/livliv-oracle.mjs");
    for package in [motion_package(), motion_keyform_package()] {
        let runtime = Runtime::new(&package).unwrap();
        let keys =
            package.model.files.get("keyforms").map(|name| {
                serde_json::from_slice::<Value>(package.resource(name).unwrap()).unwrap()
            });
        let mut child = Command::new("node")
            .args(["--import", &format!("{upstream}/test/ts.mjs")])
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                &serde_json::to_vec(
                    &json!({"manifest":package.manifest(),"poses":specs,"keyforms":keys}),
                )
                .unwrap(),
            )
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let samples: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut max_error = 0f64;
        for (case, pose) in poses.iter().copied().enumerate() {
            for layer in 0..runtime.layers.len() {
                let data = &samples[case][layer];
                let points: Vec<[f64; 2]> = data["rest"]
                    .as_array()
                    .unwrap()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()])
                    .collect();
                let vertices = runtime.prepare(layer, &points).unwrap();
                for (i, v) in vertices.iter().enumerate() {
                    let actual = runtime.position(layer, v, pose);
                    let expected = [
                        data["out"][i * 2].as_f64().unwrap(),
                        data["out"][i * 2 + 1].as_f64().unwrap(),
                    ];
                    let error = (actual[0] - expected[0]).hypot(actual[1] - expected[1]);
                    max_error = max_error.max(error);
                    assert!(error<0.0001,"case {case}, layer {layer}, point {:?}: {actual:?} != {expected:?}, error {error}",v.rest);
                }
            }
        }
        eprintln!("upstream CPU evaluator max error: {max_error} px");
    }
}

#[test]
#[ignore = "requires official sample directory; set LIVLIV_SAMPLE_DIR"]
fn official_sample_motion_tables_fit_the_bake_budget() {
    // Exercise geometry baking without emitting a model or bypassing the
    // package's watermark blocker. No official images leave the package.
    let path = std::env::var_os("LIVLIV_SAMPLE_DIR").expect("set LIVLIV_SAMPLE_DIR");
    let package = Package::from_directory(std::path::Path::new(&path))
        .unwrap()
        .validate()
        .unwrap();
    let runtime = super::runtime::Runtime::new(&package).unwrap();
    let mut summary = super::bake::BakeSummary::default();
    let mut doc = catchlight_core::formats::clm::ClmStructure::default();
    for (i, l) in package.model.layers.iter().enumerate() {
        let (_, points) = super::bake::mesh(l, package.model.canvas);
        let copies = if l.role.as_deref() == Some("mouth") {
            l.frames.len()
        } else {
            1
        } + usize::from(l.mask.is_some());
        let nodes: Vec<_> = (0..copies)
            .map(|j| NodeId::new(format!("layer-{i}/frame-{j}")).unwrap())
            .collect();
        super::bake::layer(&runtime, i, &points, &nodes, &mut doc, &mut summary).unwrap();
        doc.bindings.clear();
    }
    assert!(summary.deform_bindings > 0);
    assert!(summary.max_mixed_pose_error_pixels.is_finite());
    eprintln!(
        "official sample bake: {}",
        serde_json::to_string(&summary).unwrap()
    );
    assert!(package.convert(Options { allow_lossy: true }).is_err());
}

fn motion_keyform_package() -> ValidatedPackage {
    use base64::Engine;
    let mut files = motion_package().package.files;
    change(&mut files, |m| {
        m["files"] = json!({"keyforms":"keyforms.json"});
        m["tuning"] = json!({"turn":0.8,"nod":1.2});
    });
    let field = |direction: i16| {
        let bytes: Vec<u8> = (0..9)
            .flat_map(|i| {
                [
                    (direction * (i + 1) * 3).to_le_bytes(),
                    (direction * (i + 2)).to_le_bytes(),
                ]
                .concat()
            })
            .collect();
        base64::engine::general_purpose::STANDARD.encode(bytes)
    };
    let mask = base64::engine::general_purpose::STANDARD
        .encode([255u8, 220, 180, 150, 120, 100, 80, 60, 40]);
    let keys = json!({"version":1,"cell":48,"cols":3,"rows":3,"units":0.1,"turn":1.3,"nod":0.8,"interp":"hermite","mask":{"head":mask},"keys":[
        {"param":"angleX","value":30,"fields":{"head":field(1),"hair":field(2),"face":field(3)},"same":["torso"]},
        {"param":"angleX","value":-30,"fields":{"head":field(-1),"hair":field(-2)},"same":["torso"]},
        {"param":"angleY","value":30,"fields":{"head":field(2),"hair":field(1)},"same":["torso"]},
        {"param":"angleY","value":-30,"fields":{"head":field(-2),"hair":field(-1)},"same":["torso"]},
        {"param":"diag","value":0,"fields":{"head":field(4)}}
    ]});
    files.insert("keyforms.json".into(), serde_json::to_vec(&keys).unwrap());
    Package::from_files(files).unwrap().validate().unwrap()
}

#[test]
fn bake_limits_are_checked_before_allocating_tables() {
    let package = validated();
    let runtime = super::runtime::Runtime::new(&package).unwrap();
    let mut summary = super::bake::BakeSummary {
        stored_offset_components: 32_000_000,
        ..Default::default()
    };
    let mut doc = catchlight_core::formats::clm::ClmStructure::default();
    let error = super::bake::layer(
        &runtime,
        0,
        &[[16., 12.]],
        &[NodeId::new("test").unwrap()],
        &mut doc,
        &mut summary,
    )
    .unwrap_err();
    assert!(error.to_string().contains("offset components"));
    assert!(doc.bindings.is_empty());
}

#[test]
fn baked_combined_controls_are_evaluated_by_the_normal_puppet() {
    use super::runtime::{Pose, Runtime, PARAMETERS};
    let package = motion_package();
    let runtime = Runtime::new(&package).unwrap();
    let model = package
        .convert(Options { allow_lossy: true })
        .unwrap()
        .model;
    let model = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let mut puppet = catchlight_core::puppet::Puppet::new(&model);
    let mut pose = Pose::default();
    pose.0[0] = 19.;
    pose.0[1] = -13.;
    pose.0[2] = 21.;
    pose.0[6] = 0.35;
    pose.0[8] = 0.6;
    pose.0[10] = -0.4;
    for (i, (id, _, _, _)) in PARAMETERS.iter().enumerate() {
        puppet.set_param_value(&ParamId::new(*id).unwrap(), pose.0[i] as f32);
    }
    puppet.tick(&model, 0.);
    let mut max_error = 0f64;
    for (layer, l) in package.model.layers.iter().enumerate() {
        let (_, points) = super::bake::mesh(l, package.model.canvas);
        let vertices = runtime.prepare(layer, &points).unwrap();
        let idx = puppet
            .node_idx(&NodeId::new(format!("layer-{layer}/frame-0")).unwrap())
            .unwrap();
        let deforms = puppet.combined_deform(idx).unwrap();
        for (v, d) in vertices.iter().zip(deforms) {
            let expected = runtime.position(layer, v, pose);
            max_error = max_error.max(
                (v.rest[0] + d.x as f64 - expected[0]).hypot(v.rest[1] - d.y as f64 - expected[1]),
            );
        }
    }
    assert!(max_error < 1., "mixed-pose error {max_error} px");
    eprintln!("native puppet held-out mixed-pose error: {max_error} px");
}
