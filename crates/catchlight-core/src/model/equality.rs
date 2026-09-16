//! Authored-content comparison for transactional model publication.
//!
//! Runtime identity, generation and derived caches are not authored state.
//! The CLM structure defines authored ordering and sparse-cell semantics;
//! encoded asset bytes are compared directly without image decoding or copying.

use super::{Model, ModelError};

impl Model {
    /// Whether both models contain exactly the same authored structure and
    /// assets. Ignores runtime identity, generation and derived caches.
    ///
    /// Texture order, encoding, alpha convention and encoded bytes all matter,
    /// as do sparse binding holes, animations and extension values. Large
    /// payloads remain borrowed; structure comparison materializes the ordinary
    /// CLM structure. Invalid intermediate models return their export error.
    pub fn authored_eq(&self, other: &Self) -> Result<bool, ModelError> {
        if self.texture_order != other.texture_order || self.extensions != other.extensions {
            return Ok(false);
        }
        for id in &self.texture_order {
            let a = self.textures.get(id).ok_or(ModelError::UnknownTexture)?;
            let b = other.textures.get(id).ok_or(ModelError::UnknownTexture)?;
            if a.encoding != b.encoding || a.alpha != b.alpha || a.data != b.data {
                return Ok(false);
            }
        }
        Ok(self.to_clm_structure()? == other.to_clm_structure()?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::{
        formats::clm::{ClmMesh, TextureAlpha, TextureEncoding},
        BindingKey, BindingTarget, ExtensionKey, ExtensionValue, ModelNode, ModelNodeKind,
        ModelParam, ModelPart, ModelTexture, Name, ParamId, ScalarTarget, SeededHex,
    };

    use super::*;

    #[test]
    fn identity_generation_and_undoing_a_change_do_not_affect_authored_equality() {
        let model = Model::new();
        let mut other = Model::new();
        assert_ne!(model.identity(), other.identity());
        assert!(model.authored_eq(&other).unwrap());
        let root = other.root().unwrap().clone();
        let initial_name = other.node(&root).unwrap().name.clone();
        other
            .update_node(&root, |node| node.name = Name::truncated("changed"))
            .unwrap();
        assert!(!model.authored_eq(&other).unwrap());
        other
            .update_node(&root, |node| node.name = initial_name)
            .unwrap();
        assert_ne!(model.generation(), other.generation());
        assert!(model.authored_eq(&other).unwrap());
    }

    #[test]
    fn encoded_texture_bytes_and_metadata_are_authored_but_allocation_is_not() {
        let mut model = Model::new();
        let mut hex = SeededHex::new(9);
        let root = model.root().unwrap().clone();
        let part = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
                ),
                &mut hex,
            )
            .unwrap();
        let texture = model
            .add_texture(
                &part,
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: vec![1, 2, 3].into(),
                },
                &mut hex,
            )
            .unwrap();
        let mut other = model.clone();
        other.textures.get_mut(&texture).unwrap().data = vec![1, 2, 3].into();
        assert!(model.authored_eq(&other).unwrap());
        other.textures.get_mut(&texture).unwrap().encoding = TextureEncoding::Tga;
        assert!(!model.authored_eq(&other).unwrap());
        other.textures.get_mut(&texture).unwrap().encoding = TextureEncoding::Png;
        other.textures.get_mut(&texture).unwrap().data = vec![3, 2, 1].into();
        assert!(!model.authored_eq(&other).unwrap());
    }

    #[test]
    fn extension_payloads_and_json_values_are_authored_state() {
        let mut model = Model::new();
        let key = ExtensionKey::new("test.metadata").unwrap();
        model
            .set_extension(
                key.clone(),
                ExtensionValue::Json(serde_json::json!({"value": 1})),
            )
            .unwrap();
        let mut other = model.clone();
        assert!(model.authored_eq(&other).unwrap());
        other
            .set_extension(
                key.clone(),
                ExtensionValue::Json(serde_json::json!({"value": 2})),
            )
            .unwrap();
        assert!(!model.authored_eq(&other).unwrap());
        model
            .set_extension(key.clone(), ExtensionValue::Bytes(vec![1, 2, 3].into()))
            .unwrap();
        other
            .set_extension(key.clone(), ExtensionValue::Bytes(vec![1, 2, 3].into()))
            .unwrap();
        assert!(model.authored_eq(&other).unwrap());
        other
            .set_extension(key, ExtensionValue::Bytes(vec![3, 2, 1].into()))
            .unwrap();
        assert!(!model.authored_eq(&other).unwrap());
    }

    #[test]
    fn authored_holes_and_binding_owned_sampling_positions_are_distinct() {
        let mut model = Model::new();
        let param = ParamId::new("drive").unwrap();
        model
            .add_param_with_id(
                param.clone(),
                ModelParam::new(Name::truncated("drive"), 0.0, 1.0, 0.0),
            )
            .unwrap();
        let key = BindingKey::new(
            param.clone(),
            model.root().unwrap().clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        model
            .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        let mut other = model.clone();
        other.set_binding_key(&key, [0, 0], 0.0).unwrap();
        assert!(
            !model.authored_eq(&other).unwrap(),
            "authored identity differs from an unset hole"
        );
        other.unset_binding_key(&key, [0, 0]).unwrap();
        assert!(model.authored_eq(&other).unwrap());
        other.key_move(&key, &param, 1, 0.25).unwrap();
        assert!(!model.authored_eq(&other).unwrap());
    }
}
