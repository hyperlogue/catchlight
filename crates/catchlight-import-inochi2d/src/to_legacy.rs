//! One-time `.inx → legacy document` import: turn an inochi2d puppet into the
//! arena a [`Model`](catchlight_core::Model) is built from, which
//! `cargo xtask import` then writes as a `.clm`.
//!
//! The inx node *tree* is flattened (DFS pre-order) into the arena: each node
//! lands at a sequential index with its `parent` index recorded, so the result
//! is topologically ordered (`parent < self`). inochi's globally-unique
//! `uuid`s are resolved to those indices — `binding.node` / `mask.source` to a
//! node index, `SimplePhysics.target_param` to a param index — and then
//! dropped; the array position is the only identity the arena has, and the
//! Ids a Model mints from it are what the file stores. References that don't
//! resolve are dropped; duplicate uuids collapse to the first occurrence, so a
//! reference always names the node that claimed the uuid first.
//!
//! Like [`super::convert`], this keeps only what catchlight models and drops the
//! rest (meta, groups, automation, animations, cameras, emissive/bump slots,
//! emissionStrength). Textures are kept verbatim; the build crops them.

use std::collections::HashMap;

use crate::inx::InxModel;
use catchlight_core::formats::clm::{ClmPhysics, TextureAlpha, TextureEncoding};
use catchlight_core::formats::legacy::{
    LegacyBinding, LegacyComposite, LegacyDocument, LegacyFile, LegacyMask, LegacyMeshGroup,
    LegacyNode, LegacyNodeKind, LegacyParam, LegacyPart, LegacySimplePhysics, LegacyTexture,
};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::texture::TextureFormat;

use crate::error::ImportError;
use crate::reflect::{
    axes_of, blend, convert_binding_values, convert_mesh, convert_transform, flatten, interp,
    mask_mode, reflect_z, vec2_arr, vec3_arr,
};
use crate::schema::{
    source_binding_is_color, SchemaBinding, SchemaMask, SchemaNode, SchemaParam,
    SchemaPuppetPhysics,
};

/// Convert a parsed `.inx` model into an editable [`LegacyFile`].
pub fn from_inx_model_to_legacy(model: &InxModel) -> Result<LegacyFile, ImportError> {
    let obj = model
        .payload
        .as_object()
        .ok_or_else(|| ImportError::MalformedPayload("inx payload root is not an object".into()))?;

    let physics = obj
        .get("physics")
        .and_then(|v| serde_json::from_value::<SchemaPuppetPhysics>(v.clone()).ok())
        .map(|p| ClmPhysics {
            pixels_per_meter: p.pixels_per_meter.unwrap_or(1000.0),
            gravity: p.gravity.unwrap_or(9.8),
        })
        .unwrap_or_default();

    // Flatten the node tree, recording each node's parent index and a
    // uuid → index map for resolving cross-references.
    let mut flat: Vec<(SchemaNode, Option<u32>)> = Vec::new();
    let mut node_index: HashMap<u32, u32> = HashMap::new();
    match obj.get("nodes") {
        Some(n) => flatten(n, None, &mut flat, &mut node_index),
        None => flat.push((SchemaNode::default(), None)),
    }

    // Params keep their array order; build a uuid → index map (first-wins, which
    // first-wins on a duplicate, like the node map).
    let schema_params: Vec<SchemaParam> = obj
        .get("param")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| serde_json::from_value::<SchemaParam>(p.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut param_index: HashMap<u32, u32> = HashMap::new();
    for (i, p) in schema_params.iter().enumerate() {
        if let Some(uuid) = p.uuid {
            param_index.entry(uuid).or_insert(i as u32);
        }
    }

    let nodes = flat
        .iter()
        .map(|(s, parent)| convert_node(s, *parent, &node_index, &param_index))
        .collect::<Result<Vec<_>, ImportError>>()?;
    let params = schema_params
        .iter()
        .map(|p| convert_param(p, &node_index, &nodes))
        .collect();

    let textures = model
        .textures
        .iter()
        .map(|t| LegacyTexture {
            encoding: match t.format {
                TextureFormat::Png => TextureEncoding::Png,
                TextureFormat::Tga => TextureEncoding::Tga,
            },
            alpha: TextureAlpha::PremultipliedSrgb,
            data: t.data.to_vec(),
        })
        .collect();

    Ok(LegacyFile {
        doc: LegacyDocument {
            physics,
            nodes,
            params,
            welds: Vec::new(),
        },
        textures,
    })
}

fn convert_node(
    s: &SchemaNode,
    parent: Option<u32>,
    node_index: &HashMap<u32, u32>,
    param_index: &HashMap<u32, u32>,
) -> Result<LegacyNode, ImportError> {
    Ok(LegacyNode {
        parent,
        name: s.name.clone().unwrap_or_default(),
        enabled: s.enabled.unwrap_or(true),
        z_order: reflect_z(s.zsort.unwrap_or(0.0)),
        transform: convert_transform(s.transform.as_ref()),
        lock_to_root: s.lock_to_root.unwrap_or(false),
        kind: convert_node_kind(s, node_index, param_index)?,
    })
}

fn convert_node_kind(
    s: &SchemaNode,
    node_index: &HashMap<u32, u32>,
    param_index: &HashMap<u32, u32>,
) -> Result<LegacyNodeKind, ImportError> {
    Ok(match s.ty.as_deref().unwrap_or("") {
        "Part" => LegacyNodeKind::Part(convert_part(s, node_index)?),
        "Composite" => LegacyNodeKind::Composite(convert_composite(s, node_index)?),
        "MeshGroup" => LegacyNodeKind::MeshGroup(convert_mesh_group(s)),
        "SimplePhysics" => LegacyNodeKind::SimplePhysics(convert_simple_physics(s, param_index)),
        // Node, Camera, and any unmodeled type all become a container Group.
        _ => LegacyNodeKind::Group,
    })
}

fn convert_masks(masks: &[SchemaMask], node_index: &HashMap<u32, u32>) -> Vec<LegacyMask> {
    masks
        .iter()
        .filter_map(|m| {
            // Drop masks whose source node doesn't resolve: `.inx` is untrusted
            // and a dangling mask has nothing to clip against.
            let source = *node_index.get(&m.source?)?;
            Some(LegacyMask {
                source,
                mode: mask_mode(m.mode.as_deref()),
            })
        })
        .collect()
}

fn convert_part(s: &SchemaNode, node_index: &HashMap<u32, u32>) -> Result<LegacyPart, ImportError> {
    let albedo = match s.textures.first() {
        None => 0,
        Some(&v) => u32::try_from(v).unwrap_or(u32::MAX),
    };
    Ok(LegacyPart {
        mesh: convert_mesh(s.mesh.as_ref()),
        albedo,
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), node_index),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
    })
}

fn convert_composite(
    s: &SchemaNode,
    node_index: &HashMap<u32, u32>,
) -> Result<LegacyComposite, ImportError> {
    Ok(LegacyComposite {
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), node_index),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
        propagate_meshgroup: s.propagate_meshgroup.unwrap_or(true),
    })
}

fn convert_mesh_group(s: &SchemaNode) -> LegacyMeshGroup {
    s.log_dropped_mesh_group_color();
    LegacyMeshGroup {
        mesh: convert_mesh(s.mesh.as_ref()),
        dynamic: s.dynamic_deformation.unwrap_or(false),
        translate_children: s.translate_children.unwrap_or(false),
    }
}

fn convert_simple_physics(s: &SchemaNode, param_index: &HashMap<u32, u32>) -> LegacySimplePhysics {
    LegacySimplePhysics {
        kind: s
            .model_type
            .as_deref()
            .and_then(PendulumKind::from_str)
            .unwrap_or_default(),
        map_mode: s
            .map_mode
            .as_deref()
            .and_then(PhysicsParamMapMode::from_str)
            .unwrap_or_default(),
        local_only: s.local_only.unwrap_or(false),
        target_param: s.param.and_then(|uuid| param_index.get(&uuid).copied()),
        // Authored, unscaled — the global g-scale fold is a build step.
        gravity: s.gravity.unwrap_or(1.0),
        length: s.length.unwrap_or(100.0),
        frequency: s.frequency.unwrap_or(1.0),
        angle_damping: s.angle_damping.unwrap_or(0.5),
        length_damping: s.length_damping.unwrap_or(0.5),
        output_scale: vec2_arr(&s.output_scale, [1.0, 1.0]),
    }
}

fn convert_param(
    p: &SchemaParam,
    node_index: &HashMap<u32, u32>,
    nodes: &[LegacyNode],
) -> LegacyParam {
    let is_vec2 = p.is_vec2.unwrap_or(false);
    let (axis_x, axis_y) = axes_of(p);
    let bindings = p
        .bindings
        .iter()
        .filter_map(|b| convert_binding(b, node_index, nodes, &axis_x, &axis_y))
        .collect();
    LegacyParam {
        name: p.name.clone().unwrap_or_default(),
        is_vec2,
        min: vec2_arr(&p.min, [0.0, 0.0]),
        max: vec2_arr(&p.max, [1.0, 1.0]),
        defaults: vec2_arr(&p.defaults, [0.0, 0.0]),
        axis_points_x: axis_x,
        axis_points_y: axis_y,
        bindings,
    }
}

fn convert_binding(
    b: &SchemaBinding,
    node_index: &HashMap<u32, u32>,
    nodes: &[LegacyNode],
    axis_x: &[f32],
    axis_y: &[f32],
) -> Option<LegacyBinding> {
    // Drop bindings whose target node doesn't resolve: there is nothing for
    // them to drive.
    let node = *node_index.get(&b.node?)?;
    let values_json = b.values.as_ref()?;
    let kind = b.param_name.as_deref().unwrap_or("");
    // A mesh group is never drawn and carries no colour, so a colour binding on
    // one has nowhere to land — and writing it out would produce a `.clm` the
    // loader rejects.
    if source_binding_is_color(kind)
        && matches!(
            nodes.get(node as usize).map(|n| &n.kind),
            Some(LegacyNodeKind::MeshGroup(_))
        )
    {
        tracing::debug!(
            "dropping {:?} binding on mesh group node {}: a mesh group is never drawn",
            kind,
            node
        );
        return None;
    }
    let values = convert_binding_values(kind, values_json, b.is_set.as_deref(), axis_x, axis_y)?;
    Some(LegacyBinding {
        node,
        interpolate_mode: interp(b.interpolate_mode.as_deref()),
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inx::InxModel;
    use catchlight_core::model::{BindingTarget, ScalarTarget};
    use serde_json::json;
    /// A model authored in inochi2d's frame — Y-down, lower `zsort` in front —
    /// touching every field the import must reflect, plus controls on fields
    /// it must leave alone. Values are asymmetric and non-zero so a *missing*
    /// negation and a *doubled* one both change the result.
    ///
    /// No textures: an empty table means the alpha crop rewrites no UVs, so
    /// the authored UVs stay comparable against the literals below.
    fn reflection_fixture() -> InxModel {
        InxModel {
            payload: json!({
                "nodes": {
                    "uuid": 1,
                    "name": "root",
                    "type": "Node",
                    "zsort": 0.0,
                    "transform": {
                        "trans": [0.0, 0.0, 0.0],
                        "rot": [0.0, 0.0, 0.0],
                        "scale": [1.0, 1.0]
                    },
                    "children": [
                        {
                            "uuid": 2,
                            // Lower source zsort => nearer the viewer in the
                            // source convention, so this must come out with
                            // the *higher* base_z_order.
                            "zsort": 1.0,
                            "name": "front",
                            "type": "Part",
                            "transform": {
                                // x and z must survive; y must flip.
                                "trans": [7.0, 10.0, 3.0],
                                // rot x and z flip; rot y must survive.
                                "rot": [0.25, 0.5, 0.75],
                                "scale": [2.0, 3.0]
                            },
                            "textures": [0],
                            "mesh": {
                                "verts": [1.0, 2.0, -4.0, 6.0, 8.0, -3.0],
                                "uvs": [0.0, 0.0, 1.0, 0.25, 0.5, 1.0],
                                "indices": [0, 1, 2],
                                "origin": [1.5, 2.5]
                            },
                            "children": []
                        },
                        {
                            "uuid": 3,
                            "zsort": 5.0,
                            "name": "back",
                            "type": "Part",
                            "transform": {
                                "trans": [-2.0, -12.0, 0.0],
                                "rot": [-1.5, 2.0, -0.5],
                                "scale": [1.0, 1.0]
                            },
                            "textures": [0],
                            "mesh": {
                                "verts": [0.0, 0.0, 4.0, -9.0, -7.0, 11.0],
                                "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
                                "indices": [0, 1, 2],
                                "origin": [0.0, -6.0]
                            },
                            "children": []
                        }
                    ]
                },
                "param": [{
                    "uuid": 100,
                    "name": "reflect",
                    "is_vec2": false,
                    "min": [0.0, 0.0],
                    "max": [1.0, 1.0],
                    "defaults": [0.0, 0.0],
                    // 2 x-axis points, 1 y-axis point: `values` is [x][y], so
                    // each binding is a 2-long outer array of 1-long columns.
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": [
                        // --- reflected targets ---
                        {"node": 2, "param_name": "deform", "values": [
                            [[[3.0, 5.0], [-1.0, 2.0], [0.0, 7.0]]],
                            [[[2.0, -4.0], [6.0, 1.0], [-8.0, 0.0]]]
                        ]},
                        {"node": 2, "param_name": "transform.t.y", "values": [[10.0], [-20.0]]},
                        {"node": 2, "param_name": "transform.r.x", "values": [[0.25], [-0.5]]},
                        {"node": 3, "param_name": "transform.r.z", "values": [[1.5], [-2.5]]},
                        {"node": 3, "param_name": "zSort", "values": [[-3.0], [4.0]]},
                        // --- controls: must come through untouched ---
                        {"node": 3, "param_name": "transform.t.x", "values": [[11.0], [-22.0]]},
                        {"node": 3, "param_name": "transform.r.y", "values": [[0.75], [-1.25]]},
                        {"node": 3, "param_name": "transform.s.y", "values": [[2.0], [0.5]]},
                        {"node": 3, "param_name": "opacity", "values": [[0.25], [0.75]]}
                    ]
                }]
            }),
            textures: Vec::new(),
            vendors: Vec::new(),
        }
    }

    /// The reflection guard. `.inx` is authored Y-down with lower `zsort` in
    /// front and catchlight is Y-up with higher `z_order` in front, so this
    /// reader negates a specific set of fields — and only that set. Every
    /// reflected field below is asserted against its authored value with a
    /// non-reflected control beside it, so a missing negation and a doubled
    /// one both fail here.
    #[test]
    fn the_import_reflects_exactly_the_y_bearing_fields() {
        let doc = from_inx_model_to_legacy(&reflection_fixture()).unwrap().doc;
        let front = &doc.nodes[1];
        let back = &doc.nodes[2];
        assert_eq!(front.name, "front");
        assert_eq!(back.name, "back");

        // zsort: source lower-is-front becomes catchlight higher-is-front.
        assert_eq!(front.z_order, -1.0, "front z order");
        assert_eq!(back.z_order, -5.0, "back z order");
        assert!(
            front.z_order > back.z_order,
            "the node authored nearer the viewer must sort in front"
        );

        // Transform: translation y flips (x, z do not); rotation x and z flip
        // (rotation y and scale do not).
        assert_eq!(front.transform.translation, [7.0, -10.0, 3.0]);
        assert_eq!(front.transform.rotation, [-0.25, 0.5, -0.75]);
        assert_eq!(front.transform.scale, [2.0, 3.0]);
        assert_eq!(back.transform.translation, [-2.0, 12.0, 0.0]);
        assert_eq!(back.transform.rotation, [1.5, 2.0, 0.5]);

        // Mesh: vertex and origin y flip, uvs are texture space and do not.
        let mesh = |n: &LegacyNode| match &n.kind {
            LegacyNodeKind::Part(p) => p.mesh.clone(),
            other => panic!("expected a Part, got {other:?}"),
        };
        let front_mesh = mesh(front);
        assert_eq!(front_mesh.verts, vec![1.0, -2.0, -4.0, -6.0, 8.0, 3.0]);
        assert_eq!(
            front_mesh.uvs,
            vec![0.0, 0.0, 1.0, 0.25, 0.5, 1.0],
            "uvs are texture space and are never reflected"
        );
        assert_eq!(front_mesh.origin, [1.5, -2.5]);
        assert_eq!(mesh(back).origin, [0.0, 6.0]);

        // Bindings, in authored order.
        assert_eq!(doc.params.len(), 1, "param count");
        let bindings = &doc.params[0].bindings;
        assert_eq!(bindings.len(), 9, "binding count");

        let deform = catchlight_core::model::deform_cells(&bindings[0].values)
            .expect("binding 0 is the deform");
        assert_eq!(
            deform.iter().map(|c| c.value.clone()).collect::<Vec<_>>(),
            vec![
                vec![3.0, -5.0, -1.0, -2.0, 0.0, -7.0],
                vec![2.0, 4.0, 6.0, -1.0, -8.0, 0.0],
            ],
            "deform offsets: x survives, y flips"
        );

        let scalar = |i: usize, target: ScalarTarget| -> Vec<f32> {
            assert_eq!(
                catchlight_core::model::target_of(&bindings[i].values),
                BindingTarget::Scalar(target),
                "binding {i} target"
            );
            catchlight_core::model::scalar_cells(&bindings[i].values)
                .expect("a scalar binding")
                .iter()
                .map(|c| c.value)
                .collect()
        };

        // Reflected scalar targets.
        assert_eq!(scalar(1, ScalarTarget::Ty), vec![-10.0, 20.0]);
        assert_eq!(scalar(2, ScalarTarget::Rx), vec![-0.25, 0.5]);
        assert_eq!(scalar(3, ScalarTarget::Rz), vec![-1.5, 2.5]);
        assert_eq!(scalar(4, ScalarTarget::ZOrder), vec![3.0, -4.0]);

        // Controls: over-negation shows up here.
        assert_eq!(scalar(5, ScalarTarget::Tx), vec![11.0, -22.0]);
        assert_eq!(scalar(6, ScalarTarget::Ry), vec![0.75, -1.25]);
        assert_eq!(scalar(7, ScalarTarget::Sy), vec![2.0, 0.5]);
        assert_eq!(scalar(8, ScalarTarget::Opacity), vec![0.25, 0.75]);
    }

    /// The full reference model. No such model ships in the tree yet, so
    /// every test that needs one is `#[ignore]`d; drop a model at this path and
    /// remove the attributes to re-enable them.
    fn load_reference() -> InxModel {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../example_models/reference/reference.inx"
        );
        let bytes = std::fs::read(path).expect("read reference.inx");
        InxModel::parse(std::io::Cursor::new(bytes.as_slice())).expect("parse reference.inx")
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn reference_inx_roundtrips_through_clm() {
        let model = load_reference();
        let file = from_inx_model_to_legacy(&model).unwrap();

        let parts = file
            .doc
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, LegacyNodeKind::Part(_)))
            .count();
        assert_eq!(parts, 117, "expected 117 Part nodes");
        assert_eq!(file.textures.len(), 87);

        let bytes = catchlight_core::Model::from_legacy(&file)
            .and_then(|m| m.to_clm_bytes())
            .unwrap();
        let reopened = catchlight_core::Model::from_clm_bytes(&bytes)
            .unwrap()
            .to_legacy()
            .unwrap();
        assert_eq!(
            reopened.doc, file.doc,
            "structure must round-trip through .clm"
        );
        assert_eq!(reopened.textures, file.textures, "textures must round-trip");
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn arena_is_topologically_ordered_with_one_root() {
        let model = load_reference();
        let file = from_inx_model_to_legacy(&model).unwrap();
        let nodes = &file.doc.nodes;

        let roots = nodes.iter().filter(|n| n.parent.is_none()).count();
        assert_eq!(roots, 1, "exactly one root");
        for (i, n) in nodes.iter().enumerate() {
            if let Some(p) = n.parent {
                assert!(
                    (p as usize) < i,
                    "parent {p} must precede child {i} (topo order)"
                );
            }
        }

        // Every cross-reference resolves to an in-range index.
        let n = nodes.len() as u32;
        let p = file.doc.params.len() as u32;
        for node in nodes {
            match &node.kind {
                LegacyNodeKind::Part(part) => {
                    assert!(part.masks.iter().all(|m| m.source < n));
                }
                LegacyNodeKind::Composite(c) => assert!(c.masks.iter().all(|m| m.source < n)),
                LegacyNodeKind::SimplePhysics(sp) => {
                    assert!(sp.target_param.is_none_or(|t| t < p));
                }
                _ => {}
            }
        }
        for param in &file.doc.params {
            assert!(param.bindings.iter().all(|b| b.node < n));
        }
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn captures_authored_propagate_meshgroup() {
        let model = load_reference();
        let file = from_inx_model_to_legacy(&model).unwrap();
        // The root "Puppet Body" composite authors propagate_meshgroup=false —
        // the flag the old runtime path hardcoded to true.
        assert!(
            file.doc.nodes.iter().any(|n| matches!(
                &n.kind,
                LegacyNodeKind::Composite(c) if !c.propagate_meshgroup
            )),
            "an authored propagate_meshgroup=false composite must be captured"
        );
        assert!(file.doc.params.iter().any(|p| !p.bindings.is_empty()));
    }
}
