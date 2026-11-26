#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]
//! `catchlight.humanoid` as a maintained extension: its codec, the core's
//! upkeep of its reference table, its check, and posing a puppet.

use catchlight_core::extension::{Extension, IdRef, Payload, register};
use catchlight_core::{
    ExtensionValue, Model, ModelNode, ModelNodeKind, ModelParam, MotionResponse, Name, NodeId,
    ParamId, Puppet, ResponseInput, ResponseSource,
};
use catchlight_extension_humanoid::{CONTROLS, ControlMapping, ControlValues, Humanoid};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn map(param: &str, stops: [f32; 3]) -> ControlMapping {
    ControlMapping {
        param: pid(param),
        stops,
    }
}

/// Params `smile` (-1..1, default 0), `jaw` (0..1, default 0.2), `brow`
/// (-1..1), and `sway`, which a motion response driven by `brow` writes.
fn rig() -> Model {
    let mut model = Model::new();
    for (p, min, max, default) in [
        ("smile", -1.0, 1.0, 0.0),
        ("jaw", 0.0, 1.0, 0.2),
        ("brow", -1.0, 1.0, 0.0),
        ("sway", -1.0, 1.0, 0.0),
    ] {
        model
            .add_param_with_id(
                pid(p),
                ModelParam::new(Name::truncated(p), min, max, default),
            )
            .unwrap();
    }
    let root = model.root().unwrap().clone();
    model
        .add_node_with_id(
            NodeId::new("spring").unwrap(),
            &root,
            ModelNode::new(
                "spring",
                ModelNodeKind::MotionResponse(MotionResponse {
                    inputs: vec![ResponseInput {
                        source: ResponseSource::PoseParam { param: pid("brow") },
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

fn mappings(model: &Model) -> Vec<(String, ControlMapping)> {
    model
        .extension::<Humanoid>()
        .unwrap()
        .mappings()
        .map(|(name, m)| (name.to_owned(), m))
        .collect()
}

#[test]
fn the_standard_list_has_35_distinct_controls_23_of_them_two_sided() {
    let mut names: Vec<_> = CONTROLS.iter().map(|c| c.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), 35);
    assert_eq!(CONTROLS.iter().filter(|c| !c.one_sided).count(), 23);
    assert!(CONTROLS[..23].iter().all(|c| !c.one_sided));
}

#[test]
fn mappings_round_trip_exactly_through_a_file_as_a_json_payload() {
    let mut model = rig();
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set("mouth_smile", map("smile", [-1.0, 0.0, 1.0]));
    humanoid.set("mouth_open", map("jaw", [0.1, 0.2, 0.9]));
    humanoid.set("tail_wag", map("smile", [0.3, f32::from_bits(1), -0.7]));

    let Some(ExtensionValue::Maintained(raw)) = model.extensions().get(Humanoid::KEY) else {
        panic!("not maintained");
    };
    assert_eq!(raw.version, 1);
    assert_eq!(
        raw.refs,
        [IdRef::Param(pid("jaw")), IdRef::Param(pid("smile"))],
        "one table entry per param, in name order of first use"
    );
    let Payload::Json(json) = &raw.payload else {
        panic!("not JSON");
    };
    assert_eq!(
        json["controls"]["mouth_open"],
        serde_json::json!({"param": 0, "stops": [0.1f32, 0.2f32, 0.9f32]})
    );

    let decoded = Humanoid::decode(raw.clone()).unwrap();
    assert_eq!(&decoded, model.extension::<Humanoid>().unwrap());
    assert_eq!(decoded.encode(), *raw);

    let reopened = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    assert_eq!(mappings(&reopened), mappings(&model));
    assert_eq!(
        reopened
            .extension::<Humanoid>()
            .unwrap()
            .mapping("tail_wag")
            .unwrap()
            .stops[1]
            .to_bits(),
        1
    );
}

#[test]
fn a_non_finite_stop_survives_a_file_as_nan() {
    let mut model = rig();
    model
        .extension_or_default::<Humanoid>()
        .unwrap()
        .set("mouth_smile", map("smile", [f32::NAN, 0.0, 1.0]));
    let reopened = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let stops = reopened
        .extension::<Humanoid>()
        .unwrap()
        .mapping("mouth_smile")
        .unwrap()
        .stops;
    assert!(stops[0].is_nan());
    assert_eq!(stops[1..], [0.0, 1.0]);
}

#[test]
fn set_replaces_by_control_and_keeps_the_table_compact() {
    let mut humanoid = Humanoid::default();
    assert!(
        humanoid
            .set("mouth_open", map("jaw", [0.0, 0.0, 1.0]))
            .is_none()
    );
    let old = humanoid
        .set("mouth_open", map("smile", [0.0, 0.0, 1.0]))
        .unwrap();
    assert_eq!(old.param, pid("jaw"));
    assert_eq!(humanoid.refs(), [IdRef::Param(pid("smile"))]);
    humanoid.set("mouth_smile", map("smile", [-1.0, 0.0, 1.0]));
    assert_eq!(
        humanoid.controls_for(&pid("smile")).collect::<Vec<_>>(),
        ["mouth_open", "mouth_smile"]
    );
    humanoid.remove("mouth_open").unwrap();
    humanoid.remove("mouth_smile").unwrap();
    assert!(humanoid.refs().is_empty());
    assert!(humanoid.is_empty());
}

#[test]
fn a_renamed_param_carries_its_mapping_and_a_deleted_one_drops_it() {
    let mut model = rig();
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set("mouth_smile", map("smile", [-1.0, 0.0, 1.0]));
    humanoid.set("mouth_open", map("jaw", [0.0, 0.0, 1.0]));
    model.rename_param_id(&pid("smile"), pid("grin")).unwrap();
    assert_eq!(
        model
            .extension::<Humanoid>()
            .unwrap()
            .mapping("mouth_smile")
            .unwrap()
            .param,
        pid("grin")
    );
    model.delete_param(&pid("jaw")).unwrap();
    let humanoid = model.extension::<Humanoid>().unwrap();
    assert!(humanoid.mapping("mouth_open").is_none());
    assert_eq!(humanoid.len(), 1);
    assert_eq!(humanoid.refs(), [IdRef::Param(pid("grin"))]);
}

fn codes(model: &Model) -> Vec<String> {
    let mut codes: Vec<String> = model
        .extension_warnings()
        .into_iter()
        .map(|w| w.warning.code.into_owned())
        .collect();
    codes.sort();
    codes
}

#[test]
fn check_warns_of_shared_driven_nonstandard_and_non_finite_mappings() {
    register::<Humanoid>().unwrap();
    let mut model = rig();
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set("mouth_smile", map("smile", [-1.0, 0.0, 1.0]));
    humanoid.set("mouth_width", map("smile", [-1.0, 0.0, 1.0]));
    humanoid.set("head_yaw", map("sway", [-1.0, 0.0, 1.0]));
    humanoid.set("tail_wag", map("brow", [-1.0, 0.0, 1.0]));
    humanoid.set("mouth_open", map("jaw", [0.0, f32::INFINITY, 1.0]));
    assert_eq!(
        codes(&model),
        [
            "catchlight.humanoid/driven-param",
            "catchlight.humanoid/non-finite-stop",
            "catchlight.humanoid/nonstandard-control",
            "catchlight.humanoid/shared-param",
        ]
    );
}

#[test]
fn check_leaves_missing_params_to_the_core() {
    register::<Humanoid>().unwrap();
    let mut model = rig();
    model
        .extension_or_default::<Humanoid>()
        .unwrap()
        .set("mouth_open", map("ghost", [0.0, 0.0, 1.0]));
    assert_eq!(codes(&model), ["catchlight/dangling-reference"]);
}

fn puppet_with(model: &Model, humanoid: Humanoid) -> (Model, Puppet) {
    let mut model = model.clone();
    *model.extension_or_default::<Humanoid>().unwrap() = humanoid;
    let puppet = Puppet::new(&model);
    (model, puppet)
}

#[test]
fn pose_interpolates_between_stops_even_outside_the_range_or_descending() {
    let mut humanoid = Humanoid::default();
    // Beyond the param's -1..1, and descending.
    humanoid.set("mouth_smile", map("smile", [1.5, 0.5, -2.0]));
    // A 0 stop that is not jaw's default of 0.2.
    humanoid.set("mouth_open", map("jaw", [0.0, 0.6, 1.0]));
    let (_, mut puppet) = puppet_with(&rig(), humanoid.clone());
    let mut at = |smile: f32, open: f32| {
        let controls: ControlValues = [("mouth_smile", smile), ("mouth_open", open)]
            .into_iter()
            .collect();
        humanoid.pose(&controls, &mut puppet);
        (
            puppet.param_value_posed(&pid("smile")).unwrap(),
            puppet.param_value_posed(&pid("jaw")).unwrap(),
        )
    };
    assert_eq!(at(-1.0, 0.0), (1.5, 0.6));
    assert_eq!(at(-0.5, 0.5), (1.0, 0.8));
    assert_eq!(at(0.5, 1.0), (-0.75, 1.0));
    assert_eq!(at(1.0, 1.0), (-2.0, 1.0));
    // Inputs clamp to the control's range; mouth_open is one-sided.
    assert_eq!(at(7.0, -1.0), (-2.0, 0.6));
}

#[test]
fn an_unset_control_leaves_its_param_and_a_driven_param_is_skipped() {
    let mut humanoid = Humanoid::default();
    humanoid.set("mouth_smile", map("smile", [-1.0, 0.0, 1.0]));
    humanoid.set("mouth_open", map("jaw", [0.0, 0.0, 1.0]));
    humanoid.set("head_yaw", map("sway", [-1.0, 0.0, 1.0]));
    let (_, mut puppet) = puppet_with(&rig(), humanoid.clone());
    assert!(puppet.is_driven(&pid("sway")));
    puppet.set_param_value(&pid("jaw"), 0.33);

    let mut controls = ControlValues::new();
    controls.set("mouth_smile", 0.5);
    controls.set("head_yaw", 1.0);
    humanoid.pose(&controls, &mut puppet);
    assert_eq!(puppet.param_value_posed(&pid("smile")), Some(0.5));
    assert_eq!(puppet.param_value_posed(&pid("jaw")), Some(0.33));
    assert_eq!(puppet.param_value_posed(&pid("sway")), None);
}

#[test]
fn check_leaves_removed_params_to_the_core() {
    use catchlight_core::extension::{RawExtension, RemovedRef};

    let humanoid = Humanoid::decode(RawExtension {
        version: 1,
        refs: vec![IdRef::Removed(RemovedRef::Param(pid("jaw")))],
        payload: Payload::Json(serde_json::json!({
            "controls": {"mouth_open": {"param": 0, "stops": [0.0, 0.0, 1.0]}}
        })),
    })
    .unwrap();
    assert!(humanoid.mapping("mouth_open").is_none());
    assert!(humanoid.check(&rig()).is_empty());
}

#[test]
fn copying_values_over_others_leaves_exactly_the_source() {
    let source: ControlValues = [("mouth_open", 0.5), ("tail_wag", -0.25)]
        .into_iter()
        .collect();
    let mut copy: ControlValues = [("mouth_open", 1.0), ("head_yaw", 0.75)]
        .into_iter()
        .collect();
    copy.clone_from(&source);
    assert_eq!(copy, source);
}
