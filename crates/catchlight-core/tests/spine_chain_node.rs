#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The particle chain a spine carries, as the puppet ticks it.
//!
//! `physics_trajectory` pins the solver's curve and `clm_roundtrip` pins the
//! file; what is left is the wiring between them — that a chain hangs off the
//! spine's anchor, that its bends reach the params the spine names, that a
//! rebake carries the swing, and that settling puts it back. Every model here
//! is built through `Model`'s own API rather than through a `.clm`, so a
//! failure is the runtime's and not the reader's.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh, ClmPhysics};
use catchlight_core::id::SeededHex;
use catchlight_core::model::ModelPhysics;
use catchlight_core::model::{
    BindingKey, BindingTarget, ModelChain, ModelNode, ModelNodeKind, ModelParam, ModelPart,
    ModelSpine, ScalarTarget,
};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::{Mat4, Model, Name, NodeId, NodeIdx, ParamId, Puppet, Vec2};

const DT: f32 = 1.0 / 60.0;

/// The joints of a straight strand of `links` equal links, `each` long,
/// hanging down the node's own -Y.
fn straight(links: usize, each: f32) -> Vec<[f32; 2]> {
    (1..=links).map(|i| [0.0, -each * i as f32]).collect()
}

/// The joints of a straight strand of `links` links, root to tip: 60 px, then
/// 50, then 40, hanging down the node's own -Y.
fn joints(links: usize) -> Vec<[f32; 2]> {
    let mut y = 0.0f32;
    (0..links)
        .map(|i| {
            y -= 60.0 - 10.0 * i as f32;
            [0.0, y]
        })
        .collect()
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
        let mut spine = ModelSpine::new(joints(links));
        let mut chain_data = ModelChain::new(links);
        chain_data.gravity = 981.0;
        spine.set_chain(Some(chain_data));
        let chain = model
            .add_node(
                &root,
                ModelNode::new("hair", ModelNodeKind::Spine(spine)),
                &mut hex,
            )
            .expect("add the spine");
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
            .set_spine_targets(&self.chain, outputs)
            .expect("aim the spine");
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
        catchlight_core::NodeKind::Spine(sp) => {
            sp.chain
                .as_ref()
                .expect("the spine carries a chain")
                .particles
                .last()
                .expect("a chain has particles")
                .pos
        }
        other => panic!("not a spine: {other:?}"),
    }
}

fn anchor(puppet: &Puppet, idx: NodeIdx) -> Vec2 {
    let node = puppet.get(idx).expect("the node");
    match &node.kind {
        catchlight_core::NodeKind::Spine(sp) => {
            sp.chain.as_ref().expect("the spine carries a chain").anchor
        }
        other => panic!("not a spine: {other:?}"),
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
    let mut spine_data = ModelSpine::new(vec![[0.0, -50.0], [0.0, -90.0]]);
    let mut chain_data = ModelChain::new(2);
    chain_data.gravity = 981.0;
    spine_data.set_chain(Some(chain_data));
    let chain = model
        .add_node(
            &group,
            ModelNode::new("hair", ModelNodeKind::Spine(spine_data)),
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
    let mut spine_data = ModelSpine::new(vec![[0.0, -50.0], [0.0, -90.0]]);
    let mut chain_data = ModelChain::new(2);
    chain_data.gravity = 981.0;
    spine_data.set_chain(Some(chain_data));
    let chain = model
        .add_node(
            &group,
            ModelNode::new("hair", ModelNodeKind::Spine(spine_data)),
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
        .set_spine_joints(&f.chain, vec![[0.0, -60.0], [0.0, -110.0]])
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

/// **The first link's bend zero is the node's own down, not gravity's.** Two
/// chains under one node turned 30 degrees about Z: the sprung one has no
/// weight at all, so its spring holds it along the node and its bend reads
/// zero; the limp one hangs along gravity, which from inside the turned node
/// is 30 degrees away. This is what makes a stiff strand follow a tilted head
/// and a limp one hang off it, and it is also where the spring and
/// `link_bends` are checked against each other: they disagree the moment
/// either one measures from the wrong direction.
///
/// The sign is `link_bends`': a tip displaced toward the node's +X reads
/// positive (`a_kicked_chain_writes_its_bends_and_only_where_it_is_aimed`).
/// Turning the node *toward* +X leaves gravity on the node's -X side, so the
/// limp chain's first bend is **negative** — 30 degrees is a sixth of a half
/// turn, so -1/6.
#[test]
fn a_sprung_chain_follows_a_turned_node_and_a_limp_one_hangs() {
    let mut hex = SeededHex::new(21);
    let mut model = Model::new();
    model.set_physics(ClmPhysics {
        pixels_per_meter: 1.0,
        gravity: 1.0,
    });
    let root = model.root().expect("root").clone();
    let mut tilted = ModelNode::new("head", ModelNodeKind::Group);
    tilted.transform.rotation = [0.0, 0.0, std::f32::consts::FRAC_PI_6];
    let head = model.add_node(&root, tilted, &mut hex).expect("add group");

    // The third one is the case the rod projection's rounding used to leave
    // wobbling forever: a spring *and* real weight, so its rest pose is at an
    // angle to every axis and the projection is not exact there.
    let mut chains = Vec::new();
    for (name, stiffness, gravity_scale, links) in [
        ("stiff", 10.0f32, 0.0f32, 2usize),
        ("limp", 0.0, 1.0, 2),
        ("weighted", 4.0, 1.0, 3),
    ] {
        let params: Vec<ParamId> = (0..links)
            .map(|i| {
                model
                    .add_param(
                        ModelParam {
                            name: Name::truncated(format!("{name}{i}")),
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
        let mut spine = ModelSpine::new(straight(links, 60.0));
        let mut data = ModelChain::new(links);
        data.gravity = 981.0;
        data.set_links(
            (0..links)
                .map(|_| catchlight_core::LinkFeel {
                    gravity_scale,
                    damping: 0.5,
                    stiffness,
                    limit: None,
                })
                .collect(),
        );
        spine.set_chain(Some(data));
        let node = model
            .add_node(
                &head,
                ModelNode::new(name, ModelNodeKind::Spine(spine)),
                &mut hex,
            )
            .expect("add chain");
        model
            .set_spine_targets(&node, params.iter().cloned().map(Some).collect())
            .expect("aim the chain");
        chains.push((node, params));
    }

    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    let motion = puppet.tick(&model, DT);

    let stiff = puppet
        .param_value(&chains[0].1[0])
        .expect("the stiff chain's first bend");
    assert!(
        stiff.abs() < 1e-3,
        "a weightless sprung chain sits along its node, so its bend reads 0, got {stiff}",
    );
    let limp = puppet
        .param_value(&chains[1].1[0])
        .expect("the limp chain's first bend");
    assert!(
        (limp + 1.0 / 6.0).abs() < 1e-3,
        "a limp chain hangs along gravity, a sixth of a half turn off a node \
         turned 30 degrees toward +X, got {limp}",
    );

    // None of the three is still moving, so a viewport over a tilted rig is
    // allowed to go to sleep — including over the weighted sprung chain,
    // whose rest pose lies at an angle to every axis.
    assert!(
        !motion.physics,
        "settling put all three chains at rest under the turned node",
    );
    for _ in 0..10 {
        assert!(
            !puppet.tick(&model, DT).physics,
            "and they stay there frame after frame",
        );
    }

    // And a chain that is knocked off that pose reports motion until it has
    // unwound, then reports none again — without ever being settled a second
    // time. The sprung chain relaxes to a pose a hair off the analytic one,
    // so this is stillness being reported and not a match against it.
    let weighted = puppet
        .node_idx(&chains[2].0)
        .expect("the weighted chain baked");
    assert!(puppet.kick_chain(weighted, Vec2::new(5.0, -2.0)), "kicked");
    assert!(
        puppet.tick(&model, DT).physics,
        "the kick is motion the next frame will unwind",
    );
    let mut quiet_at = None;
    for f in 0..7200 {
        if !puppet.tick(&model, DT).physics {
            quiet_at = Some(f);
            break;
        }
    }
    let quiet_at = quiet_at.expect("the kicked chain stops within two minutes");
    for _ in 0..120 {
        assert!(
            !puppet.tick(&model, DT).physics,
            "and stays quiet after settling at frame {quiet_at}",
        );
    }
}

/// The bend the chain itself reports, one per link — what it claims its
/// params should read, before the weight decides how much of that lands.
/// Every chain here hangs under an untransformed root, so the node's frame is
/// the world's.
fn chain_bends(puppet: &Puppet, idx: NodeIdx) -> Vec<f32> {
    let node = puppet.get(idx).expect("the node");
    match &node.kind {
        catchlight_core::NodeKind::Spine(sp) => {
            let mut out = Vec::new();
            sp.chain
                .as_ref()
                .expect("the spine carries a chain")
                .link_bends(Mat4::IDENTITY, &mut out);
            out
        }
        other => panic!("not a spine: {other:?}"),
    }
}

/// A model whose chain hangs under a group turned by `rotation` radians, with
/// weightless 10 Hz links so the bend spring is the only thing holding the
/// strand — no gravity to balance against, so the pose is the whole answer.
fn posed_chain(rotation: f32, links: usize) -> (Model, NodeId, Vec<ParamId>) {
    let mut hex = SeededHex::new(29);
    let mut model = Model::new();
    model.set_physics(ClmPhysics {
        pixels_per_meter: 1.0,
        gravity: 1.0,
    });
    let root = model.root().expect("root").clone();
    let mut group = ModelNode::new("head", ModelNodeKind::Group);
    group.transform.rotation = [0.0, 0.0, rotation];
    let head = model.add_node(&root, group, &mut hex).expect("add group");

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
    let mut spine = ModelSpine::new(straight(links, 60.0));
    let mut data = ModelChain::new(links);
    data.gravity = 981.0;
    data.set_links(
        (0..links)
            .map(|_| catchlight_core::LinkFeel {
                gravity_scale: 0.0,
                damping: 0.5,
                stiffness: 10.0,
                limit: None,
            })
            .collect(),
    );
    spine.set_chain(Some(data));
    let chain = model
        .add_node(
            &head,
            ModelNode::new("hair", ModelNodeKind::Spine(spine)),
            &mut hex,
        )
        .expect("add chain");
    model
        .set_spine_targets(&chain, params.iter().cloned().map(Some).collect())
        .expect("aim the chain");
    (model, chain, params)
}

/// **A link's spring holds the bend its own param poses, and `link_bends`
/// reads that same number back.** The round trip is the whole sign
/// convention in one assertion: pose +1/6 of a half turn, and a strand stiff
/// enough to hold it stands at +1/6, tip toward the node's +X.
///
/// Weightless links, so nothing but the spring decides where they sit. The
/// param reads the chain's own claim here, because a chain at weight 1
/// decides its params outright — so this compares the solver's answer with
/// the pose that asked for it, not the pose with itself.
///
/// Under a node turned 30 degrees the answer is the same +1/6: a bend is
/// measured in the node's own frame, so a posed bend turns with the head
/// rather than against it.
#[test]
fn a_stiff_link_holds_the_bend_its_param_poses() {
    for rotation in [0.0, std::f32::consts::FRAC_PI_6] {
        let (model, chain, params) = posed_chain(rotation, 1);
        let mut puppet = Puppet::new(&model);
        puppet.set_param_value(&params[0], 1.0 / 6.0);
        puppet.settle_physics(&model);
        puppet.tick(&model, DT);

        let bend = puppet.param_value(&params[0]).expect("the bend");
        assert!(
            (bend - 1.0 / 6.0).abs() < 1e-4,
            "a stiff link posed at +1/6 stands at +1/6 under a node turned \
             {rotation} rad, got {bend}",
        );
        // And the strand really is bent, rather than the pose passing
        // through an unmoved chain: 60 px at 30 degrees off the node's down.
        let idx = puppet.node_idx(&chain).expect("the chain baked");
        let out = tip(&puppet, idx) - anchor(&puppet, idx);
        assert!(
            out.length() > 59.0 && out.normalize().y < 0.9,
            "the link swung off the node's down: {out:?}",
        );
    }
}

/// A posed bend is part of the shape `settle_physics` computes, so a chain
/// settled under one is a fixed point of the tick that follows: no motion to
/// report and a bend that does not creep.
///
/// The weighted case is the one that could have creeped — the spring pulls to
/// the pose, gravity pulls down, and the pose the settle walks has to be the
/// same balance the step would have found.
#[test]
fn a_chain_settled_under_a_posed_bend_stays_put() {
    let (mut model, chain, params) = posed_chain(0.0, 2);
    // Real weight on both links, so the rest pose is a compromise between the
    // spring's target and gravity rather than either one of them.
    let mut weighted = ModelChain::new(2);
    weighted.gravity = 981.0;
    weighted.set_links(
        (0..2)
            .map(|_| catchlight_core::LinkFeel {
                gravity_scale: 1.0,
                damping: 0.5,
                stiffness: 4.0,
                limit: None,
            })
            .collect(),
    );
    model
        .set_spine_chain(&chain, Some(weighted))
        .expect("retune the links");

    let mut puppet = Puppet::new(&model);
    puppet.set_param_value(&params[0], 0.25);
    puppet.set_param_value(&params[1], -0.125);
    puppet.settle_physics(&model);

    let idx = puppet.node_idx(&chain).expect("the chain baked");
    let settled = chain_bends(&puppet, idx);
    for f in 0..120 {
        assert!(
            !puppet.tick(&model, DT).physics,
            "frame {f}: a settled chain under a posed bend is standing still",
        );
    }
    let after = chain_bends(&puppet, idx);
    for (i, (a, b)) in settled.iter().zip(&after).enumerate() {
        assert!(
            (a - b).abs() < 1e-4,
            "link {i} crept from {a} to {b} over two seconds",
        );
    }
    // The strand really is holding a pose and not hanging: with a 4 Hz
    // spring against gravity the first joint sits well off the node's down.
    assert!(
        settled[0] > 0.05,
        "the spring holds the strand toward its posed bend: {settled:?}",
    );
}

/// Each link answers to its own param: posed +1/6 and -1/6, the strand bends
/// one way and then back, and each bend reads its own pose rather than the
/// sum of the two.
#[test]
fn every_link_holds_its_own_posed_bend() {
    let (model, _chain, params) = posed_chain(0.0, 2);
    let mut puppet = Puppet::new(&model);
    puppet.set_param_value(&params[0], 1.0 / 6.0);
    puppet.set_param_value(&params[1], -1.0 / 6.0);
    puppet.settle_physics(&model);
    puppet.tick(&model, DT);

    for (i, want) in [1.0 / 6.0f32, -1.0 / 6.0].into_iter().enumerate() {
        let got = puppet.param_value(&params[i]).expect("the bend");
        assert!(
            (got - want).abs() < 1e-4,
            "link {i} posed at {want} stands at {want}, got {got}",
        );
    }
}

/// **`weight` is how much of its param a chain decides.** At 1 the solve is
/// the value; at 0.5 the param lands half way between the pose and the
/// solve; at 0 the chain asserts nothing at all and the param keeps the pose
/// — while the strand goes on swinging, which is what makes 0 a different
/// thing from switching physics off.
#[test]
fn a_chains_weight_is_how_much_of_the_param_it_decides() {
    for weight in [1.0f32, 0.5, 0.0] {
        let mut f = Fixture::new(3);
        f.wire_outputs();
        f.model
            .update_node(&f.chain, |n| {
                let ModelNodeKind::Spine(spine) = &mut n.kind else {
                    panic!("not a spine");
                };
                let mut chain = spine.chain().cloned().expect("a chain");
                chain.weight = weight;
                spine.set_chain(Some(chain));
                Ok::<(), ()>(())
            })
            .expect("set the weight")
            .expect("the node is a spine");

        let mut puppet = f.puppet();
        puppet.settle_physics(&f.model);
        let idx = f.idx(&puppet);
        assert!(puppet.kick_chain(idx, Vec2::new(30.0, 0.0)), "kicked");
        let motion = puppet.tick(&f.model, DT);

        // The pose is every param's default, zero, so the fold is
        // `0 + weight * (bend - 0)`.
        let bends = chain_bends(&puppet, idx);
        for (i, param) in f.params.iter().enumerate() {
            let got = puppet.param_value(param).expect("the bend");
            let want = weight * bends[i];
            assert!(
                (got - want).abs() < 1e-6,
                "at weight {weight} link {i} reads {got}, want {want} \
                 ({} of the chain's own {})",
                weight,
                bends[i],
            );
        }
        if weight == 0.0 {
            for param in &f.params {
                assert_eq!(
                    puppet.param_value(param),
                    Some(0.0),
                    "a chain at weight 0 leaves its params where they were posed",
                );
            }
            assert!(
                bends.iter().any(|b| b.abs() > 1e-3),
                "and the strand is still bent from the kick: {bends:?}",
            );
            assert!(
                motion.physics,
                "a chain at weight 0 goes on simulating; it just says nothing",
            );
        }
    }
}

/// **A limit binds the param, not just the particles.** The bend a chain
/// writes is the bend the solver holds, so a kick hard enough to fling a
/// strand right over cannot push the param past the limit its link carries.
///
/// Every link is limited to an eighth of a turn here, and the kick is worth
/// far more than that on the first link alone.
#[test]
fn a_limited_chain_never_writes_a_bend_past_its_limit() {
    const LIMIT: f32 = 0.125;
    let mut f = Fixture::new(3);
    f.wire_outputs();
    f.model
        .update_node(&f.chain, |n| {
            let ModelNodeKind::Spine(spine) = &mut n.kind else {
                panic!("not a spine");
            };
            let mut chain = spine.chain().expect("a chain").clone();
            chain.set_links(
                chain
                    .links()
                    .iter()
                    .map(|l| catchlight_core::LinkFeel {
                        limit: Some(LIMIT),
                        ..*l
                    })
                    .collect(),
            );
            spine.set_chain(Some(chain));
            Ok::<(), ()>(())
        })
        .expect("limit the links")
        .expect("a spine");

    let mut puppet = f.puppet();
    puppet.settle_physics(&f.model);
    let idx = f.idx(&puppet);
    assert!(puppet.kick_chain(idx, Vec2::new(120.0, -60.0)), "kicked");

    let mut worst = 0.0f32;
    for frame in 0..600 {
        puppet.tick(&f.model, DT);
        for (i, param) in f.params.iter().enumerate() {
            let bend = puppet.param_value(param).expect("a bend");
            worst = worst.max(bend.abs());
            assert!(
                bend.abs() <= LIMIT + 1e-3,
                "frame {frame} link {i} wrote {bend}, past its limit of {LIMIT}",
            );
        }
    }
    // And the kick really did drive the links into their limits, so this is
    // a clamp holding rather than a chain that never got there.
    assert!(
        worst > LIMIT - 1e-3,
        "the kick reached the limit; the worst bend seen was {worst}",
    );
}
