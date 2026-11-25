#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Lanes on controls, expressions and visemes: their storage, their check,
//! and one frame applied in the shared order with the host's inputs.

use catchlight_core::extension::{register, Extension, IdRef, Payload, RawExtension};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    InterpolateMode, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart, Name, NodeId, ParamId,
    Puppet,
};
use catchlight_extension_animations::{
    AnimationPlayer, Animations, Clip, FrameInputs, Keyframe, Lane, LaneTarget, Rig,
};
use catchlight_extension_expressions::{Expression, Expressions, Setting};
use catchlight_extension_humanoid::{ControlMapping, Humanoid};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn lane(target: LaneTarget, keys: &[(u32, f32)]) -> Lane {
    Lane::new(
        target,
        InterpolateMode::Linear,
        keys.iter()
            .map(|&(frame, value)| Keyframe { frame, value })
            .collect(),
    )
}

fn control(name: &str) -> LaneTarget {
    LaneTarget::Control(name.into())
}

fn expression(name: &str) -> LaneTarget {
    LaneTarget::Expression(name.into())
}

fn viseme(name: &str) -> LaneTarget {
    LaneTarget::Viseme(name.into())
}

/// Params `jaw` (0..1), `brow` (-1..1) and `blush` (0..1), all resting at 0.
/// `mouth_open` maps onto `jaw` and `brow_left_height` onto `brow`, each as
/// the identity. The expression `happy` raises the brow and sets `blush`;
/// the viseme `aa` opens the mouth fully and `oh` halfway.
fn rig() -> Model {
    let mut model = Model::new();
    for (p, min) in [("jaw", 0.0), ("brow", -1.0), ("blush", 0.0)] {
        model
            .add_param_with_id(pid(p), ModelParam::new(Name::truncated(p), min, 1.0, 0.0))
            .unwrap();
    }
    let root = model.root().unwrap().clone();
    model
        .add_node_with_id(
            NodeId::new("hat").unwrap(),
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
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set(
        "mouth_open",
        ControlMapping {
            param: pid("jaw"),
            stops: [-1.0, 0.0, 1.0],
        },
    );
    humanoid.set(
        "brow_left_height",
        ControlMapping {
            param: pid("brow"),
            stops: [-1.0, 0.0, 1.0],
        },
    );
    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_groups(
        [Expression::new(
            "happy",
            vec![
                Setting::control("brow_left_height", 1.0),
                Setting::param(pid("blush"), 1.0),
            ],
        )],
        [
            Expression::new("aa", vec![Setting::control("mouth_open", 1.0)]),
            Expression::new("oh", vec![Setting::control("mouth_open", 0.5)]),
        ],
    );
    model
}

/// Eleven frames: the brow control rises from -1 to 1 while `happy` fades in
/// and `aa` cross-fades to `oh`.
fn talk() -> Clip {
    Clip {
        length: 11,
        lanes: vec![
            lane(control("brow_left_height"), &[(0, -1.0), (10, 1.0)]),
            lane(expression("happy"), &[(0, 0.0), (10, 1.0)]),
            lane(viseme("aa"), &[(0, 1.0), (10, 0.0)]),
            lane(viseme("oh"), &[(0, 0.0), (10, 1.0)]),
        ],
        ..Clip::new("talk")
    }
}

struct Host<'m> {
    model: &'m Model,
    puppet: Puppet,
    player: AnimationPlayer,
    inputs: FrameInputs,
}

impl<'m> Host<'m> {
    fn new(model: &'m Model) -> Self {
        Self {
            model,
            puppet: Puppet::new(model),
            player: AnimationPlayer::new(vec![talk()]),
            inputs: FrameInputs::default(),
        }
    }

    fn frame(&mut self, dt: f32) {
        self.player
            .advance_with(&mut self.puppet, dt, Rig::of(self.model), &self.inputs);
        self.puppet.tick(self.model, dt);
    }

    fn value(&self, param: &str) -> f32 {
        self.puppet.param_value_posed(&pid(param)).unwrap_or(0.0)
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn a_clip_poses_through_controls_expressions_and_visemes() {
    let model = rig();
    let mut host = Host::new(&model);
    assert!(host.player.play("talk"));
    host.frame(0.0);
    let dt = talk().timestep;
    let mut at = 0;
    for frame in [0, 4, 7, 10] {
        while at < frame {
            host.frame(dt);
            at += 1;
        }
        let t = frame as f32 / 10.0;
        let lane_brow = -1.0 + 2.0 * t;
        // `happy` at weight t pulls the lane's brow toward 1 and blush from
        // rest toward 1; `aa` and `oh` weigh 1 in total, so the mouth is
        // exactly their mix.
        let want = [
            ("brow", lane_brow + (1.0 - lane_brow) * t),
            ("blush", t),
            ("jaw", (1.0 - t) * 1.0 + t * 0.5),
        ];
        for (param, want) in want {
            let got = host.value(param);
            assert!(close(got, want), "frame {frame}: {param} {got} != {want}");
        }
    }
}

#[test]
fn a_lane_wins_over_the_hosts_input_of_its_name_until_the_clip_stops() {
    let model = rig();
    let mut host = Host::new(&model);
    host.inputs.controls.set("brow_left_height", 0.5);
    host.inputs.visemes.set("oh", 1.0);
    host.player.play("talk");
    host.frame(0.0);
    assert_eq!(host.value("brow"), -1.0, "the control lane");
    assert_eq!(host.value("jaw"), 1.0, "the viseme lanes: aa 1, oh 0");
    host.player.stop();
    host.frame(1.0 / 60.0);
    assert_eq!(host.value("brow"), 0.5);
    assert_eq!(host.value("jaw"), 0.5);

    let mut host = Host::new(&model);
    host.inputs.expressions.set("happy", 1.0);
    host.player.play("talk");
    host.frame(0.0);
    assert_eq!(
        host.value("blush"),
        0.0,
        "the expression lane's weight is 0"
    );
    host.player.stop();
    host.frame(1.0 / 60.0);
    assert_eq!(host.value("blush"), 1.0);
}

#[test]
fn a_held_weight_below_one_gives_the_same_value_every_frame() {
    let model = rig();
    let mut host = Host::new(&model);
    host.inputs.expressions.set("happy", 0.5);
    for _ in 0..20 {
        host.frame(1.0 / 60.0);
        assert_eq!(host.value("blush"), 0.5);
    }
}

#[test]
fn a_param_returns_to_its_base_once_no_weighted_setting_sets_it() {
    let model = rig();
    let mut host = Host::new(&model);
    host.puppet.set_param_value(&pid("blush"), 0.2);
    host.inputs.expressions.set("happy", 0.5);
    for _ in 0..5 {
        host.frame(1.0 / 60.0);
        assert!(close(host.value("blush"), 0.6));
    }
    host.inputs.expressions.clear();
    host.frame(1.0 / 60.0);
    assert_eq!(host.value("blush"), 0.2);
    host.frame(1.0 / 60.0);
    assert_eq!(host.value("blush"), 0.2);
}

#[test]
fn a_param_the_host_moves_between_frames_becomes_the_new_base() {
    let model = rig();
    let mut host = Host::new(&model);
    host.inputs.expressions.set("happy", 0.5);
    for _ in 0..3 {
        host.frame(1.0 / 60.0);
    }
    assert_eq!(host.value("blush"), 0.5);
    host.puppet.set_param_value(&pid("blush"), 0.8);
    for _ in 0..3 {
        host.frame(1.0 / 60.0);
        assert!(close(host.value("blush"), 0.9));
    }
    host.inputs.expressions.clear();
    host.frame(1.0 / 60.0);
    assert_eq!(host.value("blush"), 0.8);
}

#[test]
fn a_param_returns_to_its_base_once_no_control_maps_onto_it() {
    let model = rig();
    let mut host = Host::new(&model);
    host.puppet.set_param_value(&pid("jaw"), 0.2);
    host.inputs.controls.set("mouth_open", 0.7);
    for _ in 0..3 {
        host.frame(1.0 / 60.0);
        assert_eq!(host.value("jaw"), 0.7);
    }
    host.inputs.controls.clear();
    host.frame(1.0 / 60.0);
    assert_eq!(host.value("jaw"), 0.2);

    host.inputs.visemes.set("aa", 0.5);
    for _ in 0..3 {
        host.frame(1.0 / 60.0);
        assert!(
            close(host.value("jaw"), 0.5),
            "a control setting counts from 0"
        );
    }
    host.inputs.visemes.clear();
    host.frame(1.0 / 60.0);
    assert_eq!(host.value("jaw"), 0.2);
}

#[test]
fn with_no_clip_the_host_inputs_still_apply() {
    let model = rig();
    let mut host = Host::new(&model);
    host.inputs.controls.set("mouth_open", 0.25);
    host.inputs.visemes.set("aa", 0.5);
    assert!(host
        .player
        .advance_with(&mut host.puppet, 0.0, Rig::of(&model), &host.inputs));
    assert_eq!(host.value("jaw"), 0.25 + (1.0 - 0.25) * 0.5);
    assert!(
        !host
            .player
            .advance_with(&mut host.puppet, 0.0, Rig::of(&model), &host.inputs),
        "nothing moved"
    );
}

#[test]
fn each_lane_target_round_trips_through_a_file_and_names_by_string() {
    let mut model = rig();
    let clip = Clip {
        lanes: vec![
            lane(LaneTarget::Param(pid("blush")), &[(0, 0.5)]),
            lane(control("mouth_open"), &[(0, 1.0), (3, 0.0)]),
            lane(expression("happy"), &[(2, 1.0)]),
            lane(viseme("aa"), &[(1, f32::from_bits(1))]),
        ],
        ..talk()
    };
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([clip.clone()]);
    assert_eq!(
        model.extension::<Animations>().unwrap().refs(),
        [IdRef::Param(pid("blush"))],
        "only the param lane uses the table"
    );
    let reopened = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let back = reopened
        .extension::<Animations>()
        .unwrap()
        .clip("talk")
        .unwrap();
    assert_eq!(back, clip);

    let json = serde_json::to_value(&clip.lanes).unwrap();
    assert_eq!(json[0]["param"], "blush");
    assert_eq!(json[1]["control"], "mouth_open");
    assert_eq!(json[2]["expression"], "happy");
    assert_eq!(json[3]["viseme"], "aa");
    assert!(json[3].get("param").is_none());
    let lanes: Vec<Lane> = serde_json::from_value(json).unwrap();
    assert_eq!(lanes, clip.lanes);
    for bad in [
        serde_json::json!({"interpolation": "Linear"}),
        serde_json::json!({"param": "blush", "viseme": "aa", "interpolation": "Linear"}),
    ] {
        let error = serde_json::from_value::<Lane>(bad).unwrap_err();
        assert!(error.to_string().contains("exactly one"), "{error}");
    }

    // Deleting a param drops only its lane; reconcile keeps the named ones.
    model.delete_param(&pid("blush")).unwrap();
    let kept = model
        .extension::<Animations>()
        .unwrap()
        .clip("talk")
        .unwrap();
    assert_eq!(kept.lanes, clip.lanes[1..]);
}

#[test]
fn a_stored_lane_with_two_targets_or_none_does_not_decode() {
    use ciborium::Value;
    let text = |s: &str| Value::Text(s.into());
    for targets in [
        vec![
            (text("param"), Value::Integer(0.into())),
            (text("control"), text("mouth_open")),
        ],
        vec![],
    ] {
        let mut lane = targets;
        lane.extend([
            (text("interpolation"), text("Linear")),
            (text("frames"), Value::Array(vec![])),
            (text("values"), Value::Bytes(vec![])),
        ]);
        let clip = Value::Map(vec![
            (text("name"), text("c")),
            (text("timestep"), Value::Float(0.5)),
            (text("length"), Value::Integer(5.into())),
            (text("lead_in"), Value::Null),
            (text("lead_out"), Value::Null),
            (text("lanes"), Value::Array(vec![Value::Map(lane)])),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&Value::Array(vec![clip]), &mut bytes).unwrap();
        let raw = RawExtension {
            version: 1,
            refs: vec![IdRef::Param(pid("blush"))],
            payload: Payload::Bytes(bytes.into()),
        };
        assert!(Animations::decode(raw).is_err());
    }
}

fn codes(model: &Model) -> Vec<String> {
    let mut codes: Vec<String> = model
        .extension_warnings()
        .into_iter()
        .filter(|w| w.key.as_str() == Animations::KEY)
        .map(|w| w.warning.code.into_owned())
        .collect();
    codes.sort();
    codes
}

fn strangers() -> Animations {
    Animations::from_clips([Clip {
        lanes: vec![
            lane(control("mouth_open"), &[(0, 1.0)]),
            lane(control("tongue_out"), &[(0, 1.0)]),
            lane(expression("happy"), &[(0, 1.0)]),
            lane(expression("aa"), &[(0, 1.0)]),
            lane(viseme("aa"), &[(0, 1.0)]),
            lane(viseme("happy"), &[(0, 1.0)]),
        ],
        ..Clip::new("c")
    }])
}

#[test]
fn check_warns_of_a_lane_on_a_name_the_model_lacks() {
    register::<Animations>().unwrap();
    let mut model = rig();
    *model.extension_or_default::<Animations>().unwrap() = strangers();
    assert_eq!(
        codes(&model),
        [
            "catchlight.animations/missing-expression",
            "catchlight.animations/missing-viseme",
            "catchlight.animations/unmapped-control",
        ]
    );

    // With neither extension, every named lane does nothing.
    let mut bare = Model::new();
    *bare.extension_or_default::<Animations>().unwrap() = strangers();
    assert_eq!(
        codes(&bare),
        [
            "catchlight.animations/missing-expression",
            "catchlight.animations/missing-expression",
            "catchlight.animations/missing-viseme",
            "catchlight.animations/missing-viseme",
            "catchlight.animations/unmapped-control",
            "catchlight.animations/unmapped-control",
        ]
    );
}

#[test]
fn a_fragments_named_lanes_are_left_to_the_base() {
    register::<Animations>().unwrap();
    let base = rig();
    let mut addon = base.extract(&[NodeId::new("hat").unwrap()]);
    assert!(addon.is_fragment());
    *addon.extension_or_default::<Animations>().unwrap() = strangers();
    assert!(codes(&addon).is_empty(), "{:?}", codes(&addon));
}
