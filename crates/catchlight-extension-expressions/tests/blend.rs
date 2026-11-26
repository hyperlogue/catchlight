#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]
//! Playing expressions by weight onto controls and params.

use catchlight_core::{
    Model, ModelNode, ModelNodeKind, ModelParam, MotionResponse, Name, NodeId, ParamId, Puppet,
    ResponseInput, ResponseSource,
};
use catchlight_extension_expressions::{Expression, Expressions, Setting, Weights};
use catchlight_extension_humanoid::ControlValues;

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn weights(pairs: &[(&str, f32)]) -> Weights {
    pairs.iter().copied().collect()
}

fn none() -> Weights {
    Weights::new()
}

fn controls(expressions: &Expressions, w: &[(&str, f32)], start: &[(&str, f32)]) -> ControlValues {
    let mut controls: ControlValues = start.iter().copied().collect();
    expressions.blend_controls(&weights(w), &none(), &mut controls);
    controls
}

fn visemes() -> Expressions {
    Expressions::from_expressions([
        Expression::new(
            "aa",
            vec![
                Setting::control("mouth_open", 1.0),
                Setting::control("mouth_width", 0.5),
            ],
        ),
        Expression::new(
            "uw",
            vec![
                Setting::control("mouth_open", 0.5),
                Setting::control("mouth_width", -1.0),
            ],
        ),
    ])
}

#[test]
fn one_expression_at_a_partial_weight_fades_from_what_was_there() {
    let c = controls(&visemes(), &[("aa", 0.25)], &[("mouth_open", 0.2)]);
    assert_eq!(c.get("mouth_open"), Some(0.2 + (1.0 - 0.2) * 0.25));
    // No value yet counts as 0.
    assert_eq!(c.get("mouth_width"), Some(0.125));
    assert_eq!(c.get("mouth_smile"), None);
}

#[test]
fn two_cross_fading_give_exactly_the_weighted_mix() {
    let c = controls(
        &visemes(),
        &[("aa", 0.25), ("uw", 0.75)],
        &[("mouth_open", 0.9), ("mouth_width", 0.3)],
    );
    assert_eq!(c.get("mouth_open"), Some(0.25 * 1.0 + 0.75 * 0.5));
    assert_eq!(c.get("mouth_width"), Some(0.25 * 0.5 - 0.75));
}

#[test]
fn weights_summing_past_one_give_their_weighted_mean() {
    let c = controls(
        &visemes(),
        &[("aa", 1.0), ("uw", 1.0)],
        &[("mouth_open", 0.0)],
    );
    assert_eq!(c.get("mouth_open"), Some(0.75));
    assert_eq!(c.get("mouth_width"), Some(-0.25));
}

#[test]
fn weights_clamp_to_zero_through_one_and_a_zero_weight_changes_nothing() {
    let w = weights(&[("aa", 3.0), ("uw", -1.0), ("ghost", 1.0)]);
    assert_eq!(w.get("aa"), Some(1.0));
    assert_eq!(w.get("uw"), Some(0.0));
    let mut c: ControlValues = std::iter::once(("mouth_open", 0.3)).collect();
    visemes().blend_controls(&w, &none(), &mut c);
    assert_eq!(c.get("mouth_open"), Some(1.0));
    assert_eq!(c.get("mouth_width"), Some(0.5));
}

#[test]
fn the_blended_control_value_clamps_to_the_controls_range() {
    let expressions = Expressions::from_expressions([Expression::new(
        "wide",
        vec![Setting::control("mouth_open", -0.5)],
    )]);
    let c = controls(&expressions, &[("wide", 1.0)], &[]);
    assert_eq!(c.get("mouth_open"), Some(0.0));
}

/// Params `tears` (0..1, default 0.25) and `sway`, which a motion response
/// driven by `tears` writes.
fn rig() -> Model {
    let mut model = Model::new();
    model
        .add_param_with_id(
            pid("tears"),
            ModelParam::new(Name::truncated("tears"), 0.0, 1.0, 0.25),
        )
        .unwrap();
    model
        .add_param_with_id(
            pid("sway"),
            ModelParam::new(Name::truncated("sway"), -1.0, 1.0, 0.0),
        )
        .unwrap();
    let root = model.root().unwrap().clone();
    model
        .add_node_with_id(
            NodeId::new("spring").unwrap(),
            &root,
            ModelNode::new(
                "spring",
                ModelNodeKind::MotionResponse(MotionResponse {
                    inputs: vec![ResponseInput {
                        source: ResponseSource::PoseParam {
                            param: pid("tears"),
                        },
                        gain: 1.0,
                    }],
                    output_param: Some(pid("sway")),
                    ..MotionResponse::default()
                }),
            ),
        )
        .unwrap();
    model
}

fn weepy() -> Expressions {
    Expressions::from_expressions([
        Expression::new(
            "sad",
            vec![
                Setting::param(pid("tears"), 1.0),
                Setting::param(pid("sway"), 1.0),
                Setting::control("mouth_smile", -1.0),
            ],
        ),
        Expression::new("dry", vec![Setting::param(pid("tears"), 0.0)]),
    ])
}

#[test]
fn param_settings_blend_from_the_posed_value_or_the_default() {
    let model = rig();
    let mut puppet = Puppet::new(&model);
    weepy().blend_params(&weights(&[("sad", 0.5)]), &none(), &mut puppet);
    assert_eq!(puppet.param_value_posed(&pid("tears")), Some(0.625));

    puppet.set_param_value(&pid("tears"), 0.5);
    weepy().blend_params(
        &weights(&[("sad", 0.25), ("dry", 0.75)]),
        &none(),
        &mut puppet,
    );
    assert_eq!(puppet.param_value_posed(&pid("tears")), Some(0.25));
}

#[test]
fn param_settings_skip_a_driven_param_and_leave_controls_alone() {
    let model = rig();
    let mut puppet = Puppet::new(&model);
    assert!(puppet.is_driven(&pid("sway")));
    weepy().blend_params(&weights(&[("sad", 1.0)]), &none(), &mut puppet);
    assert_eq!(puppet.param_value_posed(&pid("tears")), Some(1.0));
    assert_eq!(puppet.param_value_posed(&pid("sway")), None);

    let c = controls(&weepy(), &[("sad", 1.0)], &[]);
    assert_eq!(c.iter().collect::<Vec<_>>(), [("mouth_smile", -1.0)]);
}

#[test]
fn a_target_set_twice_in_one_expression_plays_its_last_setting() {
    let expressions = Expressions::from_expressions([Expression::new(
        "twice",
        vec![
            Setting::control("mouth_open", 0.2),
            Setting::control("mouth_open", 0.8),
        ],
    )]);
    let c = controls(&expressions, &[("twice", 1.0)], &[]);
    assert_eq!(c.get("mouth_open"), Some(0.8));
}

/// `happy` and the viseme `aa` share a control and a param; `aa` also has
/// an expression namesake that sets something else.
fn speaking() -> Expressions {
    Expressions::from_groups(
        [
            Expression::new(
                "happy",
                vec![
                    Setting::control("mouth_open", 0.2),
                    Setting::control("mouth_smile", 1.0),
                    Setting::param(pid("tears"), 0.5),
                ],
            ),
            Expression::new("aa", vec![Setting::control("mouth_smile", -1.0)]),
        ],
        [Expression::new(
            "aa",
            vec![
                Setting::control("mouth_open", 1.0),
                Setting::param(pid("tears"), 1.0),
            ],
        )],
    )
}

#[test]
fn visemes_blend_over_expressions_on_controls() {
    let blend = |aa: f32| {
        let mut c = ControlValues::new();
        speaking().blend_controls(&weights(&[("happy", 1.0)]), &weights(&[("aa", aa)]), &mut c);
        c
    };
    let full = blend(1.0);
    assert_eq!(full.get("mouth_open"), Some(1.0), "the viseme's value");
    assert_eq!(
        full.get("mouth_smile"),
        Some(1.0),
        "the expression aa plays no part"
    );
    let half = blend(0.5);
    assert_eq!(half.get("mouth_open"), Some(0.2 + (1.0 - 0.2) * 0.5));
    let silent = blend(0.0);
    assert_eq!(silent.get("mouth_open"), Some(0.2));
}

#[test]
fn visemes_blend_over_expressions_on_params() {
    let model = rig();
    let blend = |aa: f32| {
        let mut puppet = Puppet::new(&model);
        speaking().blend_params(
            &weights(&[("happy", 1.0)]),
            &weights(&[("aa", aa)]),
            &mut puppet,
        );
        puppet.param_value_posed(&pid("tears")).unwrap()
    };
    assert_eq!(blend(1.0), 1.0);
    assert_eq!(blend(0.5), 0.5 + (1.0 - 0.5) * 0.5);
    assert_eq!(blend(0.0), 0.5);

    // The viseme alone fades from the posed value.
    let mut puppet = Puppet::new(&model);
    speaking().blend_params(&none(), &weights(&[("aa", 0.5)]), &mut puppet);
    assert_eq!(puppet.param_value_posed(&pid("tears")), Some(0.625));
}

#[test]
fn param_values_blend_from_the_base_given_and_leave_out_what_it_withholds() {
    let values = speaking().param_values(
        &weights(&[("happy", 0.5)]),
        &weights(&[("aa", 0.5)]),
        |_| Some(0.0),
    );
    assert_eq!(values, [(pid("tears"), 0.25 + (1.0 - 0.25) * 0.5)]);
    let values = speaking().param_values(&weights(&[("happy", 1.0)]), &none(), |_| None);
    assert!(values.is_empty());
}
