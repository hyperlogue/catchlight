//! Observation reports the runtime sample, including native lead-region loops.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use catchlight_core::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
use catchlight_core::{InterpolateMode, Model, ModelParam, Name, ParamId, Puppet};

#[test]
fn sample_matches_fractional_and_looped_values_without_advancing_playback() {
    let mut model = Model::new();
    let id = ParamId::new("drive").unwrap();
    model
        .add_param_with_id(
            id.clone(),
            ModelParam::new(Name::new("Drive").unwrap(), 0.0, 10.0, 0.0),
        )
        .unwrap();
    let clip = ClmAnimation {
        name: "Pulse".into(),
        timestep: 0.125,
        length: 8,
        lead_in: 2,
        lead_out: 6,
        lanes: vec![ClmLane {
            param: id.clone(),
            interpolation: InterpolateMode::Linear,
            keyframes: vec![
                ClmKeyframe {
                    frame: 0,
                    value: 0.0,
                },
                ClmKeyframe {
                    frame: 7,
                    value: 7.0,
                },
            ],
        }],
    };
    let mut puppet = Puppet::new(&model);
    puppet.set_physics_enabled(false);
    puppet.set_animations(vec![clip.clone()]);
    assert!(puppet.play_animation("Pulse"));
    assert!(puppet.animation_sample().is_none());
    puppet.tick(&model, 0.0);
    let zero = puppet.animation_sample().unwrap();
    assert_eq!(zero.frame, 0.0);
    assert_eq!(zero.time, 0.0);
    puppet.tick(&model, 0.1875);
    let sampled = puppet.animation_sample().unwrap();
    assert_eq!(sampled.frame, 1.5);
    assert_eq!(sampled.time, 0.1875);
    assert_eq!(puppet.param_value_posed(&id), Some(1.5));
    assert_eq!(puppet.animation_sample(), Some(sampled));
    puppet.tick(&model, 0.5625);
    let wrapped = puppet.animation_sample().unwrap();
    assert_eq!(wrapped.frame, 2.0);
    assert_eq!(wrapped.time, 0.25);
    assert!(wrapped.looping);
    assert_eq!(puppet.param_value(&id), Some(2.0));
    puppet.stop_animation();
    assert!(puppet.animation_sample().is_none());
    assert!(puppet.play_animation("Pulse"));
    puppet.tick(&model, 0.0);
    assert_eq!(puppet.animation_sample().unwrap().frame, 0.0);
    puppet.set_animations(vec![clip]);
    assert!(puppet.animation_sample().is_none());
}
