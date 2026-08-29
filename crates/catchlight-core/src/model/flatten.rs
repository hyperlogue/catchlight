//! The file boundary: Ids and scalar params in memory <-> array indices and
//! 2-D params on disk.
//!
//! **This is a temporary bridge.** `.clp` v0 stores no Ids and its params
//! carry two axes with the bindings nested underneath, so opening one has to
//! mint Ids and split every 2-D param, and saving has to put the halves back
//! together. cl-32i.14 replaces the format with `.clm`, which stores Ids and
//! scalar params directly, and this whole module collapses to an index remap.
//!
//! What the bridge does, in both directions:
//!
//! - **Ids.** A model read from `.clp` gets `root` for the arena's root and
//!   `node-<index>`, `param-<index>`, `tex-<index>` for the rest — the same
//!   ones every time, so two opens of one file agree about what an addon would
//!   be naming. They round-trip nowhere, because the wire has nothing to
//!   round-trip them to.
//! - **Params.** A 2-D param splits into `<name>.x` and `<name>.y`, adjacent
//!   in param order, and its bindings become two-param bindings over the pair.
//!   [`Model::flatten`] puts a pair back together when the names still match,
//!   they are still adjacent, and every binding and physics target that names
//!   either names exactly that pair. A two-param binding whose params are not
//!   such a pair cannot be written to v0 at all: that is
//!   [`ModelError::UnpairableBinding`], not a panic and not a silent drop.
//!
//! Two adjacent 1-D params that happen to be named `<n>.x` and `<n>.y`, with
//! no binding and no physics target between them, merge into one 2-D param on
//! save. Nothing distinguishes them from a split pair on the v0 wire; `.clm`
//! removes the ambiguity along with the split.

use std::collections::HashMap;
use std::sync::Arc;

use crate::formats::clp::{
    self, ClpBinding, ClpComposite, ClpDocument, ClpFile, ClpMask, ClpNode, ClpNodeKind, ClpParam,
    ClpPart, ClpSimplePhysics, ClpTexture, ClpWeld, FORMAT_VERSION,
};
use crate::{charge_clp_structure, LoadBudget};

use super::*;

/// One `.clp` v0 param slot: either one scalar Model param, or the
/// `<name>.x` / `<name>.y` pair a 2-D one splits into.
#[derive(Debug, PartialEq, Eq)]
enum V0Slot<'a> {
    One(&'a ParamId),
    Pair(&'a ParamId, &'a ParamId),
}

/// How this model's scalar params map onto v0's param table.
struct V0Params<'a> {
    slots: Vec<V0Slot<'a>>,
    /// Every Model param, and the v0 slot it lands in.
    index: HashMap<&'a ParamId, u32>,
}

impl<'a> V0Params<'a> {
    fn index_of(&self, param: &ParamId) -> Option<u32> {
        self.index.get(param).copied()
    }

    /// Whether `x` and `y` are exactly one v0 slot's pair, in that order.
    fn is_pair(&self, x: &ParamId, y: &ParamId) -> bool {
        self.index_of(x).is_some_and(|i| {
            matches!(self.slots.get(i as usize), Some(V0Slot::Pair(px, py)) if *px == x && *py == y)
        })
    }
}

/// `<base>` when the two names are `<base>.x` and `<base>.y`.
fn pair_base<'a>(x: &'a str, y: &str) -> Option<&'a str> {
    let base = x.strip_suffix(".x")?;
    if base.is_empty() || y.strip_suffix(".y") != Some(base) {
        return None;
    }
    Some(base)
}

impl Model {
    /// The v0 param table this model flattens to.
    fn v0_params(&self) -> V0Params<'_> {
        let order = self.param_ids();
        let mut slots = Vec::with_capacity(order.len());
        let mut index = HashMap::with_capacity(order.len());
        let mut i = 0;
        while i < order.len() {
            let paired = order.get(i + 1).is_some_and(|next| {
                let (Some(x), Some(y)) = (self.param(&order[i]), self.param(next)) else {
                    return false;
                };
                pair_base(x.name.as_str(), y.name.as_str()).is_some()
                    && self.pair_is_exclusive(&order[i], next)
            });
            let slot = slots.len() as u32;
            if paired {
                index.insert(&order[i], slot);
                index.insert(&order[i + 1], slot);
                slots.push(V0Slot::Pair(&order[i], &order[i + 1]));
                i += 2;
            } else {
                index.insert(&order[i], slot);
                slots.push(V0Slot::One(&order[i]));
                i += 1;
            }
        }
        V0Params { slots, index }
    }

    /// Whether nothing names `x` or `y` except as the pair `(x, y)` — the
    /// condition for merging them back into one 2-D param.
    fn pair_is_exclusive(&self, x: &ParamId, y: &ParamId) -> bool {
        let is_the_pair =
            |params: &BindingParams| matches!(params, BindingParams::Two(a, b) if a == x && b == y);
        if self.bindings.iter().any(|b| {
            (b.key.params.contains(x) || b.key.params.contains(y)) && !is_the_pair(&b.key.params)
        }) {
            return false;
        }
        !self.nodes.values().any(|n| match &n.kind {
            ModelNodeKind::SimplePhysics(ph) => {
                (ph.drives(x) || ph.drives(y))
                    && ph.target_params() != &[Some(x.clone()), Some(y.clone())]
            }
            _ => false,
        })
    }

    /// Snapshot the model into a `.clp` document: walk the tree in topological
    /// order, assign array indices, put split param pairs back together, and
    /// remap every cross-reference. Total for a valid model except for what v0
    /// genuinely cannot hold — see the module doc.
    pub fn flatten(&self) -> Result<ClpFile, ModelError> {
        let order = self.nodes_in_order();
        let node_index: HashMap<&NodeId, u32> = order
            .iter()
            .enumerate()
            .map(|(i, id)| (id, i as u32))
            .collect();
        let tex_index: HashMap<&TexId, u32> = self
            .texture_ids()
            .iter()
            .enumerate()
            .map(|(i, id)| (id, i as u32))
            .collect();
        let v0 = self.v0_params();

        for b in &self.bindings {
            if let BindingParams::Two(x, y) = &b.key.params {
                if !v0.is_pair(x, y) {
                    return Err(ModelError::UnpairableBinding {
                        node: b.key.node.to_string(),
                        target: b.key.target.name(),
                    });
                }
            }
        }

        let mut nodes = Vec::with_capacity(order.len());
        for id in &order {
            let n = self.node(id).ok_or(ModelError::UnknownNode)?;
            let parent = match n.parent() {
                Some(p) => Some(*node_index.get(p).ok_or(ModelError::UnknownNode)?),
                None => None,
            };
            nodes.push(ClpNode {
                parent,
                name: n.name.to_string(),
                enabled: n.enabled,
                z_order: n.z_order,
                transform: n.transform,
                lock_to_root: n.lock_to_root,
                kind: flatten_kind(id, &n.kind, &node_index, &tex_index, &v0)?,
            });
        }

        let mut params = Vec::with_capacity(v0.slots.len());
        for slot in &v0.slots {
            params.push(self.flatten_param(slot, &node_index)?);
        }

        let mut textures = Vec::with_capacity(self.texture_ids().len());
        for tid in self.texture_ids() {
            let t = self.texture(tid).ok_or(ModelError::UnknownTexture)?;
            textures.push(ClpTexture {
                encoding: t.encoding,
                alpha: t.alpha,
                data: (*t.data).clone(),
            });
        }

        let mut welds = Vec::with_capacity(self.welds().len());
        for w in self.welds() {
            welds.push(ClpWeld {
                a: *node_index.get(w.a()).ok_or(ModelError::UnknownNode)?,
                b: *node_index.get(w.b()).ok_or(ModelError::UnknownNode)?,
                pairs: w.pairs().to_vec(),
            });
        }

        Ok(ClpFile {
            version: FORMAT_VERSION,
            doc: ClpDocument {
                physics: *self.physics(),
                nodes,
                params,
                welds,
            },
            textures,
        })
    }

    fn flatten_param(
        &self,
        slot: &V0Slot<'_>,
        node_index: &HashMap<&NodeId, u32>,
    ) -> Result<ClpParam, ModelError> {
        let bindings = |params: BindingParams| -> Result<Vec<ClpBinding>, ModelError> {
            self.bindings
                .iter()
                .filter(|b| b.key.params == params)
                .map(|b| {
                    Ok(ClpBinding {
                        node: *node_index.get(b.node()).ok_or(ModelError::UnknownNode)?,
                        interpolate_mode: b.interpolate_mode(),
                        values: b.values.to_clp(),
                    })
                })
                .collect()
        };
        Ok(match slot {
            V0Slot::One(id) => {
                let p = self.param(id).ok_or(ModelError::UnknownParam)?;
                ClpParam {
                    name: p.name.to_string(),
                    is_vec2: false,
                    min: [p.min, 0.0],
                    max: [p.max, 0.0],
                    defaults: [p.default, 0.0],
                    axis_points_x: p.key_positions.clone(),
                    axis_points_y: vec![0.0],
                    bindings: bindings(BindingParams::One((*id).clone()))?,
                }
            }
            V0Slot::Pair(x, y) => {
                let px = self.param(x).ok_or(ModelError::UnknownParam)?;
                let py = self.param(y).ok_or(ModelError::UnknownParam)?;
                let name = pair_base(px.name.as_str(), py.name.as_str())
                    .ok_or(ModelError::UnknownParam)?;
                ClpParam {
                    name: name.to_string(),
                    is_vec2: true,
                    min: [px.min, py.min],
                    max: [px.max, py.max],
                    defaults: [px.default, py.default],
                    axis_points_x: px.key_positions.clone(),
                    axis_points_y: py.key_positions.clone(),
                    bindings: bindings(BindingParams::Two((*x).clone(), (*y).clone()))?,
                }
            }
        })
    }

    /// `flatten` then encode to `.clp` bytes.
    pub fn to_clp_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let file = self.flatten()?;
        Ok(clp::encode(&file.doc, &file.textures)?)
    }

    /// Rebuild a [`Model`] from a decoded `.clp`, minting an Id per arena slot,
    /// splitting every 2-D param, and rewiring every index back to an Id.
    /// Errors on an invalid node arena or an out-of-range cross-reference.
    pub fn from_clp_file(file: &ClpFile) -> Result<Model, ModelError> {
        Self::from_clp_file_with_budget(file, &mut LoadBudget::default())
    }

    pub fn from_clp_file_with_budget(
        file: &ClpFile,
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        charge_clp_structure(file, budget)?;
        let doc = &file.doc;
        if doc.nodes.first().is_none_or(|node| node.parent.is_some()) {
            return Err(ModelError::InvalidClpRoot);
        }
        for (i, node) in doc.nodes.iter().enumerate().skip(1) {
            match node.parent {
                Some(parent) if parent < i as u32 => {}
                Some(parent) => return Err(ModelError::InvalidClpParent { node: i, parent }),
                None => return Err(ModelError::InvalidClpRoot),
            }
        }

        let mut textures = HashMap::with_capacity(file.textures.len());
        let mut texture_order = Vec::with_capacity(file.textures.len());
        for (i, t) in file.textures.iter().enumerate() {
            let id = TexId::from_generated(format!("tex-{i}"));
            textures.insert(
                id.clone(),
                ModelTexture {
                    encoding: t.encoding,
                    alpha: t.alpha,
                    data: Arc::new(t.data.clone()),
                },
            );
            texture_order.push(id);
        }
        let tex_ids = texture_order.clone();

        // A 2-D param becomes `<name>.x` and `<name>.y`, adjacent in order;
        // `param_slots` remembers what each file slot became so its bindings
        // and any physics node aimed at it can name the same params.
        let mut params = HashMap::with_capacity(doc.params.len());
        let mut param_order = Vec::with_capacity(doc.params.len());
        let mut param_slots = Vec::with_capacity(doc.params.len());
        for (i, p) in doc.params.iter().enumerate() {
            if p.is_vec2 {
                let x = ParamId::from_generated(format!("param-{i}.x"));
                let y = ParamId::from_generated(format!("param-{i}.y"));
                params.insert(
                    x.clone(),
                    ModelParam {
                        name: Name::truncated(format!("{}.x", p.name)),
                        min: p.min[0],
                        max: p.max[0],
                        default: p.defaults[0],
                        key_positions: p.axis_points_x.clone(),
                    },
                );
                params.insert(
                    y.clone(),
                    ModelParam {
                        name: Name::truncated(format!("{}.y", p.name)),
                        min: p.min[1],
                        max: p.max[1],
                        default: p.defaults[1],
                        key_positions: p.axis_points_y.clone(),
                    },
                );
                param_order.push(x.clone());
                param_order.push(y.clone());
                param_slots.push(BindingParams::Two(x, y));
            } else {
                let id = ParamId::from_generated(format!("param-{i}"));
                params.insert(
                    id.clone(),
                    ModelParam {
                        name: Name::truncated(&p.name),
                        min: p.min[0],
                        max: p.max[0],
                        default: p.defaults[0],
                        key_positions: p.axis_points_x.clone(),
                    },
                );
                param_order.push(id.clone());
                param_slots.push(BindingParams::One(id));
            }
        }

        let node_ids: Vec<NodeId> = (0..doc.nodes.len())
            .map(|i| {
                if i == 0 {
                    NodeId::from_generated(DEFAULT_ROOT_ID.to_string())
                } else {
                    NodeId::from_generated(format!("node-{i}"))
                }
            })
            .collect();

        let mut nodes: HashMap<NodeId, ModelNode> = HashMap::with_capacity(doc.nodes.len());
        for (i, cn) in doc.nodes.iter().enumerate() {
            let mut node = ModelNode::new(&cn.name, ModelNodeKind::Group);
            node.parent = match cn.parent {
                Some(p) => Some(
                    node_ids
                        .get(p as usize)
                        .ok_or(ModelError::UnknownNode)?
                        .clone(),
                ),
                None => None,
            };
            node.enabled = cn.enabled;
            node.z_order = cn.z_order;
            node.transform = cn.transform;
            node.lock_to_root = cn.lock_to_root;
            node.kind = unflatten_kind(&cn.kind, &node_ids, &param_slots, &tex_ids)?;
            nodes.insert(node_ids[i].clone(), node);
        }
        // Children in arena order, which is the document's sibling order.
        for (i, cn) in doc.nodes.iter().enumerate() {
            if let Some(p) = cn.parent {
                let parent = node_ids.get(p as usize).ok_or(ModelError::UnknownNode)?;
                if let Some(pn) = nodes.get_mut(parent) {
                    pn.children.push(node_ids[i].clone());
                }
            }
        }
        let root = node_ids[0].clone();

        let mut bindings = Vec::new();
        for (j, cp) in doc.params.iter().enumerate() {
            for b in &cp.bindings {
                let node = node_ids
                    .get(b.node as usize)
                    .ok_or(ModelError::UnknownNode)?
                    .clone();
                bindings.push(ModelBinding {
                    key: BindingKey {
                        params: param_slots[j].clone(),
                        node,
                        target: target_of(&b.values),
                    },
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.clone().into(),
                    dense: std::sync::OnceLock::new(),
                });
            }
        }

        let mut welds = Vec::with_capacity(doc.welds.len());
        for w in &doc.welds {
            welds.push(ModelWeld::new(
                node_ids
                    .get(w.a as usize)
                    .ok_or(ModelError::UnknownNode)?
                    .clone(),
                node_ids
                    .get(w.b as usize)
                    .ok_or(ModelError::UnknownNode)?
                    .clone(),
                w.pairs.clone(),
            ));
        }

        Ok(Model {
            generation: 0,
            physics: doc.physics,
            welds,
            nodes,
            root,
            params,
            param_order,
            textures,
            texture_order,
            bindings,
        })
    }

    pub fn from_clp_bytes(bytes: &[u8]) -> Result<Model, ModelError> {
        Self::from_clp_file(&clp::decode(bytes)?)
    }
}

fn flatten_kind(
    id: &NodeId,
    kind: &ModelNodeKind,
    node_index: &HashMap<&NodeId, u32>,
    tex_index: &HashMap<&TexId, u32>,
    v0: &V0Params<'_>,
) -> Result<ClpNodeKind, ModelError> {
    Ok(match kind {
        ModelNodeKind::Group => ClpNodeKind::Group,
        ModelNodeKind::Part(p) => ClpNodeKind::Part(ClpPart {
            mesh: p.mesh().clone(),
            albedo: match p.albedo() {
                Some(t) => *tex_index.get(t).ok_or(ModelError::UnknownTexture)?,
                None => u32::MAX,
            },
            opacity: p.opacity,
            blend_mode: p.blend_mode,
            tint: p.tint,
            screen_tint: p.screen_tint,
            masks: flatten_masks(p.masks(), node_index)?,
            mask_threshold: p.mask_threshold,
        }),
        ModelNodeKind::Composite(c) => ClpNodeKind::Composite(ClpComposite {
            opacity: c.opacity,
            blend_mode: c.blend_mode,
            tint: c.tint,
            screen_tint: c.screen_tint,
            masks: flatten_masks(c.masks(), node_index)?,
            mask_threshold: c.mask_threshold,
            propagate_meshgroup: c.propagate_meshgroup,
        }),
        ModelNodeKind::MeshGroup(mg) => ClpNodeKind::MeshGroup(mg.to_clp()),
        ModelNodeKind::SimplePhysics(ph) => ClpNodeKind::SimplePhysics(ClpSimplePhysics {
            kind: ph.kind,
            map_mode: ph.map_mode,
            local_only: ph.local_only,
            target_param: flatten_physics_target(id, ph, v0)?,
            gravity: ph.gravity,
            length: ph.length,
            frequency: ph.frequency,
            angle_damping: ph.angle_damping,
            length_damping: ph.length_damping,
            output_scale: ph.output_scale,
        }),
    })
}

/// v0 aims a physics node at one param slot, so a pendulum writing two params
/// only fits if they are one slot's pair.
fn flatten_physics_target(
    id: &NodeId,
    ph: &ModelPhysics,
    v0: &V0Params<'_>,
) -> Result<Option<u32>, ModelError> {
    match ph.target_params() {
        [None, None] => Ok(None),
        [Some(x), None] => Ok(Some(v0.index_of(x).ok_or(ModelError::UnknownParam)?)),
        [Some(x), Some(y)] if v0.is_pair(x, y) => {
            Ok(Some(v0.index_of(x).ok_or(ModelError::UnknownParam)?))
        }
        _ => Err(ModelError::UnpairablePhysicsTarget(id.to_string())),
    }
}

fn flatten_masks(
    masks: &[ModelMask],
    node_index: &HashMap<&NodeId, u32>,
) -> Result<Vec<ClpMask>, ModelError> {
    masks
        .iter()
        .map(|m| {
            Ok(ClpMask {
                source: *node_index.get(m.source()).ok_or(ModelError::UnknownNode)?,
                mode: m.mode(),
            })
        })
        .collect()
}

fn unflatten_kind(
    kind: &ClpNodeKind,
    node_ids: &[NodeId],
    param_slots: &[BindingParams],
    tex_ids: &[TexId],
) -> Result<ModelNodeKind, ModelError> {
    Ok(match kind {
        ClpNodeKind::Group => ModelNodeKind::Group,
        ClpNodeKind::Part(p) => {
            let mut part = ModelPart::new(p.mesh.clone());
            part.albedo = if p.albedo == u32::MAX {
                None
            } else {
                Some(
                    tex_ids
                        .get(p.albedo as usize)
                        .ok_or(ModelError::UnknownTexture)?
                        .clone(),
                )
            };
            part.opacity = p.opacity;
            part.blend_mode = p.blend_mode;
            part.tint = p.tint;
            part.screen_tint = p.screen_tint;
            part.masks = unflatten_masks(&p.masks, node_ids)?;
            part.mask_threshold = p.mask_threshold;
            ModelNodeKind::Part(part)
        }
        ClpNodeKind::Composite(c) => {
            let mut composite = ModelComposite::new();
            composite.opacity = c.opacity;
            composite.blend_mode = c.blend_mode;
            composite.tint = c.tint;
            composite.screen_tint = c.screen_tint;
            composite.masks = unflatten_masks(&c.masks, node_ids)?;
            composite.mask_threshold = c.mask_threshold;
            composite.propagate_meshgroup = c.propagate_meshgroup;
            ModelNodeKind::Composite(composite)
        }
        ClpNodeKind::MeshGroup(mg) => ModelNodeKind::MeshGroup(ModelMeshGroup::from_clp(mg)),
        ClpNodeKind::SimplePhysics(ph) => {
            let mut physics = ModelPhysics::new(ph.kind);
            physics.map_mode = ph.map_mode;
            physics.local_only = ph.local_only;
            physics.target_params = match ph.target_param {
                Some(i) => match param_slots
                    .get(i as usize)
                    .ok_or(ModelError::UnknownParam)?
                {
                    BindingParams::One(p) => [Some(p.clone()), None],
                    BindingParams::Two(x, y) => [Some(x.clone()), Some(y.clone())],
                },
                None => [None, None],
            };
            physics.gravity = ph.gravity;
            physics.length = ph.length;
            physics.frequency = ph.frequency;
            physics.angle_damping = ph.angle_damping;
            physics.length_damping = ph.length_damping;
            physics.output_scale = ph.output_scale;
            ModelNodeKind::SimplePhysics(physics)
        }
    })
}

fn unflatten_masks(masks: &[ClpMask], node_ids: &[NodeId]) -> Result<Vec<ModelMask>, ModelError> {
    masks
        .iter()
        .map(|m| {
            Ok(ModelMask {
                source: node_ids
                    .get(m.source as usize)
                    .ok_or(ModelError::UnknownNode)?
                    .clone(),
                mode: m.mode,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{BlendMode, MaskMode};
    use crate::formats::clp::{
        ClpBindingValues, ClpCell, ClpCells, ClpIndices, ClpMesh, TextureAlpha, TextureEncoding,
    };
    use crate::id::SeededHex;
    use crate::params::InterpolateMode;

    fn sample() -> Model {
        let mut hex = SeededHex::new(2);
        let mut m = Model::new();
        let tex = m
            .add_texture(
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: Arc::new(vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4]),
                },
                &mut hex,
            )
            .unwrap();
        let root = m.root().clone();
        let part = m
            .add_node(
                &root,
                ModelNode::new(
                    "Body",
                    ModelNodeKind::Part(ModelPart::new(ClpMesh {
                        verts: vec![-1.0, -1.0, 1.0, -1.0, 0.0, 1.0],
                        uvs: vec![0.0, 1.0, 1.0, 1.0, 0.5, 0.0],
                        indices: ClpIndices::U16(vec![0, 1, 2]),
                        origin: [0.0, 0.0],
                    })),
                ),
                &mut hex,
            )
            .unwrap();
        m.set_part_albedo(&part, Some(tex)).unwrap();
        let mask_src = m
            .add_node(
                &root,
                ModelNode::new(
                    "MaskSrc",
                    ModelNodeKind::Part(ModelPart::new(ClpMesh::default())),
                ),
                &mut hex,
            )
            .unwrap();
        m.mask_add(&part, &mask_src, MaskMode::DodgeMask).unwrap();
        let param = m
            .add_param(
                ModelParam::new(Name::truncated("Mouth"), 0.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        m.bindings.push(ModelBinding {
            key: BindingKey::new(param, part, BindingTarget::Deform),
            interpolate_mode: InterpolateMode::Linear,
            values: ClpBindingValues::Deform(ClpCells {
                cells: vec![ClpCell {
                    x: 1,
                    y: 0,
                    value: vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                }],
            })
            .into(),
            dense: std::sync::OnceLock::new(),
        });
        m
    }

    fn node_named(m: &Model, name: &str) -> NodeId {
        m.nodes_in_order()
            .into_iter()
            .find(|id| m.node(id).is_some_and(|n| n.name.as_str() == name))
            .unwrap_or_else(|| panic!("no node named {name}"))
    }

    #[test]
    fn flatten_roundtrips_through_clp() {
        let m = sample();
        let bytes = m.to_clp_bytes().unwrap();
        let m2 = Model::from_clp_bytes(&bytes).unwrap();
        let bytes2 = m2.to_clp_bytes().unwrap();
        assert_eq!(bytes, bytes2, "model -> clp -> model must be byte-stable");
        assert_eq!(clp::decode(&bytes).unwrap(), m2.flatten().unwrap());
    }

    /// `.clp` v0 stores no Ids, so opening one has to mint them — the same ones
    /// every time, or two opens of one file would disagree about what an addon
    /// is naming.
    #[test]
    fn opening_a_clp_mints_the_documented_ids() {
        let file = sample().flatten().unwrap();
        let a = Model::from_clp_file(&file).unwrap();
        let b = Model::from_clp_file(&file).unwrap();

        assert_eq!(a.root().as_str(), "root");
        assert_eq!(
            a.nodes_in_order()
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
            vec!["root", "node-1", "node-2"],
        );
        assert_eq!(a.param_ids()[0].as_str(), "param-0");
        assert_eq!(a.texture_ids()[0].as_str(), "tex-0");
        assert_eq!(a.nodes_in_order(), b.nodes_in_order(), "two opens agree");
    }

    /// A `.clp` 2-D param is two scalar params plus a two-param binding, and
    /// saving puts it back byte-for-byte. Losing that would rewrite every
    /// imported model the first time the editor saved it.
    #[test]
    fn a_two_dimensional_param_splits_and_re_pairs() {
        let mut file = sample().flatten().unwrap();
        file.doc.params[0].name = "Head".into();
        file.doc.params[0].is_vec2 = true;
        file.doc.params[0].min = [-1.0, -2.0];
        file.doc.params[0].max = [1.0, 2.0];
        file.doc.params[0].defaults = [0.25, 0.5];
        file.doc.params[0].axis_points_x = vec![0.0, 0.5, 1.0];
        file.doc.params[0].axis_points_y = vec![0.0, 1.0];

        let m = Model::from_clp_file(&file).unwrap();
        let ids = m.param_ids().to_vec();
        assert_eq!(ids.len(), 2, "one 2-D param becomes two scalars");
        let (x, y) = (&ids[0], &ids[1]);
        assert_eq!(m.param(x).unwrap().name.as_str(), "Head.x");
        assert_eq!(m.param(y).unwrap().name.as_str(), "Head.y");
        assert_eq!(m.param(x).unwrap().min, -1.0);
        assert_eq!(m.param(y).unwrap().max, 2.0);
        assert_eq!(m.param(y).unwrap().default, 0.5);
        assert_eq!(m.param(x).unwrap().key_positions, vec![0.0, 0.5, 1.0]);
        assert_eq!(m.param(y).unwrap().key_positions, vec![0.0, 1.0]);

        let binding = m.bindings().next().expect("the binding survived");
        assert_eq!(
            binding.params(),
            &BindingParams::Two(x.clone(), y.clone()),
            "its binding now names both halves",
        );
        assert_eq!(m.binding_grid(binding.key()).unwrap(), (3, 2));

        assert_eq!(m.flatten().unwrap(), file, "and saving puts it back");
    }

    /// v0 has one param slot per binding, so a pair of params it cannot name
    /// together has to be refused loudly rather than silently split apart.
    #[test]
    fn an_unpairable_two_param_binding_is_a_structured_error() {
        let mut hex = SeededHex::new(6);
        let mut m = sample();
        let part = node_named(&m, "Body");
        let a = m
            .add_param(
                ModelParam::new(Name::truncated("a"), 0.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        let b = m
            .add_param(
                ModelParam::new(Name::truncated("b"), 0.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        let key = BindingKey::pair(
            a,
            b,
            part,
            BindingTarget::Scalar(crate::model::ScalarTarget::Tx),
        );
        m.add_binding(&key).unwrap();

        assert!(matches!(
            m.flatten(),
            Err(ModelError::UnpairableBinding { target: "tx", .. })
        ));
    }

    /// The physics node has to follow its param through the split, or an
    /// imported pendulum would come back driving nothing.
    #[test]
    fn a_physics_target_follows_the_split_and_the_merge() {
        let mut file = sample().flatten().unwrap();
        file.doc.params[0].name = "Head".into();
        file.doc.params[0].is_vec2 = true;
        file.doc.params[0].max = [1.0, 1.0];
        file.doc.params[0].axis_points_y = vec![0.0, 1.0];
        file.doc.nodes.push(ClpNode {
            parent: Some(0),
            name: "Pendulum".into(),
            enabled: true,
            z_order: 0.0,
            transform: Default::default(),
            lock_to_root: false,
            kind: ClpNodeKind::SimplePhysics(ClpSimplePhysics {
                kind: crate::physics::PendulumKind::RigidPendulum,
                map_mode: Default::default(),
                local_only: false,
                target_param: Some(0),
                gravity: 9.8,
                length: 100.0,
                frequency: 1.0,
                angle_damping: 0.5,
                length_damping: 0.5,
                output_scale: [1.0, 1.0],
            }),
        });

        let m = Model::from_clp_file(&file).unwrap();
        let ids = m.param_ids().to_vec();
        let pendulum = node_named(&m, "Pendulum");
        match &m.node(&pendulum).unwrap().kind {
            ModelNodeKind::SimplePhysics(ph) => assert_eq!(
                ph.target_params(),
                &[Some(ids[0].clone()), Some(ids[1].clone())],
                "the pendulum drives both halves",
            ),
            _ => panic!("expected a physics node"),
        }
        assert_eq!(m.flatten().unwrap(), file);
    }

    #[test]
    fn from_clp_rejects_multiple_roots() {
        let mut file = sample().flatten().unwrap();
        file.doc.nodes[1].parent = None;

        assert!(matches!(
            Model::from_clp_file(&file),
            Err(ModelError::InvalidClpRoot)
        ));
    }

    #[test]
    fn from_clp_rejects_disconnected_cycles() {
        let mut file = sample().flatten().unwrap();
        file.doc.nodes[1].parent = Some(2);
        file.doc.nodes[2].parent = Some(1);

        assert!(matches!(
            Model::from_clp_file(&file),
            Err(ModelError::InvalidClpParent { node: 1, parent: 2 })
        ));
    }

    #[test]
    fn from_clp_applies_the_shared_aggregate_budget() {
        let file = sample().flatten().unwrap();
        let mut budget = LoadBudget::new(crate::LoadLimits {
            nodes: 1,
            ..crate::LoadLimits::default()
        });

        let err = Model::from_clp_file_with_budget(&file, &mut budget).unwrap_err();

        assert!(matches!(
            err,
            ModelError::LoadLimit(crate::LoadLimitError {
                resource: "nodes",
                ..
            })
        ));
    }

    #[test]
    fn delete_cascades_masks_and_bindings() {
        let mut m = sample();
        // delete the part; its mask lives on it, its binding targets it.
        let part = node_named(&m, "Body");
        m.delete_node(&part).unwrap();
        // the binding that targeted it is gone with it.
        assert_eq!(m.bindings().count(), 0);
        // still flattens cleanly (no dangling index).
        assert!(m.to_clp_bytes().is_ok());
    }

    #[test]
    fn reparent_rejects_cycles_and_root() {
        let mut hex = SeededHex::new(4);
        let mut m = Model::new();
        let root = m.root().clone();
        let a = m
            .add_node(&root, ModelNode::new("A", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let b = m
            .add_node(&a, ModelNode::new("B", ModelNodeKind::Group), &mut hex)
            .unwrap();
        assert!(matches!(m.reparent(&a, &b), Err(ModelError::Cycle)));
        assert!(matches!(m.reparent(&root, &a), Err(ModelError::Root(_))));
        // a legal move is fine.
        assert!(m.reparent(&b, &root).is_ok());
    }

    /// `BlendMode` reaches the wire through the part's public fields; pin one
    /// so the round-trip test above isn't the only thing that touches them.
    #[test]
    fn part_colour_fields_reach_the_wire() {
        let mut m = sample();
        let part = node_named(&m, "Body");
        m.update_node(&part, |n| {
            if let ModelNodeKind::Part(p) = &mut n.kind {
                p.blend_mode = BlendMode::Multiply;
                p.opacity = 0.25;
            }
        })
        .unwrap();
        let file = m.flatten().unwrap();
        match &file.doc.nodes[1].kind {
            ClpNodeKind::Part(p) => {
                assert_eq!(p.blend_mode, BlendMode::Multiply);
                assert_eq!(p.opacity, 0.25);
            }
            _ => panic!("expected a part at index 1"),
        }
    }
}
