#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Live inputs and clips on a replica's puppet.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    BindingKey, BindingTarget, InterpolateMode, Model, ModelNode, ModelNodeKind, ModelParam,
    ModelPart, Name, NodeId, ParamId, Puppet, ScalarTarget,
};
use catchlight_editor_wasm::{Playing, ReplicaState};
use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane, LaneTarget};
use catchlight_extension_expressions::{Expression, Expressions, Setting};
use catchlight_extension_humanoid::{ControlMapping, Humanoid};

const DT_MS: f64 = 1000.0 / 60.0;

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn mapping(param: &str) -> ControlMapping {
    ControlMapping {
        param: pid(param),
        stops: [-1.0, 0.0, 1.0],
    }
}

/// A ramp on `wave` from 0 to 1 over ten frames of 0.1 s.
fn ramp(name: &str) -> Clip {
    Clip {
        timestep: 0.1,
        length: 11,
        lanes: vec![Lane::new(
            LaneTarget::Param(pid("wave")),
            InterpolateMode::Linear,
            vec![
                Keyframe {
                    frame: 0,
                    value: 0.0,
                },
                Keyframe {
                    frame: 10,
                    value: 1.0,
                },
            ],
        )],
        ..Clip::new(name)
    }
}

/// Params `jaw` (0..1), `brow` and `lid` (-1..1), `blush` and `wave`
/// (0..1), all resting at 0, with `wave` moving the drawn part `hat`.
/// `mouth_open` maps onto `jaw`, and `brow_left_height` and
/// `brow_right_height` both onto `brow`, as the identity. The expression
/// `happy` sets `blush`; the viseme `aa` opens the mouth fully. One clip,
/// `ramp`.
fn rig() -> Model {
    let mut model = Model::new();
    for (p, min) in [
        ("jaw", 0.0),
        ("brow", -1.0),
        ("lid", -1.0),
        ("blush", 0.0),
        ("wave", 0.0),
    ] {
        model
            .add_param_with_id(pid(p), ModelParam::new(Name::truncated(p), min, 1.0, 0.0))
            .unwrap();
    }
    let root = model.root().unwrap().clone();
    let hat = NodeId::new("hat").unwrap();
    model
        .add_node_with_id(
            hat.clone(),
            &root,
            ModelNode::new(
                "hat",
                ModelNodeKind::Part(ModelPart::new(ClmMesh {
                    verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0],
                    uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0],
                    indices: ClmIndices::U16(vec![0, 1, 2]),
                    origin: [0.0, 0.0],
                })),
            ),
        )
        .unwrap();
    let key = BindingKey::new(pid("wave"), hat, BindingTarget::Scalar(ScalarTarget::Tx));
    model
        .add_binding_with_positions(&key, vec![vec![0.0, 1.0]])
        .unwrap();
    model.set_binding_key(&key, [1, 0], 4.0).unwrap();
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set("mouth_open", mapping("jaw"));
    humanoid.set("brow_left_height", mapping("brow"));
    humanoid.set("brow_right_height", mapping("brow"));
    humanoid.set("eye_left_open", mapping("lid"));
    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_groups(
        [Expression::new(
            "happy",
            vec![Setting::param(pid("blush"), 1.0)],
        )],
        [Expression::new(
            "aa",
            vec![Setting::control("mouth_open", 1.0)],
        )],
    );
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([ramp("ramp")]);
    model
}

fn replica_of(model: &Model) -> ReplicaState {
    let mut replica = ReplicaState::new();
    feed(&mut replica, model);
    replica
}

/// A newer revision of `model`, with its extension bytes, as the feed
/// delivers after an edit.
fn feed(replica: &mut ReplicaState, model: &Model) {
    let file = model.to_clm_file().unwrap();
    for blob in file.extensions {
        replica.put_extension(blob.key.as_str(), blob.data);
    }
    let rev = replica.rev() + 1;
    assert!(replica
        .apply_structure(&model.to_structure_bytes().unwrap(), rev)
        .unwrap());
}

fn value(replica: &ReplicaState, param: &str) -> f32 {
    let (_, puppet) = replica.model_and_puppet();
    puppet.param_value_posed(&pid(param)).unwrap_or(0.0)
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn a_control_value_moves_its_param_through_the_mapping_until_cleared() {
    let mut replica = replica_of(&rig());
    replica.set_param("jaw", 0.2);
    replica.set_control("mouth_open", Some(0.7));
    assert_eq!(value(&replica, "jaw"), 0.7, "applied before the next frame");
    replica.frame(0.0);
    replica.frame(DT_MS);
    assert_eq!(value(&replica, "jaw"), 0.7);
    assert_eq!(replica.inputs().controls.get("mouth_open"), Some(&0.7));

    replica.set_control("mouth_open", None);
    assert_eq!(value(&replica, "jaw"), 0.2);
    assert!(replica.inputs().controls.is_empty());
}

#[test]
fn expression_and_viseme_weights_blend_and_clear_back() {
    let mut replica = replica_of(&rig());
    replica.set_param("blush", 0.2);
    replica.set_expression_weight("happy", Some(0.5));
    assert!(close(value(&replica, "blush"), 0.6), "0.2 halfway to 1");
    replica.set_viseme_weight("aa", Some(0.5));
    assert!(
        close(value(&replica, "jaw"), 0.5),
        "the mouth control from 0"
    );

    replica.set_expression_weight("happy", None);
    replica.set_viseme_weight("aa", None);
    assert_eq!(value(&replica, "blush"), 0.2);
    assert_eq!(value(&replica, "jaw"), 0.0);
}

#[test]
fn a_weight_held_below_one_gives_the_same_value_every_frame() {
    let mut replica = replica_of(&rig());
    replica.set_expression_weight("happy", Some(0.5));
    replica.set_viseme_weight("aa", Some(0.25));
    for step in 0..30 {
        replica.frame(f64::from(step) * DT_MS);
        assert_eq!(value(&replica, "blush"), 0.5);
        assert_eq!(value(&replica, "jaw"), 0.25);
    }
}

#[test]
fn setting_a_param_clears_only_the_controls_mapped_onto_it() {
    let mut replica = replica_of(&rig());
    replica.set_control("brow_left_height", Some(0.5));
    replica.set_control("brow_right_height", Some(0.5));
    replica.set_control("eye_left_open", Some(-0.5));
    replica.set_control("mouth_open", Some(0.5));

    assert!(replica.set_param("brow", -0.25));
    let controls = replica.inputs().controls;
    assert_eq!(
        controls.keys().map(String::as_str).collect::<Vec<_>>(),
        ["eye_left_open", "mouth_open"],
    );
    replica.frame(0.0);
    assert_eq!(value(&replica, "brow"), -0.25, "the slider holds");
    assert_eq!(value(&replica, "lid"), -0.5);
}

#[test]
fn a_playing_clip_keeps_the_viewport_drawing_until_stopped() {
    let mut replica = replica_of(&rig());
    assert!(!replica.frame(0.0), "at rest");
    assert!(!replica.play("nope"));
    assert!(replica.play("ramp"));
    assert_eq!(
        replica.playing(),
        Some(Playing {
            name: "ramp".into(),
            time: 0.0,
            frame: 0.0,
        })
    );
    assert!(replica.frame(100.0));
    assert!(replica.frame(200.0));
    assert!(close(value(&replica, "wave"), 0.2));
    let playing = replica.playing().unwrap();
    assert!(close(playing.time, 0.2) && close(playing.frame, 2.0));

    replica.stop();
    assert_eq!(replica.playing(), None);
    assert!(!replica.frame(300.0), "idle again");
}

#[test]
fn a_model_change_keeps_a_clip_that_still_exists_and_stops_one_that_does_not() {
    let mut model = rig();
    let mut replica = replica_of(&model);
    replica.play("ramp");
    replica.frame(0.0);
    replica.frame(100.0);
    replica.frame(200.0);

    model
        .extension_or_default::<Animations>()
        .unwrap()
        .insert(ramp("other"));
    feed(&mut replica, &model);
    let playing = replica.playing().unwrap();
    assert_eq!(playing.name, "ramp");
    assert!(close(playing.frame, 2.0), "from where it was");
    replica.frame(300.0);
    assert!(close(replica.playing().unwrap().frame, 3.0));

    let animations = model.extension_or_default::<Animations>().unwrap();
    animations.remove("ramp");
    animations.insert(ramp("renamed"));
    feed(&mut replica, &model);
    assert_eq!(replica.playing(), None);
    assert!(!replica.frame(400.0));
}

#[test]
fn a_model_change_keeps_the_inputs() {
    let mut model = rig();
    let mut replica = replica_of(&model);
    replica.set_expression_weight("happy", Some(0.5));
    model
        .extension_or_default::<Animations>()
        .unwrap()
        .remove("ramp");
    feed(&mut replica, &model);
    assert_eq!(replica.inputs().expressions.get("happy"), Some(&0.5));
    replica.frame(0.0);
    assert_eq!(value(&replica, "blush"), 0.5);
}

#[test]
fn editing_stops_the_clip_and_keeps_the_inputs() {
    let mut replica = replica_of(&rig());
    replica.set_control("mouth_open", Some(0.4));
    replica.play("ramp");
    replica.set_editing(true);
    assert_eq!(replica.playing(), None);
    assert_eq!(replica.inputs().controls.get("mouth_open"), Some(&0.4));
    assert_eq!(value(&replica, "jaw"), 0.4);
}

#[test]
fn clearing_all_inputs_returns_every_param() {
    let mut replica = replica_of(&rig());
    replica.set_control("eye_left_open", Some(0.5));
    replica.set_expression_weight("happy", Some(1.0));
    replica.set_viseme_weight("aa", Some(1.0));
    replica.clear_inputs();
    assert_eq!(replica.inputs(), Default::default());
    for param in ["lid", "blush", "jaw"] {
        assert_eq!(value(&replica, param), 0.0, "{param}");
    }
}

#[test]
fn with_no_inputs_and_no_clip_frames_are_the_puppets_own() {
    let model = rig();
    let mut replica = replica_of(&model);
    let mut puppet = Puppet::new(replica.model());
    replica.set_param("wave", 0.5);
    puppet.set_param_value(&pid("wave"), 0.5);
    for step in 0..5 {
        replica.frame(f64::from(step) * DT_MS);
        let dt = if step == 0 {
            0.0
        } else {
            (DT_MS / 1000.0) as f32
        };
        puppet.tick(replica.model(), dt);
    }
    let (_, posed) = replica.model_and_puppet();
    let hat = NodeId::new("hat").unwrap();
    let idx = posed.node_idx(&hat).unwrap();
    assert_eq!(
        posed.transforms().get(idx),
        puppet.transforms().get(puppet.node_idx(&hat).unwrap()),
    );
    for param in ["jaw", "brow", "lid", "blush", "wave"] {
        assert_eq!(
            posed.param_value_posed(&pid(param)),
            puppet.param_value_posed(&pid(param)),
            "{param}",
        );
    }
}
