#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A player posing a puppet from outside it.

use catchlight_core::id::SeededHex;
use catchlight_core::model::{ModelNode, ModelNodeKind};
use catchlight_core::{
    BindingKey, BindingTarget, InterpolateMode, Model, ModelParam, Name, ParamId, Puppet,
    ScalarTarget,
};
use catchlight_extension_animations::{
    AnimationPlayer, Animations, Clip, Keyframe, Lane, LaneTarget,
};

fn with_param(min: f32, max: f32) -> (Model, ParamId) {
    let mut model = Model::new();
    let id = ParamId::new("drive").unwrap();
    model
        .add_param_with_id(
            id.clone(),
            ModelParam::new(Name::new("Drive").unwrap(), min, max, 0.0),
        )
        .unwrap();
    (model, id)
}

fn clip(name: &str, timestep: f32, length: u32, param: &ParamId, keys: &[(u32, f32)]) -> Clip {
    Clip {
        name: name.into(),
        timestep,
        length,
        lead_in: None,
        lead_out: None,
        lanes: vec![Lane {
            target: LaneTarget::Param(param.clone()),
            interpolation: InterpolateMode::Linear,
            keyframes: keys
                .iter()
                .map(|&(frame, value)| Keyframe { frame, value })
                .collect(),
        }],
    }
}

#[test]
fn a_models_clips_play_on_its_puppet_and_name_themselves_while_playing() {
    let (mut model, param) = with_param(0.0, 1.0);
    *model.extension_or_default::<Animations>().unwrap() =
        Animations::from_clips([clip("Blink", 1.0 / 60.0, 4, &param, &[(0, 0.0), (3, 1.0)])]);
    let mut puppet = Puppet::new(&model);
    let mut player = AnimationPlayer::from_animations(model.extension::<Animations>().unwrap());
    assert_eq!(player.clips().len(), 1);
    assert_eq!(player.playing(), None, "nothing plays yet");
    assert!(!player.play("Wink"));

    assert!(player.play("Blink"));
    assert_eq!(player.playing(), Some("Blink"));
    assert!(
        player.advance(&mut puppet, 3.0 / 60.0),
        "a lane moved a param"
    );
    let motion = puppet.tick(&model, 3.0 / 60.0);
    assert_eq!(puppet.param_value(&param), Some(1.0));
    assert!(!motion.motion_responses, "this model has no drivers");

    player.stop();
    assert_eq!(player.playing(), None);
    assert!(!player.advance(&mut puppet, 1.0 / 60.0));
}

#[test]
fn sample_matches_fractional_and_looped_values_without_advancing_playback() {
    let (model, id) = with_param(0.0, 10.0);
    let mut pulse = clip("Pulse", 0.125, 8, &id, &[(0, 0.0), (7, 7.0)]);
    pulse.lead_in = Some(2);
    pulse.lead_out = Some(6);
    let mut puppet = Puppet::new(&model);
    puppet.set_motion_responses_enabled(false);
    let mut player = AnimationPlayer::new(vec![pulse.clone()]);
    let step = |player: &mut AnimationPlayer, puppet: &mut Puppet, dt: f32| {
        player.advance(puppet, dt);
        puppet.tick(&model, dt);
    };
    assert!(player.play("Pulse"));
    assert!(player.sample().is_none());
    step(&mut player, &mut puppet, 0.0);
    let zero = player.sample().unwrap();
    assert_eq!((zero.frame, zero.time), (0.0, 0.0));
    step(&mut player, &mut puppet, 0.1875);
    let sampled = player.sample().unwrap();
    assert_eq!((sampled.frame, sampled.time), (1.5, 0.1875));
    assert_eq!(puppet.param_value_posed(&id), Some(1.5));
    assert_eq!(player.sample(), Some(sampled));
    step(&mut player, &mut puppet, 0.5625);
    let wrapped = player.sample().unwrap();
    assert_eq!((wrapped.frame, wrapped.time), (2.0, 0.25));
    assert!(wrapped.looping);
    assert_eq!(puppet.param_value(&id), Some(2.0));
    player.stop();
    assert!(player.sample().is_none());
    assert!(player.play("Pulse"));
    step(&mut player, &mut puppet, 0.0);
    assert_eq!(player.sample().unwrap().frame, 0.0);
    player.set_clips(vec![pulse]);
    assert!(player.sample().is_none());
    assert!(!player.is_playing());
}

#[test]
fn replaced_clips_keep_the_playing_clip_by_name_from_where_it_was() {
    let (model, id) = with_param(0.0, 10.0);
    let ramp = |name: &str| clip(name, 0.125, 8, &id, &[(0, 0.0), (7, 7.0)]);
    let mut puppet = Puppet::new(&model);
    let mut player = AnimationPlayer::new(vec![ramp("a")]);
    assert!(player.play("a"));
    player.advance(&mut puppet, 0.0);
    player.advance(&mut puppet, 0.25);

    player.replace_clips(vec![ramp("b"), ramp("a")]);
    assert_eq!(player.playing(), Some("a"));
    let kept = player.sample().unwrap();
    assert_eq!((kept.index, kept.frame), (1, 2.0));
    player.advance(&mut puppet, 0.125);
    assert_eq!(player.sample().unwrap().frame, 3.0);
    assert_eq!(puppet.param_value_posed(&id), Some(3.0));

    player.replace_clips(vec![ramp("b")]);
    assert!(!player.is_playing(), "no clip is named a any more");
}

#[test]
fn a_stepped_lane_switches_on_the_keyframe() {
    let mut hex = SeededHex::new(11);
    let (mut model, param) = with_param(-1.0, 1.0);
    let root = model.root().unwrap().clone();
    let node = model
        .add_node(&root, ModelNode::new("n", ModelNodeKind::Group), &mut hex)
        .unwrap();
    let key = BindingKey::new(
        param.clone(),
        node.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    model
        .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
        .unwrap();
    for (cell, tx) in [(0, 10.0), (1, 20.0), (2, 30.0)] {
        model.set_binding_key(&key, [cell, 0], tx).unwrap();
    }
    let timestep = 1.0 / 4.0;
    let mut steps = clip(
        "steps",
        timestep,
        9,
        &param,
        &[(0, -1.0), (2, 0.0), (4, 1.0)],
    );
    steps.lanes[0].interpolation = InterpolateMode::Stepped;
    let mut puppet = Puppet::new(&model);
    let mut player = AnimationPlayer::new(vec![steps]);
    assert!(player.play("steps"));
    let idx = puppet.node_idx(&node).unwrap();
    let mut seen = Vec::new();
    for dt in [0.0, timestep, timestep, timestep, timestep] {
        player.advance(&mut puppet, dt);
        puppet.tick(&model, dt);
        seen.push(puppet.get(idx).unwrap().transform.translation.x);
    }
    assert_eq!(seen, [10.0, 10.0, 20.0, 20.0, 30.0]);
}

#[test]
fn a_lane_on_a_driven_param_is_skipped() {
    use catchlight_core::{MotionResponse, ResponseInput, ResponseSource};
    let (mut model, input) = with_param(-1.0, 1.0);
    let output = ParamId::new("follow").unwrap();
    model
        .add_param_with_id(
            output.clone(),
            ModelParam::new(Name::new("Follow").unwrap(), -1.0, 1.0, 0.0),
        )
        .unwrap();
    let root = model.root().unwrap().clone();
    let mut hex = SeededHex::new(3);
    model
        .add_node(
            &root,
            ModelNode::new(
                "spring",
                ModelNodeKind::MotionResponse(MotionResponse {
                    inputs: vec![ResponseInput {
                        source: ResponseSource::PoseParam {
                            param: input.clone(),
                        },
                        gain: 1.0,
                    }],
                    output_param: Some(output.clone()),
                    ..MotionResponse::default()
                }),
            ),
            &mut hex,
        )
        .unwrap();
    let mut puppet = Puppet::new(&model);
    assert!(puppet.is_driven(&output));
    let mut player = AnimationPlayer::new(vec![clip("c", 0.1, 4, &output, &[(0, 0.7)])]);
    player.play("c");
    assert!(!player.advance(&mut puppet, 0.0));
    assert_eq!(puppet.param_value_posed(&output), None);

    puppet.set_motion_responses_enabled(false);
    assert!(!puppet.is_driven(&output));
    assert!(player.advance(&mut puppet, 0.0));
    assert_eq!(puppet.param_value_posed(&output), Some(0.7));
}
