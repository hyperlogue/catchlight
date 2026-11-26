//! Writing a model to `.clm`: total and deterministic.

use super::*;

fn clm_extension(value: &ExtensionValue) -> ClmExtension {
    let marker = |data: &[u8]| ClmExtensionMarker {
        size: data.len() as u64,
        hash: clm::extension_hash(data),
    };
    match value {
        ExtensionValue::Json(json) => ClmExtension::Json(json.clone()),
        ExtensionValue::Bytes(data) => ClmExtension::Bytes(marker(data)),
        ExtensionValue::Maintained(raw) => ClmExtension::Maintained {
            version: raw.version,
            refs: raw.refs.clone(),
            payload: match &raw.payload {
                Payload::Json(json) => ClmPayload::Json(json.clone()),
                Payload::Bytes(data) => ClmPayload::Bytes(marker(data)),
            },
        },
    }
}

impl Model {
    /// Everything [`Self::to_clm_structure`] refuses, without building it.
    /// No edit produces either case; both stay checked so this writer's output
    /// is always readable.
    pub(in crate::model) fn check_writable(&self) -> Result<(), ModelError> {
        let fragment = self.is_fragment();
        for weld in &self.welds {
            // A fragment's weld into a base part has one end this model
            // cannot see, and only the end it can see is checked.
            for end in [weld.a(), weld.b()] {
                match validate::WeldEnd::of(self.node(end).map(|n| &n.kind), fragment) {
                    validate::WeldEnd::Part | validate::WeldEnd::Deferred => {}
                    validate::WeldEnd::NotAPart(_) => return Err(ModelError::WeldEndNotAPart),
                    validate::WeldEnd::Missing => return Err(ModelError::UnknownNode),
                }
            }
        }
        for b in &self.bindings {
            check_deform_cells(&b.key.node, self.deform_len(&b.key.node), &b.values)?;
        }
        Ok(())
    }

    /// Total for any Model whose deform cells are sized to their meshes.
    pub fn to_clm_structure(&self) -> Result<ClmStructure, ModelError> {
        self.check_writable()?;
        let order = self.nodes_in_order();
        let mut nodes = Vec::with_capacity(order.len());
        for id in &order {
            let node = self.node(id).ok_or(ModelError::UnknownNode)?;
            nodes.push(ClmNode {
                id: id.clone(),
                parent: node.parent().cloned(),
                name: node.name.to_string(),
                enabled: node.enabled,
                z_order: node.z_order,
                transform: node.transform,
                lock_to_root: node.lock_to_root,
                kind: clm_kind(&node.kind),
            });
        }

        let mut welds = Vec::with_capacity(self.welds.len());
        for weld in &self.welds {
            welds.push(ClmWeld {
                a: weld.a().clone(),
                b: weld.b().clone(),
                pairs: weld
                    .pairs()
                    .iter()
                    .map(|pair| ClmSlotPair {
                        a: pair.a.clone(),
                        b: pair.b.clone(),
                        weight: pair.weight,
                    })
                    .collect(),
            });
        }

        let mut params = Vec::with_capacity(self.param_order.len());
        for id in &self.param_order {
            let p = self.param(id).ok_or(ModelError::UnknownParam)?;
            params.push(ClmParam {
                id: id.clone(),
                name: p.name.to_string(),
                min: p.min,
                max: p.max,
                default: p.default,
            });
        }

        let mut bindings = Vec::with_capacity(self.bindings.len());
        for b in &self.bindings {
            bindings.push(ClmBinding {
                params: b.key.params.iter().cloned().collect(),
                key_positions: b.key_positions.clone(),
                node: b.key.node.clone(),
                interpolate_mode: b.interpolate_mode(),
                values: b.values.to_clm(),
            });
        }

        Ok(ClmStructure {
            nodes,
            params,
            bindings,
            welds,
            extensions: self
                .extensions
                .iter()
                .map(|(key, value)| (key.clone(), clm_extension(value)))
                .collect(),
        })
    }

    /// Copies payloads out of their `Arc`s; [`Self::to_clm_bytes`] copies once.
    pub fn to_clm_file(&self) -> Result<ClmFile, ModelError> {
        let doc = self.to_clm_structure()?;
        let mut textures = Vec::with_capacity(self.texture_order.len());
        for id in &self.texture_order {
            let t = self.texture(id).ok_or(ModelError::UnknownTexture)?;
            textures.push(ClmTexture {
                id: id.clone(),
                encoding: t.encoding,
                alpha: t.alpha,
                data: t.data.to_vec(),
            });
        }
        Ok(ClmFile {
            doc,
            textures,
            extensions: self.extension_blobs(),
            unknown_sections: self.unknown_sections.clone(),
        })
    }

    fn extension_blobs(&self) -> Vec<ClmExtensionBlob> {
        self.extensions
            .iter()
            .filter_map(|(key, value)| {
                Some(ClmExtensionBlob {
                    key: key.clone(),
                    data: value.bytes()?.to_vec(),
                })
            })
            .collect()
    }

    /// Serializes payloads from where the model holds them, copying each once.
    pub fn to_clm_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let doc = self.to_clm_structure()?;
        let mut textures = Vec::with_capacity(self.texture_order.len());
        for id in &self.texture_order {
            let t = self.texture(id).ok_or(ModelError::UnknownTexture)?;
            textures.push(clm::ClmTextureBytes {
                id,
                encoding: t.encoding,
                alpha: t.alpha,
                data: &t.data,
            });
        }
        Ok(clm::encode_borrowed(
            &doc,
            &textures,
            &self.extension_blobs(),
            &self.unknown_sections,
        )?)
    }

    /// A structure-only container: never touches a texture payload.
    pub fn to_structure_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let doc = self.to_clm_structure()?;
        let mut textures = Vec::with_capacity(self.texture_order.len());
        for id in &self.texture_order {
            let t = self.texture(id).ok_or(ModelError::UnknownTexture)?;
            textures.push(ClmTextureRef {
                id: id.clone(),
                encoding: t.encoding,
                alpha: t.alpha,
            });
        }
        Ok(clm::encode_structure(&doc, &textures)?)
    }
}

fn clm_kind(kind: &ModelNodeKind) -> ClmNodeKind {
    match kind {
        ModelNodeKind::Group => ClmNodeKind::Group,
        ModelNodeKind::Part(p) => ClmNodeKind::Part(ClmPart {
            mesh: p.mesh().clone(),
            albedo: p.albedo().cloned(),
            opacity: p.opacity,
            blend_mode: p.blend_mode,
            tint: p.tint,
            screen_tint: p.screen_tint,
            masks: clm_masks(p.masks()),
            mask_threshold: p.mask_threshold,
            slots: p
                .slots()
                .iter()
                .map(|slot| ClmSlot {
                    id: slot.id().clone(),
                    vertex: slot.vertex(),
                })
                .collect(),
        }),
        ModelNodeKind::Composite(c) => ClmNodeKind::Composite(ClmComposite {
            opacity: c.opacity,
            blend_mode: c.blend_mode,
            tint: c.tint,
            screen_tint: c.screen_tint,
            masks: clm_masks(c.masks()),
            mask_threshold: c.mask_threshold,
            propagate_meshgroup: c.propagate_meshgroup,
        }),
        ModelNodeKind::MeshGroup(mg) => ClmNodeKind::MeshGroup(ClmMeshGroup {
            mesh: mg.mesh().clone(),
            translate_children: mg.translate_children,
        }),
        ModelNodeKind::MotionResponse(r) => ClmNodeKind::MotionResponse(r.clone()),
        ModelNodeKind::Spine(spine) => ClmNodeKind::Spine(ClmSpine {
            joints: spine.joints().to_vec(),
            targets: spine.targets().to_vec(),
        }),
    }
}

fn clm_masks(masks: &[ModelMask]) -> Vec<ClmMask> {
    masks
        .iter()
        .map(|m| ClmMask {
            source: m.source().clone(),
            mode: m.mode(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::clm::{ClmCell, ClmCells};
    use crate::model::file::tests::*;

    /// Twice over, since `HashMap` order leaking into the writer would only
    /// show up on a second pass.
    #[test]
    fn saving_the_same_model_twice_writes_the_same_bytes() {
        let m = sample();
        let once = m.to_clm_bytes().unwrap();
        assert_eq!(m.to_clm_bytes().unwrap(), once);

        let reopened = Model::from_clm_bytes(&once).unwrap();
        assert_eq!(reopened.to_clm_bytes().unwrap(), once);
        assert_eq!(reopened.to_clm_bytes().unwrap(), once);
    }

    #[test]
    fn a_deform_cell_the_mesh_cannot_take_cannot_be_written() {
        let mut m = sample();
        let upper = m
            .nodes_in_order()
            .into_iter()
            .find(|id| m.node(id).is_some_and(|n| n.name.as_str() == "Upper"))
            .unwrap();
        assert!(matches!(
            m.set_node_mesh_with(&upper, quad(0.0), |_, _, offsets| offsets[..2].to_vec()),
            Err(ModelError::DeformLength {
                expected: 8,
                got: 2
            })
        ));
        assert!(m.to_clm_file().is_ok(), "the refused refit changed nothing");
        for binding in &mut m.bindings {
            if let ClmBindingValues::Deform(cells) = binding.values_mut() {
                for cell in &mut cells.cells {
                    cell.value.truncate(2);
                }
            }
        }

        assert!(matches!(
            m.to_clm_file(),
            Err(ModelError::InvalidClm(ClmLoadError::DeformCellShape {
                got: 2,
                expected: 8,
                ..
            }))
        ));
    }

    #[test]
    fn authored_cells_reach_the_wire() {
        let file = sample().to_clm_file().unwrap();
        match &file.doc.bindings[0].values {
            ClmBindingValues::Deform(ClmCells { cells }) => assert!(
                cells
                    .iter()
                    .any(|c: &ClmCell<Vec<f32>>| c.x == 1 && c.y == 1 && c.value == vec![1.0; 8]),
                "the authored cell is on the wire: {cells:?}",
            ),
            other => panic!("expected a deform binding, got {other:?}"),
        }
    }

    #[test]
    fn a_structure_carries_the_files_own_structure_section() {
        let m = sample();
        let file = clm::decode(&m.to_clm_bytes().unwrap()).unwrap();
        let structure = clm::decode_structure(&m.to_structure_bytes().unwrap()).unwrap();

        assert_eq!(structure.doc, file.doc);
        assert_eq!(
            structure.textures,
            file.textures
                .iter()
                .map(ClmTextureRef::from)
                .collect::<Vec<_>>(),
            "the manifest is the texture table with the payloads taken out",
        );
        assert!(
            m.to_structure_bytes().unwrap().len() < m.to_clm_bytes().unwrap().len(),
            "a structure carries no payloads",
        );
        assert_eq!(
            m.to_structure_bytes().unwrap(),
            m.to_structure_bytes().unwrap(),
            "and it is byte-stable like the file it comes from",
        );
    }

    #[test]
    fn the_two_container_shapes_refuse_each_other() {
        let m = sample();
        assert!(matches!(
            clm::decode(&m.to_structure_bytes().unwrap()),
            Err(clm::ClmError::StructureOnly),
        ));
        assert!(matches!(
            clm::decode_structure(&m.to_clm_bytes().unwrap()),
            Err(clm::ClmError::NotAStructure),
        ));
    }
}
