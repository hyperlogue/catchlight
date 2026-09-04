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
//! **A deform binding has to have vertices to move**, so `convert_binding`
//! drops one whose node carries no mesh: it moves nothing in the source
//! either, and the reader refuses a deform on a node it cannot size the cells
//! from.
//!
//! **A param has to be posable and a part has to name a texture the file
//! carries**, so this is where two of the crate's repairs land (the rule is in
//! the crate doc). `usable_range` widens a range the source could not move
//! along and refuses one authored backwards; `convert_part` gives a part with
//! no `textures` array the `tex-0` the source runtime draws, or no texture when
//! the rig carries none. The texture table is then cut down to what some part
//! actually draws — Ids are minted from the source's texture order, so the
//! survivors keep theirs and dropping `tex-2` never renumbers `tex-3`.
//!
//! **An animation lane names a param and an axis**, so `convert_animations`
//! is the second place the 2-D split above is undone: the lane's `uuid`
//! resolves to a slot and its `target` picks which of the slot's params it
//! drives. A lane whose param the rig does not carry, or whose axis its param
//! does not have, is dropped like any other dangling reference; an
//! interpolation `.clm` has no mode for is not a dangling reference and costs
//! the lane nothing but a warning.
//!
//! Only what catchlight models is kept; the rest (meta, groups, automation,
//! cameras, emissive/bump slots, emissionStrength) is dropped. Textures are
//! carried verbatim — a render cache is what decodes and crops them.

use std::collections::{HashMap, HashSet};

use catchlight_core::formats::clm::{
    ClmAnimation, ClmBinding, ClmComposite, ClmDocument, ClmFile, ClmKeyframe, ClmLane, ClmMask,
    ClmMesh, ClmMeshGroup, ClmNode, ClmNodeKind, ClmParam, ClmPart, ClmPhysics, ClmSimplePhysics,
    ClmTexture, TextureAlpha, TextureEncoding,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::texture::TextureFormat;
use catchlight_core::{Model, NodeId, ParamId, TexId};

use crate::error::ImportError;
use crate::inx::InxModel;
use crate::reflect::{
    axes_of, blend, convert_binding_values, convert_mesh, convert_transform, flatten, interp,
    mask_mode, reflect_z, vec2_arr, vec3_arr, NodeRef,
};
use crate::schema::{
    source_binding_is_color, SchemaAnimation, SchemaAnimationLane, SchemaBinding, SchemaMask,
    SchemaNode, SchemaParam, SchemaPuppetPhysics,
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
                    key_positions: axis_x.clone(),
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
                    key_positions: axis_x.clone(),
                });
                params.push(ClmParam {
                    id: y.clone(),
                    name: name_y,
                    min: min_y,
                    max: max_y,
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

    // A texture no part's albedo names draws nothing, so dropping it cannot
    // change the render. Ids are minted from the source's texture order and
    // the survivors keep theirs: dropping `tex-2` never renumbers `tex-3`.
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

    Ok(ClmFile {
        doc: ClmDocument {
            physics,
            nodes,
            params,
            bindings,
            // An `.inx` names no vertex, so it has no slots and no welds.
            welds: Vec::new(),
            animations: convert_animations(obj.get("animations"), &refs),
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

    /// The param an animation lane's `target` names: 0 is the x axis and
    /// anything else the y. A scalar source param has no y, so a lane aimed at
    /// one axis it does not have resolves to nothing.
    fn axis(&self, target: u8) -> Option<&ParamId> {
        match (self, target) {
            (Slot::One(id), 0) => Some(id),
            (Slot::One(_), _) => None,
            (Slot::Pair(x, _), 0) => Some(x),
            (Slot::Pair(_, y), _) => Some(y),
        }
    }

    /// The Id a message names this slot by — the x of a pair.
    fn head(&self) -> &ParamId {
        match self {
            Slot::One(id) | Slot::Pair(id, _) => id,
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
    /// Whether the node at each flat index becomes one catchlight draws — the
    /// only thing a mask may name. Parallel to `node_ids`.
    drawn_nodes: &'a [bool],
    /// How many textures the rig carries — what a part naming none is
    /// resolved against.
    textures: usize,
}

impl Refs<'_> {
    fn node_of_uuid(&self, uuid: u32) -> Option<&NodeId> {
        self.node_ids.get(*self.uuid_node.get(&uuid)? as usize)
    }

    fn slot_of_uuid(&self, uuid: u32) -> Option<&Slot> {
        self.slots.get(*self.uuid_param.get(&uuid)? as usize)
    }

    /// Whether the node this uuid names becomes a part or a composite.
    fn draws(&self, uuid: u32) -> bool {
        self.uuid_node
            .get(&uuid)
            .and_then(|&i| self.drawn_nodes.get(i as usize))
            .copied()
            .unwrap_or(false)
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

/// A param range a pose can be read against: finite and strictly increasing.
///
/// A collapsed range (`min == max`) is **widened** to `min..min + 1`. The
/// source param cannot move either — every value maps to the one point — so
/// the rest pose the rig draws is unchanged, and the widened range gives the
/// runtime something to normalize against. A bound that is not a number has no
/// pose to preserve at all: it collapses onto the other bound, or onto zero
/// when neither is finite, and widens from there. (`.inx` cannot spell an
/// infinity, but a bound too large for `f32` rounds to one.)
///
/// A finite range authored the wrong way round is not repaired: what the
/// source runtime does with `min > max` is unclear, so any widening would be a
/// guess about where the rig sits, and a guess here moves the model. The
/// import refuses it and names the param.
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

/// The mesh a node deforms, if it has one. A part and a mesh group carry one;
/// nothing else does, so a deform binding on anything else drives no vertex.
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
        "SimplePhysics" => ClmNodeKind::SimplePhysics(convert_simple_physics(s, refs)),
        // Node, Camera, and any unmodeled type all become a container Group.
        _ => ClmNodeKind::Group,
    })
}

fn convert_masks(masks: &[SchemaMask], node: NodeRef<'_>, refs: &Refs<'_>) -> Vec<ClmMask> {
    masks
        .iter()
        .filter_map(|m| {
            // Drop masks whose source node doesn't resolve: `.inx` is untrusted
            // and a dangling mask has nothing to clip against.
            let source = m.source?;
            // A source catchlight never draws goes for the same reason: a mask
            // is the source's own drawing rasterized into a stencil, and a mesh
            // group, a plain node and a pendulum each draw nothing, here and in
            // the source runtime alike. `Model::mask_add` and the `.clm` reader
            // both take only a part or a composite.
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
    // A part with no `textures` array draws slot 0 in the source runtime, so
    // that is what it draws here — but only if the rig has a slot 0; naming a
    // texture the file does not carry would be a `.clm` no reader accepts.
    // `uint.max` is the source runtime's "no texture in this slot" sentinel
    // and draws nothing; any other slot the rig does not carry has no defined
    // rendering to preserve, so the import refuses it by name.
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
        // An `.inx` names no vertex, so it fills no slot.
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
        propagate_meshgroup: s.propagate_meshgroup.unwrap_or(true),
    })
}

fn convert_mesh_group(s: &SchemaNode, node: NodeRef<'_>) -> Result<ClmMeshGroup, ImportError> {
    s.log_dropped_mesh_group_color();
    Ok(ClmMeshGroup {
        // A mesh group is never drawn, so it samples nothing through its UVs
        // and inochi2d authors none for it.
        mesh: convert_mesh(s.mesh.as_ref(), node, false)?,
        translate_children: s.translate_children.unwrap_or(false),
    })
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
    let target = nodes.iter().find(|n| &n.id == node);
    let values_json = b.values.as_ref()?;
    let kind = b.param_name.as_deref().unwrap_or("");
    // A mesh group is never drawn and carries no colour, so a colour binding on
    // one has nowhere to land — and writing it out would produce a `.clm` the
    // loader rejects.
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
    // A deform cell is one `[dx, dy]` per vertex of *this* node's mesh, so the
    // mesh has to be in scope to fit the cells to it.
    let named = NodeRef {
        id: node,
        name: target.map_or("", |n| n.name.as_str()),
    };
    let deform_len = target
        .and_then(|n| mesh_of(&n.kind))
        .map_or(0, |m| m.verts.len());
    // A node with no vertices — a group, a composite, a pendulum, a part whose
    // mesh is empty — has nothing for a deform to move, so the binding drives
    // no pixel and dropping it draws the same frame. Keeping it would write a
    // `.clm` the loader refuses (`add_binding`'s `NotMeshed`).
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
        node: node.clone(),
        interpolate_mode: interp(b.interpolate_mode.as_deref()),
        values,
    })
}

/// The rig's clips, as `.clm` animations.
///
/// inochi2d 0.8.6 writes `animations` as an object keyed by clip name; older
/// and nijigenerate variants write an array carrying a `name` per entry. Both
/// read; an unnamed array entry is called `anim-<i>`, and anything that is
/// neither shape carries no clip.
fn convert_animations(json: Option<&serde_json::Value>, refs: &Refs<'_>) -> Vec<ClmAnimation> {
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

fn convert_animation(name: String, anim: &SchemaAnimation, refs: &Refs<'_>) -> ClmAnimation {
    let lanes = anim
        .lanes
        .iter()
        .filter_map(|lane| convert_animation_lane(&name, lane, refs))
        .collect();
    ClmAnimation {
        // inochi2d's own defaults, which `.clm` shares: 60 fps, no frames,
        // and -1 for a lead-in / lead-out the author never set.
        timestep: anim.timestep.unwrap_or(1.0 / 60.0),
        length: anim.length.unwrap_or(0),
        lead_in: anim.lead_in.unwrap_or(-1),
        lead_out: anim.lead_out.unwrap_or(-1),
        name,
        lanes,
    }
}

/// One lane, or nothing when the param it drives is not one this import can
/// name. Both misses are the crate's dangling-reference rule: a lane whose
/// `uuid` no param claims, and a lane on the y axis of a source param that
/// only has an x — the second is what a 2-D lane over a scalar param is.
fn convert_animation_lane(
    animation: &str,
    lane: &SchemaAnimationLane,
    refs: &Refs<'_>,
) -> Option<ClmLane> {
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
    // inochi2d encodes the axis as an integer and reads anything but 0 as y.
    let target = lane.target.unwrap_or(0);
    let Some(param) = slot.axis(target) else {
        tracing::warn!(
            "animation {animation:?}: dropping a lane on axis {target} of param {}, \
             which has no such axis",
            slot.head()
        );
        return None;
    };
    let mut keyframes: Vec<ClmKeyframe> = lane
        .keyframes
        .iter()
        .map(|k| ClmKeyframe {
            // inochi2d defaults both to 0.
            frame: k.frame.unwrap_or(0),
            value: k.value.unwrap_or(0.0),
        })
        .collect();
    // A lane is read in frame order; the reference sorts on load
    // (`animation.d`, `updateFrames`) rather than trusting the file, and a
    // `.clm` a player can read has to be sorted before it is written.
    keyframes.sort_by_key(|k| k.frame);
    Some(ClmLane {
        param: param.clone(),
        interpolation: lane_interp(lane.interpolation.as_deref(), animation),
        keyframes,
    })
}

/// A lane's interpolation. The modes the two share map straight across; one
/// catchlight does not model falls back to its nearest equivalent and says so,
/// because a clip that plays the wrong curve is still the clip, and a dropped
/// lane is not.
fn lane_interp(mode: Option<&str>, animation: &str) -> InterpolateMode {
    match mode {
        None | Some("Nearest" | "Linear" | "Stepped" | "Cubic") => interp(mode),
        // inochi2d's Bezier branch is itself a placeholder — an eased lerp
        // gated on a per-keyframe tension it marks TODO — so Linear is what
        // the source draws today, not an approximation of something else.
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
    use std::collections::HashSet;
    /// A model authored in inochi2d's frame — Y-down, lower `zsort` in front —
    /// touching every field the import must reflect, plus controls on fields
    /// it must leave alone. Values are asymmetric and non-zero so a *missing*
    /// negation and a *doubled* one both change the result.
    ///
    /// One texture slot, so the parts' `"textures": [0]` resolves; the bytes
    /// are carried verbatim and never decoded, so the alpha crop rewrites no
    /// UVs and the authored UVs stay comparable against the literals below.
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
            textures: vec![EncodedTexture {
                format: TextureFormat::Png,
                data: std::sync::Arc::from(&b"texture bytes"[..]),
                premultiplied: true,
            }],
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
        let doc = from_inx_model(&reflection_fixture()).unwrap().doc;
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

        // Bindings, in authored order, all under the file's one param.
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
        let file = from_inx_model(&load_reference()).unwrap();

        let parts = file
            .doc
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, ClmNodeKind::Part(_)))
            .count();
        assert_eq!(parts, 117, "expected 117 Part nodes");
        assert_eq!(file.textures.len(), 87);

        let bytes = Model::from_clm_file(&file).unwrap().to_clm_bytes().unwrap();
        let reopened = Model::from_clm_bytes(&bytes)
            .unwrap()
            .to_clm_file()
            .unwrap();
        assert_eq!(
            reopened.doc, file.doc,
            "structure must round-trip through .clm"
        );
        assert_eq!(reopened.textures, file.textures, "textures must round-trip");
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn the_document_is_topologically_ordered_with_one_root() {
        let file = from_inx_model(&load_reference()).unwrap();
        let doc = &file.doc;
        let nodes = &doc.nodes;

        let roots = nodes.iter().filter(|n| n.parent.is_none()).count();
        assert_eq!(roots, 1, "exactly one root");
        let mut written: HashSet<&NodeId> = HashSet::new();
        for n in nodes {
            if let Some(p) = &n.parent {
                assert!(
                    written.contains(p),
                    "parent {p} must precede child {}",
                    n.id
                );
            }
            assert!(written.insert(&n.id), "node {} is written twice", n.id);
        }

        // Every cross-reference names a node, param or texture the file
        // carries -- which is also exactly what `Model::from_clm_file`
        // refuses, so a failure here names the field rather than the load.
        let params: HashSet<&ParamId> = doc.params.iter().map(|p| &p.id).collect();
        let textures: HashSet<&TexId> = file.textures.iter().map(|t| &t.id).collect();
        for node in nodes {
            match &node.kind {
                ClmNodeKind::Part(part) => {
                    assert!(part.masks.iter().all(|m| written.contains(&m.source)));
                    if let Some(albedo) = &part.albedo {
                        assert!(textures.contains(albedo), "albedo {albedo}");
                    }
                }
                ClmNodeKind::Composite(c) => {
                    assert!(c.masks.iter().all(|m| written.contains(&m.source)))
                }
                ClmNodeKind::SimplePhysics(sp) => {
                    assert!(sp
                        .target_params
                        .iter()
                        .flatten()
                        .all(|t| params.contains(t)));
                }
                _ => {}
            }
        }
        for binding in &doc.bindings {
            assert!(written.contains(&binding.node));
            assert!(binding.params.iter().all(|p| params.contains(p)));
        }
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn captures_authored_propagate_meshgroup() {
        let doc = from_inx_model(&load_reference()).unwrap().doc;
        // The root "Puppet Body" composite authors propagate_meshgroup=false —
        // the flag the old runtime path hardcoded to true.
        assert!(
            doc.nodes.iter().any(|n| matches!(
                &n.kind,
                ClmNodeKind::Composite(c) if !c.propagate_meshgroup
            )),
            "an authored propagate_meshgroup=false composite must be captured"
        );
        assert!(!doc.bindings.is_empty());
    }
}
