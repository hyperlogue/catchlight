use super::*;
use crate::formats::clm::ClmIndices;
use crate::id::SeededHex;
use crate::interpolate::InterpolateMode;

pub(super) struct Fixture {
    pub model: Model,
    pub root: NodeId,
    pub part: NodeId,
    pub other: NodeId,
    pub composite: NodeId,
    pub response: NodeId,
    pub param: ParamId,
    pub tex: TexId,
    pub hex: SeededHex,
}

impl Fixture {
    pub(super) fn node(&mut self, name: &str, kind: ModelNodeKind) -> NodeId {
        let root = self.root.clone();
        self.model
            .add_node(&root, ModelNode::new(name, kind), &mut self.hex)
            .unwrap()
    }

    pub(super) fn spine(&mut self) -> NodeId {
        self.node(
            "spine",
            ModelNodeKind::Spine(ModelSpine::new(vec![[0.0, -50.0], [0.0, -100.0]])),
        )
    }
}

pub(super) fn fixture() -> Fixture {
    let mut hex = SeededHex::new(7);
    let mut model = Model::new();
    let root = model.root().unwrap().clone();
    let param = model
        .add_param(
            ModelParam {
                name: Name::truncated("p"),
                min: -1.0,
                max: 1.0,
                default: 0.0,
            },
            &mut hex,
        )
        .unwrap();
    let mut fixture = Fixture {
        model,
        root,
        part: NodeId::new("placeholder").unwrap(),
        other: NodeId::new("placeholder").unwrap(),
        composite: NodeId::new("placeholder").unwrap(),
        response: NodeId::new("placeholder").unwrap(),
        param,
        tex: TexId::new("placeholder").unwrap(),
        hex,
    };
    fixture.part = fixture.node("part", part_kind());
    fixture.other = fixture.node("other", part_kind());
    // `other` draws the texture; `part` is left unmapped.
    let drawn_by = fixture.other.clone();
    fixture.tex = fixture
        .model
        .add_texture(
            &drawn_by,
            ModelTexture {
                encoding: TextureEncoding::Png,
                alpha: TextureAlpha::Straight,
                data: [0x89, b'P', b'N', b'G'][..].into(),
            },
            &mut fixture.hex,
        )
        .unwrap();
    fixture.composite = fixture.node("composite", ModelNodeKind::Composite(ModelComposite::new()));
    fixture.response = fixture.node(
        "response",
        ModelNodeKind::MotionResponse(Default::default()),
    );
    let (composite, part) = (fixture.composite.clone(), fixture.part.clone());
    fixture
        .model
        .mask_add(&composite, &part, MaskMode::Mask)
        .unwrap();
    fixture
}

pub(super) fn slot(id: &str) -> SlotId {
    SlotId::new(id).unwrap()
}

pub(super) fn pair(id: &str, weight: f32) -> SlotPair {
    SlotPair {
        a: slot(id),
        b: slot(id),
        weight,
    }
}

pub(super) fn scalar_key(r: &Fixture) -> BindingKey {
    BindingKey::new(
        r.param.clone(),
        r.part.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    )
}

pub(super) fn deform_key(r: &Fixture) -> BindingKey {
    BindingKey::new(r.param.clone(), r.part.clone(), BindingTarget::Deform)
}

#[test]
fn every_constructed_model_gets_its_own_identity() {
    let a = Model::new();
    let b = Model::new();
    assert_eq!(a.generation(), b.generation(), "both start at generation 0");
    assert_ne!(a.identity(), b.identity());
}

#[test]
fn a_value_that_is_not_a_number_is_refused_by_every_mutator_that_writes_one() {
    let mut r = fixture();
    let (part, root, param) = (r.part.clone(), r.root.clone(), r.param.clone());
    let before = r.model.generation();

    // The closure's other writes are undone along with the bad one.
    let got = r.model.update_node(&part, |n| {
        n.name = Name::truncated("renamed");
        n.enabled = false;
        n.z_order = f32::NAN;
    });
    assert!(
        matches!(got, Err(ModelError::NonFinite("a node's z-order"))),
        "{got:?}"
    );
    let got = r
        .model
        .update_node(&part, |n| n.transform.scale[0] = f32::INFINITY);
    assert!(
        matches!(got, Err(ModelError::NonFinite("a node's transform"))),
        "{got:?}"
    );
    let node = r.model.node(&part).unwrap();
    assert_eq!(node.properties.name.as_str(), "part");
    assert!(node.properties.enabled);
    assert_eq!(node.properties.z_order, 0.0);
    assert_eq!(node.properties.transform, ClmTransform::default());

    let mut adrift = ModelNode::new("adrift", ModelNodeKind::Group);
    adrift.properties.transform.translation[1] = f32::NAN;
    let got = r.model.add_node(&root, adrift, &mut r.hex);
    assert!(
        matches!(got, Err(ModelError::NonFinite("a node's transform"))),
        "{got:?}"
    );

    let mut faded = ModelComposite::new();
    faded.opacity = f32::NAN;
    let got = r.model.add_node(
        &root,
        ModelNode::new("faded", ModelNodeKind::Composite(faded)),
        &mut r.hex,
    );
    assert!(
        matches!(got, Err(ModelError::NonFinite("a node's opacity"))),
        "{got:?}"
    );
    let got = r.model.update_node_kind(&part, |kind| {
        if let ModelNodeKind::Part(p) = kind {
            p.opacity = 0.5;
            p.mask_threshold = f32::INFINITY;
        }
    });
    assert!(
        matches!(got, Err(ModelError::NonFinite("a node's mask threshold"))),
        "{got:?}"
    );
    match &r.model.node(&part).unwrap().kind {
        ModelNodeKind::Part(p) => assert_eq!(p.opacity, 1.0),
        other => panic!("not a part: {other:?}"),
    }

    let got = r.model.add_param_with_id(
        ParamId::new("nan").unwrap(),
        ModelParam::new(Name::truncated("nan"), 0.0, 1.0, f32::NAN),
    );
    assert!(
        matches!(got, Err(ModelError::NonFinite("a param's default"))),
        "{got:?}"
    );
    let got = r.model.set_param_default(&param, f32::INFINITY);
    assert!(
        matches!(got, Err(ModelError::NonFinite("a param's default"))),
        "{got:?}"
    );
    assert_eq!(r.model.param(&param).unwrap().default, 0.0);

    assert_eq!(r.model.generation(), before, "a refused edit is no edit");
    // A finite value outside the range is still the author's business.
    r.model.set_param_default(&param, 7.0).unwrap();
}

#[test]
fn editing_forward_from_a_restored_snapshot_never_reuses_a_generation() {
    let mut model = Model::new();
    let snapshot = model.clone();

    model.set_welds(Vec::new()).unwrap();
    let undone = model.generation();

    model = snapshot;
    assert_ne!(model.generation(), undone);

    model.set_welds(Vec::new()).unwrap();
    assert_ne!(
        model.generation(),
        undone,
        "a state that was undone must not get its generation back",
    );
}

/// A new mutating method belongs in this list.
#[test]
fn generation_bumps_on_every_mutating_method() {
    #[expect(
        clippy::type_complexity,
        reason = "a table of named edits reads clearer inline than behind an alias"
    )]
    let edits: Vec<(&str, Box<dyn Fn(&mut Fixture)>)> = vec![
        (
            "update_node",
            Box::new(|r| {
                r.model.update_node(&r.part, |n| n.z_order = 3.0).unwrap();
            }),
        ),
        (
            "add_node",
            Box::new(|r| {
                r.node("new", ModelNodeKind::Group);
            }),
        ),
        (
            "add_node_with_id",
            Box::new(|r| {
                r.model
                    .add_node_with_id(
                        NodeId::new("chosen").unwrap(),
                        &r.root,
                        ModelNode::new("new", ModelNodeKind::Group),
                    )
                    .unwrap();
            }),
        ),
        (
            "delete_node",
            Box::new(|r| r.model.delete_node(&r.other).unwrap()),
        ),
        (
            "reparent",
            Box::new(|r| r.model.reparent(&r.part, &r.composite).unwrap()),
        ),
        (
            "reorder",
            Box::new(|r| r.model.reorder(&r.part, 0).unwrap()),
        ),
        (
            "duplicate_subtree",
            Box::new(|r| {
                let part = r.part.clone();
                r.model.duplicate_subtree(&part, &mut r.hex).unwrap();
            }),
        ),
        (
            "rename_node_id",
            Box::new(|r| {
                r.model
                    .rename_node_id(&r.part, NodeId::new("renamed").unwrap())
                    .unwrap();
            }),
        ),
        (
            "rename_param_id",
            Box::new(|r| {
                r.model
                    .rename_param_id(&r.param, ParamId::new("renamed").unwrap())
                    .unwrap();
            }),
        ),
        (
            "rename_tex_id",
            Box::new(|r| {
                r.model
                    .rename_tex_id(&r.tex, TexId::new("renamed").unwrap())
                    .unwrap();
            }),
        ),
        (
            "set_part_albedo",
            Box::new(|r| {
                r.model
                    .set_part_albedo(&r.part, Some(r.tex.clone()))
                    .unwrap();
            }),
        ),
        (
            "set_motion_response",
            Box::new(|r| {
                r.model
                    .set_motion_response(
                        &r.response,
                        crate::MotionResponse {
                            inputs: vec![crate::ResponseInput {
                                source: crate::ResponseSource::PoseParam {
                                    param: r.param.clone(),
                                },
                                gain: 1.0,
                            }],
                            output_param: None,
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }),
        ),
        (
            "set_spine_targets",
            Box::new(|r| {
                let spine = r.spine();
                r.model
                    .set_spine_targets(&spine, vec![Some(r.param.clone()), None])
                    .unwrap();
            }),
        ),
        (
            "set_spine_joints",
            Box::new(|r| {
                let spine = r.spine();
                r.model
                    .set_spine_joints(&spine, vec![[0.0, -10.0]])
                    .unwrap();
            }),
        ),
        (
            "set_node_mesh",
            Box::new(|r| {
                r.model.set_node_mesh(&r.part, quad()).unwrap();
            }),
        ),
        (
            "set_node_mesh_with",
            Box::new(|r| {
                r.model
                    .set_node_mesh_with(&r.part, quad(), |_, _, v| v.to_vec())
                    .unwrap();
            }),
        ),
        (
            "slot_add",
            Box::new(|r| r.model.slot_add(&r.part, slot("s")).unwrap()),
        ),
        (
            "rename_slot",
            Box::new(|r| {
                r.model.slot_add(&r.part, slot("s")).unwrap();
                r.model.rename_slot(&r.part, &slot("s"), slot("t")).unwrap();
            }),
        ),
        (
            "slot_fill",
            Box::new(|r| {
                r.model.slot_add(&r.part, slot("s")).unwrap();
                r.model.slot_fill(&r.part, &slot("s"), 2).unwrap();
            }),
        ),
        (
            "slot_clear",
            Box::new(|r| {
                r.model.slot_add(&r.part, slot("s")).unwrap();
                r.model.slot_clear(&r.part, &slot("s")).unwrap();
            }),
        ),
        (
            "slot_delete",
            Box::new(|r| {
                r.model.slot_add(&r.part, slot("s")).unwrap();
                r.model.slot_delete(&r.part, &slot("s")).unwrap();
            }),
        ),
        (
            "mask_add",
            Box::new(|r| {
                r.model
                    .mask_add(&r.composite, &r.other, MaskMode::Mask)
                    .unwrap();
            }),
        ),
        (
            "mask_set_mode",
            Box::new(|r| {
                r.model
                    .mask_set_mode(&r.composite, 0, MaskMode::DodgeMask)
                    .unwrap();
            }),
        ),
        (
            "mask_reorder",
            Box::new(|r| r.model.mask_reorder(&r.composite, 0, 0).unwrap()),
        ),
        (
            "mask_delete",
            Box::new(|r| r.model.mask_delete(&r.composite, 0).unwrap()),
        ),
        (
            "add_param",
            Box::new(|r| {
                let p = param("q");
                r.model.add_param(p, &mut r.hex).unwrap();
            }),
        ),
        (
            "add_param_with_id",
            Box::new(|r| {
                r.model
                    .add_param_with_id(ParamId::new("chosen").unwrap(), param("q"))
                    .unwrap();
            }),
        ),
        (
            "delete_param",
            Box::new(|r| r.model.delete_param(&r.param).unwrap()),
        ),
        (
            "add_texture",
            Box::new(|r| {
                let t = texture();
                r.model.add_texture(&r.part, t, &mut r.hex).unwrap();
            }),
        ),
        (
            "add_texture_with_id",
            Box::new(|r| {
                r.model
                    .add_texture_with_id(TexId::new("chosen").unwrap(), &r.part, texture())
                    .unwrap();
            }),
        ),
        (
            "delete_texture",
            Box::new(|r| r.model.delete_texture(&r.tex).unwrap()),
        ),
        (
            "set_welds",
            Box::new(|r| {
                r.model
                    .set_welds(vec![ModelWeld::new(
                        r.part.clone(),
                        r.other.clone(),
                        Vec::new(),
                    )])
                    .unwrap();
            }),
        ),
        (
            "add_binding",
            Box::new(|r| r.model.add_binding(&scalar_key(r)).unwrap()),
        ),
        (
            "set_binding_key",
            Box::new(|r| {
                r.model
                    .set_binding_key(&scalar_key(r), [1, 0], 5.0)
                    .unwrap();
            }),
        ),
        (
            "unset_binding_key",
            Box::new(|r| {
                let key = scalar_key(r);
                r.model.set_binding_key(&key, [1, 0], 5.0).unwrap();
                r.model.unset_binding_key(&key, [1, 0]).unwrap();
            }),
        ),
        (
            "reset_binding_key",
            Box::new(|r| {
                let key = scalar_key(r);
                r.model.add_binding(&key).unwrap();
                r.model.reset_binding_key(&key, [1, 0]).unwrap();
            }),
        ),
        (
            "delete_binding",
            Box::new(|r| {
                let key = scalar_key(r);
                r.model.add_binding(&key).unwrap();
                r.model.delete_binding(&key).unwrap();
            }),
        ),
        (
            "set_binding_interpolate",
            Box::new(|r| {
                let key = scalar_key(r);
                r.model.add_binding(&key).unwrap();
                r.model
                    .set_binding_interpolate(&key, InterpolateMode::Cubic)
                    .unwrap();
            }),
        ),
        (
            "invert_binding",
            Box::new(|r| {
                let key = scalar_key(r);
                r.model.set_binding_key(&key, [2, 0], 5.0).unwrap();
                r.model.invert_binding(&key).unwrap();
            }),
        ),
        (
            "copy_binding_key",
            Box::new(|r| {
                let key = scalar_key(r);
                r.model.set_binding_key(&key, [0, 0], 5.0).unwrap();
                r.model.copy_binding_key(&key, [0, 0], [2, 0]).unwrap();
            }),
        ),
        (
            "set_deform_vertices",
            Box::new(|r| {
                r.model
                    .set_deform_vertices(&deform_key(r), [1, 0], vec![1.0; 8])
                    .unwrap();
            }),
        ),
        (
            "set_deform_from_transform",
            Box::new(|r| {
                r.model
                    .set_deform_from_transform(&deform_key(r), [1, 0], [1.0, 0.0], 0.0, [1.0, 1.0])
                    .unwrap();
            }),
        ),
        (
            "set_param_name",
            Box::new(|r| {
                r.model
                    .set_param_name(&r.param, Name::truncated("renamed"))
                    .unwrap();
            }),
        ),
        (
            "set_param_default",
            Box::new(|r| r.model.set_param_default(&r.param, 0.5).unwrap()),
        ),
        (
            "set_param_range",
            Box::new(|r| r.model.set_param_range(&r.param, 0.0, 2.0).unwrap()),
        ),
        (
            "key_insert",
            Box::new(|r| {
                r.model.key_insert(&scalar_key(r), &r.param, 0.25).unwrap();
            }),
        ),
        (
            "key_delete",
            Box::new(|r| r.model.key_delete(&scalar_key(r), &r.param, 1).unwrap()),
        ),
        (
            "key_move",
            Box::new(|r| r.model.key_move(&scalar_key(r), &r.param, 1, 0.6).unwrap()),
        ),
    ];

    for (name, edit) in edits {
        let mut r = fixture();
        if matches!(
            name,
            "key_insert" | "key_delete" | "key_move" | "invert_binding" | "copy_binding_key"
        ) {
            r.model
                .add_binding_with_positions(&scalar_key(&r), vec![vec![0.0, 0.5, 1.0]])
                .unwrap();
        }
        let before = r.model.generation();
        edit(&mut r);
        assert!(
            r.model.generation() > before,
            "{name} did not bump the generation",
        );
    }
}

#[test]
fn what_the_reader_refuses_cannot_be_authored() {
    let mut r = fixture();
    let root = r.root.clone();
    let before = r.model.generation();
    let mut hex = SeededHex::new(3);

    let mut odd = quad();
    odd.verts.push(0.5);
    assert!(matches!(
        r.model.add_node(
            &root,
            ModelNode::new("odd", ModelNodeKind::Part(ModelPart::new(odd))),
            &mut hex,
        ),
        Err(ModelError::MalformedMesh(_))
    ));
    let mut nan = quad();
    nan.verts[0] = f32::NAN;
    assert!(matches!(
        r.model.add_node(
            &root,
            ModelNode::new("nan", ModelNodeKind::Part(ModelPart::new(nan))),
            &mut hex,
        ),
        Err(ModelError::MalformedMesh("a vertex is not finite"))
    ));
    assert!(matches!(
        r.model.add_param_with_id(
            ParamId::new("flat").unwrap(),
            ModelParam::new(Name::truncated("flat"), 1.0, 1.0, 0.0),
        ),
        Err(ModelError::InvalidParamRange)
    ));
    assert_eq!(r.model.generation(), before, "neither edit landed");
    assert!(r.model.to_clm_bytes().is_ok());
}

#[test]
fn a_rejected_edit_leaves_the_generation_alone() {
    let mut r = fixture();
    let before = r.model.generation();
    let root = r.root.clone();
    assert!(r.model.delete_node(&root).is_err());
    assert!(r.model.reparent(&root, &r.part).is_err());
    assert!(r.model.mask_add(&r.part, &r.part, MaskMode::Mask).is_err());
    assert!(r.model.mask_delete(&r.composite, 7).is_err());
    assert!(
        r.model
            .set_binding_key(&scalar_key(&r), [9, 0], 1.0)
            .is_err()
    );
    assert_eq!(r.model.generation(), before);
}

#[test]
fn model_snapshots_share_large_payloads_until_mutated() {
    let mut r = fixture();
    let node = r.part.clone();
    let key = deform_key(&r);
    let model = &mut r.model;
    model
        .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
        .unwrap();
    model
        .set_deform_vertices(&key, [2, 0], vec![1.0; 8])
        .unwrap();

    let model = r.model;
    let mut edited = model.clone();
    let mesh_arc = |m: &Model| match &m.node(&node).unwrap().kind {
        ModelNodeKind::Part(part) => part.shared_mesh().0,
        _ => unreachable!(),
    };
    let cells_arc = |m: &Model| m.binding(&key).unwrap().values.0.clone();
    let original_mesh = mesh_arc(&model);
    let original_cells = cells_arc(&model);
    assert!(Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));
    assert!(Arc::ptr_eq(&original_cells, &cells_arc(&edited)));

    edited
        .update_node(&node, |n| n.name = Name::truncated("renamed"))
        .unwrap();
    assert!(Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));

    let mut moved = quad();
    moved.verts[0] = 7.0;
    edited.set_node_mesh(&node, moved).unwrap();
    edited
        .set_deform_vertices(&key, [2, 0], vec![2.0; 8])
        .unwrap();

    assert!(!Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));
    assert!(!Arc::ptr_eq(&original_cells, &cells_arc(&edited)));
    // The snapshot taken before the edits is byte-identical afterwards.
    assert_eq!(model.node_mesh(&node).unwrap().verts, quad().verts);
    assert_eq!(
        model
            .binding(&key)
            .unwrap()
            .values()
            .deform_cells()
            .unwrap()
            .iter()
            .map(|c| (c.x, c.y, c.value.clone()))
            .collect::<Vec<_>>(),
        vec![(2, 0, vec![1.0; 8])],
    );
}

pub(super) fn response_input(m: &Model, node: &NodeId) -> Option<ParamId> {
    match &m.node(node)?.kind {
        ModelNodeKind::MotionResponse(r) => match &r.inputs.first()?.source {
            crate::ResponseSource::PoseParam { param: p } => Some(p.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn albedo(m: &Model, node: &NodeId) -> Option<TexId> {
    match &m.node(node)?.kind {
        ModelNodeKind::Part(p) => p.albedo().cloned(),
        _ => None,
    }
}

pub(super) fn texture() -> ModelTexture {
    ModelTexture {
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data: [][..].into(),
    }
}

pub(super) fn quad() -> ClmMesh {
    ClmMesh {
        verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
        uvs: vec![0.0; 8],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    }
}

pub(super) fn part_kind() -> ModelNodeKind {
    ModelNodeKind::Part(ModelPart::new(quad()))
}

pub(super) fn param(name: &str) -> ModelParam {
    ModelParam::new(Name::truncated(name), 0.0, 1.0, 0.0)
}
