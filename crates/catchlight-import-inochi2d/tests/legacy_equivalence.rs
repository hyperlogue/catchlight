#![recursion_limit = "512"]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The two import paths write the same `.clm`, byte for byte.
//!
//! **Temporary.** `.inx → legacy document → Model::from_legacy` is the path
//! this crate shipped with; `.inx → .clm document → Model::from_clm_file` is
//! the one that replaces it. There is no `.inx` in the tree to diff the two
//! against, so this runs both over synthetic models covering every field the
//! importer reflects and every reference it resolves, and compares the bytes
//! each writes. It retires with the legacy path.

use catchlight_core::texture::{EncodedTexture, TextureFormat};
use catchlight_core::Model;
use catchlight_import_inochi2d::{from_inx_model_to_legacy, import_inx_model, InxModel};
use serde_json::json;

/// What the legacy path writes for `model`, or `None` if it refuses it.
fn through_legacy(model: &InxModel) -> Option<Vec<u8>> {
    let file = from_inx_model_to_legacy(model).ok()?;
    Model::from_legacy(&file).ok()?.to_clm_bytes().ok()
}

/// What the direct path writes for `model`, or `None` if it refuses it.
fn through_clm(model: &InxModel) -> Option<Vec<u8>> {
    import_inx_model(model).ok()?.to_clm_bytes().ok()
}

fn texture(format: TextureFormat, byte: u8) -> EncodedTexture {
    EncodedTexture {
        format,
        data: vec![byte; 16].into(),
        premultiplied: true,
    }
}

/// A model touching every node kind, every cross-reference and both param
/// shapes: a composite masked by a part that appears *after* it, a part masked
/// by another part, a mesh group, a pendulum aimed at a 2-D param, an
/// unmodelled node type, a duplicate uuid, a dangling mask, a colour binding
/// on a mesh group (dropped), a binding on a node that is not there (dropped),
/// a sparsified deform, and a param nothing binds.
fn kitchen_sink() -> InxModel {
    InxModel {
        payload: json!({
            "physics": {"pixelsPerMeter": 750.0, "gravity": 11.5},
            "nodes": {
                "uuid": 1, "name": "root", "type": "Node",
                "transform": {"trans": [1.0, 2.0, 3.0], "rot": [0.1, 0.2, 0.3], "scale": [1.5, 2.5]},
                "children": [
                    {
                        "uuid": 2, "name": "composite", "type": "Composite",
                        "zsort": 2.5, "opacity": 0.75, "blend_mode": "Multiply",
                        "tint": [0.9, 0.8, 0.7], "screenTint": [0.1, 0.2, 0.3],
                        "mask_threshold": 0.25, "propagate_meshgroup": false,
                        // Forward reference: uuid 5 is flattened after this node.
                        "masks": [{"source": 5, "mode": "DodgeMask"}, {"source": 404, "mode": "Mask"}],
                        "children": [
                            {
                                "uuid": 3, "name": "front", "type": "Part",
                                "zsort": -1.5, "enabled": false, "lockToRoot": true,
                                "textures": [1], "mask_threshold": 0.8,
                                "masks": [{"source": 5, "mode": "Mask"}],
                                "transform": {"trans": [4.0, -5.0, 6.0], "rot": [0.25, 0.5, 0.75], "scale": [2.0, 3.0]},
                                "mesh": {
                                    "verts": [1.0, 2.0, -4.0, 6.0, 8.0, -3.0],
                                    "uvs": [0.0, 0.0, 1.0, 0.25, 0.5, 1.0],
                                    "indices": [0, 1, 2],
                                    "origin": [1.5, 2.5]
                                }
                            }
                        ]
                    },
                    {
                        "uuid": 4, "name": "warp", "type": "MeshGroup",
                        "dynamic_deformation": true, "translate_children": true,
                        "opacity": 0.5, "tint": [0.2, 0.2, 0.2],
                        "mesh": {
                            "verts": [0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0],
                            "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                            "indices": [0, 1, 2, 0, 2, 3],
                            "origin": [0.0, 0.0]
                        }
                    },
                    {
                        "uuid": 5, "name": "back", "type": "Part",
                        "zsort": 7.0, "textures": [0],
                        "mesh": {
                            "verts": [0.0, 0.0, 4.0, -9.0, -7.0, 11.0],
                            "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
                            "indices": [0, 1, 2],
                            "origin": [0.0, -6.0]
                        }
                    },
                    {
                        "uuid": 6, "name": "pendulum", "type": "SimplePhysics",
                        "model_type": "SpringPendulum", "map_mode": "LengthAngle",
                        "local_only": true, "param": 200,
                        "gravity": 2.5, "length": 175.0, "frequency": 3.5,
                        "angle_damping": 0.25, "length_damping": 0.75,
                        "output_scale": [1.5, 0.5]
                    },
                    {"uuid": 7, "name": "camera", "type": "Camera"},
                    {"uuid": 5, "name": "duplicate_uuid", "type": "Node"}
                ]
            },
            "param": [
                {
                    "uuid": 100, "name": "pull", "is_vec2": false,
                    "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.25, 0.0],
                    "axis_points": [[0.0, 0.5, 1.0], [0.0]],
                    "bindings": [
                        {"node": 3, "param_name": "deform", "interpolate_mode": "Cubic",
                         "values": [
                             [[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]],
                             [[[1.0, -2.0], [3.0, -4.0], [5.0, -6.0]]],
                             [[[2.0, -4.0], [6.0, -8.0], [10.0, -12.0]]]
                         ],
                         "isSet": [[true], [false], [true]]},
                        {"node": 4, "param_name": "opacity", "values": [[0.5], [0.5], [0.5]]},
                        {"node": 999, "param_name": "opacity", "values": [[0.5], [0.5], [0.5]]}
                    ]
                },
                {
                    "uuid": 200, "name": "sway", "is_vec2": true,
                    "min": [-1.0, -2.0], "max": [1.0, 2.0], "defaults": [0.0, 0.5],
                    "axis_points": [[0.0, 1.0], [0.0, 0.5, 1.0]],
                    "bindings": [
                        {"node": 3, "param_name": "transform.t.y", "interpolate_mode": "Stepped",
                         "values": [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]},
                        {"node": 5, "param_name": "zSort",
                         "values": [[0.0, -1.0, -2.0], [1.0, 2.0, 3.0]]},
                        {"node": 5, "param_name": "transform.s.x", "interpolate_mode": "Nearest",
                         "values": [[1.0, 1.5, 2.0], [0.5, 0.75, 1.0]]}
                    ]
                },
                {
                    "uuid": 300, "name": "unbound", "is_vec2": false,
                    "min": [0.0, 0.0], "max": [10.0, 0.0], "defaults": [5.0, 0.0],
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": []
                }
            ]
        }),
        textures: vec![
            texture(TextureFormat::Png, 0xAB),
            texture(TextureFormat::Tga, 0xCD),
        ],
        vendors: Vec::new(),
    }
}

/// A tree with no `param` section, no textures on its parts and defaults
/// everywhere — the shape the reader's tolerance tests use.
fn sparse_tree(nodes: serde_json::Value) -> InxModel {
    InxModel {
        payload: json!({ "nodes": nodes }),
        textures: vec![texture(TextureFormat::Png, 0x01)],
        vendors: Vec::new(),
    }
}

fn tolerance_fixtures() -> Vec<(&'static str, InxModel)> {
    vec![
        ("empty_object", sparse_tree(json!({}))),
        ("no_transform", sparse_tree(json!({"type": "Node"}))),
        (
            "unmodelled_type",
            sparse_tree(json!({"type": "NotARealNodeType"})),
        ),
        (
            "children_not_an_array",
            sparse_tree(json!({"type": "Node", "children": "not an array"})),
        ),
        (
            "wrong_json_types",
            sparse_tree(json!({
                "type": "Composite", "enabled": "yes please", "zsort": [1, 2, 3],
                "tint": {"r": 1.0}, "opacity": false
            })),
        ),
        (
            "odd_vertex_count",
            sparse_tree(json!({
                "type": "Part", "textures": [0],
                "mesh": {"verts": [0.0, 1.0, 2.0, 3.0, 4.0], "uvs": [0.0, 0.0, 1.0, 1.0], "indices": [0, 1]}
            })),
        ),
        (
            "dangling_and_resolvable_masks",
            sparse_tree(json!({
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
            })),
        ),
        (
            "part_naming_a_texture_that_is_not_there",
            sparse_tree(json!({
                "type": "Part", "textures": [999999],
                "mesh": {"verts": [0.0, 0.0], "uvs": [0.0, 0.0], "indices": [0]}
            })),
        ),
        (
            "duplicate_uuid_binding",
            InxModel {
                payload: json!({
                    "nodes": {
                        "uuid": 99, "name": "First", "type": "Node",
                        "children": [{"uuid": 99, "name": "SecondWithSameUuid", "type": "Node"}]
                    },
                    "param": [{
                        "uuid": 10, "name": "p", "is_vec2": false,
                        "min": [0.0, 0.0], "max": [1.0, 1.0], "defaults": [0.0, 0.0],
                        "axis_points": [[0.0, 1.0], [0.0]],
                        "bindings": [{"node": 99, "param_name": "opacity", "values": [[0.25], [0.75]]}]
                    }]
                }),
                textures: vec![texture(TextureFormat::Png, 0x02)],
                vendors: Vec::new(),
            },
        ),
    ]
}

#[test]
fn both_import_paths_write_the_same_clm() {
    let mut compared = 0;
    let mut fixtures = vec![("kitchen_sink", kitchen_sink())];
    fixtures.extend(tolerance_fixtures());
    for (name, model) in fixtures {
        let legacy = through_legacy(&model);
        let direct = through_clm(&model);
        assert_eq!(
            legacy.is_some(),
            direct.is_some(),
            "{name}: the two paths disagree about whether this model imports"
        );
        if let (Some(legacy), Some(direct)) = (legacy, direct) {
            assert_eq!(
                legacy, direct,
                "{name}: the two import paths wrote different .clm bytes"
            );
            compared += 1;
        }
    }
    assert!(
        compared >= 9,
        "only {compared} fixtures produced bytes to compare"
    );
}

/// The kitchen sink is the fixture the comparison above leans on, so pin that
/// it really does exercise the whole document rather than erroring early.
#[test]
fn the_kitchen_sink_imports_into_a_model_worth_comparing() {
    let model = import_inx_model(&kitchen_sink()).expect("import");
    assert_eq!(model.node_count(), 8);
    assert_eq!(model.param_ids().len(), 4, "one 2-D param split in two");
    assert_eq!(model.texture_ids().len(), 2);
    // The colour binding on the mesh group and the one on a node that is not
    // there are both dropped; four of the six survive.
    assert_eq!(model.bindings().count(), 4);
}
