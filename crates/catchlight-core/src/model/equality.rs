//! Authored-content comparison for transactional model publication.
//!
//! A model's generation is never reissued, so the same model at the same
//! generation compares equal without being read; `Arc`-shared leaves compare
//! equal without being read.

use super::{Model, ModelBinding, ModelError, ModelNode, ModelNodeKind, same};

impl Model {
    /// Whether both models would write the same authored structure and assets.
    /// A model that could not be written returns the error its export would.
    pub fn authored_eq(&self, other: &Self) -> Result<bool, ModelError> {
        if self.identity == other.identity && self.generation == other.generation {
            return Ok(true);
        }
        if self.texture_order != other.texture_order || self.extensions != other.extensions {
            return Ok(false);
        }
        if self.unknown_sections != other.unknown_sections {
            return Ok(false);
        }
        for id in &self.texture_order {
            let unknown = || ModelError::UnknownTexture(id.clone());
            let a = self.textures.get(id).ok_or_else(unknown)?;
            let b = other.textures.get(id).ok_or_else(unknown)?;
            if a.encoding != b.encoding || a.alpha != b.alpha || a.data != b.data {
                return Ok(false);
            }
        }
        self.check_writable()?;
        other.check_writable()?;
        Ok(self.structure_eq(other))
    }

    /// Extensions are compared by [`Self::authored_eq`] already.
    fn structure_eq(&self, other: &Self) -> bool {
        if self.param_order != other.param_order
            || self.bindings.len() != other.bindings.len()
            || self.welds.len() != other.welds.len()
        {
            return false;
        }

        let order = self.nodes_in_order();
        if order != other.nodes_in_order() {
            return false;
        }
        let nodes_agree = order
            .iter()
            .all(|id| match (self.nodes.get(id), other.nodes.get(id)) {
                (Some(a), Some(b)) => node_eq(a, b),
                _ => false,
            });
        let params_agree = || {
            self.param_order
                .iter()
                .all(|id| match (self.params.get(id), other.params.get(id)) {
                    (Some(a), Some(b)) => {
                        a.name.as_str() == b.name.as_str()
                            && a.min == b.min
                            && a.max == b.max
                            && a.default == b.default
                    }
                    _ => false,
                })
        };
        nodes_agree
            && params_agree()
            && self
                .bindings
                .iter()
                .zip(&other.bindings)
                .all(|(a, b)| binding_eq(a, b))
            && self.welds == other.welds
    }
}

fn node_eq(a: &ModelNode, b: &ModelNode) -> bool {
    a.parent == b.parent
        && a.name.as_str() == b.name.as_str()
        && a.enabled == b.enabled
        && a.z_order == b.z_order
        && a.transform == b.transform
        && a.lock_to_root == b.lock_to_root
        && kind_eq(&a.kind, &b.kind)
}

fn kind_eq(a: &ModelNodeKind, b: &ModelNodeKind) -> bool {
    use ModelNodeKind as Kind;
    match (a, b) {
        (Kind::Group, Kind::Group) => true,
        (Kind::Part(a), Kind::Part(b)) => {
            a.opacity == b.opacity
                && a.blend_mode == b.blend_mode
                && a.tint == b.tint
                && a.screen_tint == b.screen_tint
                && a.mask_threshold == b.mask_threshold
                && a.albedo == b.albedo
                && same(&a.masks, &b.masks)
                && same(&a.slots, &b.slots)
                && same(&a.mesh.0, &b.mesh.0)
        }
        (Kind::Composite(a), Kind::Composite(b)) => {
            a.opacity == b.opacity
                && a.blend_mode == b.blend_mode
                && a.tint == b.tint
                && a.screen_tint == b.screen_tint
                && a.mask_threshold == b.mask_threshold
                && a.propagate_meshgroup == b.propagate_meshgroup
                && same(&a.masks, &b.masks)
        }
        (Kind::MeshGroup(a), Kind::MeshGroup(b)) => {
            a.translate_children == b.translate_children && same(&a.mesh.0, &b.mesh.0)
        }
        (Kind::MotionResponse(a), Kind::MotionResponse(b)) => a == b,
        (Kind::Spine(a), Kind::Spine(b)) => {
            same(&a.joints, &b.joints) && same(&a.targets, &b.targets)
        }
        _ => false,
    }
}

/// The variant of the values *is* the target.
fn binding_eq(a: &ModelBinding, b: &ModelBinding) -> bool {
    a.key.params == b.key.params
        && a.key.node == b.key.node
        && a.interpolate_mode == b.interpolate_mode
        && a.key_positions == b.key_positions
        && same(&a.values.0, &b.values.0)
}

#[cfg(test)]
mod tests {
    use crate::{
        BindingKey, BindingTarget, BlendMode, ExtensionKey, ExtensionValue, InterpolateMode,
        MaskMode, ModelComposite, ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam, ModelPart,
        ModelSpine, ModelTexture, ModelWeld, MotionResponse, Name, NodeId, ParamId, ScalarTarget,
        SeededHex, SlotId, SlotPair,
        formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding},
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
        // The pokes below skip the generation bump every real edit makes.
        other.bump();
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

    #[test]
    fn the_same_model_at_the_same_generation_is_equal_without_being_read() {
        let r = rich();
        let snapshot = r.model.clone();
        assert_eq!(r.model.generation(), snapshot.generation());
        assert!(r.model.authored_eq(&snapshot).unwrap());

        let mut poked = r.model.clone();
        poked.nodes.get_mut(&r.upper).unwrap().properties.name = Name::truncated("poked");
        assert!(
            r.model.authored_eq(&poked).unwrap(),
            "nothing was compared: the generation says nothing changed"
        );
        poked.bump();
        assert!(
            !r.model.authored_eq(&poked).unwrap(),
            "once the generation moves, the content is what answers"
        );

        let twin = rich();
        assert_ne!(r.model.identity(), twin.model.identity());
        assert!(r.model.authored_eq(&twin.model).unwrap());
    }

    /// Reference: compare the two written structures.
    fn by_writing_both(a: &Model, b: &Model) -> bool {
        a.texture_order == b.texture_order
            && a.extensions == b.extensions
            && a.texture_order.iter().all(|id| {
                let (x, y) = (&a.textures[id], &b.textures[id]);
                x.encoding == y.encoding && x.alpha == y.alpha && x.data == y.data
            })
            && a.to_clm_structure().unwrap() == b.to_clm_structure().unwrap()
    }

    #[test]
    fn the_in_place_comparison_agrees_with_comparing_written_structures() {
        let r = rich();
        type Edit = Box<dyn Fn(&mut Model, &Rich, &mut SeededHex)>;
        let edits: Vec<(&str, Edit)> = vec![
            (
                "name",
                Box::new(|m, r, _| {
                    m.update_node(&r.upper, |n| n.name = Name::truncated("renamed"))
                        .unwrap();
                }),
            ),
            (
                "z order",
                Box::new(|m, r, _| m.update_node(&r.upper, |n| n.z_order = 3.0).unwrap()),
            ),
            (
                "transform",
                Box::new(|m, r, _| {
                    m.update_node(&r.lower, |n| n.transform.rotation[2] = 0.5)
                        .unwrap();
                }),
            ),
            (
                "enabled",
                Box::new(|m, r, _| m.update_node(&r.lower, |n| n.enabled = false).unwrap()),
            ),
            (
                "lock to root",
                Box::new(|m, r, _| m.update_node(&r.lower, |n| n.lock_to_root = true).unwrap()),
            ),
            (
                "part opacity",
                Box::new(|m, r, _| {
                    m.update_node_kind(&r.upper, |kind| {
                        if let ModelNodeKind::Part(p) = kind {
                            p.opacity = 0.5;
                        }
                    })
                    .unwrap();
                }),
            ),
            (
                "composite blend mode",
                Box::new(|m, r, _| {
                    m.update_node_kind(&r.face, |kind| {
                        if let ModelNodeKind::Composite(c) = kind {
                            c.blend_mode = BlendMode::Multiply;
                        }
                    })
                    .unwrap();
                }),
            ),
            (
                "mesh group flag",
                Box::new(|m, r, _| {
                    m.update_node_kind(&r.bend, |kind| {
                        if let ModelNodeKind::MeshGroup(mg) = kind {
                            mg.translate_children = false;
                        }
                    })
                    .unwrap();
                }),
            ),
            (
                "part mesh",
                Box::new(|m, r, _| {
                    m.set_node_mesh(&r.lower, quad(5.0)).unwrap();
                }),
            ),
            (
                "mesh group mesh",
                Box::new(|m, r, _| {
                    m.set_node_mesh(&r.bend, quad(5.0)).unwrap();
                }),
            ),
            (
                "mask added",
                Box::new(|m, r, _| m.mask_add(&r.upper, &r.lower, MaskMode::Mask).unwrap()),
            ),
            (
                "mask mode",
                Box::new(|m, r, _| m.mask_set_mode(&r.face, 0, MaskMode::Mask).unwrap()),
            ),
            (
                "slot added",
                Box::new(|m, r, _| m.slot_add(&r.upper, slot("extra")).unwrap()),
            ),
            (
                "slot refilled",
                Box::new(|m, r, _| m.slot_fill(&r.upper, &slot("seam"), 2).unwrap()),
            ),
            (
                "weld weight",
                Box::new(|m, r, _| {
                    m.set_weld_slot_weight(&r.upper, &r.lower, &slot("seam"), 0.75)
                        .unwrap();
                }),
            ),
            (
                "weld removed",
                Box::new(|m, _, _| m.set_welds(Vec::new()).unwrap()),
            ),
            (
                "param added",
                Box::new(|m, _, hex| {
                    m.add_param(ModelParam::new(Name::truncated("new"), 0.0, 1.0, 0.0), hex)
                        .unwrap();
                }),
            ),
            (
                "param name",
                Box::new(|m, r, _| m.set_param_name(&r.x, Name::truncated("other")).unwrap()),
            ),
            (
                "param range",
                Box::new(|m, r, _| m.set_param_range(&r.x, -2.0, 2.0).unwrap()),
            ),
            (
                "param default",
                Box::new(|m, r, _| m.set_param_default(&r.x, 0.5).unwrap()),
            ),
            (
                "scalar cell",
                Box::new(|m, r, _| m.set_binding_key(&r.tx, [0, 0], 4.0).unwrap()),
            ),
            (
                "deform cell",
                Box::new(|m, r, _| {
                    m.set_deform_vertices(&r.deform, [0, 0], vec![0.5; 8])
                        .unwrap();
                }),
            ),
            (
                "key position",
                Box::new(|m, r, _| m.key_move(&r.tx, &r.x, 0, 0.1).unwrap()),
            ),
            (
                "interpolation",
                Box::new(|m, r, _| {
                    m.set_binding_interpolate(&r.tx, InterpolateMode::Stepped)
                        .unwrap();
                }),
            ),
            (
                "binding added",
                Box::new(|m, r, _| {
                    m.add_binding(&BindingKey::new(
                        r.x.clone(),
                        r.lower.clone(),
                        BindingTarget::Scalar(ScalarTarget::Opacity),
                    ))
                    .unwrap();
                }),
            ),
            (
                "spine joints",
                Box::new(|m, r, _| {
                    m.set_spine_joints(&r.spine, vec![[0.0, -5.0], [0.0, -9.0]])
                        .unwrap();
                }),
            ),
            (
                "spine targets",
                Box::new(|m, r, _| {
                    m.set_spine_targets(&r.spine, vec![None, Some(r.x.clone())])
                        .unwrap();
                }),
            ),
            (
                "response",
                Box::new(|m, r, _| {
                    m.set_motion_response(
                        &r.sway,
                        MotionResponse {
                            period_s: 1.5,
                            ..MotionResponse::default()
                        },
                    )
                    .unwrap();
                }),
            ),
            (
                "sibling order",
                Box::new(|m, r, _| m.reorder(&r.lower, 0).unwrap()),
            ),
            (
                "parent",
                Box::new(|m, r, _| m.reparent(&r.lower, &r.face).unwrap()),
            ),
            (
                "node added",
                Box::new(|m, r, hex| {
                    m.add_node(&r.face, ModelNode::new("new", ModelNodeKind::Group), hex)
                        .unwrap();
                }),
            ),
            (
                "node deleted",
                Box::new(|m, r, _| m.delete_node(&r.sway).unwrap()),
            ),
            (
                "extension",
                Box::new(|m, _, _| {
                    m.set_extension(
                        ExtensionKey::new("test.note").unwrap(),
                        ExtensionValue::Json(serde_json::json!(1)),
                    )
                    .unwrap();
                }),
            ),
            (
                "texture added",
                Box::new(|m, r, hex| {
                    m.add_texture(&r.lower, texture(7), hex).unwrap();
                }),
            ),
        ];

        for (what, edit) in &edits {
            let mut hex = SeededHex::new(77);
            let mut edited = r.model.clone();
            edit(&mut edited, &r, &mut hex);
            assert_ne!(edited.generation(), r.model.generation(), "{what}");

            assert!(
                !by_writing_both(&r.model, &edited),
                "{what}: the edit was meant to change the written structure"
            );
            assert!(!r.model.authored_eq(&edited).unwrap(), "{what}");
            assert!(!edited.authored_eq(&r.model).unwrap(), "{what}, reversed");

            let mut again = r.model.clone();
            edit(&mut again, &r, &mut SeededHex::new(77));
            assert!(by_writing_both(&edited, &again), "{what}");
            assert!(edited.authored_eq(&again).unwrap(), "{what}, made twice");
        }

        let mut touched = r.model.clone();
        let name = touched.node(&r.upper).unwrap().name.clone();
        touched.update_node(&r.upper, |n| n.name = name).unwrap();
        assert_ne!(touched.generation(), r.model.generation());
        assert!(by_writing_both(&r.model, &touched));
        assert!(r.model.authored_eq(&touched).unwrap());
    }

    #[test]
    fn an_unwritable_model_reports_its_export_error() {
        let r = rich();
        let mut broken = r.model.clone();
        broken.nodes.get_mut(&r.lower).unwrap().kind = ModelNodeKind::Group;
        broken.bindings.retain(|b| b.key.node != r.lower);
        broken.bump();

        assert!(matches!(
            broken.to_clm_structure(),
            Err(ModelError::WeldEndNotAPart)
        ));
        assert!(matches!(
            r.model.authored_eq(&broken),
            Err(ModelError::WeldEndNotAPart)
        ));
        assert!(matches!(
            broken.authored_eq(&r.model),
            Err(ModelError::WeldEndNotAPart)
        ));
    }

    fn slot(id: &str) -> SlotId {
        SlotId::new(id).unwrap()
    }

    fn quad(x: f32) -> ClmMesh {
        ClmMesh {
            verts: vec![x - 1.0, -1.0, x + 1.0, -1.0, x + 1.0, 1.0, x - 1.0, 1.0],
            uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    fn texture(byte: u8) -> ModelTexture {
        ModelTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: vec![byte; 4].into(),
        }
    }

    struct Rich {
        model: Model,
        upper: NodeId,
        lower: NodeId,
        face: NodeId,
        bend: NodeId,
        spine: NodeId,
        sway: NodeId,
        x: ParamId,
        tx: BindingKey,
        deform: BindingKey,
    }

    fn rich() -> Rich {
        let mut hex = SeededHex::new(21);
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let mut add = |model: &mut Model, parent: &NodeId, name: &str, kind: ModelNodeKind| {
            model
                .add_node(parent, ModelNode::new(name, kind), &mut hex)
                .unwrap()
        };
        let part = |x: f32| ModelNodeKind::Part(ModelPart::new(quad(x)));
        let upper = add(&mut model, &root, "upper", part(0.0));
        let lower = add(&mut model, &root, "lower", part(3.0));
        let face = add(
            &mut model,
            &root,
            "face",
            ModelNodeKind::Composite(ModelComposite::new()),
        );
        let bend = add(
            &mut model,
            &face,
            "bend",
            ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad(6.0))),
        );
        let spine = add(
            &mut model,
            &root,
            "spine",
            ModelNodeKind::Spine(ModelSpine::new(vec![[0.0, -5.0], [0.0, -10.0]])),
        );
        let sway = add(
            &mut model,
            &root,
            "sway",
            ModelNodeKind::MotionResponse(MotionResponse::default()),
        );
        model.add_texture(&upper, texture(1), &mut hex).unwrap();
        model.mask_add(&face, &lower, MaskMode::DodgeMask).unwrap();
        for part in [&upper, &lower] {
            model.slot_add(part, slot("seam")).unwrap();
            model.slot_fill(part, &slot("seam"), 1).unwrap();
        }
        model
            .set_welds(vec![ModelWeld::new(
                upper.clone(),
                lower.clone(),
                vec![SlotPair {
                    a: slot("seam"),
                    b: slot("seam"),
                    weight: 0.25,
                }],
            )])
            .unwrap();

        let x = ParamId::new("x").unwrap();
        model
            .add_param_with_id(
                x.clone(),
                ModelParam::new(Name::truncated("x"), -1.0, 1.0, 0.0),
            )
            .unwrap();
        model
            .set_spine_targets(&spine, vec![Some(x.clone()), None])
            .unwrap();
        let tx = BindingKey::new(
            x.clone(),
            upper.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        model
            .add_binding_with_positions(&tx, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        model.set_binding_key(&tx, [2, 0], 1.0).unwrap();
        let deform = BindingKey::new(x.clone(), lower.clone(), BindingTarget::Deform);
        model
            .add_binding_with_positions(&deform, vec![vec![0.0, 1.0]])
            .unwrap();
        model
            .set_deform_vertices(&deform, [1, 0], vec![1.0; 8])
            .unwrap();
        model
            .set_extension(
                ExtensionKey::new("test.metadata").unwrap(),
                ExtensionValue::Json(serde_json::json!({"value": 1})),
            )
            .unwrap();

        Rich {
            model,
            upper,
            lower,
            face,
            bend,
            spine,
            sway,
            x,
            tx,
            deform,
        }
    }
}
