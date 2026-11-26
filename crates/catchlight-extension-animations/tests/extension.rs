#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `catchlight.animations` as a maintained extension: its codec, the core's
//! upkeep of its reference table, its check, and addon merging.

use catchlight_core::extension::{register, Extension, IdRef, Payload};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    ExtensionValue, InstallError, InterpolateMode, Model, ModelNode, ModelNodeKind, ModelParam,
    ModelPart, MotionResponse, Name, NodeId, ParamId, ResponseInput, ResponseSource,
};
use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane, LaneTarget};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn rig() -> Model {
    let mut model = Model::new();
    for p in ["eye", "mouth", "brow"] {
        model
            .add_param_with_id(pid(p), ModelParam::new(Name::truncated(p), -1.0, 1.0, 0.0))
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
    model
}

fn lane(param: &str, keys: &[(u32, f32)]) -> Lane {
    Lane::new(
        LaneTarget::Param(pid(param)),
        InterpolateMode::Cubic,
        keys.iter()
            .map(|&(frame, value)| Keyframe { frame, value })
            .collect(),
    )
}

fn clip(name: &str, lanes: Vec<Lane>) -> Clip {
    Clip {
        lanes,
        length: 30,
        ..Clip::new(name)
    }
}

fn names(model: &Model) -> Vec<String> {
    model
        .extension::<Animations>()
        .unwrap()
        .names()
        .map(String::from)
        .collect()
}

#[test]
fn clips_round_trip_through_a_file_bit_for_bit_as_a_columnar_byte_payload() {
    let awkward = [
        f32::from_bits(0x7fc0_1234),
        f32::from_bits(1),
        -0.0,
        f32::MAX,
        0.1,
        f32::NEG_INFINITY,
    ];
    let mut model = rig();
    let blink = Clip {
        name: "Blink".into(),
        timestep: 1.0 / 24.0,
        length: 7,
        lead_in: Some(1),
        lead_out: None,
        lanes: vec![
            Lane::new(
                LaneTarget::Param(pid("eye")),
                InterpolateMode::Stepped,
                awkward
                    .iter()
                    .enumerate()
                    .map(|(i, &value)| Keyframe {
                        frame: i as u32,
                        value,
                    })
                    .collect(),
            ),
            lane("mouth", &[(0, 0.25), (u32::MAX, -0.5)]),
        ],
    };
    *model.extension_or_default::<Animations>().unwrap() =
        Animations::from_clips([blink.clone(), clip("Idle", vec![lane("eye", &[(3, 1.0)])])]);

    let Some(ExtensionValue::Maintained(raw)) = model.extensions().get(Animations::KEY) else {
        panic!("not maintained");
    };
    assert_eq!(raw.version, 1);
    assert_eq!(
        raw.refs,
        [IdRef::Param(pid("eye")), IdRef::Param(pid("mouth"))],
        "one table entry per param, shared between clips"
    );
    assert!(matches!(raw.payload, Payload::Bytes(_)));

    let reopened = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let back = reopened
        .extension::<Animations>()
        .unwrap()
        .clip("Blink")
        .unwrap();
    let bits = |c: &Clip| -> Vec<u32> {
        c.lanes
            .iter()
            .flat_map(|l| l.keyframes().iter().map(|k| k.value.to_bits()))
            .collect()
    };
    assert_eq!(bits(&back), bits(&blink));
    assert_eq!(back.lanes[1], blink.lanes[1]);
    assert_eq!(
        (back.name, back.lead_in, back.length),
        ("Blink".into(), Some(1), 7)
    );
    assert_eq!(names(&reopened), ["Blink", "Idle"]);
}

#[test]
fn insert_replaces_by_name_sorts_keyframes_and_keeps_the_table_compact() {
    let mut animations = Animations::default();
    assert!(animations
        .insert(clip("a", vec![lane("eye", &[(9, 1.0), (2, 0.0)])]))
        .is_none());
    assert_eq!(
        animations.clip("a").unwrap().lanes[0].keyframes()[0].frame,
        2
    );
    let old = animations
        .insert(clip("a", vec![lane("mouth", &[(0, 1.0)])]))
        .unwrap();
    assert_eq!(old.lanes[0].target, LaneTarget::Param(pid("eye")));
    assert_eq!(animations.refs(), [IdRef::Param(pid("mouth"))]);
    assert_eq!(animations.len(), 1);
    animations.remove("a").unwrap();
    assert!(animations.refs().is_empty());
}

#[test]
fn a_renamed_param_carries_its_lanes_and_a_deleted_one_drops_them() {
    let mut model = rig();
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([clip(
        "talk",
        vec![lane("eye", &[(0, 1.0)]), lane("mouth", &[(0, 1.0)])],
    )]);
    model.rename_param_id(&pid("mouth"), pid("jaw")).unwrap();
    let params = |model: &Model| -> Vec<ParamId> {
        model
            .extension::<Animations>()
            .unwrap()
            .clip("talk")
            .unwrap()
            .lanes
            .into_iter()
            .filter_map(|l| l.param().cloned())
            .collect()
    };
    assert_eq!(params(&model), [pid("eye"), pid("jaw")]);
    model.delete_param(&pid("eye")).unwrap();
    assert_eq!(params(&model), [pid("jaw")]);
    assert_eq!(
        model.extension::<Animations>().unwrap().refs(),
        [IdRef::Param(pid("jaw"))]
    );
}

#[test]
fn check_warns_of_driven_params_and_leaves_missing_ones_to_the_core() {
    register::<Animations>().unwrap();
    let mut model = rig();
    let root = model.root().unwrap().clone();
    model
        .add_node_with_id(
            NodeId::new("spring").unwrap(),
            &root,
            ModelNode::new(
                "spring",
                ModelNodeKind::MotionResponse(MotionResponse {
                    inputs: vec![ResponseInput {
                        source: ResponseSource::PoseParam { param: pid("eye") },
                        gain: 1.0,
                    }],
                    output_param: Some(pid("brow")),
                    ..MotionResponse::default()
                }),
            ),
        )
        .unwrap();
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([clip(
        "c",
        vec![lane("brow", &[(0, 1.0)]), lane("ghost", &[(0, 1.0)])],
    )]);
    let mut codes: Vec<String> = model
        .extension_warnings()
        .into_iter()
        .map(|w| w.warning.code.into_owned())
        .collect();
    codes.sort();
    assert_eq!(
        codes,
        [
            "catchlight.animations/driven-param",
            "catchlight/dangling-reference",
        ]
    );
}

#[test]
fn an_addons_clips_merge_on_install_refuse_a_taken_name_and_leave_on_uninstall() {
    register::<Animations>().unwrap();
    let mut base = rig();
    let mut addon = base.extract(&[NodeId::new("hat").unwrap()]);
    assert!(
        addon.extensions().is_empty(),
        "an extracted subtree leaves the clips behind"
    );
    base.delete_node(&NodeId::new("hat").unwrap()).unwrap();
    *base.extension_or_default::<Animations>().unwrap() =
        Animations::from_clips([clip("idle", vec![lane("eye", &[(0, 1.0)])])]);
    *addon.extension_or_default::<Animations>().unwrap() =
        Animations::from_clips([clip("tip", vec![lane("brow", &[(0, 1.0)])])]);
    assert!(addon
        .requirements()
        .iter()
        .any(|r| r.id == catchlight_core::Required::Param(pid("brow"))));

    let installed = base.install(&addon).unwrap();
    assert_eq!(names(&base), ["idle", "tip"]);
    base.extension_mut::<Animations>()
        .unwrap()
        .insert(clip("wave", vec![]));
    base.uninstall(&installed);
    assert_eq!(names(&base), ["idle", "wave"]);

    *addon.extension_mut::<Animations>().unwrap() = Animations::from_clips([clip("idle", vec![])]);
    assert!(matches!(
        base.install(&addon),
        Err(InstallError::ExtensionRefused { .. })
    ));
    assert_eq!(names(&base), ["idle", "wave"]);
}
