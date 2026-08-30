//! `legacy document → LegacyPuppet` build: load the arena into a runtime [`LegacyPuppet`]
//! (the editor/preview path, and the structural inverse of
//! [`super::to_legacy`]). A model file reaches this through a
//! [`Model`](crate::Model) — `.clm` bytes, then
//! [`Model::to_legacy`](crate::Model::to_legacy) — so nothing here reads a
//! file.
//!
//! The flat arena makes this a linear fill — walk `nodes` in topological order
//! and [`LegacyPuppet::insert_child`] each under its already-inserted parent — then
//! reuse the inx path's mesh-group bake ([`bake_mesh_groups`]) and texture crop
//! ([`crop_textures`]) so the result is the same `LegacyPuppet` an `.inx` load builds.
//!
//! The arena carries no uuids, but the runtime is uuid-keyed (masks resolve a
//! node by uuid, params/physics by uuid), so the build synthesizes `uuid =
//! arena index`: node index → node uuid, param index → param uuid. The two are
//! independent namespaces in `LegacyPuppet`, so the overlap is harmless. The one
//! authored-vs-runtime transform redone here is the global g-scale fold into
//! each SimplePhysics node's gravity (the arena stores it authored/unscaled).

use glam::{Vec2, Vec3};

use crate::components::{
    CompositeData, Mask, Mesh, MeshGroupData, MeshIndices, Node, NodeIdx, NodeKind, PartData,
    TextureId, Transform,
};
use crate::deform::DeformStack;
use crate::formats::clm::{
    ClmBindingValues, ClmCells, ClmIndices, ClmMesh, ClmTransform, TextureAlpha, TextureEncoding,
};
use crate::formats::legacy::{
    LegacyComposite, LegacyFile, LegacyMask, LegacyMeshGroup, LegacyNode, LegacyNodeKind,
    LegacyParam, LegacyPart, LegacySimplePhysics, LegacyTexture, LegacyWeld,
};
use crate::formats::{ModelTexture, TextureFormat};
use crate::legacy_puppet::LegacyPuppet;
use crate::load_budget::{charge_legacy_structure, LoadBudget};
use crate::meshgroup::MeshGroupAttachments;
use crate::params::{
    Binding, BindingValues, DeformMatrix, Matrix, MeshGroupColorBindingError, Param,
};
use crate::physics::SimplePhysicsData;

use super::convert::{bake_mesh_groups, binding_is_all_zero};
use super::error::ImportError;

/// Build a runtime [`LegacyPuppet`] from a decoded [`LegacyFile`], downsampling each
/// texture by `texture_halvings` power-of-two steps (0 = full resolution).
pub fn from_legacy(file: &LegacyFile, texture_halvings: u32) -> Result<LegacyPuppet, ImportError> {
    from_legacy_with_budget(file, texture_halvings, &mut LoadBudget::default())
}

pub fn from_legacy_with_budget(
    file: &LegacyFile,
    texture_halvings: u32,
    budget: &mut LoadBudget,
) -> Result<LegacyPuppet, ImportError> {
    from_legacy_impl(file, texture_halvings, None, budget)
}

/// [`from_legacy`] with a texture-prep memo: the editor rebuilds the puppet on
/// every document edit, and re-decoding megabytes of unchanged PNGs per edit
/// is what would otherwise dominate that rebuild.
pub fn from_legacy_cached(
    file: &LegacyFile,
    texture_halvings: u32,
    cache: &mut super::alpha_crop::TexturePrepCache,
) -> Result<LegacyPuppet, ImportError> {
    from_legacy_impl(
        file,
        texture_halvings,
        Some(cache),
        &mut LoadBudget::default(),
    )
}

fn from_legacy_impl(
    file: &LegacyFile,
    texture_halvings: u32,
    cache: Option<&mut super::alpha_crop::TexturePrepCache>,
    budget: &mut LoadBudget,
) -> Result<LegacyPuppet, ImportError> {
    charge_legacy_structure(file, budget)?;
    let doc = &file.doc;
    let model_textures = build_model_textures(&file.textures)?;
    for texture in &model_textures {
        let (width, height) = texture
            .dimensions()
            .map_err(|error| ImportError::TextureDecode(error.to_string()))?;
        budget.check_texture_dimensions(width, height)?;
    }
    // The .clm stores authored, unscaled physics; the runtime integrator wants
    // gravity pre-folded with the puppet-level pixelsPerMeter × gravity, exactly
    // as the inx path folds it (convert.rs).
    let g_scale = doc.physics.pixels_per_meter * doc.physics.gravity;

    let mut puppet = LegacyPuppet::new();
    // arena index → runtime NodeIdx. Topological order (`parent < self`)
    // guarantees a node's parent is already present, so this is a linear fill.
    let mut node_ids: Vec<NodeIdx> = Vec::with_capacity(doc.nodes.len());
    for (i, legacy_node) in doc.nodes.iter().enumerate() {
        let parent = match legacy_node.parent {
            None => puppet.root(),
            Some(p) => *node_ids.get(p as usize).ok_or_else(|| {
                ImportError::MalformedPayload(format!(
                    "node {i} parent index {p} is not a preceding node"
                ))
            })?,
        };
        let node = build_node(legacy_node, g_scale)?;
        node_ids.push(puppet.insert_child(parent, node, Some(i as u32)));
    }

    bake_mesh_groups(&mut puppet);

    let puppet_textures = super::alpha_crop::crop_textures_cached(
        &mut puppet,
        &model_textures,
        texture_halvings,
        cache,
    )?;
    puppet.set_textures(puppet_textures);

    let params = doc
        .params
        .iter()
        .enumerate()
        .map(|(j, p)| build_param(j as u32, p, &node_ids, &doc.nodes))
        .collect::<Result<Vec<_>, ImportError>>()?;
    puppet.set_params(params);

    puppet.set_welds(build_welds(&doc.welds, &node_ids, &doc.nodes)?);

    Ok(puppet)
}

/// Welds validate hard: the arena is machine-written, so a bad weld means the
/// writer is broken — reject the model rather than render a silently
/// half-welded puppet.
fn build_welds(
    welds: &[LegacyWeld],
    node_ids: &[NodeIdx],
    nodes: &[LegacyNode],
) -> Result<Vec<crate::weld::Weld>, ImportError> {
    let bad = |msg: String| ImportError::MalformedPayload(msg);
    let part_vert_count = |idx: u32| -> Result<usize, ImportError> {
        match nodes.get(idx as usize).map(|n| &n.kind) {
            Some(LegacyNodeKind::Part(p)) => Ok(p.mesh.verts.len() / 2),
            Some(_) => Err(bad(format!("weld endpoint {idx} is not a Part"))),
            None => Err(bad(format!("weld endpoint {idx} is not a node"))),
        }
    };

    let mut seen_pairs: Vec<(u32, u32)> = Vec::with_capacity(welds.len());
    let mut out = Vec::with_capacity(welds.len());
    for (i, w) in welds.iter().enumerate() {
        if w.a == w.b {
            return Err(bad(format!("weld {i} welds node {} to itself", w.a)));
        }
        let key = (w.a.min(w.b), w.a.max(w.b));
        if seen_pairs.contains(&key) {
            return Err(bad(format!(
                "weld {i} duplicates the pair {{{}, {}}}",
                key.0, key.1
            )));
        }
        seen_pairs.push(key);
        let a_verts = part_vert_count(w.a)?;
        let b_verts = part_vert_count(w.b)?;
        let mut pairs = Vec::with_capacity(w.pairs.len());
        for p in &w.pairs {
            if p.a_vert as usize >= a_verts || p.b_vert as usize >= b_verts {
                return Err(bad(format!(
                    "weld {i} pair ({}, {}) out of range ({a_verts} / {b_verts} verts)",
                    p.a_vert, p.b_vert
                )));
            }
            if !p.weight.is_finite() || !(0.0..=1.0).contains(&p.weight) {
                return Err(bad(format!(
                    "weld {i} pair ({}, {}) weight {} outside [0, 1]",
                    p.a_vert, p.b_vert, p.weight
                )));
            }
            pairs.push(crate::weld::WeldPair {
                a_vert: p.a_vert,
                b_vert: p.b_vert,
                weight: p.weight,
            });
        }
        out.push(crate::weld::Weld {
            a: node_ids[w.a as usize],
            b: node_ids[w.b as usize],
            pairs,
        });
    }
    Ok(out)
}

fn build_node(legacy: &LegacyNode, g_scale: f32) -> Result<Node, ImportError> {
    let transform = build_transform(&legacy.transform);
    let kind = build_node_kind(legacy, &transform, g_scale)?;
    Ok(Node {
        name: legacy.name.clone(),
        enabled: legacy.enabled,
        base_transform: transform,
        base_z_order: legacy.z_order,
        transform,
        z_order: legacy.z_order,
        lock_to_root: legacy.lock_to_root,
        kind,
    })
}

fn build_transform(t: &ClmTransform) -> Transform {
    Transform {
        translation: Vec3::from_array(t.translation),
        rotation: Vec3::from_array(t.rotation),
        scale: Vec2::from_array(t.scale),
    }
}

fn build_node_kind(
    legacy: &LegacyNode,
    transform: &Transform,
    g_scale: f32,
) -> Result<NodeKind, ImportError> {
    Ok(match &legacy.kind {
        LegacyNodeKind::Group => NodeKind::Group,
        LegacyNodeKind::Part(p) => NodeKind::Part(Box::new(build_part(p)?)),
        LegacyNodeKind::Composite(c) => NodeKind::Composite(Box::new(build_composite(c))),
        LegacyNodeKind::MeshGroup(m) => NodeKind::MeshGroup(Box::new(build_mesh_group(m)?)),
        LegacyNodeKind::SimplePhysics(s) => {
            NodeKind::SimplePhysics(Box::new(build_simple_physics(s, transform, g_scale)))
        }
    })
}

fn build_part(p: &LegacyPart) -> Result<PartData, ImportError> {
    let mesh = build_mesh(&p.mesh)?;
    let deform_stack = DeformStack::new(mesh.vertices.len());
    let tint = Vec3::from_array(p.tint);
    let screen_tint = Vec3::from_array(p.screen_tint);
    Ok(PartData {
        mesh,
        albedo_texture: TextureId(p.albedo),
        opacity: p.opacity,
        base_opacity: p.opacity,
        tint,
        base_tint: tint,
        screen_tint,
        base_screen_tint: screen_tint,
        blend_mode: p.blend_mode,
        masks: build_masks(&p.masks),
        mask_threshold: p.mask_threshold,
        deform_stack,
    })
}

fn build_composite(c: &LegacyComposite) -> CompositeData {
    let tint = Vec3::from_array(c.tint);
    let screen_tint = Vec3::from_array(c.screen_tint);
    CompositeData {
        opacity: c.opacity,
        base_opacity: c.opacity,
        tint,
        base_tint: tint,
        screen_tint,
        base_screen_tint: screen_tint,
        blend_mode: c.blend_mode,
        masks: build_masks(&c.masks),
        propagate_mesh_group: c.propagate_meshgroup,
        mask_threshold: c.mask_threshold,
    }
}

fn build_mesh_group(m: &LegacyMeshGroup) -> Result<MeshGroupData, ImportError> {
    let mesh = build_mesh(&m.mesh)?;
    let deform_stack = DeformStack::new(mesh.vertices.len());
    Ok(MeshGroupData {
        mesh,
        dynamic: m.dynamic,
        translate_children: m.translate_children,
        deform_stack,
        // Filled by bake_mesh_groups, like the inx path.
        attachments: MeshGroupAttachments::default(),
        bitmap: None,
    })
}

fn build_simple_physics(
    s: &LegacySimplePhysics,
    transform: &Transform,
    g_scale: f32,
) -> SimplePhysicsData {
    let anchor = Vec2::new(transform.translation.x, transform.translation.y);
    SimplePhysicsData {
        kind: s.kind,
        map_mode: s.map_mode,
        local_only: s.local_only,
        target_param_id: s.target_param,
        gravity: s.gravity * g_scale,
        length: s.length,
        frequency: s.frequency,
        angle_damping: s.angle_damping,
        length_damping: s.length_damping,
        output_scale: Vec2::from_array(s.output_scale),
        offset_output_scale: Vec2::ONE,
        bob: anchor + Vec2::new(0.0, s.length),
        spring_vel: Vec2::ZERO,
        d_angle: 0.0,
        anchor,
        anchor_initialized: false,
    }
}

fn build_masks(masks: &[LegacyMask]) -> Vec<Mask> {
    masks
        .iter()
        .map(|m| Mask {
            source_uuid: m.source,
            mode: m.mode,
        })
        .collect()
}

/// Rebuild a runtime [`Mesh`], re-establishing the invariant the deform/runtime
/// loops rely on (every index in range, uvs 1:1 with vertices) — the same
/// validation `convert.rs` does on the inx path.
fn build_mesh(m: &ClmMesh) -> Result<Mesh, ImportError> {
    let vertices: Vec<Vec2> = m
        .verts
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Vec2::new(c[0], c[1]))
        .collect();
    let uvs: Vec<Vec2> = m
        .uvs
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Vec2::new(c[0], c[1]))
        .collect();
    if !uvs.is_empty() && uvs.len() != vertices.len() {
        return Err(ImportError::MalformedPayload(format!(
            "mesh has {} vertices but {} uvs",
            vertices.len(),
            uvs.len()
        )));
    }
    let indices = match &m.indices {
        ClmIndices::U16(v) => MeshIndices::U16(v.clone()),
        ClmIndices::U32(v) => MeshIndices::U32(v.clone()),
    };
    if let Some(bad) = indices.iter_u32().find(|&i| i as usize >= vertices.len()) {
        return Err(ImportError::MalformedPayload(format!(
            "mesh triangle index {bad} out of range for {} vertices",
            vertices.len()
        )));
    }
    Ok(Mesh::new(
        vertices,
        uvs,
        indices,
        Vec2::from_array(m.origin),
    ))
}

fn build_param(
    id: u32,
    p: &LegacyParam,
    node_ids: &[NodeIdx],
    nodes: &[LegacyNode],
) -> Result<Param, ImportError> {
    let width = p.axis_points_x.len().max(1);
    let height = p.axis_points_y.len().max(1);
    let mut bindings = Vec::with_capacity(p.bindings.len());
    for b in &p.bindings {
        // A binding whose target node doesn't resolve is dropped, as on the
        // inx path.
        let Some(&node) = node_ids.get(b.node as usize) else {
            continue;
        };
        let legacy_node = nodes.get(b.node as usize);
        if let Some(target) = color_target(&b.values) {
            if matches!(
                legacy_node.map(|n| &n.kind),
                Some(LegacyNodeKind::MeshGroup(_))
            ) {
                return Err(MeshGroupColorBindingError {
                    param: id,
                    node: b.node,
                    target,
                }
                .into());
            }
        }
        let values = build_binding_values(
            &b.values,
            legacy_node,
            width,
            height,
            &p.axis_points_x,
            &p.axis_points_y,
        )?;
        // The same load-time sanitization the inx path applies: a binding that
        // contributes nothing regardless of param value never reaches the runtime.
        if binding_is_all_zero(&values) {
            continue;
        }
        bindings.push(Binding::new(node, b.interpolate_mode, values));
    }
    Ok(Param {
        id,
        name: p.name.clone(),
        is_vec2: p.is_vec2,
        min: Vec2::from_array(p.min),
        max: Vec2::from_array(p.max),
        defaults: Vec2::from_array(p.defaults),
        axis_points_x: p.axis_points_x.clone(),
        axis_points_y: p.axis_points_y.clone(),
        bindings,
    })
}

/// The colour target a binding drives, or `None` for a target that is not a
/// colour. Named after the field it would have folded into, so the load error
/// names the property the binding meant to drive.
fn color_target(v: &ClmBindingValues) -> Option<&'static str> {
    use ClmBindingValues as V;
    Some(match v {
        V::Opacity(_) => "opacity",
        V::TintR(_) => "tint.r",
        V::TintG(_) => "tint.g",
        V::TintB(_) => "tint.b",
        V::ScreenTintR(_) => "screen_tint.r",
        V::ScreenTintG(_) => "screen_tint.g",
        V::ScreenTintB(_) => "screen_tint.b",
        _ => return None,
    })
}

/// Derive the dense evaluation grid from the stored authored cells — the
/// loader half of the single-layer keypoint model (see [`crate::fill`]).
fn build_binding_values(
    v: &ClmBindingValues,
    node: Option<&LegacyNode>,
    width: usize,
    height: usize,
    axis_x: &[f32],
    axis_y: &[f32],
) -> Result<BindingValues, crate::deform::DeformShapeError> {
    let scalar = |c: &ClmCells<f32>, identity: f32| -> Matrix<f32> {
        let authored: Vec<((u32, u32), f32)> =
            c.cells.iter().map(|c| ((c.x, c.y), c.value)).collect();
        Matrix {
            width,
            height,
            data: crate::fill::derive_dense(width, height, axis_x, axis_y, &authored, &identity),
        }
    };
    use ClmBindingValues as V;
    Ok(match v {
        V::Deform(c) => {
            BindingValues::Deform(build_deform_matrix(c, node, width, height, axis_x, axis_y)?)
        }
        V::ZOrder(c) => BindingValues::ZOrder(scalar(c, 0.0)),
        V::TransformTX(c) => BindingValues::TransformTX(scalar(c, 0.0)),
        V::TransformTY(c) => BindingValues::TransformTY(scalar(c, 0.0)),
        V::TransformSX(c) => BindingValues::TransformSX(scalar(c, 1.0)),
        V::TransformSY(c) => BindingValues::TransformSY(scalar(c, 1.0)),
        V::TransformRX(c) => BindingValues::TransformRX(scalar(c, 0.0)),
        V::TransformRY(c) => BindingValues::TransformRY(scalar(c, 0.0)),
        V::TransformRZ(c) => BindingValues::TransformRZ(scalar(c, 0.0)),
        V::Opacity(c) => BindingValues::Opacity(scalar(c, 1.0)),
        V::TintR(c) => BindingValues::TintR(scalar(c, 1.0)),
        V::TintG(c) => BindingValues::TintG(scalar(c, 1.0)),
        V::TintB(c) => BindingValues::TintB(scalar(c, 1.0)),
        V::ScreenTintR(c) => BindingValues::ScreenTintR(scalar(c, 0.0)),
        V::ScreenTintG(c) => BindingValues::ScreenTintG(scalar(c, 0.0)),
        V::ScreenTintB(c) => BindingValues::ScreenTintB(scalar(c, 0.0)),
        V::OutputScaleX(c) => BindingValues::OutputScaleX(scalar(c, 1.0)),
        V::OutputScaleY(c) => BindingValues::OutputScaleY(scalar(c, 1.0)),
    })
}

/// Deform cells are stored flat as `[x, y, x, y, …]`; the fill derives the
/// unauthored cells, then `from_cells` packs the dense grid into the runtime's
/// uniform-stride [`DeformMatrix`]. An empty authored set derives all-zero
/// offsets sized to the node's mesh.
fn build_deform_matrix(
    c: &ClmCells<Vec<f32>>,
    node: Option<&LegacyNode>,
    width: usize,
    height: usize,
    axis_x: &[f32],
    axis_y: &[f32],
) -> Result<DeformMatrix, crate::deform::DeformShapeError> {
    let vlen = c
        .cells
        .iter()
        .map(|cell| cell.value.len())
        .max()
        .filter(|&n| n > 0)
        .unwrap_or_else(|| node_mesh_flat_len(node));
    let identity = vec![0.0f32; vlen];
    let authored: Vec<((u32, u32), Vec<f32>)> = c
        .cells
        .iter()
        .map(|cell| ((cell.x, cell.y), cell.value.clone()))
        .collect();
    let dense = crate::fill::derive_dense(width, height, axis_x, axis_y, &authored, &identity);
    let cells: Vec<Vec<Vec2>> = dense
        .iter()
        .map(|cell| {
            cell.as_chunks::<2>()
                .0
                .iter()
                .map(|c| Vec2::new(c[0], c[1]))
                .collect()
        })
        .collect();
    DeformMatrix::from_cells(width, height, cells)
}

fn node_mesh_flat_len(node: Option<&LegacyNode>) -> usize {
    match node.map(|n| &n.kind) {
        Some(LegacyNodeKind::Part(p)) => p.mesh.verts.len(),
        Some(LegacyNodeKind::MeshGroup(mg)) => mg.mesh.verts.len(),
        _ => 0,
    }
}

fn build_model_textures(textures: &[LegacyTexture]) -> Result<Vec<ModelTexture>, ImportError> {
    textures
        .iter()
        .map(|t| {
            // crop_textures decodes via ModelTexture::decode; the `premultiplied`
            // flag tells it whether to unwind inochi2d's premultiply-in-sRGB
            // (inx-sourced) or take the bytes as straight-alpha (editor-authored).
            Ok(ModelTexture {
                format: match t.encoding {
                    TextureEncoding::Png => TextureFormat::Png,
                    TextureEncoding::Tga => TextureFormat::Tga,
                },
                data: t.data.clone().into(),
                premultiplied: t.alpha == TextureAlpha::PremultipliedSrgb,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::InxModel;
    use serde_json::json;

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

    fn kind_name(k: &NodeKind) -> &'static str {
        match k {
            NodeKind::Group => "Group",
            NodeKind::Part(_) => "Part",
            NodeKind::Composite(_) => "Composite",
            NodeKind::MeshGroup(_) => "MeshGroup",
            NodeKind::SimplePhysics(_) => "SimplePhysics",
        }
    }

    #[test]
    fn inx_and_legacy_paths_reflect_z_order_identically() {
        let model = InxModel {
            payload: json!({
                "nodes": {
                    "uuid": 1,
                    "name": "root",
                    "type": "Node",
                    "zsort": 2.5,
                    "children": []
                },
                "param": [{
                    "uuid": 10,
                    "name": "depth",
                    "is_vec2": false,
                    "min": [0.0, 0.0],
                    "max": [1.0, 1.0],
                    "defaults": [0.0, 0.0],
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": [{
                        "node": 1,
                        "param_name": "zSort",
                        "values": [[-3.0], [4.0]]
                    }]
                }]
            }),
            textures: Vec::new(),
            vendors: Vec::new(),
        };

        let direct = super::super::from_inx_model(&model).unwrap();
        let legacy = super::super::from_inx_model_to_legacy(&model).unwrap();
        let rebuilt = from_legacy(&legacy, 0).unwrap();

        assert_eq!(legacy.doc.nodes[0].z_order, -2.5);
        assert_eq!(
            direct
                .iter()
                .map(|(_, node)| node.base_z_order)
                .collect::<Vec<_>>(),
            rebuilt
                .iter()
                .map(|(_, node)| node.base_z_order)
                .collect::<Vec<_>>()
        );
        let direct_values = &direct.params()[0].bindings[0].values;
        let rebuilt_values = &rebuilt.params()[0].bindings[0].values;
        let (BindingValues::ZOrder(direct), BindingValues::ZOrder(rebuilt)) =
            (direct_values, rebuilt_values)
        else {
            panic!("expected ZOrder bindings");
        };
        assert_eq!(direct.data, vec![3.0, -4.0]);
        assert_eq!(direct.data, rebuilt.data);
    }

    /// Name of a [`BindingValues`] variant, so a mismatch names the target
    /// instead of dumping two matrices.
    fn binding_kind_name(v: &BindingValues) -> &'static str {
        use BindingValues as V;
        match v {
            V::ZOrder(_) => "zSort",
            V::TransformTX(_) => "transform.t.x",
            V::TransformTY(_) => "transform.t.y",
            V::TransformSX(_) => "transform.s.x",
            V::TransformSY(_) => "transform.s.y",
            V::TransformRX(_) => "transform.r.x",
            V::TransformRY(_) => "transform.r.y",
            V::TransformRZ(_) => "transform.r.z",
            V::Deform(_) => "deform",
            V::Opacity(_) => "opacity",
            V::TintR(_) => "tint.r",
            V::TintG(_) => "tint.g",
            V::TintB(_) => "tint.b",
            V::ScreenTintR(_) => "screenTint.r",
            V::ScreenTintG(_) => "screenTint.g",
            V::ScreenTintB(_) => "screenTint.b",
            V::OutputScaleX(_) => "outputScale.x",
            V::OutputScaleY(_) => "outputScale.y",
        }
    }

    /// The dense grid behind every non-deform binding kind.
    fn scalar_matrix(v: &BindingValues) -> Option<&Matrix<f32>> {
        use BindingValues as V;
        match v {
            V::Deform(_) => None,
            V::ZOrder(m)
            | V::TransformTX(m)
            | V::TransformTY(m)
            | V::TransformSX(m)
            | V::TransformSY(m)
            | V::TransformRX(m)
            | V::TransformRY(m)
            | V::TransformRZ(m)
            | V::Opacity(m)
            | V::TintR(m)
            | V::TintG(m)
            | V::TintB(m)
            | V::ScreenTintR(m)
            | V::ScreenTintG(m)
            | V::ScreenTintB(m)
            | V::OutputScaleX(m)
            | V::OutputScaleY(m) => Some(m),
        }
    }

    /// Element-wise compare that reports the first differing index. A whole-
    /// slice `assert_eq!` on a model-sized mesh buries the one bad element in
    /// thousands of good ones.
    fn assert_slices_match<T: PartialEq + std::fmt::Debug>(a: &[T], b: &[T], what: &str) {
        assert_eq!(a.len(), b.len(), "{what} length");
        if let Some(i) = a.iter().zip(b).position(|(x, y)| x != y) {
            assert_eq!(a[i], b[i], "{what} at index {i}");
        }
    }

    fn assert_transforms_match(a: &Transform, b: &Transform, what: &str) {
        assert_eq!(a.translation, b.translation, "{what} translation");
        assert_eq!(a.rotation, b.rotation, "{what} rotation");
        assert_eq!(a.scale, b.scale, "{what} scale");
    }

    fn assert_meshes_match(a: &Mesh, b: &Mesh, what: &str) {
        assert_slices_match(&a.vertices, &b.vertices, &format!("{what} verts"));
        assert_slices_match(&a.uvs, &b.uvs, &format!("{what} uvs"));
        assert_eq!(a.origin, b.origin, "{what} origin");
        // By value, not by variant: the two paths pick U16 vs U32 storage
        // independently, but the triangle list itself must match.
        assert_slices_match(
            &a.indices.iter_u32().collect::<Vec<_>>(),
            &b.indices.iter_u32().collect::<Vec<_>>(),
            &format!("{what} indices"),
        );
    }

    fn assert_binding_values_match(a: &BindingValues, b: &BindingValues, what: &str) {
        assert_eq!(
            binding_kind_name(a),
            binding_kind_name(b),
            "{what} binding target"
        );
        match (a, b) {
            (BindingValues::Deform(da), BindingValues::Deform(db)) => {
                assert_eq!(
                    (da.width, da.height),
                    (db.width, db.height),
                    "{what} deform grid"
                );
                assert_eq!(da.vert_count, db.vert_count, "{what} deform vert count");
                assert_slices_match(
                    da.offsets(),
                    db.offsets(),
                    &format!("{what} deform offsets"),
                );
            }
            // The kind names matched above, so anything else is a matching
            // pair of scalar-grid kinds.
            _ => {
                if let (Some(ma), Some(mb)) = (scalar_matrix(a), scalar_matrix(b)) {
                    assert_eq!((ma.width, ma.height), (mb.width, mb.height), "{what} grid");
                    assert_slices_match(&ma.data, &mb.data, &format!("{what} values"));
                }
            }
        }
    }

    /// `.inx → LegacyPuppet` and `.inx → .clm → LegacyPuppet` must build the same runtime
    /// puppet. Both insert nodes in the same DFS pre-order, so `iter()` (slot
    /// order) aligns them node-for-node, and `NodeIdx` is the slot index, so
    /// binding targets compare directly too.
    ///
    /// The single intentional divergence is Composite `propagate_mesh_group`
    /// (the inx path hardcodes `true`; the arena build honors the authored
    /// value), which is asserted separately and deliberately left out here.
    /// Param *ids* also differ by construction — the arena has no uuids, so
    /// the build synthesizes `id = array index` — so params are matched by
    /// order.
    ///
    /// Shared by [`reference_legacy_build_matches_inx_puppet`] and
    /// [`synthetic_model_reflects_identically_on_both_paths`] so the model-gated
    /// test and the always-on one cannot check different things.
    fn assert_puppets_match(inx_puppet: &LegacyPuppet, file_puppet: &LegacyPuppet) {
        assert_eq!(inx_puppet.len(), file_puppet.len(), "node count");
        assert_eq!(
            inx_puppet.textures().len(),
            file_puppet.textures().len(),
            "texture count"
        );
        for (a, b) in inx_puppet.textures().iter().zip(file_puppet.textures()) {
            assert_eq!((a.width, a.height), (b.width, b.height), "texture dims");
        }

        for ((id, a), (_, b)) in inx_puppet.iter().zip(file_puppet.iter()) {
            let id = id.0;
            assert_eq!(a.name, b.name, "node {id} name");
            assert_eq!(a.enabled, b.enabled, "node {id} enabled");
            assert_eq!(a.base_z_order, b.base_z_order, "node {id} z order");
            assert_transforms_match(
                &a.base_transform,
                &b.base_transform,
                &format!("node {id} transform"),
            );
            assert_eq!(kind_name(&a.kind), kind_name(&b.kind), "node {id} kind");
            match (&a.kind, &b.kind) {
                (NodeKind::Part(pa), NodeKind::Part(pb)) => {
                    assert_eq!(pa.albedo_texture, pb.albedo_texture, "node {id} albedo");
                    assert_meshes_match(&pa.mesh, &pb.mesh, &format!("node {id} mesh"));
                    assert_eq!(pa.blend_mode, pb.blend_mode, "node {id} blend");
                    assert_eq!(pa.masks.len(), pb.masks.len(), "node {id} masks");
                    assert_eq!(pa.opacity, pb.opacity, "node {id} opacity");
                }
                (NodeKind::Composite(ca), NodeKind::Composite(cb)) => {
                    assert_eq!(ca.blend_mode, cb.blend_mode, "node {id} composite blend");
                    assert_eq!(ca.opacity, cb.opacity, "node {id} composite opacity");
                    assert_eq!(ca.masks.len(), cb.masks.len(), "node {id} composite masks");
                }
                (NodeKind::MeshGroup(ma), NodeKind::MeshGroup(mb)) => {
                    assert_eq!(ma.dynamic, mb.dynamic, "node {id} mg dynamic");
                    assert_eq!(
                        ma.translate_children, mb.translate_children,
                        "node {id} mg tc"
                    );
                    assert_meshes_match(&ma.mesh, &mb.mesh, &format!("node {id} mg mesh"));
                }
                (NodeKind::SimplePhysics(sa), NodeKind::SimplePhysics(sb)) => {
                    assert_eq!(sa.kind, sb.kind, "node {id} phys kind");
                    assert_eq!(sa.map_mode, sb.map_mode, "node {id} phys map_mode");
                    assert_eq!(sa.length, sb.length, "node {id} phys length");
                    assert!(
                        (sa.gravity - sb.gravity).abs() < 1e-3,
                        "node {id} phys gravity (g-scale fold): {} vs {}",
                        sa.gravity,
                        sb.gravity
                    );
                }
                // Group/Group, and any kind pair the name assert above
                // already rejected.
                _ => {}
            }
        }

        assert_eq!(
            inx_puppet.params().len(),
            file_puppet.params().len(),
            "param count"
        );
        for (pa, pb) in inx_puppet.params().iter().zip(file_puppet.params()) {
            assert_eq!(pa.name, pb.name, "param name");
            let name = &pa.name;
            assert_eq!(pa.is_vec2, pb.is_vec2, "param {name} is_vec2");
            assert_eq!(pa.min, pb.min, "param {name} min");
            assert_eq!(pa.max, pb.max, "param {name} max");
            assert_eq!(pa.defaults, pb.defaults, "param {name} defaults");
            assert_eq!(pa.axis_points_x, pb.axis_points_x, "param {name} axis x");
            assert_eq!(pa.axis_points_y, pb.axis_points_y, "param {name} axis y");
            assert_eq!(
                pa.bindings.len(),
                pb.bindings.len(),
                "param {name} binding count (after the all-zero filter)"
            );
            for (i, (ba, bb)) in pa.bindings.iter().zip(&pb.bindings).enumerate() {
                let what = format!(
                    "param {name} binding {i} ({})",
                    binding_kind_name(&ba.values)
                );
                assert_eq!(ba.node, bb.node, "{what} target node");
                assert_eq!(
                    ba.interpolate_mode, bb.interpolate_mode,
                    "{what} interpolate mode"
                );
                assert_binding_values_match(&ba.values, &bb.values, &what);
            }
        }
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn reference_legacy_build_matches_inx_puppet() {
        let model = load_reference();
        let inx_puppet = super::super::from_inx_model(&model).unwrap();
        let legacy = super::super::from_inx_model_to_legacy(&model).unwrap();
        let file_puppet = from_legacy(&legacy, 0).unwrap();
        assert_puppets_match(&inx_puppet, &file_puppet);
    }

    /// A model authored in inochi2d's frame — Y-down, lower `zsort` in front —
    /// touching every field the two import paths must reflect, plus controls
    /// on fields they must leave alone. Values are asymmetric and non-zero so
    /// a *missing* negation and a *doubled* one both change the result.
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

    fn node_named<'a>(puppet: &'a LegacyPuppet, name: &str) -> &'a Node {
        puppet
            .iter()
            .find(|(_, n)| n.name == name)
            .map(|(_, n)| n)
            .expect("node present")
    }

    fn part_mesh<'a>(puppet: &'a LegacyPuppet, name: &str) -> &'a Mesh {
        match &node_named(puppet, name).kind {
            NodeKind::Part(p) => &p.mesh,
            other => {
                assert_eq!(kind_name(other), "Part", "node {name} kind");
                unreachable!()
            }
        }
    }

    /// The dense values of the binding at `index`, checking its target kind
    /// first so a dropped binding fails on the name rather than on a silently
    /// shifted index.
    fn scalar_binding(param: &Param, index: usize, kind: &str) -> Vec<f32> {
        assert!(
            index < param.bindings.len(),
            "no binding at index {index} (wanted {kind})"
        );
        let values = &param.bindings[index].values;
        assert_eq!(binding_kind_name(values), kind, "binding {index} target");
        scalar_matrix(values)
            .map(|m| m.data.clone())
            .unwrap_or_default()
    }

    /// The always-on half of the reflection guard: with no reference model in
    /// the tree, this is what keeps `convert.rs` (inx → LegacyPuppet) and
    /// `to_legacy.rs` (inx → .clm → LegacyPuppet) negating the *same* set of fields.
    ///
    /// Agreement alone is not enough — both paths could forget the same
    /// negation — so the absolute assertions below pin the authored → runtime
    /// values, and every reflected field is paired with a non-reflected
    /// control that must survive unchanged.
    #[test]
    fn synthetic_model_reflects_identically_on_both_paths() {
        let model = reflection_fixture();
        let inx_puppet = super::super::from_inx_model(&model).unwrap();
        let legacy = super::super::from_inx_model_to_legacy(&model).unwrap();
        let file_puppet = from_legacy(&legacy, 0).unwrap();

        // 1. The two paths agree, field for field.
        assert_puppets_match(&inx_puppet, &file_puppet);

        // 2. ...and they agree on the *right* answer. Checked on the inx
        //    puppet; step 1 has already pinned the legacy one to it.
        let front = node_named(&inx_puppet, "front");
        let back = node_named(&inx_puppet, "back");

        // zsort: source lower-is-front becomes catchlight higher-is-front.
        assert_eq!(front.base_z_order, -1.0, "front z order");
        assert_eq!(back.base_z_order, -5.0, "back z order");
        assert!(
            front.base_z_order > back.base_z_order,
            "the node authored nearer the viewer must sort in front"
        );

        // Transform: translation y flips (x, z do not); rotation x and z flip
        // (rotation y and scale do not).
        assert_eq!(
            front.base_transform.translation,
            Vec3::new(7.0, -10.0, 3.0),
            "front translation"
        );
        assert_eq!(
            front.base_transform.rotation,
            Vec3::new(-0.25, 0.5, -0.75),
            "front rotation"
        );
        assert_eq!(
            front.base_transform.scale,
            Vec2::new(2.0, 3.0),
            "front scale"
        );
        assert_eq!(
            back.base_transform.translation,
            Vec3::new(-2.0, 12.0, 0.0),
            "back translation"
        );
        assert_eq!(
            back.base_transform.rotation,
            Vec3::new(1.5, 2.0, 0.5),
            "back rotation"
        );

        // Mesh: vertex and origin y flip, uvs are texture space and do not.
        let front_mesh = part_mesh(&inx_puppet, "front");
        assert_eq!(
            front_mesh.vertices,
            vec![
                Vec2::new(1.0, -2.0),
                Vec2::new(-4.0, -6.0),
                Vec2::new(8.0, 3.0)
            ],
            "front mesh verts"
        );
        assert_eq!(
            front_mesh.uvs,
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.25),
                Vec2::new(0.5, 1.0)
            ],
            "front mesh uvs (texture space, never reflected)"
        );
        assert_eq!(front_mesh.origin, Vec2::new(1.5, -2.5), "front mesh origin");
        assert_eq!(
            part_mesh(&inx_puppet, "back").origin,
            Vec2::new(0.0, 6.0),
            "back mesh origin"
        );

        // Bindings, in authored order.
        assert_eq!(inx_puppet.params().len(), 1, "param count");
        let param = &inx_puppet.params()[0];
        assert_eq!(param.bindings.len(), 9, "binding count");

        assert_eq!(
            binding_kind_name(&param.bindings[0].values),
            "deform",
            "binding 0 target"
        );
        let BindingValues::Deform(deform) = &param.bindings[0].values else {
            unreachable!()
        };
        assert_eq!(
            deform.offsets(),
            &[
                Vec2::new(3.0, -5.0),
                Vec2::new(-1.0, -2.0),
                Vec2::new(0.0, -7.0),
                Vec2::new(2.0, 4.0),
                Vec2::new(6.0, -1.0),
                Vec2::new(-8.0, 0.0),
            ],
            "deform offsets: x survives, y flips"
        );

        // Reflected scalar targets.
        assert_eq!(
            scalar_binding(param, 1, "transform.t.y"),
            vec![-10.0, 20.0],
            "transform.t.y binding"
        );
        assert_eq!(
            scalar_binding(param, 2, "transform.r.x"),
            vec![-0.25, 0.5],
            "transform.r.x binding"
        );
        assert_eq!(
            scalar_binding(param, 3, "transform.r.z"),
            vec![-1.5, 2.5],
            "transform.r.z binding"
        );
        assert_eq!(
            scalar_binding(param, 4, "zSort"),
            vec![3.0, -4.0],
            "zSort binding"
        );

        // Controls: over-negation shows up here.
        assert_eq!(
            scalar_binding(param, 5, "transform.t.x"),
            vec![11.0, -22.0],
            "transform.t.x binding (control)"
        );
        assert_eq!(
            scalar_binding(param, 6, "transform.r.y"),
            vec![0.75, -1.25],
            "transform.r.y binding (control)"
        );
        assert_eq!(
            scalar_binding(param, 7, "transform.s.y"),
            vec![2.0, 0.5],
            "transform.s.y binding (control)"
        );
        assert_eq!(
            scalar_binding(param, 8, "opacity"),
            vec![0.25, 0.75],
            "opacity binding (control)"
        );

        // The .clm intermediate itself, so a drift that cancels out across
        // to_legacy + from_legacy still shows up.
        assert_eq!(legacy.doc.nodes[1].z_order, -1.0, "legacy front z order");
        assert_eq!(legacy.doc.nodes[2].z_order, -5.0, "legacy back z order");
        assert_eq!(
            legacy.doc.nodes[1].transform.translation,
            [7.0, -10.0, 3.0],
            "legacy front translation"
        );
        assert_eq!(
            legacy.doc.nodes[1].transform.rotation,
            [-0.25, 0.5, -0.75],
            "legacy front rotation"
        );
    }

    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    fn triangle_part() -> LegacyNode {
        LegacyNode {
            parent: Some(0),
            name: "part".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: LegacyNodeKind::Part(LegacyPart {
                mesh: ClmMesh {
                    verts: vec![0.0, 0.0, 10.0, 0.0, 5.0, 10.0],
                    uvs: vec![0.0, 0.0, 1.0, 0.0, 0.5, 1.0],
                    indices: ClmIndices::U16(vec![0, 1, 2]),
                    origin: [0.0, 0.0],
                },
                albedo: 0,
                opacity: 1.0,
                blend_mode: crate::components::BlendMode::Normal,
                tint: [1.0, 1.0, 1.0],
                screen_tint: [0.0, 0.0, 0.0],
                masks: Vec::new(),
                mask_threshold: 0.5,
            }),
        }
    }

    fn two_part_file(welds: Vec<crate::formats::legacy::LegacyWeld>) -> LegacyFile {
        let root = LegacyNode {
            parent: None,
            name: "root".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: LegacyNodeKind::Group,
        };
        LegacyFile {
            doc: crate::formats::legacy::LegacyDocument {
                physics: Default::default(),
                nodes: vec![root, triangle_part(), triangle_part()],
                params: Vec::new(),
                welds,
            },
            textures: vec![LegacyTexture {
                encoding: TextureEncoding::Png,
                alpha: TextureAlpha::Straight,
                data: tiny_png(),
            }],
        }
    }

    fn weld(
        a: u32,
        b: u32,
        a_vert: u32,
        b_vert: u32,
        weight: f32,
    ) -> crate::formats::legacy::LegacyWeld {
        crate::formats::legacy::LegacyWeld {
            a,
            b,
            pairs: vec![crate::model::ModelWeldPair {
                a_vert,
                b_vert,
                weight,
            }],
        }
    }

    #[test]
    fn valid_welds_load_and_resolve_to_node_ids() {
        let file = two_part_file(vec![weld(1, 2, 0, 1, 0.25)]);
        let puppet = from_legacy(&file, 0).unwrap();
        assert_eq!(puppet.welds().len(), 1);
        let w = &puppet.welds()[0];
        assert_ne!(w.a, w.b);
        assert!(matches!(
            puppet.get(w.a).map(|n| &n.kind),
            Some(NodeKind::Part(_))
        ));
        assert_eq!(w.pairs.len(), 1);
        assert_eq!(w.pairs[0].weight, 0.25);
    }

    #[test]
    fn aggregate_mesh_budget_is_checked_before_runtime_construction() {
        let file = two_part_file(Vec::new());
        let mut budget = LoadBudget::new(crate::load_budget::LoadLimits {
            vertices: 5,
            ..crate::load_budget::LoadLimits::default()
        });

        let err = match from_legacy_with_budget(&file, 0, &mut budget) {
            Err(error) => error,
            Ok(_) => panic!("expected the aggregate vertex budget to reject the file"),
        };

        assert!(matches!(
            err,
            ImportError::LoadLimit(crate::load_budget::LoadLimitError {
                resource: "vertices",
                got: 6,
                ..
            })
        ));
    }

    #[test]
    fn malformed_welds_are_a_hard_error() {
        let cases: Vec<(&str, Vec<crate::formats::legacy::LegacyWeld>)> = vec![
            ("self weld", vec![weld(1, 1, 0, 0, 0.5)]),
            ("dangling node", vec![weld(1, 9, 0, 0, 0.5)]),
            ("non-Part endpoint", vec![weld(0, 1, 0, 0, 0.5)]),
            ("a_vert out of range", vec![weld(1, 2, 3, 0, 0.5)]),
            ("b_vert out of range", vec![weld(1, 2, 0, 3, 0.5)]),
            ("weight above 1", vec![weld(1, 2, 0, 0, 1.5)]),
            ("weight NaN", vec![weld(1, 2, 0, 0, f32::NAN)]),
            (
                "duplicate unordered pair",
                vec![weld(1, 2, 0, 0, 0.5), weld(2, 1, 1, 1, 0.5)],
            ),
        ];
        for (name, welds) in cases {
            let file = two_part_file(welds);
            match from_legacy(&file, 0) {
                Err(ImportError::MalformedPayload(_)) => {}
                Err(other) => panic!("{name}: expected MalformedPayload, got {other:?}"),
                Ok(_) => panic!("{name}: expected MalformedPayload, got a puppet"),
            }
        }
    }

    /// A one-mesh-group, one-param file whose single binding aims `values` at
    /// the mesh group at index 1 — the shape no writer should produce.
    fn colored_mesh_group_file(values: ClmBindingValues) -> LegacyFile {
        use crate::formats::legacy as f;
        let root = LegacyNode {
            parent: None,
            name: "root".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: LegacyNodeKind::Group,
        };
        let mesh_group = LegacyNode {
            parent: Some(0),
            name: "lattice".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: LegacyNodeKind::MeshGroup(LegacyMeshGroup {
                mesh: ClmMesh::default(),
                dynamic: false,
                translate_children: true,
            }),
        };
        LegacyFile {
            doc: f::LegacyDocument {
                physics: Default::default(),
                nodes: vec![root, mesh_group],
                params: vec![f::LegacyParam {
                    name: "shade".into(),
                    is_vec2: false,
                    min: [0.0, 0.0],
                    max: [1.0, 1.0],
                    defaults: [0.0, 0.0],
                    axis_points_x: vec![0.0, 1.0],
                    axis_points_y: vec![0.0],
                    bindings: vec![f::LegacyBinding {
                        node: 1,
                        interpolate_mode: crate::params::InterpolateMode::Linear,
                        values,
                    }],
                }],
                welds: Vec::new(),
            },
            textures: Vec::new(),
        }
    }

    fn one_cell(value: f32) -> ClmCells<f32> {
        ClmCells {
            cells: vec![crate::formats::clm::ClmCell { x: 1, y: 0, value }],
        }
    }

    /// A mesh group is never drawn, so it has no colour for an `Opacity` /
    /// `Tint*` / `ScreenTint*` binding to fold into. Such a file is refused,
    /// naming the param, the node and the target — dropping the binding
    /// silently would hide a broken model.
    #[test]
    fn color_binding_on_a_mesh_group_is_refused_at_load() {
        use ClmBindingValues as V;
        let cases = [
            (V::Opacity(one_cell(0.25)), "opacity"),
            (V::TintR(one_cell(0.5)), "tint.r"),
            (V::TintG(one_cell(0.5)), "tint.g"),
            (V::TintB(one_cell(0.5)), "tint.b"),
            (V::ScreenTintR(one_cell(0.5)), "screen_tint.r"),
            (V::ScreenTintG(one_cell(0.5)), "screen_tint.g"),
            (V::ScreenTintB(one_cell(0.5)), "screen_tint.b"),
        ];
        for (values, target) in cases {
            let file = colored_mesh_group_file(values);
            // Through the encoded bytes, so the refusal covers a file on disk.
            let bytes = crate::Model::from_legacy(&file)
                .and_then(|m| m.to_clm_bytes())
                .unwrap();
            let err = match crate::load::load_model(&bytes, crate::load::ModelFormat::Clm, 0) {
                Err(ImportError::MeshGroupColorBinding(err)) => err,
                Err(other) => panic!("{target}: expected MeshGroupColorBinding, got {other:?}"),
                Ok(_) => panic!("{target}: a colour binding on a mesh group must not load"),
            };
            assert_eq!(
                err,
                MeshGroupColorBindingError {
                    param: 0,
                    node: 1,
                    target,
                }
            );
        }
    }

    /// The control: the same binding on a node that *is* drawn still loads.
    #[test]
    fn color_binding_on_a_part_still_loads() {
        let mut file = colored_mesh_group_file(ClmBindingValues::Opacity(one_cell(0.25)));
        file.doc.nodes[1] = triangle_part();
        file.textures = vec![LegacyTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: tiny_png(),
        }];
        let bytes = crate::Model::from_legacy(&file)
            .and_then(|m| m.to_clm_bytes())
            .unwrap();
        let puppet = crate::load::load_model(&bytes, crate::load::ModelFormat::Clm, 0)
            .expect("a colour binding on a part loads");
        assert_eq!(puppet.params().len(), 1);
        assert_eq!(puppet.params()[0].bindings.len(), 1);
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn build_honors_authored_propagate_meshgroup() {
        let model = load_reference();
        let legacy = super::super::from_inx_model_to_legacy(&model).unwrap();
        let file_puppet = from_legacy(&legacy, 0).unwrap();
        // The model's root "Puppet Body" composite authors
        // propagate_mesh_group=false; the build must carry that through (the
        // inx path would force true).
        assert!(
            file_puppet.iter().any(|(_, n)| matches!(
                &n.kind,
                NodeKind::Composite(c) if !c.propagate_mesh_group
            )),
            "an authored propagate_mesh_group=false composite must survive the build"
        );
    }
}
