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
    assert_eq!(face.mesh.verts, [-14., 12., -10., 12., -14., 8., -10., 8.]);
    assert_eq!(face.mesh.uvs, [0., 0., 1., 0., 0., 1., 1., 1.]);
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
