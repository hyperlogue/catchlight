use glam::{Vec2, Vec3};

use crate::{
    animation::{Animation, AnimationLane, Keyframe},
    components::{
        BlendMode, CompositeData, MaskBinding, MaskMode, Mesh, MeshGroupData, MeshIndices, Node,
        NodeIdx, NodeKind, PartData, TextureId, Transform,
    },
    deform::DeformStack,
    formats::ModelTexture,
    load_budget::MAX_PARAM_GRID_CELLS,
    meshgroup::MeshGroupBindings,
    params::{Binding, BindingValues, DeformMatrix, InterpolateMode, Matrix, Param},
    physics::{PhysicsModel, PhysicsParamMapMode, SimplePhysicsData},
    puppet::Puppet,
};

use super::error::ImportError;
use super::schema::{
    source_binding_is_color, SchemaAnimation, SchemaBinding, SchemaMask, SchemaMesh, SchemaNode,
    SchemaParam, SchemaPuppetPhysics, SchemaTransform,
};

/// Default puppet-level physics constants from inochi2d
/// (`source/inochi2d/core/puppet.d:193-198`): `pixelsPerMeter = 1000`,
/// `gravity = 9.8`. Their product is the multiplier inochi2d folds into
/// each `SimplePhysics` node's effective gravity at simulation time;
/// catchlight applies it at import time so the integrator stays unit-clean.
const DEFAULT_PUPPET_GRAVITY: f32 = 9.8;
const DEFAULT_PUPPET_PIXELS_PER_METER: f32 = 1000.0;
const DEFAULT_GRAVITY_SCALE: f32 = DEFAULT_PUPPET_GRAVITY * DEFAULT_PUPPET_PIXELS_PER_METER;

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Test entry into the recursive node walker. Production callers go
    /// through `schema_to_puppet` which seeds `g_scale` from the root
    /// JSON's `physics` block; tests stub it to inochi2d defaults so
    /// loaded SimplePhysics nodes get the same gravity they would in
    /// the reference runtime.
    pub(crate) fn load_subtree(
        root: &serde_json::Value,
        parent: NodeIdx,
        puppet: &mut Puppet,
    ) -> Result<NodeIdx, ImportError> {
        super::load_subtree(root, parent, puppet, DEFAULT_GRAVITY_SCALE)
    }
}

pub(super) fn schema_to_puppet(
    payload: &serde_json::Value,
    textures: Vec<ModelTexture>,
    texture_halvings: u32,
) -> Result<Puppet, ImportError> {
    let root_obj = payload
        .as_object()
        .ok_or_else(|| ImportError::InvalidFieldType {
            field: "<root>",
            expected: "object",
            got: json_type(payload).to_string(),
        })?;

    let mut puppet = Puppet::new();

    // Puppet-level physics multipliers feed into each SimplePhysics node's
    // effective gravity (inochi2d does this in `getGravity()` at sim time;
    // we pre-fold it into `SimplePhysicsData.gravity` at import so the
    // integrator only ever sees pixel-units gravity).
    let g_scale = root_obj
        .get("physics")
        .and_then(|v| serde_json::from_value::<SchemaPuppetPhysics>(v.clone()).ok())
        .map(|p| {
            p.gravity.unwrap_or(DEFAULT_PUPPET_GRAVITY)
                * p.pixels_per_meter
                    .unwrap_or(DEFAULT_PUPPET_PIXELS_PER_METER)
        })
        .unwrap_or(DEFAULT_GRAVITY_SCALE);

    if let Some(nodes) = root_obj.get("nodes") {
        let parent = puppet.root();
        load_subtree(nodes, parent, &mut puppet, g_scale)?;
    }

    bake_mesh_groups(&mut puppet);

    // Decode textures into the canonical straight-alpha sRGB
    // `PuppetTexture` representation (inochi2d's premultiply-in-sRGB
    // convention is unwound during decode so every consumer downstream
    // sees the same bytes), then crop each to its opaque bounding box and
    // rewrite part UVs. Runs after the node walk because the crop needs
    // every part's mesh UVs and albedo slot.
    let puppet_textures =
        super::alpha_crop::crop_textures(&mut puppet, &textures, texture_halvings)?;
    puppet.set_textures(puppet_textures);

    if let Some(params_value) = root_obj.get("param") {
        let params_json = params_value
            .as_array()
            .ok_or_else(|| ImportError::InvalidFieldType {
                field: "param",
                expected: "array",
                got: json_type(params_value).to_string(),
            })?;
        let params = convert_params_from_json(params_json, &puppet);
        puppet.set_params(params);
    }

    if let Some(anim_json) = root_obj.get("animations") {
        let anims = convert_animations(anim_json);
        puppet.set_animations(anims);
    }

    Ok(puppet)
}

fn json_type(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

pub(crate) fn load_subtree(
    root: &serde_json::Value,
    parent: NodeIdx,
    puppet: &mut Puppet,
    g_scale: f32,
) -> Result<NodeIdx, ImportError> {
    let root_id = parse_and_insert_value(root, parent, puppet, g_scale)?;

    let mut stack: Vec<(&serde_json::Value, NodeIdx)> = Vec::new();
    push_children_rev(root, root_id, &mut stack);

    while let Some((value, parent_id)) = stack.pop() {
        let id = parse_and_insert_value(value, parent_id, puppet, g_scale)?;
        push_children_rev(value, id, &mut stack);
    }
    Ok(root_id)
}

fn push_children_rev<'a>(
    value: &'a serde_json::Value,
    parent: NodeIdx,
    stack: &mut Vec<(&'a serde_json::Value, NodeIdx)>,
) {
    let Some(children) = value.get("children").and_then(|v| v.as_array()) else {
        return;
    };
    for child in children.iter().rev() {
        stack.push((child, parent));
    }
}

fn parse_and_insert_value(
    value: &serde_json::Value,
    parent: NodeIdx,
    puppet: &mut Puppet,
    g_scale: f32,
) -> Result<NodeIdx, ImportError> {
    let mut node = parse_node_shallow(value)?;
    node.children = Vec::new();
    insert_node(node, parent, puppet, g_scale)
}

/// Deserialize a single node's fields without descending into children.
/// Children stay as the underlying JSON Value, walked iteratively by
/// `load_subtree`. A non-object value folds to a default SchemaNode —
/// matches legacy behavior so malformed nodes degrade to empty rather
/// than aborting the whole import.
fn parse_node_shallow(value: &serde_json::Value) -> Result<SchemaNode, ImportError> {
    let mut node: SchemaNode = SchemaNode::default();
    let serde_json::Value::Object(map) = value else {
        return Ok(node);
    };
    let mut sanitized = serde_json::Map::with_capacity(map.len());
    for (k, v) in map {
        if k == "children" {
            continue;
        }
        sanitized.insert(k.clone(), v.clone());
    }
    if let Ok(parsed) = serde_json::from_value::<SchemaNode>(serde_json::Value::Object(sanitized)) {
        node = parsed;
    }
    Ok(node)
}

fn insert_node(
    schema: SchemaNode,
    parent: NodeIdx,
    puppet: &mut Puppet,
    g_scale: f32,
) -> Result<NodeIdx, ImportError> {
    let uuid = schema.uuid;
    let name = schema.name.clone().unwrap_or_default();
    // Inochi2D 0.8.6: enabled defaults to true when absent.
    let enabled = schema.enabled.unwrap_or(true);
    let transform = convert_transform(schema.transform.as_ref());
    // Inochi2D 0.8.6: zsort defaults to 0 when absent.
    let z_order = reflect_z(schema.zsort.unwrap_or(0.0));

    let kind = convert_node_kind(&schema, &transform, g_scale)?;

    let node = Node {
        name,
        enabled,
        base_transform: transform,
        base_z_order: z_order,
        transform,
        z_order,
        // Inochi2D 0.8.6: lockToRoot defaults to false when absent.
        lock_to_root: schema.lock_to_root.unwrap_or(false),
        kind,
    };
    Ok(puppet.insert_child(parent, node, uuid))
}

fn convert_node_kind(
    schema: &SchemaNode,
    transform: &Transform,
    g_scale: f32,
) -> Result<NodeKind, ImportError> {
    // Inochi2D 0.8.6: missing/unknown type falls through to Empty (container node).
    Ok(match schema.ty.as_deref().unwrap_or("") {
        "Part" => NodeKind::Part(Box::new(convert_part(schema)?)),
        "Composite" => NodeKind::Composite(Box::new(convert_composite(schema)?)),
        "MeshGroup" => NodeKind::MeshGroup(Box::new(convert_mesh_group(schema)?)),
        "SimplePhysics" => {
            NodeKind::SimplePhysics(Box::new(convert_simple_physics(schema, transform, g_scale)))
        }
        _ => NodeKind::Empty,
    })
}

fn convert_part(schema: &SchemaNode) -> Result<PartData, ImportError> {
    // Inochi2D 0.8.6: missing/empty textures array leaves albedo at slot 0
    // (matches inox2d behavior; the renderer tolerates an unmapped slot).
    // An index outside u32 (negative sentinel or oversized) becomes
    // u32::MAX, which the renderer's slot-count guard culls — a wrapping
    // cast could alias a real slot instead.
    let albedo_texture = TextureId(match schema.textures.first() {
        None => 0,
        Some(&t) => u32::try_from(t).unwrap_or(u32::MAX),
    });
    // Slots 1 (emissive) and 2 (bumpmap) are intentionally ignored: catchlight
    // renders albedo only. They'd be re-imported when a lighting pass exists.
    let mesh = schema
        .mesh
        .as_ref()
        .map(convert_mesh)
        .transpose()?
        .unwrap_or_default();
    // Inochi2D 0.8.6: opacity defaults to 1 (fully opaque) when absent.
    let opacity = schema.opacity.unwrap_or(1.0);
    let blend_mode = convert_blend_mode(schema.blend_mode.as_deref())?;
    let tint = convert_vec3(&schema.tint, Vec3::ONE);
    let screen_tint = convert_vec3(&schema.screen_tint, Vec3::ZERO);
    let masks = convert_masks(schema.masks.as_deref().unwrap_or(&[]));
    // Inochi2D 0.8.6: maskThreshold defaults to 0.5 when absent.
    let mask_threshold = schema.mask_threshold.unwrap_or(0.5);
    let deform_stack = DeformStack::new(mesh.vertices.len());

    Ok(PartData {
        mesh,
        albedo_texture,
        opacity,
        base_opacity: opacity,
        tint,
        base_tint: tint,
        screen_tint,
        base_screen_tint: screen_tint,
        blend_mode,
        masks,
        mask_threshold,
        deform_stack,
    })
}

fn convert_composite(schema: &SchemaNode) -> Result<CompositeData, ImportError> {
    // Inochi2D 0.8.6: opacity defaults to 1 when absent.
    let opacity = schema.opacity.unwrap_or(1.0);
    Ok(CompositeData {
        opacity,
        base_opacity: opacity,
        tint: convert_vec3(&schema.tint, Vec3::ONE),
        base_tint: convert_vec3(&schema.tint, Vec3::ONE),
        screen_tint: convert_vec3(&schema.screen_tint, Vec3::ZERO),
        base_screen_tint: convert_vec3(&schema.screen_tint, Vec3::ZERO),
        blend_mode: convert_blend_mode(schema.blend_mode.as_deref())?,
        masks: convert_masks(schema.masks.as_deref().unwrap_or(&[])),
        propagate_mesh_group: schema.propagate_meshgroup.unwrap_or(true),
        // Inochi2D 0.8.6: maskThreshold defaults to 0.5 when absent.
        mask_threshold: schema.mask_threshold.unwrap_or(0.5),
    })
}

fn convert_mesh_group(schema: &SchemaNode) -> Result<MeshGroupData, ImportError> {
    let mesh = schema
        .mesh
        .as_ref()
        .map(convert_mesh)
        .transpose()?
        .unwrap_or_default();
    let deform_stack = DeformStack::new(mesh.vertices.len());
    schema.log_dropped_mesh_group_color();
    Ok(MeshGroupData {
        mesh,
        // Inochi2D 0.8.6: dynamic_deformation / translate_children default false.
        dynamic: schema.dynamic_deformation.unwrap_or(false),
        translate_children: schema.translate_children.unwrap_or(false),
        deform_stack,
        bindings: MeshGroupBindings::default(),
        bitmap: None,
    })
}

fn convert_simple_physics(
    schema: &SchemaNode,
    transform: &Transform,
    g_scale: f32,
) -> SimplePhysicsData {
    // Inochi2D 0.8.6: missing/unknown model_type and map_mode fall through to
    // PhysicsModel::default() (Pendulum) / PhysicsParamMapMode::default().
    let model = schema
        .model_type
        .as_deref()
        .and_then(PhysicsModel::from_str)
        .unwrap_or_default();
    let map_mode = schema
        .map_mode
        .as_deref()
        .and_then(PhysicsParamMapMode::from_str)
        .unwrap_or_default();
    // Inochi2D 0.8.6 SimplePhysics spec defaults when field absent:
    //   gravity 1, length 100, frequency 1, angle_damping 0.5,
    //   length_damping 0.5, local_only false. `g_scale` folds in the
    //   puppet-level pixelsPerMeter × gravity (default 1000 × 9.8 = 9800).
    let gravity = schema.gravity.unwrap_or(1.0) * g_scale;
    let length = schema.length.unwrap_or(100.0);
    let frequency = schema.frequency.unwrap_or(1.0);
    let angle_damping = schema.angle_damping.unwrap_or(0.5);
    let length_damping = schema.length_damping.unwrap_or(0.5);
    let output_scale = convert_vec2(&schema.output_scale, Vec2::ONE);
    let anchor = Vec2::new(transform.translation.x, transform.translation.y);

    SimplePhysicsData {
        model,
        map_mode,
        local_only: schema.local_only.unwrap_or(false),
        target_param_id: schema.param,
        gravity,
        length,
        frequency,
        angle_damping,
        length_damping,
        output_scale,
        offset_output_scale: Vec2::ONE,
        bob: anchor + Vec2::new(0.0, length),
        spring_vel: Vec2::ZERO,
        d_angle: 0.0,
        anchor,
        anchor_initialized: false,
    }
}

fn convert_transform(t: Option<&SchemaTransform>) -> Transform {
    let Some(t) = t else {
        return Transform::default();
    };
    let mut transform = Transform {
        translation: convert_vec3(&t.trans, Vec3::ZERO),
        rotation: convert_vec3(&t.rot, Vec3::ZERO),
        scale: convert_vec2(&t.scale, Vec2::ONE),
    };
    reflect_transform_y(&mut transform);
    transform
}

/// inochi2d authors in a Y-down frame; catchlight is Y-up. Reflect across the
/// X axis: negate translation Y and the two Euler angles a Y-flip inverts
/// (rotation about X and Z). Rotation about Y and scale are unchanged.
fn reflect_transform_y(t: &mut Transform) {
    t.translation.y = -t.translation.y;
    t.rotation.x = -t.rotation.x;
    t.rotation.z = -t.rotation.z;
}

/// Build a `Mesh` from an untrusted schema mesh, establishing the
/// invariant the deform/runtime paths rely on: every triangle index is
/// in range for `vertices`, and (when present) `uvs` pair 1:1 with
/// vertices. Rejecting a malformed mesh here keeps those downstream
/// per-frame loops from having to defend against — or panic on — a bad
/// index baked in from the file.
fn convert_mesh(m: &SchemaMesh) -> Result<Mesh, ImportError> {
    let mut vertices: Vec<Vec2> = m
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
    // A non-multiple-of-3 index count is tolerated: the runtime walks
    // whole triangles and ignores any trailing partial one. Only an
    // out-of-range index is rejected, since that is what would otherwise
    // poison the per-frame deform indexing.
    if let Some(&bad) = m.indices.iter().find(|&&i| i as usize >= vertices.len()) {
        return Err(ImportError::MalformedPayload(format!(
            "mesh triangle index {bad} out of range for {} vertices",
            vertices.len()
        )));
    }
    let indices = MeshIndices::from_usize_iter(m.indices.iter().map(|&n| n as usize));
    let mut origin = convert_vec2(&m.origin, Vec2::ZERO);
    // Reflect mesh geometry into catchlight's Y-up frame (see reflect_transform_y).
    // UVs are texture space and stay as authored.
    for v in &mut vertices {
        v.y = -v.y;
    }
    origin.y = -origin.y;
    Ok(Mesh::new(vertices, uvs, indices, origin))
}

fn convert_masks(masks: &[SchemaMask]) -> Vec<MaskBinding> {
    masks
        .iter()
        .filter_map(|m| {
            let source_uuid = m.source?;
            let mode = match m.mode.as_deref() {
                Some("DodgeMask") => MaskMode::DodgeMask,
                _ => MaskMode::Mask,
            };
            Some(MaskBinding { source_uuid, mode })
        })
        .collect()
}

fn convert_blend_mode(s: Option<&str>) -> Result<BlendMode, ImportError> {
    match s {
        None => Ok(BlendMode::default()),
        Some(name) => BlendMode::from_name(name)
            .ok_or_else(|| ImportError::UnknownBlendMode(name.to_string())),
    }
}

// Inochi2D 0.8.6: vector fields (trans/rot/scale/tint/screenTint/min/max/...)
// fall back to the spec default per missing component (white tint, zero
// translation, unit scale, ...). Shorter-than-expected arrays fill the rest.
fn convert_vec2(src: &[f32], default: Vec2) -> Vec2 {
    Vec2::new(
        src.first().copied().unwrap_or(default.x),
        src.get(1).copied().unwrap_or(default.y),
    )
}

fn convert_vec3(src: &[f32], default: Vec3) -> Vec3 {
    Vec3::new(
        src.first().copied().unwrap_or(default.x),
        src.get(1).copied().unwrap_or(default.y),
        src.get(2).copied().unwrap_or(default.z),
    )
}

pub(crate) fn bake_mesh_groups(puppet: &mut Puppet) {
    let mg_ids: Vec<NodeIdx> = puppet
        .iter()
        .filter(|(_, n)| matches!(n.kind, NodeKind::MeshGroup(_)))
        .map(|(id, _)| id)
        .collect();
    let mut transforms = crate::puppet::GlobalTransforms::new();
    puppet.compute_transforms(&mut transforms);
    for id in mg_ids {
        let baked = crate::meshgroup::bake_mesh_group_bindings(puppet, &transforms, id);
        if let Some(node) = puppet.get_mut(id) {
            if let NodeKind::MeshGroup(mg) = &mut node.kind {
                let bitmap = crate::meshgroup::MgTriangleBitmap::build(&mg.mesh);
                mg.bindings = baked;
                mg.bitmap = bitmap;
            }
        }
    }
}

fn convert_interpolate_mode(s: Option<&str>) -> InterpolateMode {
    match s {
        Some("Nearest") => InterpolateMode::Nearest,
        Some("Stepped") => InterpolateMode::Stepped,
        Some("Cubic") => InterpolateMode::Cubic,
        // The reference's Bezier lane branch is itself a placeholder
        // (an eased lerp gated on per-keyframe tension, marked TODO);
        // catchlight maps it to Linear rather than modelling the
        // placeholder.
        Some("Bezier") => InterpolateMode::Linear,
        _ => InterpolateMode::Linear,
    }
}

fn convert_params_from_json(vals: &[serde_json::Value], puppet: &Puppet) -> Vec<Param> {
    let mut params: Vec<Param> = vals
        .iter()
        .filter_map(|v| {
            let schema: SchemaParam = serde_json::from_value(v.clone()).ok()?;
            convert_param(&schema, puppet)
        })
        .collect();
    // nijigenerate can split one logical param into two entries sharing a
    // uuid (the reference rig's palms: transform bindings in one entry, deform in the
    // other). The reference keeps a value per Parameter object and
    // resolves uuid references first-match, so a later duplicate is never
    // driven and forever evaluates at its own defaults. Renumbering it to
    // a fresh uuid that nothing references reproduces exactly that, while
    // keeping Puppet's uuid-keyed param storage sound.
    // `used` holds every original uuid up front (plus each fresh
    // assignment), so a fresh uuid can never collide with a not-yet-walked
    // entry — without this, a `next` that wraps past u32::MAX could steal
    // a later param's uuid and redirect the driver targeting it.
    let mut used: std::collections::HashSet<u32> = params.iter().map(|p| p.id).collect();
    let mut walked: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut next = params.iter().map(|p| p.id).max().unwrap_or(0);
    for p in &mut params {
        if walked.insert(p.id) {
            continue;
        }
        next = next.wrapping_add(1);
        while !used.insert(next) {
            next = next.wrapping_add(1);
        }
        tracing::warn!(
            "duplicate param UUID {} ({:?}); renumbered to {}",
            p.id,
            p.name,
            next
        );
        p.id = next;
    }
    params
}

fn convert_param(p: &SchemaParam, puppet: &Puppet) -> Option<Param> {
    let id = p.uuid?;
    let name = p.name.clone().unwrap_or_default();
    // Inochi2D 0.8.6: is_vec2 defaults to false (scalar param).
    let is_vec2 = p.is_vec2.unwrap_or(false);
    let min = convert_vec2(&p.min, Vec2::ZERO);
    let max = convert_vec2(&p.max, Vec2::ONE);
    let defaults = convert_vec2(&p.defaults, Vec2::ZERO);

    // Inochi2D defaults axisPoints to [[0,1],[0,1]]; only non-vec2
    // params collapse the y axis to a single stop (param/package.d).
    let default_y = || if is_vec2 { vec![0.0, 1.0] } else { vec![0.0] };
    let (mut axis_points_x, mut axis_points_y) = match p.axis_points.as_ref() {
        Some(axes) => {
            let x = axes.first().cloned().unwrap_or_else(|| vec![0.0, 1.0]);
            let y = axes.get(1).cloned().unwrap_or_else(default_y);
            (x, y)
        }
        None => (vec![0.0, 1.0], default_y()),
    };
    // A zero-point axis can't be interpolated and would make apply a
    // silent no-op (or, without the guard in Param::apply, an index
    // panic). Default a degenerate axis to a single stop so the param
    // still drives its bindings.
    if axis_points_x.is_empty() {
        axis_points_x = vec![0.0];
    }
    if axis_points_y.is_empty() {
        axis_points_y = vec![0.0];
    }

    let bindings = p
        .bindings
        .iter()
        .filter_map(|b| convert_binding(b, puppet))
        .filter(|b| !binding_is_all_zero(&b.values))
        .collect();

    Some(Param {
        id,
        name,
        is_vec2,
        min,
        max,
        defaults,
        axis_points_x,
        axis_points_y,
        bindings,
    })
}

fn convert_binding(b: &SchemaBinding, puppet: &Puppet) -> Option<Binding> {
    let node_uuid = b.node?;
    let node = puppet.node_for_uuid(node_uuid)?;
    // Missing param_name falls through the match below to `_ => None`, which
    // drops the binding. Treating absence as "" mirrors that.
    let kind = b.param_name.as_deref().unwrap_or("");
    // A mesh group is never drawn and carries no colour, so a colour binding on
    // one has nowhere to land. Drop it here rather than write it out: the `.clp`
    // loader rejects that shape outright.
    if source_binding_is_color(kind)
        && matches!(
            puppet.get(node).map(|n| &n.kind),
            Some(NodeKind::MeshGroup(_))
        )
    {
        tracing::debug!(
            "dropping {:?} binding on mesh group node {}: a mesh group is never drawn",
            kind,
            node_uuid
        );
        return None;
    }
    let mode = convert_interpolate_mode(b.interpolate_mode.as_deref());
    let values_json = b.values.as_ref()?;

    let mut values = match kind {
        "deform" => parse_matrix_vec2_list(values_json).map(BindingValues::Deform),
        "zSort" => parse_matrix_f32(values_json).map(BindingValues::ZOrder),
        "transform.t.x" => parse_matrix_f32(values_json).map(BindingValues::TransformTX),
        "transform.t.y" => parse_matrix_f32(values_json).map(BindingValues::TransformTY),
        "transform.s.x" => parse_matrix_f32(values_json).map(BindingValues::TransformSX),
        "transform.s.y" => parse_matrix_f32(values_json).map(BindingValues::TransformSY),
        "transform.r.x" => parse_matrix_f32(values_json).map(BindingValues::TransformRX),
        "transform.r.y" => parse_matrix_f32(values_json).map(BindingValues::TransformRY),
        "transform.r.z" => parse_matrix_f32(values_json).map(BindingValues::TransformRZ),
        "opacity" => parse_matrix_f32(values_json).map(BindingValues::Opacity),
        "tint.r" => parse_matrix_f32(values_json).map(BindingValues::TintR),
        "tint.g" => parse_matrix_f32(values_json).map(BindingValues::TintG),
        "tint.b" => parse_matrix_f32(values_json).map(BindingValues::TintB),
        "screenTint.r" => parse_matrix_f32(values_json).map(BindingValues::ScreenTintR),
        "screenTint.g" => parse_matrix_f32(values_json).map(BindingValues::ScreenTintG),
        "screenTint.b" => parse_matrix_f32(values_json).map(BindingValues::ScreenTintB),
        "outputScale.x" => parse_matrix_f32(values_json).map(BindingValues::OutputScaleX),
        "outputScale.y" => parse_matrix_f32(values_json).map(BindingValues::OutputScaleY),
        other => {
            tracing::warn!(
                "dropping unsupported param binding kind {:?} on node {}",
                other,
                node_uuid
            );
            None
        }
    }?;
    reflect_binding_outputs(&mut values);

    Some(Binding::new(node, mode, values))
}

/// Reflect source-space binding outputs at the import boundary: spatial Y into
/// Catchlight's Y-up frame, and zSort into its higher-in-front convention.
fn reflect_binding_outputs(values: &mut BindingValues) {
    match values {
        BindingValues::ZOrder(m) => {
            for value in &mut m.data {
                *value = reflect_z(*value);
            }
        }
        BindingValues::TransformTY(m)
        | BindingValues::TransformRX(m)
        | BindingValues::TransformRZ(m) => {
            for v in &mut m.data {
                *v = -*v;
            }
        }
        BindingValues::Deform(dm) => {
            for offset in dm.offsets_mut() {
                offset.y = -offset.y;
            }
        }
        _ => {}
    }
}

fn reflect_z(value: f32) -> f32 {
    if value == 0.0 {
        0.0
    } else {
        -value
    }
}

/// True when a binding contributes nothing to its target regardless of
/// param value: every additive cell is exactly 0, so bilinear
/// interpolation also yields 0. Conservative — exact `0.0` only, no
/// epsilon, additive kinds only. Multiplicative kinds (Scale, Opacity)
/// are kept even when all-zero because their identity is 1, not 0.
/// Lets `apply_params` skip the binding entirely (and `combine_deforms`
/// doesn't see a reactivated source).
pub(crate) fn binding_is_all_zero(values: &BindingValues) -> bool {
    match values {
        BindingValues::Deform(dm) => dm.offsets().iter().all(|v| *v == Vec2::ZERO),
        BindingValues::ZOrder(m)
        | BindingValues::TransformTX(m)
        | BindingValues::TransformTY(m)
        | BindingValues::TransformRX(m)
        | BindingValues::TransformRY(m)
        | BindingValues::TransformRZ(m)
        | BindingValues::ScreenTintR(m)
        | BindingValues::ScreenTintG(m)
        | BindingValues::ScreenTintB(m) => m.data.iter().all(|v| *v == 0.0),
        BindingValues::TransformSX(_)
        | BindingValues::TransformSY(_)
        | BindingValues::Opacity(_)
        | BindingValues::TintR(_)
        | BindingValues::TintG(_)
        | BindingValues::TintB(_)
        | BindingValues::OutputScaleX(_)
        | BindingValues::OutputScaleY(_) => false,
    }
}

fn parse_matrix_f32(v: &serde_json::Value) -> Option<Matrix<f32>> {
    let rows = v.as_array()?;
    let width = rows.len();
    if width == 0 {
        return None;
    }
    // Inochi2D 0.8.6: empty/non-array first column yields a 0-height matrix
    // (no keyframes); later mismatched cells likewise fall to 0.
    let height = rows[0].as_array().map(|a| a.len()).unwrap_or(0);
    // width comes from the outer array, height from the *first* column only, so
    // `[[...H items...], [], [], ...]` is O(W+H) of JSON that demands O(W*H) of
    // memory. Cap it (and use checked math: usize is 32-bit on wasm32, where
    // the product would otherwise wrap and the writes below index out of range).
    let cells = width.checked_mul(height)?;
    if cells as u64 > MAX_PARAM_GRID_CELLS {
        return None;
    }
    let mut data = vec![0.0f32; cells];
    for (x, col) in rows.iter().enumerate() {
        let arr = col.as_array()?;
        for (y, cell) in arr.iter().enumerate().take(height) {
            data[y * width + x] = cell.as_f64().unwrap_or(0.0) as f32;
        }
    }
    Some(Matrix {
        width,
        height,
        data,
    })
}

fn parse_matrix_vec2_list(v: &serde_json::Value) -> Option<DeformMatrix> {
    let rows = v.as_array()?;
    let width = rows.len();
    if width == 0 {
        return None;
    }
    // Inochi2D 0.8.6: see parse_matrix_f32 comment; 0-height when first
    // column is not an array, missing x/y coords drop individual points.
    let height = rows[0].as_array().map(|a| a.len()).unwrap_or(0);
    // See parse_matrix_f32: same O(W+H) JSON -> O(W*H) allocation amplifier.
    let cells = width.checked_mul(height)?;
    if cells as u64 > MAX_PARAM_GRID_CELLS {
        return None;
    }
    let mut data: Vec<Vec<Vec2>> = vec![Vec::new(); cells];
    for (x, col) in rows.iter().enumerate() {
        let arr = col.as_array()?;
        for (y, cell) in arr.iter().enumerate().take(height) {
            let pts = cell
                .as_array()
                .map(|pts| {
                    pts.iter()
                        .filter_map(|p| {
                            let a = p.as_array()?;
                            Some(Vec2::new(
                                a.first()?.as_f64()? as f32,
                                a.get(1)?.as_f64()? as f32,
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            data[y * width + x] = pts;
        }
    }
    DeformMatrix::from_cells(width, height, data).ok()
}

fn convert_animations(json: &serde_json::Value) -> Vec<Animation> {
    // Inochi2D 0.8.6 ships `animations` as an object keyed by animation name;
    // older / nijigenerate variants ship an array with a "name" field per
    // entry. Accept both; unnamed array entries fall back to `anim_<index>`.
    let entries: Vec<(String, &serde_json::Value)> = if let Some(obj) = json.as_object() {
        obj.iter().map(|(k, v)| (k.clone(), v)).collect()
    } else if let Some(arr) = json.as_array() {
        arr.iter()
            .enumerate()
            .map(|(i, v)| {
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| format!("anim_{}", i));
                (name, v)
            })
            .collect()
    } else {
        return Vec::new();
    };

    entries
        .into_iter()
        .filter_map(|(name, v)| {
            let anim: SchemaAnimation = serde_json::from_value(v.clone()).ok()?;
            Some(convert_animation(name, anim))
        })
        .collect()
}

fn convert_animation(name: String, anim: SchemaAnimation) -> Animation {
    // Inochi2D 0.8.6: timestep defaults to 1/60 (60 fps), length to 0 frames,
    // leadIn/leadOut to -1 (no loop region).
    let timestep = anim.timestep.unwrap_or(1.0 / 60.0);
    let length = anim.length.unwrap_or(0);
    let lead_in = anim.lead_in.unwrap_or(-1);
    let lead_out = anim.lead_out.unwrap_or(-1);
    let lanes = anim
        .lanes
        .into_iter()
        .filter_map(|lane| {
            let param_id = lane.uuid?;
            // Inochi2D 0.8.6: target axis defaults to 0 (x-axis for vec2 params);
            // it encodes the axis as an integer, and anything but 0 means y.
            let axis = match lane.target.unwrap_or(0) {
                0 => crate::params::ParamAxis::X,
                _ => crate::params::ParamAxis::Y,
            };
            let interpolation = convert_interpolate_mode(lane.interpolation.as_deref());
            let mut keyframes: Vec<Keyframe> = lane
                .keyframes
                .into_iter()
                .map(|kf| Keyframe {
                    // Inochi2D 0.8.6: keyframe frame/value default to 0.
                    frame: kf.frame.unwrap_or(0),
                    value: kf.value.unwrap_or(0.0),
                })
                .collect();
            // value_at assumes frame order; the reference stable-sorts on
            // load (animation.d updateFrames).
            keyframes.sort_by_key(|kf| kf.frame);
            Some(AnimationLane {
                param_id,
                axis,
                keyframes,
                interpolation,
            })
        })
        .collect();

    Animation {
        name,
        timestep,
        length,
        lead_in,
        lead_out,
        lanes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::InxModel;
    use serde_json::json;

    #[test]
    fn non_object_root_returns_invalid_field_type() {
        let payload = json!([1, 2, 3]);
        let err = schema_to_puppet(&payload, Vec::new(), 0)
            .err()
            .expect("expected error");
        match err {
            ImportError::InvalidFieldType {
                field, expected, ..
            } => {
                assert_eq!(field, "<root>");
                assert_eq!(expected, "object");
            }
            other => panic!("expected InvalidFieldType, got {:?}", other),
        }
    }

    #[test]
    fn non_array_param_returns_invalid_field_type() {
        let payload = json!({ "param": { "not": "an array" } });
        let err = schema_to_puppet(&payload, Vec::new(), 0)
            .err()
            .expect("expected error");
        match err {
            ImportError::InvalidFieldType {
                field, expected, ..
            } => {
                assert_eq!(field, "param");
                assert_eq!(expected, "array");
            }
            other => panic!("expected InvalidFieldType, got {:?}", other),
        }
    }

    #[test]
    fn wrong_magic_bubbles_up_as_inx_container() {
        let bytes = b"WRONGMAG\0\0\0\0";
        let err = super::super::parse(bytes).err().expect("expected error");
        assert!(
            matches!(err, ImportError::InxContainer(_)),
            "expected InxContainer, got {:?}",
            err
        );
    }

    #[test]
    fn malformed_json_payload_bubbles_up_as_inx_container() {
        let mut data = Vec::new();
        data.extend_from_slice(b"TRNSRTS\0");
        let bad_json = b"{not valid json";
        data.extend_from_slice(&(bad_json.len() as u32).to_be_bytes());
        data.extend_from_slice(bad_json);
        data.extend_from_slice(b"TEX_SECT");
        data.extend_from_slice(&0u32.to_be_bytes());
        let err = super::super::parse(&data).err().expect("expected error");
        assert!(
            matches!(err, ImportError::InxContainer(_)),
            "expected InxContainer, got {:?}",
            err
        );
    }

    #[test]
    fn scalar_root_payload_surfaces_from_inx_model() {
        let mut data = Vec::new();
        data.extend_from_slice(b"TRNSRTS\0");
        let payload = b"42";
        data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
        data.extend_from_slice(b"TEX_SECT");
        data.extend_from_slice(&0u32.to_be_bytes());
        let model = InxModel::parse(std::io::Cursor::new(&data)).unwrap();
        let err = super::super::from_inx_model(&model)
            .err()
            .expect("expected error");
        match err {
            ImportError::InvalidFieldType {
                field, expected, ..
            } => {
                assert_eq!(field, "<root>");
                assert_eq!(expected, "object");
            }
            other => panic!("expected InvalidFieldType, got {:?}", other),
        }
    }

    #[test]
    fn unknown_blend_mode_on_part_reports_unknown_blend_mode() {
        let payload = json!({
            "nodes": {
                "type": "Part",
                "uuid": 1u32,
                "blend_mode": "NotARealMode",
            }
        });
        let err = schema_to_puppet(&payload, Vec::new(), 0)
            .err()
            .expect("expected error");
        match err {
            ImportError::UnknownBlendMode(name) => assert_eq!(name, "NotARealMode"),
            other => panic!("expected UnknownBlendMode, got {:?}", other),
        }
    }

    #[test]
    fn missing_blend_mode_falls_back_to_default_not_error() {
        let payload = json!({
            "nodes": {
                "type": "Part",
                "uuid": 1u32,
            }
        });
        let puppet = schema_to_puppet(&payload, Vec::new(), 0).expect("import succeeds");
        let part = puppet
            .iter()
            .find_map(|(_, n)| match &n.kind {
                NodeKind::Part(p) => Some(p),
                _ => None,
            })
            .expect("part inserted");
        assert_eq!(part.blend_mode, BlendMode::default());
    }

    #[test]
    fn all_zero_deform_binding_is_dropped_at_load() {
        // Build a Param JSON with two bindings on a single Part: one
        // all-zero Deform that should be dropped, and one all-zero
        // TransformTX that should also be dropped. Plus a non-zero
        // Deform that must be kept.
        let payload = json!({
            "nodes": {
                "type": "Part",
                "uuid": 1u32,
                "mesh": {
                    "verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                    "uvs": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                    "indices": [0, 1, 2],
                    "origin": [0.0, 0.0],
                },
            },
            "param": [{
                "uuid": 100u32,
                "name": "p",
                "is_vec2": false,
                "min": [0.0, 0.0],
                "max": [1.0, 1.0],
                "defaults": [0.0, 0.0],
                "axis_points": [[0.0, 1.0], [0.0]],
                "bindings": [
                    {
                        "node": 1u32,
                        "param_name": "deform",
                        "values": [
                            [[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]],
                            [[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]],
                        ],
                    },
                    {
                        "node": 1u32,
                        "param_name": "transform.t.x",
                        "values": [[0.0], [0.0]],
                    },
                    {
                        "node": 1u32,
                        "param_name": "deform",
                        "values": [
                            [[[0.5, 0.0], [0.0, 0.0], [0.0, 0.0]]],
                            [[[0.5, 0.0], [0.0, 0.0], [0.0, 0.0]]],
                        ],
                    },
                ],
            }],
        });
        let puppet = schema_to_puppet(&payload, Vec::new(), 0).expect("import succeeds");
        let params = puppet.params();
        assert_eq!(params.len(), 1);
        assert_eq!(
            params[0].bindings.len(),
            1,
            "all-zero bindings should be filtered, only the non-zero deform survives"
        );
        assert!(matches!(
            params[0].bindings[0].values,
            BindingValues::Deform(_)
        ));
    }

    #[test]
    fn duplicate_uuid_param_is_renumbered_and_never_driven() {
        // Mirrors reference.inx, where nijigenerate split one physics param
        // into two entries sharing a uuid: transform bindings in the
        // first, a deform binding in the second. Driving the original
        // uuid must move the first entry only; the renumbered second
        // entry stays at its own defaults (0.25 here, distinguishing
        // "own defaults" from both "driven" and "zero").
        let payload = json!({
            "nodes": {
                "type": "Part",
                "uuid": 1u32,
                "mesh": {
                    "verts": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                    "uvs": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                    "indices": [0, 1, 2],
                    "origin": [0.0, 0.0],
                },
            },
            "param": [
                {
                    "uuid": 100u32,
                    "name": "Physics - Palm",
                    "is_vec2": false,
                    "min": [0.0, 0.0],
                    "max": [1.0, 1.0],
                    "defaults": [0.0, 0.0],
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": [{
                        "node": 1u32,
                        "param_name": "transform.t.x",
                        "values": [[0.0], [10.0]],
                    }],
                },
                {
                    "uuid": 100u32,
                    "name": "Physics - Palm",
                    "is_vec2": false,
                    "min": [0.0, 0.0],
                    "max": [1.0, 1.0],
                    "defaults": [0.25, 0.0],
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": [{
                        "node": 1u32,
                        "param_name": "deform",
                        "values": [
                            [[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]],
                            [[[8.0, 0.0], [8.0, 0.0], [8.0, 0.0]]],
                        ],
                    }],
                },
            ],
        });
        let mut puppet = schema_to_puppet(&payload, Vec::new(), 0).expect("import succeeds");
        let params = puppet.params();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].id, 100);
        assert_ne!(params[1].id, 100, "duplicate must get a fresh uuid");
        assert_eq!(
            puppet
                .param_by_name("Physics - Palm")
                .expect("param by name")
                .id,
            100,
            "name lookup must resolve to the first entry"
        );

        puppet.set_param_value(100, Vec2::new(1.0, 0.0));
        puppet.reset_dynamic_state();
        puppet.reset_deforms();
        puppet.apply_params();
        puppet.combine_deforms();

        let part_id = puppet.node_for_uuid(1).expect("part node");
        let node = puppet.get(part_id).expect("part node data");
        assert!(
            (node.transform.translation.x - 10.0).abs() < 1e-5,
            "first entry must follow the driven value, got tx={}",
            node.transform.translation.x
        );
        let NodeKind::Part(p) = &node.kind else {
            panic!("node isn't a Part");
        };
        for v in p.deform_stack.combined() {
            assert!(
                (v.x - 2.0).abs() < 1e-5,
                "renumbered entry must evaluate at its own defaults (0.25 * 8.0), got {}",
                v.x
            );
        }
    }

    #[test]
    fn renumbering_never_steals_a_later_params_uuid() {
        // Pathological corner: the max original uuid is u32::MAX, so the
        // fresh-uuid counter wraps to 0 — which a later param already
        // owns. The fresh uuid must skip every original uuid, walked or
        // not; otherwise the later param gets treated as the duplicate
        // and renumbered, silently redirecting any driver targeting it.
        let payload = json!({
            "nodes": {
                "type": "Part",
                "uuid": 1u32,
            },
            "param": [
                { "uuid": 4294967295u32, "name": "a" },
                { "uuid": 4294967295u32, "name": "a dup" },
                { "uuid": 0u32, "name": "b" },
            ],
        });
        let puppet = schema_to_puppet(&payload, Vec::new(), 0).expect("import succeeds");
        let uuids: Vec<u32> = puppet.params().iter().map(|p| p.id).collect();
        assert_eq!(uuids[0], u32::MAX, "first occurrence keeps its uuid");
        assert_eq!(uuids[2], 0, "later param must keep its original uuid");
        assert!(
            uuids[1] != u32::MAX && uuids[1] != 0,
            "fresh uuid must avoid all originals, got {}",
            uuids[1]
        );
    }

    #[test]
    fn puppet_physics_block_scales_simple_physics_gravity() {
        // Mirror inochi2d's `getGravity()`: node.gravity *
        // puppet.physics.gravity * puppet.physics.pixelsPerMeter. With
        // puppet defaults of {gravity=9.8, pixelsPerMeter=1000}, a node
        // with gravity=1 lands at 9800 — not the legacy hardcoded 981.
        let payload = json!({
            "physics": {
                "pixelsPerMeter": 500.0,
                "gravity": 5.0,
            },
            "nodes": {
                "type": "SimplePhysics",
                "uuid": 1u32,
                "gravity": 2.0,
                "length": 100.0,
            }
        });
        let puppet = schema_to_puppet(&payload, Vec::new(), 0).expect("import succeeds");
        let phys = puppet
            .iter()
            .find_map(|(_, n)| match &n.kind {
                NodeKind::SimplePhysics(p) => Some(p.as_ref()),
                _ => None,
            })
            .expect("SimplePhysics node imported");
        // 2 * 5 * 500 = 5000
        assert!(
            (phys.gravity - 5000.0).abs() < 1e-3,
            "expected gravity 5000, got {}",
            phys.gravity,
        );
    }

    #[test]
    fn missing_puppet_physics_falls_back_to_inochi2d_defaults() {
        // No `physics` block → use inochi2d's PuppetPhysics defaults
        // (gravity=9.8, pixelsPerMeter=1000) → final = 1 * 9.8 * 1000 = 9800.
        let payload = json!({
            "nodes": {
                "type": "SimplePhysics",
                "uuid": 1u32,
                "gravity": 1.0,
            }
        });
        let puppet = schema_to_puppet(&payload, Vec::new(), 0).expect("import succeeds");
        let phys = puppet
            .iter()
            .find_map(|(_, n)| match &n.kind {
                NodeKind::SimplePhysics(p) => Some(p.as_ref()),
                _ => None,
            })
            .expect("SimplePhysics node imported");
        assert!(
            (phys.gravity - 9800.0).abs() < 1e-3,
            "expected default gravity 9800, got {}",
            phys.gravity,
        );
        assert!((phys.angle_damping - 0.5).abs() < 1e-6);
        assert!((phys.length_damping - 0.5).abs() < 1e-6);
    }

    #[test]
    fn every_new_blend_mode_round_trips_through_importer() {
        // Covers the 8 variants added in catchlight-2ma. Covers each
        // enum value to guard against a typo in the string table or a
        // new variant being skipped from `from_name` by accident.
        let cases = [
            ("Overlay", BlendMode::Overlay),
            ("ColorBurn", BlendMode::ColorBurn),
            ("LinearBurn", BlendMode::LinearBurn),
            ("Darken", BlendMode::Darken),
            ("Lighten", BlendMode::Lighten),
            ("Add", BlendMode::Add),
            ("Inverse", BlendMode::Inverse),
            ("Subtract", BlendMode::Subtract),
        ];
        for (s, expected) in cases {
            let payload = json!({
                "nodes": {
                    "type": "Part",
                    "uuid": 1u32,
                    "blend_mode": s,
                }
            });
            let puppet = schema_to_puppet(&payload, Vec::new(), 0)
                .unwrap_or_else(|e| panic!("import of {:?} failed: {:?}", s, e));
            let part = puppet
                .iter()
                .find_map(|(_, n)| match &n.kind {
                    NodeKind::Part(p) => Some(p),
                    _ => None,
                })
                .expect("part inserted");
            assert_eq!(part.blend_mode, expected, "mode {}", s);
            assert_eq!(BlendMode::from_name(s), Some(expected));
        }
    }
}
