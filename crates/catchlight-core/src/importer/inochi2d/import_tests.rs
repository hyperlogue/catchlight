use super::from_inx_model;
use crate::components::*;
use crate::formats::InxModel;
use crate::puppet::Puppet;
use std::io::Cursor;

fn parse_node_json(json: &str) -> Puppet {
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    let mut puppet = Puppet::new();
    let root = puppet.root();
    super::convert::test_support::load_subtree(&value, root, &mut puppet).expect("load_subtree");
    puppet
}

#[test]
fn test_load_simple_model() {
    let json_payload = r#"{
        "nodes": {
            "uuid": 1,
            "name": "Root",
            "type": "Node",
            "transform": {
                "trans": [0.0, 0.0, 0.0],
                "rot": [0.0, 0.0, 0.0],
                "scale": [1.0, 1.0]
            },
            "children": []
        }
    }"#;

    let magic = b"TRNSRTS\0";
    let payload_bytes = json_payload.as_bytes();
    let payload_len = (payload_bytes.len() as u32).to_be_bytes();
    let tex_sect = b"TEX_SECT";
    let tex_count = 0u32.to_be_bytes();

    let mut data = Vec::new();
    data.extend_from_slice(magic);
    data.extend_from_slice(&payload_len);
    data.extend_from_slice(payload_bytes);
    data.extend_from_slice(tex_sect);
    data.extend_from_slice(&tex_count);

    let model = InxModel::parse(Cursor::new(data)).unwrap();
    let puppet = from_inx_model(&model).expect("import");

    assert_eq!(puppet.len(), 2);
}

#[test]
fn mask_threshold_parses_when_present_and_defaults_to_half() {
    let with_threshold = parse_node_json(
        r#"{
            "type": "Part",
            "textures": [0],
            "mesh": {"verts":[], "uvs":[], "indices":[]},
            "mask_threshold": 0.25,
            "masks": [{"source": 7, "mode": "Mask"}]
        }"#,
    );
    let part = with_threshold
        .iter()
        .find_map(|(_, n)| match &n.kind {
            NodeKind::Part(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(part.mask_threshold, 0.25);

    let without = parse_node_json(
        r#"{
            "type": "Part",
            "textures": [0],
            "mesh": {"verts":[], "uvs":[], "indices":[]},
            "masks": [{"source": 7, "mode": "Mask"}]
        }"#,
    );
    let part = without
        .iter()
        .find_map(|(_, n)| match &n.kind {
            NodeKind::Part(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(part.mask_threshold, 0.5);

    let composite = parse_node_json(
        r#"{
            "type": "Composite",
            "mask_threshold": 0.8,
            "masks": [{"source": 13, "mode": "DodgeMask"}]
        }"#,
    );
    let comp = composite
        .iter()
        .find_map(|(_, n)| match &n.kind {
            NodeKind::Composite(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(comp.mask_threshold, 0.8);
    assert_eq!(comp.masks.len(), 1);
    assert_eq!(comp.masks[0].source_uuid, 13);
    assert_eq!(comp.masks[0].mode, MaskMode::DodgeMask);
}

#[test]
fn mask_with_missing_source_is_dropped() {
    let puppet = parse_node_json(
        r#"{
            "uuid": 42,
            "name": "P",
            "type": "Part",
            "transform": {"trans":[0,0,0], "rot":[0,0,0], "scale":[1,1]},
            "textures": [0],
            "mesh": {"verts":[], "uvs":[], "indices":[]},
            "masks": [
                {"mode": "Mask"},
                {"source": 7, "mode": "Mask"}
            ]
        }"#,
    );

    let node = puppet
        .iter()
        .find_map(|(_, n)| if n.name == "P" { Some(n) } else { None })
        .unwrap();
    let NodeKind::Part(part) = &node.kind else {
        panic!("expected Part");
    };
    assert_eq!(part.masks.len(), 1);
    assert_eq!(part.masks[0].source_uuid, 7);
}

#[test]
fn duplicate_uuid_does_not_shadow_first() {
    let puppet = parse_node_json(
        r#"{
            "uuid": 99,
            "name": "First",
            "type": "Node",
            "transform": {"trans":[0,0,0], "rot":[0,0,0], "scale":[1,1]},
            "children": [
                {
                    "uuid": 99,
                    "name": "SecondWithSameUuid",
                    "type": "Node",
                    "transform": {"trans":[0,0,0], "rot":[0,0,0], "scale":[1,1]}
                }
            ]
        }"#,
    );

    let mapped = puppet.node_for_uuid(99).unwrap();
    assert_eq!(puppet.get(mapped).unwrap().name, "First");
}

#[test]
fn load_deeply_nested_tree_does_not_overflow() {
    const DEPTH: usize = 5_000;

    fn leaf() -> serde_json::Value {
        serde_json::Value::Object(serde_json::Map::new())
    }
    fn wrap(child: serde_json::Value) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), serde_json::Value::String("Node".into()));
        obj.insert("children".into(), serde_json::Value::Array(vec![child]));
        serde_json::Value::Object(obj)
    }

    let mut value = leaf();
    for _ in 0..DEPTH {
        value = wrap(value);
    }

    let mut puppet = Puppet::new();
    let root = puppet.root();
    super::convert::test_support::load_subtree(&value, root, &mut puppet).expect("load_subtree");
    assert_eq!(puppet.len(), DEPTH + 2);
    // `serde_json::Value`'s Drop is recursive, so dropping a 5_000-deep
    // value overflows the stack. Leaking it here is deliberate: the test
    // is about `load_subtree`, not about teardown.
    std::mem::forget(value);
}

#[test]
fn empty_object_loads_as_single_empty_node() {
    let puppet = parse_node_json("{}");
    assert_eq!(puppet.len(), 2);
}

#[test]
fn missing_transform_defaults_to_identity() {
    let puppet = parse_node_json(r#"{"type":"Node"}"#);
    let (_, node) = puppet.iter().find(|(id, _)| *id != puppet.root()).unwrap();
    assert_eq!(node.transform.translation, glam::Vec3::ZERO);
    assert_eq!(node.transform.scale, glam::Vec2::ONE);
}

#[test]
fn wrong_type_fields_fall_through_to_defaults() {
    let puppet = parse_node_json(
        r#"{
            "type": "Composite",
            "enabled": "yes please",
            "zsort": [1, 2, 3],
            "tint": {"r": 1.0},
            "opacity": false
        }"#,
    );
    let (_, node) = puppet.iter().find(|(id, _)| *id != puppet.root()).unwrap();
    assert!(node.enabled);
    assert_eq!(node.z_order, 0.0);
    let NodeKind::Composite(comp) = &node.kind else {
        panic!("expected Composite");
    };
    assert_eq!(comp.tint, glam::Vec3::ONE);
    assert_eq!(comp.opacity, 1.0);
}

#[test]
fn unknown_type_becomes_empty() {
    let puppet = parse_node_json(r#"{"type":"NotARealNodeType"}"#);
    let (_, node) = puppet.iter().find(|(id, _)| *id != puppet.root()).unwrap();
    assert!(matches!(node.kind, NodeKind::Group));
}

#[test]
fn non_array_children_is_ignored() {
    let puppet = parse_node_json(
        r#"{
            "type": "Node",
            "children": "not an array"
        }"#,
    );
    assert_eq!(puppet.len(), 2);
}

#[test]
fn part_with_invalid_texture_index_still_loads() {
    let puppet = parse_node_json(
        r#"{
            "type": "Part",
            "textures": [999999],
            "mesh": {"verts":[0.0,0.0], "uvs":[0.0,0.0], "indices":[0]}
        }"#,
    );
    let (_, node) = puppet.iter().find(|(id, _)| *id != puppet.root()).unwrap();
    let NodeKind::Part(part) = &node.kind else {
        panic!("expected Part");
    };
    assert_eq!(part.albedo_texture, TextureId(999999));
}

#[test]
fn mesh_with_odd_vertex_coordinate_count_drops_trailing_entry() {
    let puppet = parse_node_json(
        r#"{
            "type": "Part",
            "textures": [0],
            "mesh": {
                "verts": [0.0, 1.0, 2.0, 3.0, 4.0],
                "uvs": [0.0, 0.0, 1.0, 1.0],
                "indices": [0, 1]
            }
        }"#,
    );
    let (_, node) = puppet.iter().find(|(id, _)| *id != puppet.root()).unwrap();
    let NodeKind::Part(part) = &node.kind else {
        panic!("expected Part");
    };
    assert_eq!(part.mesh.vertices.len(), 2);
}

fn try_parse_node_json(json: &str) -> Result<Puppet, super::ImportError> {
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    let mut puppet = Puppet::new();
    let root = puppet.root();
    super::convert::test_support::load_subtree(&value, root, &mut puppet)?;
    Ok(puppet)
}

#[test]
fn mesh_with_out_of_range_index_is_rejected() {
    // Index 5 references a vertex that doesn't exist (only 2). The
    // importer must reject rather than bake an index the per-frame
    // deform path would then have to defend against.
    let err = try_parse_node_json(
        r#"{
            "type": "Part",
            "textures": [0],
            "mesh": {"verts":[0.0,0.0,1.0,1.0], "uvs":[0.0,0.0,1.0,1.0], "indices":[0,1,5]}
        }"#,
    )
    .err()
    .expect("expected rejection");
    assert!(err.to_string().contains("out of range"), "got: {err}");
}

#[test]
fn mesh_with_mismatched_uv_count_is_rejected() {
    // 3 vertices but only 1 uv: the per-vertex uv pairing would be
    // ambiguous, so the importer rejects it at the edge.
    let err = try_parse_node_json(
        r#"{
            "type": "Part",
            "textures": [0],
            "mesh": {"verts":[0.0,0.0,1.0,1.0,2.0,2.0], "uvs":[0.0,0.0], "indices":[0,1,2]}
        }"#,
    )
    .err()
    .expect("expected rejection");
    assert!(err.to_string().contains("uvs"), "got: {err}");
}
