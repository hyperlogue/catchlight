//! What the `.inx` reader does with a hostile or sloppy node tree.
//!
//! Every case here is a shape a real export has produced or could: a mask
//! naming a node that is not there, two nodes sharing a uuid, a field of the
//! wrong JSON type, a node type catchlight does not model. The reader's
//! contract is that none of them is an error — the node loads with the
//! offending part dropped or defaulted — and these pin which half that is.

use super::to_legacy::from_inx_model_to_legacy;
use crate::formats::clm::ClmIndices;
use crate::formats::legacy::{LegacyDocument, LegacyNodeKind};
use crate::formats::InxModel;
use crate::importer::ImportError;
use serde_json::json;

/// Read one node tree, given as the `.inx` payload's `nodes` value.
fn doc(nodes: serde_json::Value) -> LegacyDocument {
    try_doc(nodes).expect("import")
}

fn try_doc(nodes: serde_json::Value) -> Result<LegacyDocument, ImportError> {
    let model = InxModel {
        payload: json!({ "nodes": nodes }),
        textures: Vec::new(),
        vendors: Vec::new(),
    };
    from_inx_model_to_legacy(&model).map(|f| f.doc)
}

fn node_named<'a>(doc: &'a LegacyDocument, name: &str) -> &'a crate::formats::legacy::LegacyNode {
    doc.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node named {name}"))
}

#[test]
fn a_whole_container_reads_into_one_node() {
    let payload = r#"{
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

    let mut data = Vec::new();
    data.extend_from_slice(b"TRNSRTS\0");
    data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    data.extend_from_slice(payload.as_bytes());
    data.extend_from_slice(b"TEX_SECT");
    data.extend_from_slice(&0u32.to_be_bytes());

    let model = InxModel::parse(std::io::Cursor::new(data.as_slice())).expect("parse");
    let file = from_inx_model_to_legacy(&model).expect("import");
    assert_eq!(file.doc.nodes.len(), 1);
    assert_eq!(file.doc.nodes[0].name, "Root");
    assert_eq!(file.doc.nodes[0].parent, None);
}

#[test]
fn mask_threshold_parses_when_present_and_defaults_to_half() {
    // The source has to be a node the tree carries, or the mask is dropped.
    let tree = |threshold: serde_json::Value, kind: &str| {
        json!({
            "uuid": 1, "name": "root", "type": "Node",
            "children": [
                {"uuid": 7, "name": "source", "type": "Part", "textures": [0],
                 "mesh": {"verts": [], "uvs": [], "indices": []}},
                {"uuid": 8, "name": "target", "type": kind, "textures": [0],
                 "mesh": {"verts": [], "uvs": [], "indices": []},
                 "mask_threshold": threshold,
                 "masks": [{"source": 7, "mode": "DodgeMask"}]}
            ]
        })
    };

    let d = doc(tree(json!(0.25), "Part"));
    let LegacyNodeKind::Part(part) = &node_named(&d, "target").kind else {
        panic!("expected Part");
    };
    assert_eq!(part.mask_threshold, 0.25);

    let d = doc(tree(serde_json::Value::Null, "Part"));
    let LegacyNodeKind::Part(part) = &node_named(&d, "target").kind else {
        panic!("expected Part");
    };
    assert_eq!(part.mask_threshold, 0.5, "an absent threshold is half");

    let d = doc(tree(json!(0.8), "Composite"));
    let LegacyNodeKind::Composite(comp) = &node_named(&d, "target").kind else {
        panic!("expected Composite");
    };
    assert_eq!(comp.mask_threshold, 0.8);
    assert_eq!(comp.masks.len(), 1);
    // Masks name a node index, and `source` sits at index 1 of the flattening.
    assert_eq!(comp.masks[0].source, 1);
    assert_eq!(comp.masks[0].mode, crate::components::MaskMode::DodgeMask);
}

#[test]
fn a_mask_whose_source_is_not_there_is_dropped() {
    let d = doc(json!({
        "uuid": 1, "name": "root", "type": "Node",
        "children": [
            {"uuid": 7, "name": "source", "type": "Part", "textures": [0],
             "mesh": {"verts": [], "uvs": [], "indices": []}},
            {"uuid": 42, "name": "P", "type": "Part", "textures": [0],
             "mesh": {"verts": [], "uvs": [], "indices": []},
             "masks": [
                {"mode": "Mask"},
                {"source": 999, "mode": "Mask"},
                {"source": 7, "mode": "Mask"}
             ]}
        ]
    }));
    let LegacyNodeKind::Part(part) = &node_named(&d, "P").kind else {
        panic!("expected Part");
    };
    assert_eq!(part.masks.len(), 1, "only the resolvable mask survives");
    assert_eq!(part.masks[0].source, 1);
}

#[test]
fn a_duplicate_uuid_does_not_shadow_the_first_node() {
    let model = InxModel {
        payload: json!({
            "nodes": {
                "uuid": 99, "name": "First", "type": "Node",
                "children": [
                    {"uuid": 99, "name": "SecondWithSameUuid", "type": "Node"}
                ]
            },
            // A reference to 99 has to land on the node that claimed it first.
            "param": [{
                "uuid": 10, "name": "p", "is_vec2": false,
                "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 1.0], [0.0]],
                "bindings": [{"node": 99, "param_name": "opacity", "values": [[0.25], [0.75]]}]
            }]
        }),
        textures: Vec::new(),
        vendors: Vec::new(),
    };
    let doc = from_inx_model_to_legacy(&model).expect("import").doc;
    assert_eq!(doc.nodes.len(), 2);
    assert_eq!(doc.nodes[0].name, "First");
    assert_eq!(
        doc.params[0].bindings[0].node, 0,
        "the binding resolves to the first claimant of uuid 99"
    );
}

#[test]
fn a_deeply_nested_tree_does_not_overflow() {
    const DEPTH: usize = 5_000;

    // Built by hand rather than with `json!`: the macro's temporaries nest as
    // deeply as the value does, and their drop glue overflows a test thread's
    // stack before the reader is even called.
    fn wrap(child: serde_json::Value) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), serde_json::Value::String("Node".into()));
        obj.insert("children".into(), serde_json::Value::Array(vec![child]));
        serde_json::Value::Object(obj)
    }
    let mut value = serde_json::Value::Object(serde_json::Map::new());
    for _ in 0..DEPTH {
        value = wrap(value);
    }
    let mut payload = serde_json::Map::new();
    payload.insert("nodes".into(), value);
    let model = InxModel {
        payload: serde_json::Value::Object(payload),
        textures: Vec::new(),
        vendors: Vec::new(),
    };

    let d = from_inx_model_to_legacy(&model).expect("import").doc;
    assert_eq!(d.nodes.len(), DEPTH + 1);
    for (i, node) in d.nodes.iter().enumerate().skip(1) {
        assert_eq!(node.parent, Some(i as u32 - 1), "node {i}'s parent");
    }
    // `serde_json::Value`'s Drop is recursive, so dropping a 5_000-deep value
    // overflows the stack. Leaking it here is deliberate: the test is about
    // the reader, not about teardown.
    std::mem::forget(model);
}

#[test]
fn an_empty_object_reads_as_one_default_node() {
    let d = doc(json!({}));
    assert_eq!(d.nodes.len(), 1);
    assert_eq!(d.nodes[0].name, "");
    assert!(matches!(d.nodes[0].kind, LegacyNodeKind::Group));
}

#[test]
fn a_missing_transform_defaults_to_identity() {
    let d = doc(json!({"type": "Node"}));
    assert_eq!(d.nodes[0].transform.translation, [0.0, 0.0, 0.0]);
    assert_eq!(d.nodes[0].transform.rotation, [0.0, 0.0, 0.0]);
    assert_eq!(d.nodes[0].transform.scale, [1.0, 1.0]);
}

#[test]
fn fields_of_the_wrong_json_type_fall_through_to_defaults() {
    let d = doc(json!({
        "type": "Composite",
        "enabled": "yes please",
        "zsort": [1, 2, 3],
        "tint": {"r": 1.0},
        "opacity": false
    }));
    assert!(d.nodes[0].enabled);
    assert_eq!(d.nodes[0].z_order, 0.0);
    let LegacyNodeKind::Composite(comp) = &d.nodes[0].kind else {
        panic!("expected Composite");
    };
    assert_eq!(comp.tint, [1.0, 1.0, 1.0]);
    assert_eq!(comp.opacity, 1.0);
}

#[test]
fn an_unmodelled_node_type_becomes_a_group() {
    let d = doc(json!({"type": "NotARealNodeType"}));
    assert!(matches!(d.nodes[0].kind, LegacyNodeKind::Group));
}

#[test]
fn children_that_are_not_an_array_are_ignored() {
    let d = doc(json!({"type": "Node", "children": "not an array"}));
    assert_eq!(d.nodes.len(), 1);
}

#[test]
fn a_part_naming_a_texture_that_is_not_there_still_loads() {
    let d = doc(json!({
        "type": "Part",
        "textures": [999999],
        "mesh": {"verts": [0.0, 0.0], "uvs": [0.0, 0.0], "indices": [0]}
    }));
    let LegacyNodeKind::Part(part) = &d.nodes[0].kind else {
        panic!("expected Part");
    };
    assert_eq!(part.albedo, 999999);
}

#[test]
fn a_mesh_with_an_odd_coordinate_count_keeps_the_pairs_it_has() {
    let d = doc(json!({
        "type": "Part",
        "textures": [0],
        "mesh": {"verts": [0.0, 1.0, 2.0, 3.0, 4.0], "uvs": [0.0, 0.0, 1.0, 1.0], "indices": [0, 1]}
    }));
    let LegacyNodeKind::Part(part) = &d.nodes[0].kind else {
        panic!("expected Part");
    };
    // The trailing lone coordinate is not a vertex; the pairs before it are.
    assert_eq!(part.mesh.verts.len() / 2, 2);
    assert_eq!(part.mesh.indices, ClmIndices::U16(vec![0, 1]));
}
