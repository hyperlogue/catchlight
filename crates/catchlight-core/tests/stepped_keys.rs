#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A `Stepped` key holds from its own position.
//!
//! The rule is one sentence — a key's value shows at that key and until the
//! next one — and it has to hold on both axes it is read over: a binding's
//! key positions and an animation lane's frames. A pose resting exactly on a
//! key is the common case, not an edge: a param's default usually is one.

use catchlight_core::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
use catchlight_core::id::SeededHex;
use catchlight_core::model::{ModelNode, ModelNodeKind, ModelParam};
use catchlight_core::{
    BindingKey, BindingTarget, InterpolateMode, Model, Name, NodeId, ParamId, Pose, Puppet,
    ScalarTarget,
};

/// A param over `-1..=1` driving one group's `Tx` through keys at -1, 0 and 1
/// holding 10, 20 and 30.
fn three_keys(mode: InterpolateMode) -> (Model, ParamId, NodeId, BindingKey) {
    let mut hex = SeededHex::new(11);
    let mut model = Model::new();
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
    let root = model.root().unwrap().clone();
    let node = model
        .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
        .unwrap();
    let key = BindingKey::new(
        param.clone(),
        node.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    model
        .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
        .unwrap();
    for (cell, value) in [10.0f32, 20.0, 30.0].into_iter().enumerate() {
        model.set_binding_key(&key, [cell as u32, 0], value).unwrap();
    }
    model.set_binding_interpolate(&key, mode).unwrap();
    (model, param, node, key)
}

/// What the puppet folds into the node's `Tx` and what the model reports for
/// the same pose. They share one interpolator and must agree.
fn tx_at(
    model: &Model,
    param: &ParamId,
    node: &NodeId,
    key: &BindingKey,
    value: f32,
) -> (f32, f32) {
    let mut puppet = Puppet::new(model);
    puppet.set_param_value(param, value);
    puppet.tick(model, 0.0);
    let idx = puppet.node_idx(node).unwrap();
    let folded = puppet.get(idx).unwrap().transform.translation.x;
    let mut pose = Pose::new();
    pose.set(param.clone(), value);
    (folded, model.eval_scalar(key, &pose).unwrap())
}

#[test]
fn a_stepped_binding_shows_each_key_at_its_own_position() {
    let (model, param, node, key) = three_keys(InterpolateMode::Stepped);
    for (value, expected) in [
        (-1.0, 10.0),
        (-0.5, 10.0),
        (-0.0001, 10.0),
        // The interior key, where the param also rests.
        (0.0, 20.0),
        (0.5, 20.0),
        (0.9999, 20.0),
        (1.0, 30.0),
    ] {
        let (folded, evaluated) = tx_at(&model, &param, &node, &key, value);
        assert_eq!(folded, expected, "fold at {value}");
        assert_eq!(evaluated, expected, "eval_scalar at {value}");
    }
}

/// Opening the bracket at an interior key must not move any other mode: each
/// already read the key's own value there.
#[test]
fn the_other_modes_still_read_the_key_at_its_position() {
    for mode in [
        InterpolateMode::Linear,
        InterpolateMode::Nearest,
        InterpolateMode::Cubic,
    ] {
        let (model, param, node, key) = three_keys(mode);
        for (value, expected) in [(-1.0, 10.0), (0.0, 20.0), (1.0, 30.0)] {
            let (folded, evaluated) = tx_at(&model, &param, &node, &key, value);
            assert_eq!(folded, expected, "{mode:?} fold at {value}");
            assert_eq!(evaluated, expected, "{mode:?} eval_scalar at {value}");
        }
    }
}

#[test]
fn a_stepped_lane_switches_on_the_keyframe() {
    let (model, param, node, _) = three_keys(InterpolateMode::Linear);
    let mut puppet = Puppet::new(&model);
    // One frame per tick, so playback lands exactly on each keyframe.
    let timestep = 1.0 / 4.0;
    puppet.set_animations(vec![ClmAnimation {
        name: "steps".into(),
        timestep,
        length: 9,
        lead_in: -1,
        lead_out: -1,
        lanes: vec![ClmLane {
            param: param.clone(),
            keyframes: vec![
                ClmKeyframe {
                    frame: 0,
                    value: -1.0,
                },
                ClmKeyframe {
                    frame: 2,
                    value: 0.0,
                },
                ClmKeyframe {
                    frame: 4,
                    value: 1.0,
                },
            ],
            interpolation: InterpolateMode::Stepped,
        }],
    }]);
    assert!(puppet.play_animation("steps"));
    let idx = puppet.node_idx(&node).unwrap();
    let mut seen = Vec::new();
    puppet.tick(&model, 0.0);
    seen.push(puppet.get(idx).unwrap().transform.translation.x);
    for _ in 0..4 {
        puppet.tick(&model, timestep);
        seen.push(puppet.get(idx).unwrap().transform.translation.x);
    }
    // Frames 0..=4: the key at frame 2 takes effect on frame 2, and the one
    // at frame 4 on frame 4.
    assert_eq!(seen, [10.0, 10.0, 20.0, 20.0, 30.0]);
}
