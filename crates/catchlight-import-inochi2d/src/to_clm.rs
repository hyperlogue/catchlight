//! The document: the reflected values assembled into the `.clm` a
//! [`Model`](catchlight_core::Model) is read from.
//!
//! The inx node *tree* is flattened DFS pre-order, so the document is
//! topologically ordered (a parent is written before its children) with the
//! root first and sibling order preserved. inochi's globally-unique `uuid`s
//! resolve against that flattening — `binding.node` / `mask.source` to a node,
//! `SimplePhysics.param` to a param — and are then dropped. References that do
//! not resolve are dropped; duplicate uuids collapse to the first occurrence,
//! so a reference always names the node that claimed the uuid first.
//!
//! **Ids are derived from position, and this is the only place they are
//! minted.** The root is `root`; node *i* of the flattening is `node-<i>`;
//! param *i* is `param-<i>`, or `param-<i>.x` / `param-<i>.y` when the source
//! param is 2-D; texture *i* is `tex-<i>`. Nothing random and nothing
//! time-dependent: two imports of one `.inx` agree, down to the byte, about
//! what an addon would be naming. Once minted they are stored in the `.clm`
//! and never derived again — renaming one is the author's business.
//!
//! **A source 2-D param becomes two scalar params**, `<name>.x` and
//! `<name>.y`, adjacent in param order, and every binding under it becomes a
//! two-param binding over the pair. A pendulum aimed at one is aimed at both.
//!
//! Only what catchlight models is kept; the rest (meta, groups, automation,
//! animations, cameras, emissive/bump slots, emissionStrength) is dropped.
//! Textures are carried verbatim — a render cache is what decodes and crops
//! them.

use std::collections::HashMap;

use catchlight_core::formats::clm::{
    ClmBinding, ClmComposite, ClmDocument, ClmFile, ClmMask, ClmMeshGroup, ClmNode, ClmNodeKind,
    ClmParam, ClmPart, ClmPhysics, ClmSimplePhysics, ClmTexture, TextureAlpha, TextureEncoding,
};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::texture::TextureFormat;
use catchlight_core::{Model, NodeId, ParamId, TexId};

use crate::error::ImportError;
use crate::inx::InxModel;
use crate::reflect::{
    axes_of, blend, convert_binding_values, convert_mesh, convert_transform, flatten, interp,
    mask_mode, reflect_z, vec2_arr, vec3_arr,
};
use crate::schema::{
    source_binding_is_color, SchemaBinding, SchemaMask, SchemaNode, SchemaParam,
    SchemaPuppetPhysics,
};

/// Import a parsed `.inx` into a [`Model`].
///
/// The document [`from_inx_model`] builds is read back through
/// [`Model::from_clm_file`], so an import that succeeds has already been
/// through the same reader a `.clm` off disk goes through: whatever this
/// returns can be written and opened again.
pub fn import_inx_model(model: &InxModel) -> Result<Model, ImportError> {
    Ok(Model::from_clm_file(&from_inx_model(model)?)?)
}

/// Parse `.inx` (or `.inp` — one container) bytes and import them.
pub fn import_inx_bytes(bytes: &[u8]) -> Result<Model, ImportError> {
    import_inx_model(&InxModel::parse(std::io::Cursor::new(bytes))?)
}

/// Reflect a parsed `.inx` model into the `.clm` document it becomes.
pub fn from_inx_model(model: &InxModel) -> Result<ClmFile, ImportError> {
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

    // Flatten the node tree, recording each node's parent position and a
    // uuid → position map for resolving cross-references.
    let mut flat: Vec<(SchemaNode, Option<u32>)> = Vec::new();
    let mut uuid_node: HashMap<u32, u32> = HashMap::new();
    match obj.get("nodes") {
        Some(n) => flatten(n, None, &mut flat, &mut uuid_node),
        None => flat.push((SchemaNode::default(), None)),
    }
    let node_ids = node_ids(flat.len())?;

    // Params keep their array order; build a uuid → position map (first-wins
    // on a duplicate, like the node map).
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

    let refs = Refs {
        node_ids: &node_ids,
        uuid_node: &uuid_node,
        uuid_param: &uuid_param,
        slots: &slots,
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
            Slot::One(id) => params.push(ClmParam {
                id: id.clone(),
                name,
                min: min[0],
                max: max[0],
                default: defaults[0],
                key_positions: axis_x.clone(),
            }),
            Slot::Pair(x, y) => {
                params.push(ClmParam {
                    id: x.clone(),
                    name: format!("{name}.x"),
                    min: min[0],
                    max: max[0],
                    default: defaults[0],
                    key_positions: axis_x.clone(),
                });
                params.push(ClmParam {
                    id: y.clone(),
                    name: format!("{name}.y"),
                    min: min[1],
                    max: max[1],
                    default: defaults[1],
                    key_positions: axis_y.clone(),
                });
            }
        }
        bindings.extend(
            p.bindings
                .iter()
                .filter_map(|b| convert_binding(b, &slots[i], &nodes, &refs, &axis_x, &axis_y)),
        );
    }

    let textures = model
        .textures
        .iter()
        .enumerate()
        .map(|(i, t)| {
            Ok(ClmTexture {
                id: tex_id(i as u32)?,
                encoding: match t.format {
                    TextureFormat::Png => TextureEncoding::Png,
                    TextureFormat::Tga => TextureEncoding::Tga,
                },
                alpha: TextureAlpha::PremultipliedSrgb,
                data: t.data.to_vec(),
            })
        })
        .collect::<Result<Vec<_>, ImportError>>()?;

    Ok(ClmFile {
        doc: ClmDocument {
            physics,
            nodes,
            params,
            bindings,
            // An `.inx` has no seams, so it has no welds; inochi2d has no
            // animation the runtime reads either (see the crate doc).
            welds: Vec::new(),
            animations: Vec::new(),
        },
        textures,
    })
}

/// What one source param became: one scalar param, or the `<name>.x` /
/// `<name>.y` pair a 2-D one splits into.
enum Slot {
    One(ParamId),
    Pair(ParamId, ParamId),
}

impl Slot {
    /// The params a binding under this slot names, in `x`, `y` order.
    fn params(&self) -> Vec<ParamId> {
        match self {
            Slot::One(id) => vec![id.clone()],
            Slot::Pair(x, y) => vec![x.clone(), y.clone()],
        }
    }

    /// The two params a pendulum aimed at this slot writes.
    fn targets(&self) -> [Option<ParamId>; 2] {
        match self {
            Slot::One(id) => [Some(id.clone()), None],
            Slot::Pair(x, y) => [Some(x.clone()), Some(y.clone())],
        }
    }
}

/// Everything a node's cross-references resolve against.
struct Refs<'a> {
    node_ids: &'a [NodeId],
    uuid_node: &'a HashMap<u32, u32>,
    uuid_param: &'a HashMap<u32, u32>,
    slots: &'a [Slot],
}

impl Refs<'_> {
    fn node_of_uuid(&self, uuid: u32) -> Option<&NodeId> {
        self.node_ids.get(*self.uuid_node.get(&uuid)? as usize)
    }

    fn slot_of_uuid(&self, uuid: u32) -> Option<&Slot> {
        self.slots.get(*self.uuid_param.get(&uuid)? as usize)
    }
}

/// `root`, then `node-1`, `node-2`, … — one Id per node of the flattening.
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

/// `param-<i>` per scalar source param, `param-<i>.x` / `param-<i>.y` per 2-D
/// one.
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

fn convert_node(
    s: &SchemaNode,
    index: usize,
    parent: Option<u32>,
    refs: &Refs<'_>,
) -> Result<ClmNode, ImportError> {
    Ok(ClmNode {
        id: refs
            .node_ids
            .get(index)
            .ok_or(ImportError::MissingField("node"))?
            .clone(),
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
        kind: convert_node_kind(s, refs)?,
    })
}

fn convert_node_kind(s: &SchemaNode, refs: &Refs<'_>) -> Result<ClmNodeKind, ImportError> {
    Ok(match s.ty.as_deref().unwrap_or("") {
        "Part" => ClmNodeKind::Part(convert_part(s, refs)?),
        "Composite" => ClmNodeKind::Composite(convert_composite(s, refs)?),
        "MeshGroup" => ClmNodeKind::MeshGroup(convert_mesh_group(s)),
        "SimplePhysics" => ClmNodeKind::SimplePhysics(convert_simple_physics(s, refs)),
        // Node, Camera, and any unmodeled type all become a container Group.
        _ => ClmNodeKind::Group,
    })
}

fn convert_masks(masks: &[SchemaMask], refs: &Refs<'_>) -> Vec<ClmMask> {
    masks
        .iter()
        .filter_map(|m| {
            // Drop masks whose source node doesn't resolve: `.inx` is untrusted
            // and a dangling mask has nothing to clip against.
            Some(ClmMask {
                source: refs.node_of_uuid(m.source?)?.clone(),
                mode: mask_mode(m.mode.as_deref()),
            })
        })
        .collect()
}

fn convert_part(s: &SchemaNode, refs: &Refs<'_>) -> Result<ClmPart, ImportError> {
    // A part with no `textures` array takes slot 0, which is what the source
    // runtime does; an index too large to be one is no texture at all.
    let albedo = match s.textures.first() {
        None => Some(tex_id(0)?),
        Some(&v) => match u32::try_from(v) {
            Ok(v) => Some(tex_id(v)?),
            Err(_) => None,
        },
    };
    Ok(ClmPart {
        mesh: convert_mesh(s.mesh.as_ref()),
        albedo,
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), refs),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
        // An `.inx` names no vertex, so it fills no seam.
        seams: Vec::new(),
    })
}

fn convert_composite(s: &SchemaNode, refs: &Refs<'_>) -> Result<ClmComposite, ImportError> {
    Ok(ClmComposite {
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), refs),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
        propagate_meshgroup: s.propagate_meshgroup.unwrap_or(true),
    })
}

fn convert_mesh_group(s: &SchemaNode) -> ClmMeshGroup {
    s.log_dropped_mesh_group_color();
    ClmMeshGroup {
        mesh: convert_mesh(s.mesh.as_ref()),
        dynamic: s.dynamic_deformation.unwrap_or(false),
        translate_children: s.translate_children.unwrap_or(false),
    }
}

fn convert_simple_physics(s: &SchemaNode, refs: &Refs<'_>) -> ClmSimplePhysics {
    ClmSimplePhysics {
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
        target_params: s
            .param
            .and_then(|uuid| refs.slot_of_uuid(uuid))
            .map(Slot::targets)
            .unwrap_or_default(),
        // Authored, unscaled — the global g-scale fold is a build step.
        gravity: s.gravity.unwrap_or(1.0),
        length: s.length.unwrap_or(100.0),
        frequency: s.frequency.unwrap_or(1.0),
        angle_damping: s.angle_damping.unwrap_or(0.5),
        length_damping: s.length_damping.unwrap_or(0.5),
        output_scale: vec2_arr(&s.output_scale, [1.0, 1.0]),
    }
}

fn convert_binding(
    b: &SchemaBinding,
    slot: &Slot,
    nodes: &[ClmNode],
    refs: &Refs<'_>,
    axis_x: &[f32],
    axis_y: &[f32],
) -> Option<ClmBinding> {
    // Drop bindings whose target node doesn't resolve: there is nothing for
    // them to drive.
    let node = refs.node_of_uuid(b.node?)?;
    let values_json = b.values.as_ref()?;
    let kind = b.param_name.as_deref().unwrap_or("");
    // A mesh group is never drawn and carries no colour, so a colour binding on
    // one has nowhere to land — and writing it out would produce a `.clm` the
    // loader rejects.
    if source_binding_is_color(kind)
        && matches!(
            nodes.iter().find(|n| &n.id == node).map(|n| &n.kind),
            Some(ClmNodeKind::MeshGroup(_))
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
    Some(ClmBinding {
        params: slot.params(),
        node: node.clone(),
        interpolate_mode: interp(b.interpolate_mode.as_deref()),
        values,
    })
}
