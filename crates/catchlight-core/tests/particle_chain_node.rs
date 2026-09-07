#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The particle chain as a node the puppet ticks.
//!
//! `physics_trajectory` pins the solver's curve and `clm_roundtrip` pins the
//! file; what is left is the wiring between them — that a chain hangs off the
//! node's anchor, that its bends reach the params it names, that a rebake
//! carries the swing, and that settling puts it back. Every model here is
//! built through `Model`'s own API rather than through a `.clm`, so a failure
//! is the runtime's and not the reader's.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh, ClmPhysics};
use catchlight_core::id::SeededHex;
use catchlight_core::model::ModelPhysics;
use catchlight_core::model::{
    BindingKey, BindingTarget, ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelParticleChain,
    ScalarTarget,
};
use catchlight_core::physics::{ChainLink, PendulumKind, PhysicsParamMapMode};
use catchlight_core::{Model, Name, NodeId, NodeIdx, ParamId, Puppet, Vec2};

const DT: f32 = 1.0 / 60.0;

fn link(length: f32) -> ChainLink {
    ChainLink {
        length,
        gravity_scale: 1.0,
        damping: 0.5,
        time_scale: 1.0,
    }
}

fn quad() -> ClmMesh {
    ClmMesh {
        verts: vec![-10.0, -10.0, 10.0, -10.0, 10.0, 10.0, -10.0, 10.0],
        uvs: vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    }
}

/// A model whose only node under the root is a three-link chain, with one
/// param per link ready to be aimed at.
///
/// Model-level physics is `1.0 * 1.0` so the chain's own `gravity` is the
/// effective one, exactly as `physics_trajectory`'s fixture does it.
struct Fixture {
    model: Model,
    hex: SeededHex,
    chain: NodeId,
    params: Vec<ParamId>,
}

impl Fixture {
    fn new(links: usize) -> Self {
        let mut hex = SeededHex::new(5);
        let mut model = Model::new();
        model.set_physics(ClmPhysics {
            pixels_per_meter: 1.0,
            gravity: 1.0,
        });
        let params: Vec<ParamId> = (0..links)
            .map(|i| {
                model
                    .add_param(
                        ModelParam {
                            name: Name::truncated(format!("bend{i}")),
                            min: -1.0,
                            max: 1.0,
                            default: 0.0,
                            key_positions: vec![0.0, 0.5, 1.0],
                        },
                        &mut hex,
                    )
                    .expect("add param")
            })
            .collect();
        let root = model.root().expect("a fresh model has a root").clone();
        let mut chain_data =
            ModelParticleChain::new((0..links).map(|i| link(60.0 - 10.0 * i as f32)).collect());
        chain_data.gravity = 981.0;
        let chain = model
            .add_node(
                &root,
                ModelNode::new("hair", ModelNodeKind::ParticleChain(chain_data)),
                &mut hex,
            )
            .expect("add the chain");
        Self {
            model,
            hex,
            chain,
            params,
        }
    }

    /// Aim every link at its own param.
    fn wire_outputs(&mut self) {
        let outputs = self.params.iter().cloned().map(Some).collect();
        self.model
            .set_chain_outputs(&self.chain, outputs)
            .expect("aim the chain");
    }

    fn puppet(&self) -> Puppet {
        Puppet::new(&self.model)
    }

    fn idx(&self, puppet: &Puppet) -> NodeIdx {
        puppet.node_idx(&self.chain).expect("the chain baked")
    }
}

/// Where a chain's tip is in the puppet's own frame, so a test can watch it
/// swing without reaching into the solver.
fn tip(puppet: &Puppet, idx: NodeIdx) -> Vec2 {
    let node = puppet.get(idx).expect("the node");
    match &node.kind {
        catchlight_core::NodeKind::ParticleChain(c) => {
            c.particles.last().expect("a chain has particles").pos
        }
        other => panic!("not a chain: {other:?}"),
    }
}

fn anchor(puppet: &Puppet, idx: NodeIdx) -> Vec2 {
    let node = puppet.get(idx).expect("the node");
    match &node.kind {
        catchlight_core::NodeKind::ParticleChain(c) => c.anchor,
        other => panic!("not a chain: {other:?}"),
    }
}

/// A chain hangs from the node's **world** anchor, so a group the pose slides
/// takes the chain with it — and takes it the same frame, because the anchor
/// pre-pass folds the transform bindings at the pose the caller just set.
///
/// The one-frame lag lives one step further out, on a driver's *output*: see
/// `a_chain_anchored_on_a_driver_follows_it_one_frame_late`.
#[test]
fn a_chain_follows_a_posed_group_the_same_frame() {
    let mut hex = SeededHex::new(3);
    let mut model = Model::new();
    model.set_physics(ClmPhysics {
        pixels_per_meter: 1.0,
        gravity: 1.0,
    });
    let slide = model
        .add_param(
            ModelParam {
                name: Name::truncated("slide"),
                min: -1.0,
                max: 1.0,
                default: 0.0,
                key_positions: vec![0.0, 0.5, 1.0],
            },
            &mut hex,
        )
        .expect("add param");
    let root = model.root().expect("root").clone();
    let group = model
        .add_node(&root, ModelNode::new("arm", ModelNodeKind::Group), &mut hex)
        .expect("add group");
    let mut chain_data = ModelParticleChain::new(vec![link(50.0), link(40.0)]);
    chain_data.gravity = 981.0;
    let chain = model
        .add_node(
            &group,
            ModelNode::new("hair", ModelNodeKind::ParticleChain(chain_data)),
            &mut hex,
        )
        .expect("add chain");
    // The group slides along x with the param, so the chain's world anchor
    // moves without the chain's own transform moving.
    let key = BindingKey::new(
        slide.clone(),
        group.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    model.add_binding(&key).expect("bind the group's tx");
    model
        .set_binding_key(&key, [2, 0], 200.0)
        .expect("the param's top key slides the group");

    let mut puppet = Puppet::new(&model);
    let idx = puppet.node_idx(&chain).expect("the chain baked");
    puppet.settle_physics(&model);
    puppet.tick(&model, DT);
    let settled = anchor(&puppet, idx);
    assert!(
        settled.x.abs() < 1e-3,
        "the rest pose leaves the anchor at the origin, got {settled}"
    );

    puppet.set_param_value(&slide, 1.0);
    puppet.tick(&model, DT);
    let moved = anchor(&puppet, idx);
    assert!(
        (moved.x - 200.0).abs() < 1e-3,
        "the pre-pass folds the pose the caller just set, got {moved}"
    );
    // The anchor teleported; the tip did not, because the rods have to drag it
    // there. That is the whole reason the chain is a chain.
    let tip = tip(&puppet, idx);
    assert!(
        (tip.x - moved.x).abs() > 10.0,
        "the tip lags the anchor it is being dragged by, got {tip} under {moved}"
    );
}

/// A chain whose anchor a *driver* moves follows it one frame late, exactly as
/// a chained `SimplePhysics` driver does: every driver's output is applied at
/// its last-frame value, so the two couple with one frame of delay.
#[test]
fn a_chain_anchored_on_a_driver_follows_it_one_frame_late() {
    let mut hex = SeededHex::new(9);
    let mut model = Model::new();
    model.set_physics(ClmPhysics {
        pixels_per_meter: 1.0,
        gravity: 1.0,
    });
    let swing = model
        .add_param(
            ModelParam {
                // `XY` reports the bob's displacement as a fraction of the
                // pendulum's length, so the driver's output lives in -1..1.
                name: Name::truncated("swing"),
                min: -1.0,
                max: 1.0,
                default: 0.0,
                key_positions: vec![0.0, 0.5, 1.0],
            },
            &mut hex,
        )
        .expect("add param");
    let root = model.root().expect("root").clone();
    let mut pendulum = ModelPhysics::new(PendulumKind::RigidPendulum);
    pendulum.map_mode = PhysicsParamMapMode::XY;
    pendulum.gravity = 981.0;
    pendulum.length = 100.0;
    pendulum.angle_damping = 0.05;
    let driver = model
        .add_node(
            &root,
            ModelNode::new("driver", ModelNodeKind::SimplePhysics(pendulum)),
            &mut hex,
        )
        .expect("add driver");
    model
        .set_physics_targets(&driver, [Some(swing.clone()), None])
        .expect("aim the driver");
    let group = model
        .add_node(&root, ModelNode::new("arm", ModelNodeKind::Group), &mut hex)
        .expect("add group");
    let key = BindingKey::new(
        swing.clone(),
        group.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    model.add_binding(&key).expect("bind the group's tx");
    model
        .set_binding_key(&key, [2, 0], 300.0)
        .expect("the driver's output slides the group");
    let mut chain_data = ModelParticleChain::new(vec![link(50.0), link(40.0)]);
    chain_data.gravity = 981.0;
    let chain = model
        .add_node(
            &group,
            ModelNode::new("hair", ModelNodeKind::ParticleChain(chain_data)),
            &mut hex,
        )
        .expect("add chain");

    let mut puppet = Puppet::new(&model);
    let chain_idx = puppet.node_idx(&chain).expect("the chain baked");
    let driver_idx = puppet.node_idx(&driver).expect("the driver baked");
    puppet.settle_physics(&model);
    puppet.tick(&model, DT);
    assert!(
        anchor(&puppet, chain_idx).x.abs() < 1e-3,
        "everything starts at the origin"
    );

    // Throw the pendulum. Its output moves the group, but only the frame
    // after: this frame's anchor pose was built from last frame's output.
    assert!(
        puppet.place_driver(driver_idx, Vec2::new(90.0, 40.0)),
        "placed the bob"
    );
    puppet.tick(&model, DT);
    let same_frame = anchor(&puppet, chain_idx);
    assert!(
        same_frame.x.abs() < 1e-3,
        "the driver's fresh output has not reached the anchor yet, got {same_frame}"
    );

    puppet.tick(&model, DT);
    let next_frame = anchor(&puppet, chain_idx);
    assert!(
        next_frame.x.abs() > 100.0,
        "the next frame's pre-pass applies it, got {next_frame}"
    );
}

/// A rebake keeps the swing when the edit left the chain's shape alone, and
/// re-hangs it when the edit changed how many links it has. Both halves of the
/// rule, on the same puppet.
#[test]
fn a_rebake_carries_the_particles_unless_the_links_changed() {
    let mut f = Fixture::new(3);
    f.wire_outputs();
    let mut puppet = f.puppet();
    let idx = f.idx(&puppet);
    puppet.settle_physics(&f.model);
    puppet.tick(&f.model, DT);

    assert!(puppet.kick_chain(idx, Vec2::new(30.0, 0.0)), "kicked");
    puppet.tick(&f.model, DT);
    let mid_swing = tip(&puppet, idx);
    assert!(
        mid_swing.x.abs() > 1.0,
        "the kick left the chain off vertical, got {mid_swing}"
    );

    // An edit that says nothing about the chain.
    f.model
        .update_node(&f.chain, |n| n.name = Name::truncated("hair (renamed)"))
        .expect("rename");
    puppet.sync(&f.model);
    let idx = f.idx(&puppet);
    let carried = tip(&puppet, idx);
    assert!(
        (carried - mid_swing).length() < 1e-4,
        "the rebake carried the swing: {mid_swing} became {carried}"
    );

    // An edit that reshapes the chain has no rod to put the old point back
    // on, so the fresh bake stands and the next tick re-hangs it.
    f.model
        .set_chain_links(&f.chain, vec![link(60.0), link(50.0)])
        .expect("reshape");
    puppet.sync(&f.model);
    let idx = f.idx(&puppet);
    let rehung = tip(&puppet, idx);
    assert!(
        (rehung - mid_swing).length() > 1.0,
        "the reshaped chain did not keep the old particles, got {rehung}"
    );
    puppet.tick(&f.model, DT);
    let after = tip(&puppet, idx);
    assert!(
        (after - anchor(&puppet, idx)).length() > 0.0,
        "the re-hung chain hangs off its anchor, got {after}"
    );
}

/// A kicked chain writes its bends into the params it names, with the sign
/// `link_bends` promises: a tip displaced toward +X reads positive. A chain
/// that names nothing moves no param at all.
#[test]
fn a_kicked_chain_writes_its_bends_and_only_where_it_is_aimed() {
    let mut f = Fixture::new(3);
    f.wire_outputs();
    let mut puppet = f.puppet();
    let idx = f.idx(&puppet);
    puppet.settle_physics(&f.model);
    puppet.tick(&f.model, DT);
    for p in &f.params {
        assert_eq!(
            puppet.param_value(p),
            Some(0.0),
            "a hanging chain bends nowhere"
        );
    }

    // +X in the drivers' Y-down frame is +X of the node, which is the
    // direction `link_bends` calls positive.
    assert!(puppet.kick_chain(idx, Vec2::new(30.0, 0.0)), "kicked");
    puppet.tick(&f.model, DT);
    let first = puppet
        .param_value(&f.params[0])
        .expect("link 0's param exists");
    assert!(
        first > 0.0,
        "a +X kick bends link 0 toward +X, so its param reads positive, got {first}"
    );

    // The same model with nothing aimed: the kick still moves the chain, and
    // no param hears about it.
    let bare = Fixture::new(3);
    // Same shape, no outputs.
    let mut puppet = bare.puppet();
    let idx = bare.idx(&puppet);
    puppet.settle_physics(&bare.model);
    puppet.tick(&bare.model, DT);
    assert!(puppet.kick_chain(idx, Vec2::new(30.0, 0.0)), "kicked");
    puppet.tick(&bare.model, DT);
    let moved = tip(&puppet, idx);
    assert!(moved.x.abs() > 1.0, "the chain itself moved, got {moved}");
    for p in &bare.params {
        assert_eq!(
            puppet.param_value(p),
            None,
            "an unaimed chain claims no param, so the param reads unposed"
        );
    }
}

/// `settle_physics` is the chain's way back to rest, and `Motion` is how a
/// viewport hears about it: true while the kick is still unwinding, false once
/// the chain is hanging straight again.
#[test]
fn settling_a_kicked_chain_stops_the_motion() {
    let mut f = Fixture::new(3);
    f.wire_outputs();
    let mut puppet = f.puppet();
    let idx = f.idx(&puppet);
    puppet.settle_physics(&f.model);
    assert!(
        !puppet.tick(&f.model, DT).physics,
        "a settled chain is not moving"
    );

    assert!(puppet.kick_chain(idx, Vec2::new(30.0, 0.0)), "kicked");
    assert!(
        puppet.tick(&f.model, DT).physics,
        "the kick is motion the next frame will unwind"
    );

    puppet.settle_physics(&f.model);
    assert!(
        !puppet.tick(&f.model, DT).physics,
        "settling put it back at rest"
    );
    let tip = tip(&puppet, idx);
    let anchor = anchor(&puppet, idx);
    assert!(
        (tip.x - anchor.x).abs() < 1e-3,
        "a settled chain hangs straight down, got {tip} under {anchor}"
    );
}

/// A chain under a part-bearing model still bakes and ticks: the chain is a
/// driver, never drawn, and it does not disturb the geometry around it.
#[test]
fn a_chain_beside_a_part_leaves_the_part_alone() {
    let mut f = Fixture::new(2);
    f.wire_outputs();
    let root = f.model.root().expect("root").clone();
    let part = f
        .model
        .add_node(
            &root,
            ModelNode::new("body", ModelNodeKind::Part(ModelPart::new(quad()))),
            &mut f.hex,
        )
        .expect("add part");

    let mut puppet = f.puppet();
    let part_idx = puppet.node_idx(&part).expect("the part baked");
    puppet.settle_physics(&f.model);
    puppet.tick(&f.model, DT);
    let before = puppet.transforms().get(part_idx);

    let idx = f.idx(&puppet);
    assert!(puppet.kick_chain(idx, Vec2::new(40.0, 0.0)), "kicked");
    puppet.tick(&f.model, DT);
    assert_eq!(
        before,
        puppet.transforms().get(part_idx),
        "a swinging chain nothing binds does not move a part"
    );
}
