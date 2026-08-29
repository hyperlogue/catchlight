//! The file boundary: stable ids in memory <-> array indices on disk.

use std::collections::HashMap;
use std::sync::Arc;

use slotmap::SlotMap;

use crate::formats::clp::{
    self, ClpBinding, ClpComposite, ClpDocument, ClpFile, ClpMask, ClpNode, ClpNodeKind, ClpParam,
    ClpPart, ClpSimplePhysics, ClpTexture, ClpWeld, FORMAT_VERSION,
};
use crate::{charge_clp_structure, LoadBudget};

use super::*;

impl Model {
    /// Snapshot the model into a `.clp` document: walk the tree in topological
    /// order, assign array indices, and remap every cross-reference. Total for a
    /// valid model (the only errors are internal-invariant violations).
    pub fn flatten(&self) -> Result<ClpFile, ModelError> {
        let order = self.nodes_in_order();
        let node_index: HashMap<NodeKey, u32> = order
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u32))
            .collect();
        let param_index: HashMap<ParamKey, u32> = self
            .param_ids()
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u32))
            .collect();
        let tex_index: HashMap<TexKey, u32> = self
            .texture_ids()
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u32))
            .collect();

        let mut nodes = Vec::with_capacity(order.len());
        for &id in &order {
            let n = self.node(id).ok_or(ModelError::UnknownNode)?;
            let parent = match n.parent() {
                Some(p) => Some(*node_index.get(&p).ok_or(ModelError::UnknownNode)?),
                None => None,
            };
            nodes.push(ClpNode {
                parent,
                name: n.name.clone(),
                enabled: n.enabled,
                z_order: n.z_order,
                transform: n.transform,
                lock_to_root: n.lock_to_root,
                kind: flatten_kind(&n.kind, &node_index, &tex_index, &param_index)?,
            });
        }

        let mut params = Vec::with_capacity(self.param_ids().len());
        for &pid in self.param_ids() {
            let p = self.param(pid).ok_or(ModelError::UnknownParam)?;
            let mut bindings = Vec::with_capacity(p.bindings.len());
            for b in &p.bindings {
                bindings.push(ClpBinding {
                    node: *node_index.get(&b.node).ok_or(ModelError::UnknownNode)?,
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.to_clp(),
                });
            }
            params.push(ClpParam {
                name: p.name.clone(),
                is_vec2: p.is_vec2,
                min: p.min,
                max: p.max,
                defaults: p.defaults,
                axis_points_x: p.axis_points_x.clone(),
                axis_points_y: p.axis_points_y.clone(),
                bindings,
            });
        }

        let mut textures = Vec::with_capacity(self.texture_ids().len());
        for &tid in self.texture_ids() {
            let t = self.texture(tid).ok_or(ModelError::UnknownTexture)?;
            textures.push(ClpTexture {
                encoding: t.encoding,
                alpha: t.alpha,
                data: (*t.data).clone(),
            });
        }

        let mut welds = Vec::with_capacity(self.welds.len());
        for w in &self.welds {
            welds.push(ClpWeld {
                a: *node_index.get(&w.a).ok_or(ModelError::UnknownNode)?,
                b: *node_index.get(&w.b).ok_or(ModelError::UnknownNode)?,
                pairs: (*w.pairs).clone(),
            });
        }

        Ok(ClpFile {
            version: FORMAT_VERSION,
            doc: ClpDocument {
                physics: self.physics,
                nodes,
                params,
                welds,
            },
            textures,
        })
    }

    /// `flatten` then encode to `.clp` bytes.
    pub fn to_clp_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let file = self.flatten()?;
        Ok(clp::encode(&file.doc, &file.textures)?)
    }

    /// Rebuild an [`Model`] from a decoded `.clp`, recovering a fresh stable
    /// id per arena slot and rewiring every index back to an id. Errors on an
    /// invalid node arena or an out-of-range cross-reference.
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

        let mut textures = SlotMap::with_key();
        let mut texture_order = Vec::with_capacity(file.textures.len());
        let mut tex_ids = Vec::with_capacity(file.textures.len());
        for t in &file.textures {
            let id = textures.insert(ModelTexture {
                encoding: t.encoding,
                alpha: t.alpha,
                data: Arc::new(t.data.clone()),
            });
            texture_order.push(id);
            tex_ids.push(id);
        }

        let mut params: SlotMap<ParamKey, ModelParam> = SlotMap::with_key();
        let mut param_order = Vec::with_capacity(doc.params.len());
        let mut param_ids = Vec::with_capacity(doc.params.len());
        for p in &doc.params {
            let id = params.insert(ModelParam {
                name: p.name.clone(),
                is_vec2: p.is_vec2,
                min: p.min,
                max: p.max,
                defaults: p.defaults,
                axis_points_x: p.axis_points_x.clone(),
                axis_points_y: p.axis_points_y.clone(),
                bindings: Vec::new(),
            });
            param_order.push(id);
            param_ids.push(id);
        }

        let mut nodes: SlotMap<NodeKey, ModelNode> = SlotMap::with_key();
        let mut node_ids = Vec::with_capacity(doc.nodes.len());
        for cn in &doc.nodes {
            node_ids.push(nodes.insert(ModelNode::new(cn.name.clone(), ModelNodeKind::Group)));
        }

        for (i, cn) in doc.nodes.iter().enumerate() {
            let self_id = node_ids[i];
            let parent = match cn.parent {
                Some(p) => Some(*node_ids.get(p as usize).ok_or(ModelError::UnknownNode)?),
                None => None,
            };
            if let Some(p) = parent {
                if let Some(pn) = nodes.get_mut(p) {
                    pn.children.push(self_id);
                }
            }
            let kind = unflatten_kind(&cn.kind, &node_ids, &param_ids, &tex_ids)?;
            if let Some(n) = nodes.get_mut(self_id) {
                n.parent = parent;
                n.enabled = cn.enabled;
                n.z_order = cn.z_order;
                n.transform = cn.transform;
                n.lock_to_root = cn.lock_to_root;
                n.kind = kind;
            }
        }
        let root = node_ids[0];

        for (j, cp) in doc.params.iter().enumerate() {
            let mut bindings = Vec::with_capacity(cp.bindings.len());
            for b in &cp.bindings {
                bindings.push(ModelBinding {
                    node: *node_ids
                        .get(b.node as usize)
                        .ok_or(ModelError::UnknownNode)?,
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.clone().into(),
                });
            }
            if let Some(p) = params.get_mut(param_ids[j]) {
                p.bindings = bindings;
            }
        }

        let mut welds = Vec::with_capacity(doc.welds.len());
        for w in &doc.welds {
            welds.push(ModelWeld {
                a: *node_ids.get(w.a as usize).ok_or(ModelError::UnknownNode)?,
                b: *node_ids.get(w.b as usize).ok_or(ModelError::UnknownNode)?,
                pairs: Arc::new(w.pairs.clone()),
            });
        }

        Ok(Model {
            physics: doc.physics,
            welds,
            nodes,
            root,
            params,
            param_order,
            textures,
            texture_order,
        })
    }

    pub fn from_clp_bytes(bytes: &[u8]) -> Result<Model, ModelError> {
        Self::from_clp_file(&clp::decode(bytes)?)
    }
}

fn flatten_kind(
    kind: &ModelNodeKind,
    node_index: &HashMap<NodeKey, u32>,
    tex_index: &HashMap<TexKey, u32>,
    param_index: &HashMap<ParamKey, u32>,
) -> Result<ClpNodeKind, ModelError> {
    Ok(match kind {
        ModelNodeKind::Group => ClpNodeKind::Group,
        ModelNodeKind::Part(p) => ClpNodeKind::Part(ClpPart {
            mesh: p.mesh.to_clp(),
            albedo: match p.albedo {
                Some(t) => *tex_index.get(&t).ok_or(ModelError::UnknownTexture)?,
                None => u32::MAX,
            },
            opacity: p.opacity,
            blend_mode: p.blend_mode,
            tint: p.tint,
            screen_tint: p.screen_tint,
            masks: flatten_masks(&p.masks, node_index)?,
            mask_threshold: p.mask_threshold,
        }),
        ModelNodeKind::Composite(c) => ClpNodeKind::Composite(ClpComposite {
            opacity: c.opacity,
            blend_mode: c.blend_mode,
            tint: c.tint,
            screen_tint: c.screen_tint,
            masks: flatten_masks(&c.masks, node_index)?,
            mask_threshold: c.mask_threshold,
            propagate_meshgroup: c.propagate_meshgroup,
        }),
        ModelNodeKind::MeshGroup(mg) => ClpNodeKind::MeshGroup(mg.to_clp()),
        ModelNodeKind::SimplePhysics(ph) => ClpNodeKind::SimplePhysics(ClpSimplePhysics {
            kind: ph.kind,
            map_mode: ph.map_mode,
            local_only: ph.local_only,
            target_param: match ph.target_param {
                Some(p) => Some(*param_index.get(&p).ok_or(ModelError::UnknownParam)?),
                None => None,
            },
            gravity: ph.gravity,
            length: ph.length,
            frequency: ph.frequency,
            angle_damping: ph.angle_damping,
            length_damping: ph.length_damping,
            output_scale: ph.output_scale,
        }),
    })
}

fn flatten_masks(
    masks: &[ModelMask],
    node_index: &HashMap<NodeKey, u32>,
) -> Result<Vec<ClpMask>, ModelError> {
    masks
        .iter()
        .map(|m| {
            Ok(ClpMask {
                source: *node_index.get(&m.source).ok_or(ModelError::UnknownNode)?,
                mode: m.mode,
            })
        })
        .collect()
}

fn unflatten_kind(
    kind: &ClpNodeKind,
    node_ids: &[NodeKey],
    param_ids: &[ParamKey],
    tex_ids: &[TexKey],
) -> Result<ModelNodeKind, ModelError> {
    Ok(match kind {
        ClpNodeKind::Group => ModelNodeKind::Group,
        ClpNodeKind::Part(p) => ModelNodeKind::Part(ModelPart {
            mesh: p.mesh.clone().into(),
            albedo: if p.albedo == u32::MAX {
                None
            } else {
                Some(
                    *tex_ids
                        .get(p.albedo as usize)
                        .ok_or(ModelError::UnknownTexture)?,
                )
            },
            opacity: p.opacity,
            blend_mode: p.blend_mode,
            tint: p.tint,
            screen_tint: p.screen_tint,
            masks: unflatten_masks(&p.masks, node_ids)?,
            mask_threshold: p.mask_threshold,
        }),
        ClpNodeKind::Composite(c) => ModelNodeKind::Composite(ModelComposite {
            opacity: c.opacity,
            blend_mode: c.blend_mode,
            tint: c.tint,
            screen_tint: c.screen_tint,
            masks: unflatten_masks(&c.masks, node_ids)?,
            mask_threshold: c.mask_threshold,
            propagate_meshgroup: c.propagate_meshgroup,
        }),
        ClpNodeKind::MeshGroup(mg) => ModelNodeKind::MeshGroup(ModelMeshGroup::from_clp(mg)),
        ClpNodeKind::SimplePhysics(ph) => ModelNodeKind::SimplePhysics(ModelPhysics {
            kind: ph.kind,
            map_mode: ph.map_mode,
            local_only: ph.local_only,
            target_param: match ph.target_param {
                Some(i) => Some(*param_ids.get(i as usize).ok_or(ModelError::UnknownParam)?),
                None => None,
            },
            gravity: ph.gravity,
            length: ph.length,
            frequency: ph.frequency,
            angle_damping: ph.angle_damping,
            length_damping: ph.length_damping,
            output_scale: ph.output_scale,
        }),
    })
}

fn unflatten_masks(masks: &[ClpMask], node_ids: &[NodeKey]) -> Result<Vec<ModelMask>, ModelError> {
    masks
        .iter()
        .map(|m| {
            Ok(ModelMask {
                source: *node_ids
                    .get(m.source as usize)
                    .ok_or(ModelError::UnknownNode)?,
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
    use crate::params::InterpolateMode;

    fn sample() -> Model {
        let mut m = Model::new();
        let tex = m.add_texture(ModelTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: Arc::new(vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4]),
        });
        let root = m.root();
        let part = m
            .add_node(
                root,
                ModelNode::new(
                    "Body",
                    ModelNodeKind::Part(ModelPart {
                        mesh: ClpMesh {
                            verts: vec![-1.0, -1.0, 1.0, -1.0, 0.0, 1.0],
                            uvs: vec![0.0, 1.0, 1.0, 1.0, 0.5, 0.0],
                            indices: ClpIndices::U16(vec![0, 1, 2]),
                            origin: [0.0, 0.0],
                        }
                        .into(),
                        albedo: Some(tex),
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        tint: [1.0; 3],
                        screen_tint: [0.0; 3],
                        masks: Vec::new(),
                        mask_threshold: 0.5,
                    }),
                ),
            )
            .unwrap();
        let mask_src = m
            .add_node(root, ModelNode::new("MaskSrc", ModelNodeKind::Group))
            .unwrap();
        if let Some(ModelNodeKind::Part(p)) = m.node_mut(part).map(|n| &mut n.kind) {
            p.masks.push(ModelMask {
                source: mask_src,
                mode: MaskMode::DodgeMask,
            });
        }
        m.add_param(ModelParam {
            name: "Mouth".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![ModelBinding {
                node: part,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::Deform(ClpCells {
                    cells: vec![ClpCell {
                        x: 1,
                        y: 0,
                        value: vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    }],
                })
                .into(),
            }],
        });
        m
    }

    #[test]
    fn flatten_roundtrips_through_clp() {
        let m = sample();
        let bytes = m.to_clp_bytes().unwrap();
        let m2 = Model::from_clp_bytes(&bytes).unwrap();
        let bytes2 = m2.to_clp_bytes().unwrap();
        assert_eq!(
            bytes, bytes2,
            "edit-model -> clp -> edit-model must be byte-stable"
        );
        assert_eq!(clp::decode(&bytes).unwrap(), m2.flatten().unwrap());
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
        let part = m
            .nodes_in_order()
            .into_iter()
            .find(|&id| matches!(m.node(id).map(|n| &n.kind), Some(ModelNodeKind::Part(_))))
            .unwrap();
        m.delete_node(part).unwrap();
        // the surviving param must have dropped the binding that targeted it.
        let pid = m.param_ids()[0];
        assert!(m.param(pid).unwrap().bindings.is_empty());
        // still flattens cleanly (no dangling index).
        assert!(m.to_clp_bytes().is_ok());
    }

    #[test]
    fn reparent_rejects_cycles_and_root() {
        let mut m = Model::new();
        let root = m.root();
        let a = m
            .add_node(root, ModelNode::new("A", ModelNodeKind::Group))
            .unwrap();
        let b = m
            .add_node(a, ModelNode::new("B", ModelNodeKind::Group))
            .unwrap();
        assert!(matches!(m.reparent(a, b), Err(ModelError::Cycle)));
        assert!(matches!(m.reparent(root, a), Err(ModelError::Root(_))));
        // a legal move is fine.
        assert!(m.reparent(b, root).is_ok());
    }
}
