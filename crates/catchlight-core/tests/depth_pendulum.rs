#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A `Tz` binding and the pendulum that feels it.
//!
//! The fixture hangs one driver under a group whose translation two params
//! bind: `depth` moves it along z, `slide` along x. A depth pendulum must
//! swing when the group moves in depth and not when it slides across the
//! screen; a screen pendulum must do the reverse, which is also what shows a
//! `Tz` binding never leaks into what the screen sees.

use catchlight_core::formats::clm::ClmPhysics;
use catchlight_core::id::SeededHex;
use catchlight_core::model::{ModelNode, ModelNodeKind, ModelParam, ModelPhysics};
use catchlight_core::physics::{PendulumKind, PendulumPlane, PhysicsParamMapMode};
use catchlight_core::{BindingKey, BindingTarget, Model, Name, ParamId, Puppet, ScalarTarget};

const DT: f32 = 1.0 / 60.0;

struct Rig {
    model: Model,
    depth: ParamId,
    slide: ParamId,
    angle: ParamId,
}

fn rig(plane: PendulumPlane) -> Rig {
    let mut hex = SeededHex::new(5);
    let mut model = Model::new();
    model.set_physics(ClmPhysics {
        pixels_per_meter: 1.0,
        gravity: 1.0,
        ..ClmPhysics::default()
    });
    let param = |model: &mut Model, hex: &mut SeededHex, name: &str| {
        model
            .add_param(
                ModelParam {
                    name: Name::truncated(name),
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                },
                hex,
            )
            .unwrap()
    };
    let depth = param(&mut model, &mut hex, "depth");
    let slide = param(&mut model, &mut hex, "slide");
    let angle = param(&mut model, &mut hex, "angle");
    let root = model.root().unwrap().clone();
    let pivot = model
        .add_node(
            &root,
            ModelNode::new("pivot", ModelNodeKind::Group),
            &mut hex,
        )
        .unwrap();
    for (param, target) in [(&depth, ScalarTarget::Tz), (&slide, ScalarTarget::Tx)] {
        let key = BindingKey::new(param.clone(), pivot.clone(), BindingTarget::Scalar(target));
        model
            .add_binding_with_positions(&key, vec![vec![0.0, 1.0]])
            .unwrap();
        model.set_binding_key(&key, [0, 0], -40.0).unwrap();
        model.set_binding_key(&key, [1, 0], 40.0).unwrap();
    }
    let mut driver = ModelPhysics::new(PendulumKind::RigidPendulum);
    driver.map_mode = PhysicsParamMapMode::AngleLength;
    driver.plane = plane;
    driver.gravity = 981.0;
    driver.length = 100.0;
    driver.angle_damping = 0.5;
    let node = model
        .add_node(
            &pivot,
            ModelNode::new("driver", ModelNodeKind::SimplePhysics(driver)),
            &mut hex,
        )
        .unwrap();
    model
        .set_physics_targets(&node, [Some(angle.clone()), None])
        .unwrap();
    Rig {
        model,
        depth,
        slide,
        angle,
    }
}

/// The largest swing the driver reports over half a second after `param`
/// steps from rest to its maximum.
fn swing_after_step(rig: &Rig, param: &ParamId) -> f32 {
    let mut puppet = Puppet::new(&rig.model);
    for _ in 0..30 {
        puppet.tick(&rig.model, DT);
    }
    puppet.set_param_value(param, 1.0);
    (0..30)
        .map(|_| {
            puppet.tick(&rig.model, DT);
            puppet.param_value(&rig.angle).unwrap().abs()
        })
        .fold(0.0, f32::max)
}

#[test]
fn a_depth_pendulum_swings_with_depth_and_ignores_the_screen() {
    let rig = rig(PendulumPlane::Depth);
    let depth = swing_after_step(&rig, &rig.depth);
    let slide = swing_after_step(&rig, &rig.slide);
    assert!(depth > 0.02, "a step in depth swings it: {depth}");
    assert!(slide < 1e-6, "a slide across the screen does not: {slide}");
}

#[test]
fn a_screen_pendulum_ignores_depth() {
    let rig = rig(PendulumPlane::Screen);
    let depth = swing_after_step(&rig, &rig.depth);
    let slide = swing_after_step(&rig, &rig.slide);
    assert!(slide > 0.02, "a slide across the screen swings it: {slide}");
    assert!(depth < 1e-6, "a step in depth does not: {depth}");
}

/// Both swing the same way for the same step: the depth plane is the screen
/// plane with z in x's place, not a differently scaled or mirrored copy.
#[test]
fn depth_and_screen_swing_alike_for_the_same_step() {
    let depth = swing_after_step(&rig(PendulumPlane::Depth), &rig(PendulumPlane::Depth).depth);
    let screen = swing_after_step(
        &rig(PendulumPlane::Screen),
        &rig(PendulumPlane::Screen).slide,
    );
    assert!((depth - screen).abs() < 1e-5, "{depth} vs {screen}");
}

#[test]
fn a_depth_pendulum_and_a_tz_binding_survive_a_save() {
    let rig = rig(PendulumPlane::Depth);
    let bytes = rig.model.to_clm_bytes().unwrap();
    let loaded = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(loaded.to_clm_bytes().unwrap(), bytes, "byte for byte");
    let swing = swing_after_step(
        &Rig {
            model: loaded,
            depth: rig.depth.clone(),
            slide: rig.slide.clone(),
            angle: rig.angle.clone(),
        },
        &rig.depth,
    );
    assert!(
        swing > 0.02,
        "the loaded pendulum still feels depth: {swing}"
    );
}
