#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Addons end to end: what `extract` cuts out, `install` puts back, and what a
//! puppet does when a model gains an addon between two of its ticks.

use std::collections::{HashMap, HashSet};

use catchlight_core::formats::clm::{
    ClmAnimation, ClmBinding, ClmFile, ClmIndices, ClmKeyframe, ClmLane, ClmMesh, TextureAlpha,
    TextureEncoding,
};
use catchlight_core::id::{Name, NodeId, ParamId, SeededHex, SlotId, TexId};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::physics::PendulumKind;
use catchlight_core::{
    target_of, BindingKey, BindingTarget, HexSource, InstallError, MaskMode, Model, ModelComposite,
    ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelPhysics, ModelTexture,
    ModelWeld, Puppet, ScalarTarget, SlotPair,
};

// ---------------------------------------------------------------- generator

/// The one source of randomness in this file. `SeededHex` is the crate's own
/// deterministic bit source, so a failing seed is a failing test forever.
struct Rng(SeededHex);

impl Rng {
    fn new(seed: u32) -> Self {
        Self(SeededHex::new(seed))
    }

    fn bits(&mut self) -> u32 {
        self.0.next_bits()
    }

    /// Uniform-enough over `0..n`; `n` is always tiny here.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        self.bits() as usize % n
    }

    fn one_in(&mut self, n: u32) -> bool {
        self.bits().is_multiple_of(n)
    }

    fn unit(&mut self) -> f32 {
        (self.bits() % 2001) as f32 / 1000.0 - 1.0
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[self.below(from.len())]
    }
}

fn quad(x: f32) -> ClmMesh {
    ClmMesh {
        verts: vec![x - 1.0, -1.0, x + 1.0, -1.0, x + 1.0, 1.0, x - 1.0, 1.0],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    }
}

const SCALARS: [ScalarTarget; 4] = [
    ScalarTarget::Tx,
    ScalarTarget::Ty,
    ScalarTarget::Rz,
    ScalarTarget::ZOrder,
];

/// A random but well-formed model: a tree of every node kind, params, shared
/// textures, masks, physics targets, one- and two-param bindings, welded slots
/// and an animation.
fn random_model(rng: &mut Rng) -> Model {
    let mut hex = SeededHex::new(rng.bits());
    let mut m = Model::new();
    let root = m.root().unwrap().clone();

    let params: Vec<ParamId> = (0..2 + rng.below(3))
        .map(|i| {
            let p = m
                .add_param(
                    ModelParam::new(Name::truncated(format!("P{i}")), -1.0, 1.0, 0.0),
                    &mut hex,
                )
                .unwrap();
            if rng.one_in(2) {
                m.key_insert(&p, 0.5).unwrap();
            }
            p
        })
        .collect();

    let mut nodes = vec![root.clone()];
    let mut parts = Vec::new();
    for i in 0..6 + rng.below(8) {
        let parent = rng.pick(&nodes).clone();
        let kind = match rng.below(5) {
            0 => ModelNodeKind::Group,
            1 => ModelNodeKind::Composite(ModelComposite::new()),
            2 => ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad(i as f32))),
            3 => ModelNodeKind::SimplePhysics(ModelPhysics::new(PendulumKind::RigidPendulum)),
            _ => ModelNodeKind::Part(ModelPart::new(quad(i as f32))),
        };
        let is_part = matches!(kind, ModelNodeKind::Part(_));
        let mut node = ModelNode::new(format!("N{i}"), kind);
        node.z_order = rng.below(5) as f32;
        node.enabled = !rng.one_in(6);
        node.transform.translation = [rng.unit(), rng.unit(), 0.0];
        let id = m.add_node(&parent, node, &mut hex).unwrap();
        if is_part {
            parts.push(id.clone());
        }
        nodes.push(id);
    }

    // A texture goes to the part that draws it, so the parts come first: one
    // each, then a few repointed so some are shared and some parts unmapped.
    // A repoint that takes the last user takes the texture with it, which is
    // churn this generator should produce.
    for (i, id) in parts.iter().enumerate() {
        m.add_texture(
            id,
            ModelTexture {
                encoding: TextureEncoding::Png,
                alpha: TextureAlpha::Straight,
                data: vec![i as u8; 6].into(),
            },
            &mut hex,
        )
        .unwrap();
    }
    for id in &parts {
        let live: Vec<TexId> = m.texture_ids().to_vec();
        match rng.below(6) {
            0 => m.set_part_albedo(id, None).unwrap(),
            1 if !live.is_empty() => {
                let pick = rng.pick(&live).clone();
                m.set_part_albedo(id, Some(pick)).unwrap();
            }
            _ => {}
        }
    }

    for id in nodes.clone() {
        let kind = m.node(&id).map(|n| n.kind.name()).unwrap_or_default();
        if kind == "physics" && !params.is_empty() {
            let x = rng.pick(&params).clone();
            m.set_physics_targets(&id, [Some(x), None]).unwrap();
        }
        if (kind == "part" || kind == "composite") && !parts.is_empty() && rng.one_in(3) {
            let source = rng.pick(&parts).clone();
            if source != id {
                m.mask_add(&id, &source, MaskMode::DodgeMask).unwrap();
            }
        }
    }

    for id in &nodes {
        if *id == root || !rng.one_in(2) {
            continue;
        }
        let meshed = m.node_mesh(id).is_some();
        let target = if meshed && rng.one_in(2) {
            BindingTarget::Deform
        } else if m.node(id).is_some_and(|n| n.kind.name() == "mesh_group") {
            BindingTarget::Scalar(ScalarTarget::Tx)
        } else {
            BindingTarget::Scalar(*rng.pick(&SCALARS))
        };
        let x = rng.pick(&params).clone();
        let key = if rng.one_in(3) {
            let y = rng.pick(&params).clone();
            if y == x {
                BindingKey::new(x, id.clone(), target)
            } else {
                BindingKey::pair(x, y, id.clone(), target)
            }
        } else {
            BindingKey::new(x, id.clone(), target)
        };
        if m.add_binding(&key).is_err() {
            continue;
        }
        let (w, h) = m.binding_grid(&key).unwrap();
        let cell = [rng.below(w as usize) as u32, rng.below(h as usize) as u32];
        match target {
            BindingTarget::Deform => {
                let len = m.deform_len(id);
                m.set_deform_vertices(&key, cell, vec![rng.unit(); len])
                    .unwrap();
            }
            BindingTarget::Scalar(_) => m.set_binding_key(&key, cell, rng.unit()).unwrap(),
        }
    }

    // One weld between two parts, if there are two to weld.
    if parts.len() >= 2 {
        let a = parts[0].clone();
        let b = parts[1].clone();
        let slots = [SlotId::new("s0").unwrap(), SlotId::new("s1").unwrap()];
        for (i, node) in [&a, &b].into_iter().enumerate() {
            for (j, slot) in slots.iter().enumerate() {
                m.slot_add(node, slot.clone()).unwrap();
                m.slot_fill(node, slot, (i + j) as u32).unwrap();
            }
        }
        m.set_welds(vec![ModelWeld::new(
            a,
            b,
            vec![
                SlotPair {
                    a: slots[0].clone(),
                    b: slots[0].clone(),
                    weight: 0.5,
                },
                SlotPair {
                    a: slots[1].clone(),
                    b: slots[1].clone(),
                    weight: 0.25,
                },
            ],
        )])
        .unwrap();
    }

    if !params.is_empty() {
        m.set_animations(vec![ClmAnimation {
            name: "idle".into(),
            length: 10,
            lanes: vec![ClmLane {
                param: rng.pick(&params).clone(),
                interpolation: InterpolateMode::Linear,
                keyframes: vec![ClmKeyframe {
                    frame: 3,
                    value: 0.25,
                }],
            }],
            ..ClmAnimation::default()
        }])
        .unwrap();
    }
    m
}

// ------------------------------------------------------------ canonical form

/// A model as its `.clm` file with every order the round trip is allowed
/// to change taken out: sibling order (nodes carry their parent, so sorting
/// them by Id erases it) and the order of the flat lists.
fn canonical(m: &Model) -> ClmFile {
    let mut file = m.to_clm_file().unwrap();
    let doc = &mut file.doc;
    doc.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    doc.params.sort_by(|a, b| a.id.cmp(&b.id));
    doc.bindings.sort_by_key(binding_key);
    doc.welds
        .sort_by_key(|w| (w.a.to_string(), w.b.to_string()));
    doc.animations.sort_by(|a, b| a.name.cmp(&b.name));
    file.textures.sort_by(|a, b| a.id.cmp(&b.id));
    file
}

fn binding_key(b: &ClmBinding) -> (String, Vec<String>, String) {
    (
        b.node.to_string(),
        b.params.iter().map(|p| p.to_string()).collect(),
        format!("{:?}", target_of(&b.values)),
    )
}

/// Everything below `id`, `id` included.
fn subtree(m: &Model, id: &NodeId) -> HashSet<NodeId> {
    let mut out = HashSet::new();
    let mut stack = vec![id.clone()];
    while let Some(n) = stack.pop() {
        if let Some(node) = m.node(&n) {
            stack.extend(node.children().iter().cloned());
        }
        out.insert(n);
    }
    out
}

/// Nodes that can be cut out and put back: everything but the root, minus the
/// subtrees something *outside* them masks with. A mask lives on the node it
/// clips, so a base drawable masked by an addon's part is not something an
/// addon can carry — see `a_mask_on_a_base_node_is_not_the_addons_to_carry`.
fn extractable(m: &Model) -> Vec<NodeId> {
    m.nodes_in_order()
        .into_iter()
        .filter(|id| Some(id) != m.root())
        .filter(|id| {
            let inside = subtree(m, id);
            !m.nodes_in_order().iter().any(|other| {
                !inside.contains(other)
                    && m.node(other)
                        .is_some_and(|n| masks_of(n).iter().any(|source| inside.contains(*source)))
            })
        })
        .collect()
}

fn masks_of(n: &ModelNode) -> Vec<&NodeId> {
    match &n.kind {
        ModelNodeKind::Part(p) => p.masks().iter().map(|m| m.source()).collect(),
        ModelNodeKind::Composite(c) => c.masks().iter().map(|m| m.source()).collect(),
        _ => Vec::new(),
    }
}

/// Cut `root` out of `base` the way an author would: delete the subtree. The
/// textures nothing else draws go with it — that is `delete_node`'s job now,
/// not the caller's.
fn without(base: &Model, root: &NodeId) -> Model {
    let mut cut = base.clone();
    cut.delete_node(root).unwrap();
    cut
}

#[test]
fn removing_a_subtree_and_installing_it_back_restores_the_model() {
    let mut checked = 0;
    let mut clashed = 0;
    for seed in 0..40u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9) ^ 0xC0FF_EE00);
        let base = random_model(&mut rng);
        let candidates = extractable(&base);
        if candidates.is_empty() {
            continue;
        }
        let root = rng.pick(&candidates).clone();

        let addon = base.extract(std::slice::from_ref(&root));
        assert!(addon.is_fragment(), "seed {seed}: {root} came out complete");
        assert!(
            addon.param_ids().is_empty(),
            "seed {seed}: params travelled"
        );
        assert!(
            addon.animations().is_empty(),
            "seed {seed}: animations travelled"
        );

        let mut cut = without(&base, &root);
        // A texture the cut base still draws is one the addon carries a copy
        // of: `extract` copies rather than requiring, and an Id an addon
        // provides is exclusive. Those two together say this is not a round
        // trip, and the install says so rather than merging silently.
        if let Some(shared) = addon
            .texture_ids()
            .iter()
            .find(|t| cut.texture(t).is_some())
        {
            assert_eq!(
                cut.install(&addon).unwrap_err(),
                InstallError::Collision {
                    kind: "texture",
                    id: shared.to_string(),
                },
                "seed {seed}, root {root}"
            );
            clashed += 1;
            continue;
        }
        cut.install(&addon)
            .unwrap_or_else(|e| panic!("seed {seed}, root {root}: {e}"));

        assert_eq!(
            canonical(&cut),
            canonical(&base),
            "seed {seed}, root {root}"
        );
        checked += 1;
    }
    assert!(
        checked + clashed > 30,
        "only {} of 40 seeds produced a case",
        checked + clashed
    );
    assert!(checked > 0, "no seed round-tripped");
    assert!(clashed > 0, "no seed shared a texture across the cut");
}

/// The receipt is exact both ways: install then uninstall is the identity, on
/// the same random models.
#[test]
fn uninstalling_an_addon_undoes_installing_it() {
    for seed in 100..120u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9));
        let base = random_model(&mut rng);
        let candidates = extractable(&base);
        if candidates.is_empty() {
            continue;
        }
        let root = rng.pick(&candidates).clone();
        let addon = base.extract(std::slice::from_ref(&root));
        let cut = without(&base, &root);
        // See the note in the round-trip test: a texture the cut base kept is
        // one the addon now provides too, and install refuses that.
        if addon.texture_ids().iter().any(|t| cut.texture(t).is_some()) {
            continue;
        }

        let mut m = cut.clone();
        let installed = m.install(&addon).unwrap();
        m.uninstall(&installed);
        assert_eq!(canonical(&m), canonical(&cut), "seed {seed}, root {root}");
    }
}

/// Extracting a subtree that another addon's requirements point into is fine;
/// what an addon cannot carry is a mask on a node it does not provide, since a
/// mask lives on the node it clips.
#[test]
fn a_mask_on_a_base_node_is_not_the_addons_to_carry() {
    let mut hex = SeededHex::new(3);
    let mut m = Model::new();
    let root = m.root().unwrap().clone();
    let clipped = m
        .add_node(
            &root,
            ModelNode::new("Clipped", ModelNodeKind::Part(ModelPart::new(quad(0.0)))),
            &mut hex,
        )
        .unwrap();
    let stencil = m
        .add_node(
            &root,
            ModelNode::new("Stencil", ModelNodeKind::Part(ModelPart::new(quad(3.0)))),
            &mut hex,
        )
        .unwrap();
    m.mask_add(&clipped, &stencil, MaskMode::DodgeMask).unwrap();

    let addon = m.extract(std::slice::from_ref(&stencil));
    assert_eq!(
        addon.requirements().nodes().collect::<Vec<_>>(),
        [&root],
        "the stencil needs its parent and nothing else — the mask is not its own"
    );

    let mut cut = m.clone();
    cut.delete_node(&stencil).unwrap();
    cut.install(&addon).unwrap();
    assert!(
        masks_of(cut.node(&clipped).unwrap()).is_empty(),
        "the mask stayed on the base node and cannot come back with the addon"
    );
}

// ------------------------------------------------------------------- puppets

/// One base model plus a hat addon for it, both hand-built so the assertions
/// can name what they check.
fn puppet_rig() -> (Model, Model, ParamId, NodeId) {
    let mut hex = SeededHex::new(21);
    let mut m = Model::new();
    let root = m.root().unwrap().clone();
    let param = m
        .add_param(
            ModelParam::new(Name::truncated("Lean"), -1.0, 1.0, 0.0),
            &mut hex,
        )
        .unwrap();
    let body = m
        .add_node(
            &root,
            ModelNode::new("Body", ModelNodeKind::Part(ModelPart::new(quad(0.0)))),
            &mut hex,
        )
        .unwrap();
    let key = BindingKey::new(
        param.clone(),
        body.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    m.add_binding(&key).unwrap();
    m.set_binding_key(&key, [0, 0], -4.0).unwrap();
    m.set_binding_key(&key, [1, 0], 4.0).unwrap();

    // The addon: a hat under the root, deformed by the base's own param.
    let mut scratch = m.clone();
    let hat = scratch
        .add_node(
            &root,
            ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad(0.0)))),
            &mut hex,
        )
        .unwrap();
    let hat_key = BindingKey::new(
        param.clone(),
        hat.clone(),
        BindingTarget::Scalar(ScalarTarget::Ty),
    );
    scratch.add_binding(&hat_key).unwrap();
    scratch.set_binding_key(&hat_key, [1, 0], 7.0).unwrap();
    let addon = scratch.extract(std::slice::from_ref(&hat));

    (m, addon, param, body)
}

#[test]
fn an_addon_installed_between_two_ticks_keeps_the_pose() {
    let (mut model, addon, param, body) = puppet_rig();
    let mut puppet = Puppet::new(&model);
    puppet.set_param_value(&param, 0.5);
    puppet.tick(&model, 1.0 / 60.0);

    let body_idx = puppet.node_idx(&body).unwrap();
    let before = puppet.transforms().get(body_idx);
    let generation = puppet.baked_generation();

    model.install(&addon).unwrap();
    assert_ne!(
        model.generation(),
        generation,
        "an install moves the model out from under the puppet"
    );
    puppet.tick(&model, 1.0 / 60.0);

    assert_eq!(puppet.param_value(&param), Some(0.5), "the pose survived");
    assert_eq!(
        puppet.transforms().get(puppet.node_idx(&body).unwrap()),
        before,
        "the base node folds to the same place"
    );
    let hat_id = model
        .nodes_in_order()
        .into_iter()
        .find(|id| model.node(id).is_some_and(|n| n.name.as_str() == "Hat"))
        .expect("the hat is in the model");
    let hat_idx = puppet.node_idx(&hat_id).expect("and in the rebaked arena");
    assert!(
        (puppet.transforms().get(hat_idx).w_axis.y - 5.25).abs() < 1e-4,
        "the addon's own binding folds at the pose the puppet already held"
    );
}

/// A puppet of an addon is empty: an addon is installed into a model, not
/// animated on its own.
#[test]
fn a_puppet_of_a_fragment_holds_nothing() {
    let (_, addon, param, _) = puppet_rig();
    let mut puppet = Puppet::new(&addon);
    assert_eq!(puppet.len(), 1, "just the arena's own empty root slot");
    assert!(puppet.param_ids().next().is_none());
    // And it is inert rather than broken: ticking it does nothing at all.
    puppet.set_param_value(&param, 0.5);
    puppet.settle_physics(&addon);
    puppet.tick(&addon, 1.0 / 60.0);
    assert_eq!(puppet.len(), 1);
}

/// The `.clm` shapes are disjoint, so a tool that does not know which it has
/// decodes once and tries both readers.
#[test]
fn a_file_is_one_shape_or_the_other() {
    let (model, addon, _, _) = puppet_rig();
    for (bytes, is_fragment) in [
        (model.to_clm_bytes().unwrap(), false),
        (addon.to_clm_bytes().unwrap(), true),
    ] {
        let read = Model::from_clm_bytes(&bytes)
            .ok()
            .or_else(|| Model::from_clm_bytes_fragment(&bytes).ok())
            .expect("one of the two readers takes it");
        assert_eq!(read.is_fragment(), is_fragment);
        assert_eq!(
            Model::from_clm_bytes(&bytes).is_ok(),
            !is_fragment,
            "and only one of them does"
        );
    }
}

/// `install` refuses an addon whose requirements the base cannot meet, and
/// says which Id it was.
#[test]
fn installing_into_the_wrong_base_names_what_is_missing() {
    let (model, addon, param, _) = puppet_rig();
    let mut other = model.clone();
    other.delete_param(&param).unwrap();
    let err = other.install(&addon).unwrap_err();
    assert!(matches!(err, InstallError::Missing { .. }));
    assert!(err.to_string().contains(param.as_str()), "{err}");
}

/// Guards the generator itself: the models it builds are the shape the tests
/// above assume.
#[test]
fn the_generator_builds_complete_models() {
    let mut seen_kinds: HashMap<&str, usize> = HashMap::new();
    for seed in 0..40u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9) ^ 0xC0FF_EE00);
        let m = random_model(&mut rng);
        assert!(!m.is_fragment());
        assert!(m.requirements().is_empty());
        assert!(m.to_clm_bytes().is_ok());
        for id in m.nodes_in_order() {
            *seen_kinds
                .entry(m.node(&id).unwrap().kind.name())
                .or_default() += 1;
        }
    }
    for kind in ["group", "part", "composite", "mesh_group", "physics"] {
        assert!(
            seen_kinds.contains_key(kind),
            "no {kind} was ever generated"
        );
    }
}
