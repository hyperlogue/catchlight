//! Assembles reflected `.inx` values into the `.clm` a
//! [`Model`](catchlight_core::Model) is read from.
//!
//! Ids are minted only here, from flattening position (`root`, `node-<i>`,
//! `param-<i>` or `param-<i>.x`/`.y`, `tex-<i>`), so two imports of one `.inx`
//! agree byte for byte.

use std::collections::{HashMap, HashSet};

use catchlight_core::extension::{Extension, Payload};
use catchlight_core::formats::clm::{
    extension_hash, ClmBinding, ClmComposite, ClmExtension, ClmExtensionBlob, ClmExtensionMarker,
    ClmFile, ClmMask, ClmMesh, ClmMeshGroup, ClmNode, ClmNodeKind, ClmParam, ClmPart, ClmPayload,
    ClmStructure, ClmTexture, TextureAlpha, TextureEncoding,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::texture::TextureFormat;
use catchlight_core::{Model, NodeId, ParamId, TexId};
use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane, LaneTarget};

use crate::error::ImportError;
use crate::inx::InxModel;
use crate::reflect::{
    axes_of, blend, convert_binding_values, convert_mesh, convert_transform, flatten, interp,
    mask_mode, reflect_z, vec2_arr, vec3_arr, NodeRef,
};
use crate::schema::{
    source_binding_is_color, SchemaAnimation, SchemaAnimationLane, SchemaBinding, SchemaMask,
    SchemaNode, SchemaParam,
};

/// Import a parsed `.inx` into a [`Model`].
pub fn import_inx_model(model: &InxModel) -> Result<Model, ImportError> {
    Ok(Model::from_clm_file(&from_inx_model(model)?)?)
}

/// Parse `.inx` (or `.inp` — one container) bytes and import them.
pub fn import_inx_bytes(bytes: &[u8]) -> Result<Model, ImportError> {
    import_inx_model(&InxModel::parse(std::io::Cursor::new(bytes))?)
}

/// Reflect a parsed `.inx` model into the `.clm` structure it becomes.
pub fn from_inx_model(model: &InxModel) -> Result<ClmFile, ImportError> {
    let obj = model
        .payload
        .as_object()
        .ok_or_else(|| ImportError::MalformedPayload("inx payload root is not an object".into()))?;

    let mut flat: Vec<(SchemaNode, Option<u32>)> = Vec::new();
    let mut uuid_node: HashMap<u32, u32> = HashMap::new();
    match obj.get("nodes") {
        Some(n) => flatten(n, None, &mut flat, &mut uuid_node),
        None => flat.push((SchemaNode::default(), None)),
    }
    let node_ids = node_ids(flat.len())?;

    let schema_params: Vec<SchemaParam> = obj
        .get("param")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| serde_json::from_value::<SchemaParam>(p.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut uuid_param: HashMap<u32, u32> = HashMap::new();
    for (i, p) in schema_params.iter().enumerate() {
        if let Some(uuid) = p.uuid {
            uuid_param.entry(uuid).or_insert(i as u32);
        }
    }
    let slots = param_slots(&schema_params)?;

    let drawn_nodes: Vec<bool> = flat
        .iter()
        .map(|(s, _)| matches!(s.ty.as_deref(), Some("Part" | "Composite")))
        .collect();
    let refs = Refs {
        node_ids: &node_ids,
        uuid_node: &uuid_node,
        uuid_param: &uuid_param,
        slots: &slots,
        drawn_nodes: &drawn_nodes,
        textures: model.textures.len(),
    };

    let mut nodes = Vec::with_capacity(flat.len());
    for (i, (s, parent)) in flat.iter().enumerate() {
        nodes.push(convert_node(s, i, *parent, &refs)?);
    }

    let mut params = Vec::with_capacity(slots.len());
    let mut bindings = Vec::new();
    for (i, p) in schema_params.iter().enumerate() {
        let (axis_x, axis_y) = axes_of(p);
        let name = p.name.clone().unwrap_or_default();
        let min = vec2_arr(&p.min, [0.0, 0.0]);
        let max = vec2_arr(&p.max, [1.0, 1.0]);
        let defaults = vec2_arr(&p.defaults, [0.0, 0.0]);
        match &slots[i] {
            Slot::One(id) => {
                let (min, max) = usable_range(id, &name, min[0], max[0])?;
                params.push(ClmParam {
                    id: id.clone(),
                    name,
                    min,
                    max,
                    default: defaults[0],
                });
            }
            Slot::Pair(x, y) => {
                let (name_x, name_y) = (format!("{name}.x"), format!("{name}.y"));
                let (min_x, max_x) = usable_range(x, &name_x, min[0], max[0])?;
                let (min_y, max_y) = usable_range(y, &name_y, min[1], max[1])?;
                params.push(ClmParam {
                    id: x.clone(),
                    name: name_x,
                    min: min_x,
                    max: max_x,
                    default: defaults[0],
                });
                params.push(ClmParam {
                    id: y.clone(),
                    name: name_y,
                    min: min_y,
                    max: max_y,
                    default: defaults[1],
                });
            }
        }
        bindings.extend(
            p.bindings
                .iter()
                .filter_map(|b| convert_binding(b, &slots[i], &nodes, &refs, &axis_x, &axis_y)),
        );
    }

    // Ids follow source texture order, so dropping one never renumbers the rest.
    let drawn: HashSet<&TexId> = nodes
        .iter()
        .filter_map(|n| match &n.kind {
            ClmNodeKind::Part(p) => p.albedo.as_ref(),
            _ => None,
        })
        .collect();
    let mut textures = Vec::with_capacity(model.textures.len());
    for (i, t) in model.textures.iter().enumerate() {
        let id = tex_id(i as u32)?;
        if !drawn.contains(&id) {
            tracing::warn!("dropping texture {id}: no part draws it");
            continue;
        }
        textures.push(ClmTexture {
            id,
            encoding: match t.format {
                TextureFormat::Png => TextureEncoding::Png,
                TextureFormat::Tga => TextureEncoding::Tga,
            },
            alpha: TextureAlpha::PremultipliedSrgb,
            data: t.data.to_vec(),
        });
    }

    let mut file = ClmFile {
        doc: ClmStructure {
            nodes,
            params,
            bindings,
            welds: Vec::new(),
            extensions: Default::default(),
        },
        textures,
        extensions: Vec::new(),
        unknown_sections: Vec::new(),
    };
    let clips = convert_animations(obj.get("animations"), &refs);
    if !clips.is_empty() {
        insert_extension(&mut file, &Animations::from_clips(clips))?;
    }
    Ok(file)
}

enum Slot {
    One(ParamId),
    Pair(ParamId, ParamId),
}

impl Slot {
    fn params(&self) -> Vec<ParamId> {
        match self {
            Slot::One(id) => vec![id.clone()],
            Slot::Pair(x, y) => vec![x.clone(), y.clone()],
        }
    }

    /// inochi2d reads a lane `target` of 0 as x and anything else as y.
    fn axis(&self, target: u8) -> Option<&ParamId> {
        match (self, target) {
            (Slot::One(id), 0) => Some(id),
            (Slot::One(_), _) => None,
            (Slot::Pair(x, _), 0) => Some(x),
            (Slot::Pair(_, y), _) => Some(y),
        }
    }

    fn head(&self) -> &ParamId {
        match self {
            Slot::One(id) | Slot::Pair(id, _) => id,
        }
    }
}

struct Refs<'a> {
    node_ids: &'a [NodeId],
    uuid_node: &'a HashMap<u32, u32>,
    uuid_param: &'a HashMap<u32, u32>,
    slots: &'a [Slot],
    /// Parallel to `node_ids`.
    drawn_nodes: &'a [bool],
    textures: usize,
}

impl Refs<'_> {
    fn node_of_uuid(&self, uuid: u32) -> Option<&NodeId> {
        self.node_ids.get(*self.uuid_node.get(&uuid)? as usize)
    }

    fn slot_of_uuid(&self, uuid: u32) -> Option<&Slot> {
        self.slots.get(*self.uuid_param.get(&uuid)? as usize)
    }

    fn draws(&self, uuid: u32) -> bool {
        self.uuid_node
            .get(&uuid)
            .and_then(|&i| self.drawn_nodes.get(i as usize))
            .copied()
            .unwrap_or(false)
    }
}

fn node_ids(count: usize) -> Result<Vec<NodeId>, ImportError> {
    (0..count)
        .map(|i| {
            Ok(if i == 0 {
                NodeId::new("root")?
            } else {
                NodeId::new(format!("node-{i}"))?
            })
        })
        .collect()
}

fn param_slots(params: &[SchemaParam]) -> Result<Vec<Slot>, ImportError> {
    params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            Ok(if p.is_vec2.unwrap_or(false) {
                Slot::Pair(
                    ParamId::new(format!("param-{i}.x"))?,
                    ParamId::new(format!("param-{i}.y"))?,
                )
            } else {
                Slot::One(ParamId::new(format!("param-{i}"))?)
            })
        })
        .collect()
}

fn tex_id(i: u32) -> Result<TexId, ImportError> {
    Ok(TexId::new(format!("tex-{i}"))?)
}

/// Widening a collapsed or non-finite range cannot move the rig, since the
/// source param cannot move either. What inochi2d does with `min > max` is
/// unclear, so that is refused rather than guessed.
fn usable_range(id: &ParamId, name: &str, min: f32, max: f32) -> Result<(f32, f32), ImportError> {
    if min.is_finite() && max.is_finite() {
        if min < max {
            return Ok((min, max));
        }
        if min > max {
            return Err(ImportError::InvertedParamRange {
                id: id.to_string(),
                name: name.to_string(),
                min,
                max,
            });
        }
    }
    let base = if min.is_finite() {
        min
    } else if max.is_finite() {
        max
    } else {
        0.0
    };
    tracing::warn!(
        "param {id} ({name:?}): the range {min}..{max} cannot be posed against; widening it to \
         {base}..{}",
        base + 1.0
    );
    Ok((base, base + 1.0))
}

fn mesh_of(kind: &ClmNodeKind) -> Option<&ClmMesh> {
    match kind {
        ClmNodeKind::Part(p) => Some(&p.mesh),
        ClmNodeKind::MeshGroup(mg) => Some(&mg.mesh),
        _ => None,
    }
}

fn convert_node(
    s: &SchemaNode,
    index: usize,
    parent: Option<u32>,
    refs: &Refs<'_>,
) -> Result<ClmNode, ImportError> {
    let id = refs
        .node_ids
        .get(index)
        .ok_or(ImportError::MissingField("node"))?;
    let named = NodeRef {
        id,
        name: s.name.as_deref().unwrap_or_default(),
    };
    Ok(ClmNode {
        id: id.clone(),
        parent: match parent {
            Some(p) => Some(
                refs.node_ids
                    .get(p as usize)
                    .ok_or(ImportError::MissingField("parent"))?
                    .clone(),
            ),
            None => None,
        },
        name: s.name.clone().unwrap_or_default(),
        enabled: s.enabled.unwrap_or(true),
        z_order: reflect_z(s.zsort.unwrap_or(0.0)),
        transform: convert_transform(s.transform.as_ref()),
        lock_to_root: s.lock_to_root.unwrap_or(false),
        kind: convert_node_kind(s, named, refs)?,
    })
}

fn convert_node_kind(
    s: &SchemaNode,
    node: NodeRef<'_>,
    refs: &Refs<'_>,
) -> Result<ClmNodeKind, ImportError> {
    Ok(match s.ty.as_deref().unwrap_or("") {
        "Part" => ClmNodeKind::Part(convert_part(s, node, refs)?),
        "Composite" => ClmNodeKind::Composite(convert_composite(s, node, refs)?),
        "MeshGroup" => ClmNodeKind::MeshGroup(convert_mesh_group(s, node)?),
        _ => ClmNodeKind::Group,
    })
}

fn convert_masks(masks: &[SchemaMask], node: NodeRef<'_>, refs: &Refs<'_>) -> Vec<ClmMask> {
    masks
        .iter()
        .filter_map(|m| {
            let source = m.source?;
            // A mesh group or plain node draws nothing in either runtime, and
            // the `.clm` reader takes only a part or composite as mask source.
            if !refs.draws(source) {
                tracing::warn!(
                    "node {} ({:?}): dropping the mask on source {source}, which is not drawn",
                    node.id,
                    node.name,
                );
                return None;
            }
            Some(ClmMask {
                source: refs.node_of_uuid(source)?.clone(),
                mode: mask_mode(m.mode.as_deref()),
            })
        })
        .collect()
}

fn convert_part(
    s: &SchemaNode,
    node: NodeRef<'_>,
    refs: &Refs<'_>,
) -> Result<ClmPart, ImportError> {
    // inochi2d draws slot 0 for a part with no `textures` array, and treats
    // `uint.max` as an empty slot.
    let albedo = match s.textures.first() {
        None if refs.textures > 0 => {
            tracing::warn!(
                "part {} ({:?}) names no texture; giving it tex-0, which is what the source \
                 runtime draws",
                node.id,
                node.name,
            );
            Some(tex_id(0)?)
        }
        None => {
            tracing::warn!(
                "part {} ({:?}) names no texture and the rig carries none; it draws nothing",
                node.id,
                node.name,
            );
            None
        }
        Some(&v) if v == i64::from(u32::MAX) => {
            tracing::warn!(
                "part {} ({:?}) marks its texture slot empty; it draws nothing",
                node.id,
                node.name,
            );
            None
        }
        Some(&v) => match u32::try_from(v) {
            Ok(slot) if (slot as usize) < refs.textures => Some(tex_id(slot)?),
            _ => {
                return Err(ImportError::TextureOutOfRange {
                    id: node.id.to_string(),
                    name: node.name.to_string(),
                    slot: v,
                    count: refs.textures,
                })
            }
        },
    };
    Ok(ClmPart {
        mesh: convert_mesh(s.mesh.as_ref(), node, true)?,
        albedo,
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), node, refs),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
        slots: Vec::new(),
    })
}

fn convert_composite(
    s: &SchemaNode,
    node: NodeRef<'_>,
    refs: &Refs<'_>,
) -> Result<ClmComposite, ImportError> {
    Ok(ClmComposite {
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), node, refs),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
        // Absent means `false`: inochi2d v0.8.7's `deserializeFromFghj`
        // else-branch overrides the field initializer's `true`.
        propagate_meshgroup: s.propagate_meshgroup.unwrap_or(false),
    })
}

fn convert_mesh_group(s: &SchemaNode, node: NodeRef<'_>) -> Result<ClmMeshGroup, ImportError> {
    s.log_dropped_mesh_group_color();
    Ok(ClmMeshGroup {
        // Never drawn, so inochi2d authors no UVs for it.
        mesh: convert_mesh(s.mesh.as_ref(), node, false)?,
        translate_children: s.translate_children.unwrap_or(false),
    })
}

fn convert_binding(
    b: &SchemaBinding,
    slot: &Slot,
    nodes: &[ClmNode],
    refs: &Refs<'_>,
    axis_x: &[f32],
    axis_y: &[f32],
) -> Option<ClmBinding> {
    let node = refs.node_of_uuid(b.node?)?;
    let target = nodes.iter().find(|n| &n.id == node);
    let values_json = b.values.as_ref()?;
    let kind = b.param_name.as_deref().unwrap_or("");
    // The `.clm` loader rejects a colour binding on a mesh group.
    if source_binding_is_color(kind)
        && matches!(target.map(|n| &n.kind), Some(ClmNodeKind::MeshGroup(_)))
    {
        tracing::debug!(
            "dropping {:?} binding on mesh group node {}: a mesh group is never drawn",
            kind,
            node
        );
        return None;
    }
    let named = NodeRef {
        id: node,
        name: target.map_or("", |n| n.name.as_str()),
    };
    let deform_len = target
        .and_then(|n| mesh_of(&n.kind))
        .map_or(0, |m| m.verts.len());
    // Moves nothing, and the `.clm` loader refuses it (`NotMeshed`).
    if kind == "deform" && deform_len == 0 {
        tracing::warn!(
            "dropping the deform binding on node {} ({:?}): the node has no mesh vertices to move",
            named.id,
            named.name,
        );
        return None;
    }
    let values = convert_binding_values(
        kind,
        values_json,
        b.is_set.as_deref(),
        axis_x,
        axis_y,
        named,
        deform_len,
    )?;
    Some(ClmBinding {
        params: slot.params(),
        key_positions: match slot {
            Slot::One(_) => vec![axis_x.to_vec()],
            Slot::Pair(_, _) => vec![axis_x.to_vec(), axis_y.to_vec()],
        },
        node: node.clone(),
        interpolate_mode: interp(b.interpolate_mode.as_deref()),
        values,
    })
}

/// inochi2d 0.8.6 writes `animations` as an object keyed by clip name; older
/// and nijigenerate variants write an array with a `name` per entry.
fn convert_animations(json: Option<&serde_json::Value>, refs: &Refs<'_>) -> Vec<Clip> {
    let entries: Vec<(String, &serde_json::Value)> = match json {
        Some(serde_json::Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let name = v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| format!("anim-{i}"));
                (name, v)
            })
            .collect(),
        _ => return Vec::new(),
    };
    entries
        .into_iter()
        .filter_map(|(name, v)| {
            let anim: SchemaAnimation = serde_json::from_value(v.clone()).ok()?;
            Some(convert_animation(name, &anim, refs))
        })
        .collect()
}

fn convert_animation(name: String, anim: &SchemaAnimation, refs: &Refs<'_>) -> Clip {
    let lanes = anim
        .lanes
        .iter()
        .filter_map(|lane| convert_animation_lane(&name, lane, refs))
        .collect();
    // inochi2d marks an absent lead-in or lead-out with -1.
    let frame = |f: Option<i32>| f.and_then(|f| u32::try_from(f).ok());
    Clip {
        // inochi2d's default.
        timestep: anim.timestep.unwrap_or(1.0 / 60.0),
        length: frame(anim.length).unwrap_or(0),
        lead_in: frame(anim.lead_in),
        lead_out: frame(anim.lead_out),
        name,
        lanes,
    }
}

fn convert_animation_lane(
    animation: &str,
    lane: &SchemaAnimationLane,
    refs: &Refs<'_>,
) -> Option<Lane> {
    let slot = match lane.uuid.and_then(|uuid| refs.slot_of_uuid(uuid)) {
        Some(slot) => slot,
        None => {
            tracing::warn!(
                "animation {animation:?}: dropping a lane whose param {:?} is not in this rig",
                lane.uuid
            );
            return None;
        }
    };
    let target = lane.target.unwrap_or(0);
    let Some(param) = slot.axis(target) else {
        tracing::warn!(
            "animation {animation:?}: dropping a lane on axis {target} of param {}, \
             which has no such axis",
            slot.head()
        );
        return None;
    };
    let mut keyframes: Vec<Keyframe> = Vec::with_capacity(lane.keyframes.len());
    for k in &lane.keyframes {
        let frame = k.frame.unwrap_or(0);
        let Ok(frame) = u32::try_from(frame) else {
            tracing::warn!(
                "animation {animation:?}: dropping a keyframe at frame {frame}, before the clip \
                 starts"
            );
            continue;
        };
        keyframes.push(Keyframe {
            frame,
            value: k.value.unwrap_or(0.0),
        });
    }
    Some(Lane::new(
        LaneTarget::Param(param.clone()),
        lane_interp(lane.interpolation.as_deref(), animation),
        keyframes,
    ))
}

/// Files `data` the way a model saves it, so the file carries it as stored.
fn insert_extension<T: Extension>(file: &mut ClmFile, data: &T) -> Result<(), ImportError> {
    let raw = data.encode();
    let key = T::KEY.parse()?;
    let payload = match raw.payload {
        Payload::Json(json) => ClmPayload::Json(json),
        Payload::Bytes(bytes) => {
            file.extensions.push(ClmExtensionBlob {
                key: T::KEY.parse()?,
                data: bytes.to_vec(),
            });
            ClmPayload::Bytes(ClmExtensionMarker {
                size: bytes.len() as u64,
                hash: extension_hash(&bytes),
            })
        }
    };
    file.doc.extensions.insert(
        key,
        ClmExtension::Maintained {
            version: raw.version,
            refs: raw.refs,
            payload,
        },
    );
    Ok(())
}

fn lane_interp(mode: Option<&str>, animation: &str) -> InterpolateMode {
    match mode {
        None | Some("Nearest" | "Linear" | "Stepped" | "Cubic") => interp(mode),
        // inochi2d's Bezier is an unfinished placeholder that draws Linear.
        Some(other) => {
            tracing::warn!(
                "animation {animation:?}: interpolation {other:?} has no catchlight \
                 equivalent; the lane plays Linear"
            );
            InterpolateMode::Linear
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inx::InxModel;
    use catchlight_core::model::{BindingTarget, ScalarTarget};
    use catchlight_core::texture::EncodedTexture;
    use serde_json::json;
    /// Authored in inochi2d's frame (Y-down, lower `zsort` in front). Values
    /// are asymmetric and non-zero so a missing and a doubled negation both
    /// change the result.
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
                            "zsort": 1.0,
                            "name": "front",
                            "type": "Part",
                            "transform": {
                                "trans": [7.0, 10.0, 3.0],
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
                    // `values` is [x][y]: 2 x points by 1 y point.
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
            textures: vec![EncodedTexture {
                format: TextureFormat::Png,
                data: std::sync::Arc::from(&b"texture bytes"[..]),
                premultiplied: true,
            }],
            vendors: Vec::new(),
        }
    }

    #[test]
    fn the_import_reflects_exactly_the_y_bearing_fields() {
        let doc = from_inx_model(&reflection_fixture()).unwrap().doc;
        let front = &doc.nodes[1];
        let back = &doc.nodes[2];
        assert_eq!(front.name, "front");
        assert_eq!(back.name, "back");

        assert_eq!(front.z_order, -1.0, "front z order");
        assert_eq!(back.z_order, -5.0, "back z order");
        assert!(
            front.z_order > back.z_order,
            "the node authored nearer the viewer must sort in front"
        );

        assert_eq!(front.transform.translation, [7.0, -10.0, 3.0]);
        assert_eq!(front.transform.rotation, [-0.25, 0.5, -0.75]);
        assert_eq!(front.transform.scale, [2.0, 3.0]);
        assert_eq!(back.transform.translation, [-2.0, 12.0, 0.0]);
        assert_eq!(back.transform.rotation, [1.5, 2.0, 0.5]);

        let mesh = |n: &ClmNode| match &n.kind {
            ClmNodeKind::Part(p) => p.mesh.clone(),
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

        assert_eq!(doc.params.len(), 1, "param count");
        let bindings = &doc.bindings;
        assert_eq!(bindings.len(), 9, "binding count");
        assert!(
            bindings
                .iter()
                .all(|b| b.params == vec![doc.params[0].id.clone()]),
            "every binding names the one scalar param"
        );

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

        assert_eq!(scalar(1, ScalarTarget::Ty), vec![-10.0, 20.0]);
        assert_eq!(scalar(2, ScalarTarget::Rx), vec![-0.25, 0.5]);
        assert_eq!(scalar(3, ScalarTarget::Rz), vec![-1.5, 2.5]);
        assert_eq!(scalar(4, ScalarTarget::ZOrder), vec![3.0, -4.0]);

        assert_eq!(scalar(5, ScalarTarget::Tx), vec![11.0, -22.0]);
        assert_eq!(scalar(6, ScalarTarget::Ry), vec![0.75, -1.25]);
        assert_eq!(scalar(7, ScalarTarget::Sy), vec![2.0, 0.5]);
        assert_eq!(scalar(8, ScalarTarget::Opacity), vec![0.25, 0.75]);
    }

    fn composite_fixture(propagate: Option<bool>) -> InxModel {
        let mut composite = json!({
            "uuid": 2,
            "name": "composite",
            "type": "Composite",
            "zsort": 0.0,
            "children": []
        });
        if let Some(v) = propagate {
            composite["propagate_meshgroup"] = json!(v);
        }
        InxModel {
            payload: json!({
                "nodes": {
                    "uuid": 1,
                    "name": "root",
                    "type": "Node",
                    "zsort": 0.0,
                    "children": [composite]
                }
            }),
            textures: Vec::new(),
            vendors: Vec::new(),
        }
    }

    #[test]
    fn propagate_meshgroup_defaults_to_false_when_the_key_is_absent() {
        let propagate = |model: InxModel| {
            from_inx_model(&model)
                .unwrap()
                .doc
                .nodes
                .iter()
                .find_map(|n| match &n.kind {
                    ClmNodeKind::Composite(c) => Some(c.propagate_meshgroup),
                    _ => None,
                })
                .expect("the fixture holds one composite")
        };
        assert!(!propagate(composite_fixture(None)), "key absent");
        assert!(
            propagate(composite_fixture(Some(true))),
            "key authored true"
        );
        assert!(
            !propagate(composite_fixture(Some(false))),
            "key authored false"
        );
    }
}
