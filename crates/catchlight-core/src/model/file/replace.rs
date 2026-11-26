//! Rebuilding a model in place from a pushed structure.

use super::read::{Shape, bytes_missing, duplicate, extensions_of};
use super::*;

impl Model {
    /// Rebuild from a structure-only container, taking texture payloads from
    /// `textures` and byte extension payloads from `extension_bytes`.
    /// Returning an `Arc` the client already holds lets a render cache keep its
    /// GPU texture. `budget` defaults to [`LoadBudget::default`].
    ///
    /// Keeps [`Self::identity`] and bumps [`Self::generation`]. On error the
    /// model is untouched: the new state is built whole before the swap.
    pub fn replace_structure(
        &mut self,
        structure: &[u8],
        textures: impl Fn(&TexId) -> Option<ModelTexture>,
        extension_bytes: impl Fn(&ExtensionKey) -> Option<Arc<[u8]>>,
        budget: Option<&mut LoadBudget>,
    ) -> Result<(), LoadError> {
        let mut default = LoadBudget::default();
        let budget = budget.unwrap_or(&mut default);
        let structure = clm::decode_structure_with_budget(structure, budget)?;

        let mut table = HashMap::with_capacity(structure.textures.len());
        let mut order = Vec::with_capacity(structure.textures.len());
        for want in &structure.textures {
            let found = textures(&want.id).ok_or_else(|| ClmLoadError::MissingTexture {
                id: want.id.clone(),
            })?;
            // The manifest wins over the lookup, so replicas fed the same
            // structure hold the same model.
            let texture = ModelTexture {
                encoding: want.encoding,
                alpha: want.alpha,
                data: found.data,
            };
            if table.insert(want.id.clone(), texture).is_some() {
                return Err(duplicate(want.id.clone()));
            }
            order.push(want.id.clone());
        }
        // The count was charged with the manifest.
        for texture in table.values() {
            budget.charge(LoadResource::EncodedBytes, texture.data.len() as u64)?;
        }
        charge_clm_structure(&structure.doc, budget)?;

        let checked = |key: &ExtensionKey, marker: &ClmExtensionMarker| {
            let data = extension_bytes(key).ok_or_else(|| bytes_missing(key))?;
            clm::check_marker(key, marker, &data)?;
            Ok(data)
        };
        let extensions = extensions_of(&structure.doc, checked, Some(&self.extensions))?;
        let mut next = Self::build(&structure.doc, table, order, extensions, Shape::Complete)?;
        next.unknown_sections = std::mem::take(&mut self.unknown_sections);
        self.become_model(next);
        Ok(())
    }

    /// [`Self::replace_structure`] from a `Model` in memory; payloads are
    /// `Arc`-shared.
    pub fn replace_from(&mut self, other: &Self) {
        self.become_model(other.clone());
    }

    fn become_model(&mut self, next: Self) {
        let identity = self.identity;
        *self = next;
        self.identity = identity;
        self.bump();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::id::SeededHex;
    use crate::model::file::tests::*;

    fn store(m: &Model) -> HashMap<TexId, ModelTexture> {
        m.texture_ids()
            .iter()
            .filter_map(|id| Some((id.clone(), m.texture(id)?.clone())))
            .collect()
    }

    #[test]
    fn a_structures_textures_are_listed_without_building_a_model() {
        let m = sample();
        let listed = clm::structure_texture_ids(
            &m.to_structure_bytes().unwrap(),
            &mut LoadBudget::default(),
        )
        .unwrap();

        assert_eq!(
            listed.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            m.texture_ids(),
        );
        let held = m.texture(&listed[0].id).unwrap();
        assert_eq!(listed[0].encoding, held.encoding);
        assert_eq!(listed[0].alpha, held.alpha);
    }

    #[test]
    fn applying_a_structure_rebuilds_the_model_over_the_payloads_it_has() {
        let server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let held = store(&replica);

        replica
            .replace_structure(
                &server.to_structure_bytes().unwrap(),
                |id| held.get(id).cloned(),
                |_| None,
                None,
            )
            .unwrap();

        assert_eq!(
            replica.to_clm_bytes().unwrap(),
            server.to_clm_bytes().unwrap()
        );
        assert_eq!(replica.texture_ids(), server.texture_ids());
        for id in replica.texture_ids() {
            assert!(
                Arc::ptr_eq(&replica.texture(id).unwrap().data, &held[id].data),
                "the payload the store held is the payload the model now holds",
            );
        }
    }

    #[test]
    fn a_structure_is_read_as_strictly_as_a_file() {
        let m = sample();
        let mut doc = m.to_clm_structure().unwrap();
        doc.nodes[1].parent = Some(NodeId::new("ghost").unwrap());
        let refs: Vec<ClmTextureRef> = m
            .texture_ids()
            .iter()
            .map(|id| ClmTextureRef {
                id: id.clone(),
                encoding: m.texture(id).unwrap().encoding,
                alpha: m.texture(id).unwrap().alpha,
            })
            .collect();
        let bytes = clm::encode_structure(&doc, &refs).unwrap();

        let mut replica = m.clone();
        let held = store(&m);
        let err = replica
            .replace_structure(&bytes, |id| held.get(id).cloned(), |_| None, None)
            .unwrap_err();

        assert!(matches!(
            err,
            LoadError::Invalid(ClmLoadError::DanglingParent { .. })
        ));
    }

    #[test]
    fn a_payload_the_caller_lacks_names_itself_and_changes_nothing() {
        let server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let (was, generation) = (replica.to_clm_bytes().unwrap(), replica.generation());

        let err = replica
            .replace_structure(
                &server.to_structure_bytes().unwrap(),
                |_| None,
                |_| None,
                None,
            )
            .unwrap_err();

        let wanted = server.texture_ids()[0].clone();
        assert!(
            matches!(&err, LoadError::Invalid(ClmLoadError::MissingTexture { id }) if *id == wanted),
            "got {err:?}",
        );
        assert_eq!(replica.to_clm_bytes().unwrap(), was);
        assert_eq!(
            replica.generation(),
            generation,
            "a refused push is not an edit"
        );
    }

    #[test]
    fn replacing_keeps_the_identity_and_moves_the_generation() {
        let server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let (identity, generation) = (replica.identity(), replica.generation());
        let held = store(&replica);

        replica
            .replace_structure(
                &server.to_structure_bytes().unwrap(),
                |id| held.get(id).cloned(),
                |_| None,
                None,
            )
            .unwrap();
        assert_eq!(replica.identity(), identity);
        assert!(replica.generation() > generation);

        let before = replica.generation();
        replica.replace_from(&server);
        assert_eq!(replica.identity(), identity, "not even from another model");
        assert!(replica.generation() > before);
    }

    #[test]
    fn replace_from_shares_the_payloads_it_is_handed() {
        let server = sample();
        let mut replica = Model::new();
        replica.replace_from(&server);

        assert_eq!(
            replica.to_clm_bytes().unwrap(),
            server.to_clm_bytes().unwrap()
        );
        for id in server.texture_ids() {
            assert!(Arc::ptr_eq(
                &replica.texture(id).unwrap().data,
                &server.texture(id).unwrap().data,
            ));
        }
    }

    #[test]
    fn a_puppet_carries_its_pose_and_its_drivers_across_a_replace() {
        let mut server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let held = store(&replica);

        let param = replica.param_ids()[0].clone();
        let mut puppet = crate::Puppet::new(&replica);
        puppet.set_param_value(&param, 0.75);
        for _ in 0..30 {
            puppet.tick(&replica, 1.0 / 60.0);
        }
        let posed = puppet.param_value(&param).unwrap();

        let root = server.root().unwrap().clone();
        server
            .add_node(
                &root,
                ModelNode::new("Added", ModelNodeKind::Group),
                &mut SeededHex::new(77),
            )
            .unwrap();
        replica
            .replace_structure(
                &server.to_structure_bytes().unwrap(),
                |id| held.get(id).cloned(),
                |_| None,
                None,
            )
            .unwrap();

        puppet.sync(&replica);
        assert_eq!(
            puppet.baked_generation(),
            replica.generation(),
            "it rebaked"
        );
        assert_eq!(puppet.param_value(&param), Some(posed), "the pose crossed");
    }
}
