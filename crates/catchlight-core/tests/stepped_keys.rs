#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]
//! A `Stepped` key holds from its own position.

use catchlight_core::id::SeededHex;
use catchlight_core::model::{ModelNode, ModelNodeKind, ModelParam};
use catchlight_core::{
    BindingKey, BindingTarget, InterpolateMode, Model, Name, NodeId, ParamId, Pose, Puppet,
    ScalarTarget,
};

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
        model
            .set_binding_key(&key, [cell as u32, 0], value)
            .unwrap();
    }
    model.set_binding_interpolate(&key, mode).unwrap();
    (model, param, node, key)
}

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
