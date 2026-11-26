#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]
//! Playback against motion responses: a clip's cuts restart the responses
//! that read the cut param, and seamless playback matches direct input.

use std::ops::{Deref, DerefMut};

use catchlight_core::{
    BindingKey, BindingTarget, Model, ModelNode, ModelNodeKind, ModelParam, MotionResponse, Name,
    NodeId, ParamId, Puppet, ResponseInput, ResponseSource, ScalarTarget,
};
use catchlight_extension_animations::{
    AnimationPlayer, Clip, Keyframe, Lane, LaneTarget, LiveInputs, Rig,
};
use catchlight_extension_humanoid::{ControlMapping, Humanoid};

/// A puppet with a player, ticked the way a host ticks them.
#[derive(Clone)]
struct Played {
    puppet: Puppet,
    player: AnimationPlayer,
}

impl Played {
    fn new(model: &Model) -> Self {
        Self {
            puppet: Puppet::new(model),
            player: AnimationPlayer::default(),
        }
    }

    fn tick(&mut self, model: &Model, dt: f32) {
        self.player
            .advance_with(&mut self.puppet, dt, Rig::of(model), &LiveInputs::default());
        self.puppet.tick(model, dt);
    }
}

impl Deref for Played {
    type Target = Puppet;

    fn deref(&self) -> &Puppet {
        &self.puppet
    }
}

impl DerefMut for Played {
    fn deref_mut(&mut self) -> &mut Puppet {
        &mut self.puppet
    }
}

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn nid(s: &str) -> NodeId {
    NodeId::new(s).unwrap()
}

fn model(damping: f32) -> Model {
    let mut m = Model::new();
    let root = m.root().unwrap().clone();
    for p in ["roll", "broad", "bend"] {
        m.add_param_with_id(pid(p), ModelParam::new(Name::truncated(p), -1., 1., 0.))
            .unwrap();
    }
    for (name, parent, source, output) in [
        (
            "upper",
            root,
            ResponseSource::PoseParam { param: pid("roll") },
            "broad",
        ),
        ("tip", nid("upper"), ResponseSource::Parent, "bend"),
    ] {
        m.add_node_with_id(
            nid(name),
            &parent,
            ModelNode::new(
                name,
                ModelNodeKind::MotionResponse(MotionResponse {
                    inputs: vec![ResponseInput { source, gain: 1. }],
                    period_s: 0.4,
                    damping_ratio: damping,
                    output_param: Some(pid(output)),
                }),
            ),
        )
        .unwrap();
    }
    m
}

fn slot_model() -> (Model, catchlight_core::SlotId) {
    use catchlight_core::{
        ModelPart, SlotId,
        formats::clm::{ClmIndices, ClmMesh},
    };
    let mut m = model(0.7);
    let root = m.root().unwrap().clone();
    let mesh = ClmMesh {
        verts: vec![0., 0., 30., 40., 0., 20.],
        uvs: vec![],
        indices: ClmIndices::U16(vec![0, 1, 2]),
        origin: [0., 0.],
    };
    let lattice = ClmMesh {
        verts: vec![-100., -100., 200., -100., -100., 200.],
        ..mesh.clone()
    };
    m.add_node_with_id(
        nid("group"),
        &root,
        ModelNode::new(
            "Group",
            ModelNodeKind::MeshGroup(catchlight_core::ModelMeshGroup::new(lattice)),
        ),
    )
    .unwrap();
    m.add_node_with_id(
        nid("art"),
        &nid("group"),
        ModelNode::new("Art", ModelNodeKind::Part(ModelPart::new(mesh))),
    )
    .unwrap();
    let slot = SlotId::new("root").unwrap();
    m.slot_add(&nid("art"), slot.clone()).unwrap();
    m.slot_fill(&nid("art"), &slot, 1).unwrap();
    let config = MotionResponse {
        inputs: vec![ResponseInput {
            gain: 1.,
            source: ResponseSource::PartSlot {
                part: nid("art"),
                slot: slot.clone(),
                angle_deg: 4_f32.atan2(3.).to_degrees(),
                reference_length: 100.,
            },
        }],
        period_s: 0.4,
        damping_ratio: 0.7,
        output_param: Some(pid("broad")),
    };
    m.set_motion_response(&nid("upper"), config).unwrap();
    (m, slot)
}

fn cut_clip() -> Clip {
    Clip {
        name: "turn".into(),
        timestep: 1. / 60.,
        length: 6,
        lead_in: Some(1),
        lead_out: Some(4),
        lanes: vec![Lane::new(
            LaneTarget::Param(pid("roll")),
            catchlight_core::InterpolateMode::Linear,
            vec![
                Keyframe {
                    frame: 0,
                    value: -1.,
                },
                Keyframe {
                    frame: 4,
                    value: 1.,
                },
            ],
        )],
    }
}

#[test]
fn loop_and_restart_initialize_at_the_new_clip_sample() {
    let m = model(0.75);
    let mut p = Played::new(&m);
    let clip = cut_clip();
    let dt = clip.timestep;
    p.player.set_clips(vec![clip]);
    assert!(p.player.play("turn"));
    p.tick(&m, 0.);
    for _ in 0..3 {
        p.tick(&m, dt);
    }
    assert!(p.response_trace(&nid("tip")).unwrap().lag.abs() > 0.001);
    p.tick(&m, dt);
    assert_eq!(p.player.sample().unwrap().frame, 1.);
    for name in ["upper", "tip"] {
        assert_eq!(p.response_trace(&nid(name)).unwrap().lag, 0.);
    }
    p.tick(&m, dt);
    assert!(p.player.play("turn"));
    p.tick(&m, 0.);
    assert_eq!(p.response_trace(&nid("tip")).unwrap().lag, 0.);
    p.tick(&m, dt);
    p.player.advance(&mut p.puppet, -dt);
    p.tick(&m, 0.);
    assert_eq!(p.response_trace(&nid("upper")).unwrap().lag, 0.);
}

/// [`cut_clip`] with its lane on the control `head_roll`, which the model
/// maps onto `roll`.
fn control_cut() -> (Model, Clip) {
    let mut m = model(0.75);
    m.extension_or_default::<Humanoid>().unwrap().set(
        "head_roll",
        ControlMapping {
            param: pid("roll"),
            stops: [-1., 0., 1.],
        },
    );
    let mut clip = cut_clip();
    clip.lanes[0].target = LaneTarget::Control("head_roll".into());
    (m, clip)
}

#[test]
fn a_control_lanes_loop_cut_and_restart_initialize_at_the_new_sample() {
    let (m, clip) = control_cut();
    let mut p = Played::new(&m);
    let dt = clip.timestep;
    p.player.set_clips(vec![clip]);
    assert!(p.player.play("turn"));
    p.tick(&m, 0.);
    for _ in 0..3 {
        p.tick(&m, dt);
    }
    assert_eq!(p.param_value_posed(&pid("roll")), Some(0.5));
    assert!(p.response_trace(&nid("tip")).unwrap().lag.abs() > 0.001);
    p.tick(&m, dt);
    assert_eq!(p.player.sample().unwrap().frame, 1.);
    for name in ["upper", "tip"] {
        assert_eq!(p.response_trace(&nid(name)).unwrap().lag, 0., "{name}");
    }
    p.tick(&m, dt);
    assert!(p.player.play("turn"));
    p.tick(&m, 0.);
    assert_eq!(p.response_trace(&nid("tip")).unwrap().lag, 0.);
    p.tick(&m, dt);
    p.player
        .advance_with(&mut p.puppet, -dt, Rig::of(&m), &LiveInputs::default());
    p.tick(&m, 0.);
    assert_eq!(p.response_trace(&nid("upper")).unwrap().lag, 0.);
}

#[test]
fn a_control_lane_without_a_cut_keeps_the_response_moving() {
    let (m, mut clip) = control_cut();
    // A closed loop: the wrap is no discontinuity.
    let lane = &clip.lanes[0];
    clip.lanes[0] = Lane::new(
        lane.target.clone(),
        lane.interpolation,
        vec![
            Keyframe {
                frame: 1,
                value: -1.,
            },
            Keyframe {
                frame: 2,
                value: 0.,
            },
            Keyframe {
                frame: 3,
                value: -1.,
            },
        ],
    );
    let mut p = Played::new(&m);
    let dt = clip.timestep;
    p.player.set_clips(vec![clip]);
    p.player.play("turn");
    p.tick(&m, 0.);
    for _ in 0..3 {
        p.tick(&m, dt);
    }
    p.tick(&m, dt);
    assert_eq!(p.player.sample().unwrap().frame, 1.);
    assert!(p.response_trace(&nid("upper")).unwrap().lag.abs() > 0.001);
}

#[test]
fn rebaking_on_a_loop_cut_resets_slot_input_and_its_follower() {
    let (mut m, _) = slot_model();
    let key = BindingKey::new(
        pid("roll"),
        nid("art"),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    m.set_binding_key(&key, [0, 0], -100.).unwrap();
    m.set_binding_key(&key, [1, 0], 100.).unwrap();
    let mut p = Played::new(&m);
    let clip = cut_clip();
    let dt = clip.timestep;
    p.player.set_clips(vec![clip]);
    p.player.play("turn");
    p.tick(&m, 0.);
    for _ in 0..3 {
        p.tick(&m, dt);
    }
    assert!(p.response_trace(&nid("tip")).unwrap().lag.abs() > 0.001);
    m.update_node(&nid("art"), |n| n.name = Name::truncated("renamed"))
        .unwrap();
    p.tick(&m, dt);
    assert_eq!(p.player.sample().unwrap().frame, 1.);
    for name in ["upper", "tip"] {
        assert_eq!(p.response_trace(&nid(name)).unwrap().lag, 0.);
    }
}

#[test]
fn a_cut_preserves_slot_velocity_when_another_response_moves_the_rendered_vertex() {
    let (mut m, slot) = slot_model();
    let root = m.root().unwrap().clone();
    let other = nid("other_art");
    let mut art = m.node(&nid("art")).unwrap().clone();
    art.name = Name::truncated("other");
    m.add_node_with_id(other.clone(), &root, art).unwrap();
    m.add_param_with_id(
        pid("slide"),
        ModelParam::new(Name::truncated("slide"), -1., 1., 0.),
    )
    .unwrap();
    for (param, part) in [(pid("roll"), nid("art")), (pid("slide"), other.clone())] {
        let key = BindingKey::new(param, part, BindingTarget::Scalar(ScalarTarget::Tx));
        m.set_binding_key(&key, [0, 0], -100.).unwrap();
        m.set_binding_key(&key, [1, 0], 100.).unwrap();
    }
    let driven = BindingKey::new(
        pid("broad"),
        other.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    m.set_binding_key(&driven, [0, 0], -100.).unwrap();
    m.set_binding_key(&driven, [2, 0], 100.).unwrap();
    let vertex = m.slot(&other, &slot).unwrap().vertex().unwrap() as usize;
    let second = nid("other_lock");
    m.add_node_with_id(
        second.clone(),
        &root,
        ModelNode::new(
            "Other lock",
            ModelNodeKind::MotionResponse(MotionResponse {
                inputs: vec![ResponseInput {
                    gain: 1.,
                    source: ResponseSource::PartSlot {
                        part: other.clone(),
                        slot,
                        angle_deg: 0.,
                        reference_length: 100.,
                    },
                }],
                ..Default::default()
            }),
        ),
    )
    .unwrap();
    // The slot reader precedes the response that drives its rendered vertex.
    m.reorder(&second, 0).unwrap();
    let rendered_x = |p: &Played| {
        catchlight_core::geometry::EvaluatedGeometry::new(&m, &p.puppet)
            .unwrap()
            .mesh(&other)
            .unwrap()
            .vertex(vertex)
            .unwrap()
            .world
            .x
    };
    let mut playing = Played::new(&m);
    let mut reference = Played::new(&m);
    let clip = cut_clip();
    let dt = clip.timestep;
    playing.player.set_clips(vec![clip]);
    playing.player.play("turn");
    for i in 0..=5 {
        let step = if i == 0 { 0. } else { dt };
        for p in [&mut playing, &mut reference] {
            p.set_param_value(&pid("slide"), i as f32 / 5.);
            p.tick(&m, step);
        }
        assert_eq!(
            playing.response_inputs(&second).unwrap()[0].position,
            reference.response_inputs(&second).unwrap()[0].position,
        );
        let velocity = playing.response_trace(&second).unwrap().velocity;
        assert_eq!(
            velocity,
            reference.response_trace(&second).unwrap().velocity
        );
        if i == 3 {
            assert!((rendered_x(&playing) - rendered_x(&reference)).abs() > 1.);
        }
        if i == 4 {
            assert_eq!(playing.player.sample().unwrap().frame, 1.);
            assert_eq!(playing.response_trace(&nid("upper")).unwrap().lag, 0.);
            assert_eq!(rendered_x(&playing), rendered_x(&reference));
            assert!(velocity.abs() > 0.01);
        }
    }
}

#[test]
fn playback_started_after_an_edit_uses_the_new_slot_dependencies() {
    let (mut m, _) = slot_model();
    let mut p = Played::new(&m);
    let clip = cut_clip();
    let dt = clip.timestep;
    p.player.set_clips(vec![clip]);
    p.tick(&m, 0.);
    let art = p.node_idx(&nid("art")).unwrap();
    p.set_scratch_transform(
        art,
        catchlight_core::ScratchTransform {
            translation: Some(catchlight_core::Vec3::new(50., 0., 0.)),
            ..Default::default()
        },
    );
    p.tick(&m, dt);
    assert!(p.response_trace(&nid("upper")).unwrap().velocity.abs() > 0.01);

    let key = BindingKey::new(
        pid("roll"),
        nid("art"),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    m.set_binding_key(&key, [0, 0], -100.).unwrap();
    p.player.play("turn");
    p.tick(&m, 0.);
    assert_eq!(p.response_trace(&nid("upper")).unwrap().lag, 0.);
}

#[test]
fn seamless_playback_preserves_the_same_cascade_as_uninterrupted_input() {
    let m = model(0.7);
    let dt = 1. / 60.;
    let values = [0., 1., 0.5, 0.];
    for clock in [
        catchlight_core::ResponseClock::InputIntervals,
        catchlight_core::ResponseClock::Fixed60,
    ] {
        let mut playing = Played::new(&m);
        let mut direct = Played::new(&m);
        playing.set_response_clock(clock);
        direct.set_response_clock(clock);
        playing.player.set_clips(vec![Clip {
            name: "turn".into(),
            timestep: dt,
            length: 4,
            lead_in: None,
            lead_out: None,
            lanes: vec![Lane::new(
                LaneTarget::Param(pid("roll")),
                catchlight_core::InterpolateMode::Linear,
                values
                    .iter()
                    .enumerate()
                    .map(|(i, &value)| Keyframe {
                        frame: i as u32,
                        value,
                    })
                    .collect(),
            )],
        }]);
        assert!(playing.player.play("turn"));
        for i in 0..16 {
            let step = if i == 0 { 0. } else { dt };
            direct.set_param_value(&pid("roll"), values[i % 4]);
            direct.tick(&m, step);
            playing.tick(&m, step);
            for id in ["upper", "tip"] {
                assert_eq!(
                    playing.response_trace(&nid(id)).unwrap().lag,
                    direct.response_trace(&nid(id)).unwrap().lag,
                    "frame {i}"
                );
            }
        }
        let before = playing.response_trace(&nid("tip")).unwrap().lag;
        assert!(before.abs() > 0.001);
        assert!(playing.player.play("turn"));
        playing.tick(&m, 0.);
        direct.tick(&m, 0.);
        assert_eq!(
            playing.response_trace(&nid("tip")).unwrap().lag,
            direct.response_trace(&nid("tip")).unwrap().lag
        );
    }
}

#[test]
fn sampled_sine_carries_lag_across_its_fastest_step() {
    for (count, fps) in [(32, 60.), (6, 12.)] {
        check_sine(count, fps);
    }
}

fn check_sine(count: u32, fps: f32) {
    let m = model(0.75);
    let dt = 1. / fps;
    let value = |i: u32| ((i as f32 + 0.5) * std::f32::consts::TAU / count as f32).sin();
    let mut p = Played::new(&m);
    let mut reference = Played::new(&m);
    p.player.set_clips(vec![Clip {
        name: "sine".into(),
        timestep: dt,
        length: count,
        lead_in: None,
        lead_out: None,
        lanes: vec![Lane::new(
            LaneTarget::Param(pid("roll")),
            catchlight_core::InterpolateMode::Linear,
            (0..count)
                .map(|frame| Keyframe {
                    frame,
                    value: value(frame),
                })
                .collect(),
        )],
    }]);
    p.player.play("sine");
    for i in 0..=2 * count {
        let step = if i == 0 { 0. } else { dt };
        reference.set_param_value(&pid("roll"), value(i % count));
        reference.tick(&m, step);
        p.tick(&m, step);
        assert!(
            (p.response_trace(&nid("upper")).unwrap().lag
                - reference.response_trace(&nid("upper")).unwrap().lag)
                .abs()
                < 1e-5,
            "frame {i}"
        );
    }
    p.set_param_value(&pid("roll"), value(0).next_up());
    p.player.play("sine");
    p.tick(&m, 0.);
    assert!(
        (p.response_trace(&nid("upper")).unwrap().lag
            - reference.response_trace(&nid("upper")).unwrap().lag)
            .abs()
            < 1e-5
    );
}
