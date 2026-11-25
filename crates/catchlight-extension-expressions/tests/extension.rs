#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `catchlight.expressions` as a maintained extension: its codec, the core's
//! upkeep of its reference table, its check, and addon merging.

use catchlight_core::extension::{register, Extension, IdRef, Payload, RawExtension, RemovedRef};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    ExtensionValue, InstallError, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart, Name,
    NodeId, ParamId,
};
use catchlight_extension_expressions::{Expression, Expressions, Setting};
use catchlight_extension_humanoid::{ControlMapping, Humanoid};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn rig() -> Model {
    let mut model = Model::new();
    for p in ["tears", "blush", "smile"] {
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

fn names(model: &Model) -> Vec<String> {
    model
        .extension::<Expressions>()
        .unwrap()
        .names()
        .map(String::from)
        .collect()
}

#[test]
fn expressions_round_trip_exactly_through_a_file_as_a_json_payload() {
    let mut model = rig();
    let crying = Expression::new(
        "crying",
        vec![
            Setting::control("mouth_smile", -0.6),
            Setting::param(pid("tears"), 1.0),
            Setting::control("ear_droop", f32::from_bits(1)),
        ],
    );
    let happy = Expression::new("happy", vec![Setting::param(pid("blush"), 0.3)]);
    *model.extension_or_default::<Expressions>().unwrap() =
        Expressions::from_expressions([crying.clone(), happy.clone()]);

    let Some(ExtensionValue::Maintained(raw)) = model.extensions().get(Expressions::KEY) else {
        panic!("not maintained");
    };
    assert_eq!(raw.version, 1);
    assert_eq!(
        raw.refs,
        [IdRef::Param(pid("tears")), IdRef::Param(pid("blush"))]
    );
    let Payload::Json(json) = &raw.payload else {
        panic!("not JSON");
    };
    assert_eq!(
        json["expressions"][0]["settings"].as_array().unwrap()[..2],
        [
            serde_json::json!({"control": "mouth_smile", "value": -0.6f32}),
            serde_json::json!({"param": 0, "value": 1.0}),
        ]
    );
    let decoded = Expressions::decode(raw.clone()).unwrap();
    assert_eq!(&decoded, model.extension::<Expressions>().unwrap());
    assert_eq!(decoded.encode(), *raw);

    let reopened = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let back = reopened.extension::<Expressions>().unwrap();
    assert_eq!(back.expressions(), [crying, happy]);
}

#[test]
fn a_non_finite_value_survives_a_file_as_nan() {
    let mut model = rig();
    *model.extension_or_default::<Expressions>().unwrap() =
        Expressions::from_expressions([Expression::new(
            "odd",
            vec![Setting::control("mouth_open", f32::NAN)],
        )]);
    let reopened = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let odd = reopened
        .extension::<Expressions>()
        .unwrap()
        .get("odd")
        .unwrap();
    assert!(odd.settings[0].value.is_nan());
}

#[test]
fn a_setting_naming_both_a_control_and_a_param_does_not_decode() {
    let raw = RawExtension {
        version: 1,
        refs: vec![IdRef::Param(pid("tears"))],
        payload: Payload::Json(
            serde_json::json!({"expressions": [{"name": "x", "settings": [
                {"control": "mouth_open", "param": 0, "value": 1.0}
            ]}]}),
        ),
    };
    assert!(Expressions::decode(raw).is_err());
}

#[test]
fn insert_replaces_by_name_in_place_and_keeps_the_table_compact() {
    let mut expressions = Expressions::from_expressions([
        Expression::new("a", vec![Setting::param(pid("tears"), 1.0)]),
        Expression::new("b", vec![]),
    ]);
    let old = expressions
        .insert(Expression::new(
            "a",
            vec![Setting::param(pid("blush"), 1.0)],
        ))
        .unwrap();
    assert_eq!(old.settings, [Setting::param(pid("tears"), 1.0)]);
    assert_eq!(expressions.names().collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(expressions.refs(), [IdRef::Param(pid("blush"))]);
    expressions.remove("a").unwrap();
    assert!(expressions.refs().is_empty());
    assert!(expressions.get("a").is_none());
}

#[test]
fn a_renamed_param_carries_its_setting_and_a_deleted_one_drops_it() {
    let mut model = rig();
    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_expressions([
        Expression::new(
            "sad",
            vec![
                Setting::param(pid("tears"), 1.0),
                Setting::control("mouth_smile", -0.5),
            ],
        ),
        Expression::new("shy", vec![Setting::param(pid("blush"), 1.0)]),
    ]);
    model.rename_param_id(&pid("tears"), pid("weep")).unwrap();
    let get = |model: &Model, name: &str| {
        model
            .extension::<Expressions>()
            .unwrap()
            .get(name)
            .unwrap()
            .settings
    };
    assert_eq!(get(&model, "sad")[0], Setting::param(pid("weep"), 1.0));
    model.delete_param(&pid("blush")).unwrap();
    assert!(get(&model, "shy").is_empty(), "the expression stays");
    assert_eq!(
        model.extension::<Expressions>().unwrap().refs(),
        [IdRef::Param(pid("weep"))]
    );
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

fn map(param: &str) -> ControlMapping {
    ControlMapping {
        param: pid(param),
        stops: [-1.0, 0.0, 1.0],
    }
}

#[test]
fn check_warns_of_each_setting_that_does_not_fit() {
    register::<Expressions>().unwrap();
    register::<Humanoid>().unwrap();
    let mut model = rig();
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set("mouth_smile", map("smile"));
    humanoid.set("ear_droop", map("tears"));
    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_expressions([
        Expression::new("mapped", vec![Setting::param(pid("smile"), 1.0)]),
        // Two unmapped controls make one warning.
        Expression::new(
            "unmapped",
            vec![
                Setting::control("mouth_open", 0.5),
                Setting::control("tongue_out", 0.5),
            ],
        ),
        Expression::new("nonstandard", vec![Setting::control("ear_droop", 0.5)]),
        // mouth_open is one-sided.
        Expression::new(
            "range",
            vec![
                Setting::control("mouth_smile", 1.5),
                Setting::control("mouth_open", -0.5),
            ],
        ),
        Expression::new("nan", vec![Setting::param(pid("blush"), f32::NAN)]),
        Expression::new(
            "twice",
            vec![
                Setting::param(pid("blush"), 0.1),
                Setting::param(pid("blush"), 0.2),
            ],
        ),
        Expression::new("fine", vec![Setting::control("mouth_smile", -1.0)]),
    ]);
    assert_eq!(
        codes(&model),
        [
            "catchlight.expressions/duplicate-target",
            "catchlight.expressions/mapped-param",
            "catchlight.expressions/non-finite-value",
            "catchlight.expressions/nonstandard-control",
            "catchlight.expressions/unmapped-control",
            "catchlight.expressions/unmapped-control",
            "catchlight.expressions/value-out-of-range",
            "catchlight.expressions/value-out-of-range",
            "catchlight.humanoid/nonstandard-control",
        ]
    );
}

#[test]
fn without_humanoid_data_unmapped_controls_warn_once_in_total() {
    register::<Expressions>().unwrap();
    let mut model = rig();
    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_expressions([
        Expression::new("a", vec![Setting::control("mouth_open", 0.5)]),
        Expression::new("b", vec![Setting::control("mouth_smile", 0.5)]),
    ]);
    assert_eq!(codes(&model), ["catchlight.expressions/unmapped-control"]);
}

#[test]
fn check_leaves_missing_and_removed_params_to_the_core() {
    register::<Expressions>().unwrap();
    let mut model = rig();
    *model.extension_or_default::<Expressions>().unwrap() =
        Expressions::from_expressions([Expression::new(
            "a",
            vec![Setting::param(pid("ghost"), 0.5)],
        )]);
    assert_eq!(codes(&model), ["catchlight/dangling-reference"]);

    let removed = Expressions::decode(RawExtension {
        version: 1,
        refs: vec![IdRef::Removed(RemovedRef::Param(pid("tears")))],
        payload: Payload::Json(serde_json::json!({"expressions": [
            {"name": "a", "settings": [{"param": 0, "value": 1.0}]}
        ]})),
    })
    .unwrap();
    assert!(removed.check(&rig()).is_empty());
    assert!(removed.get("a").unwrap().settings.is_empty());
}

#[test]
fn an_addons_expressions_merge_refuse_a_taken_name_and_leave_on_uninstall() {
    register::<Expressions>().unwrap();
    let mut base = rig();
    let mut addon = base.extract(&[NodeId::new("hat").unwrap()]);
    base.delete_node(&NodeId::new("hat").unwrap()).unwrap();
    *base.extension_or_default::<Expressions>().unwrap() =
        Expressions::from_expressions([Expression::new(
            "happy",
            vec![Setting::param(pid("blush"), 1.0)],
        )]);
    *addon.extension_or_default::<Expressions>().unwrap() = Expressions::from_expressions([
        Expression::new("tipped", vec![Setting::param(pid("tears"), 1.0)]),
        Expression::new("aa", vec![Setting::control("mouth_open", 1.0)]),
    ]);

    let installed = base.install(&addon).unwrap();
    assert_eq!(names(&base), ["happy", "tipped", "aa"]);
    assert_eq!(
        base.extension::<Expressions>()
            .unwrap()
            .get("tipped")
            .unwrap()
            .settings,
        [Setting::param(pid("tears"), 1.0)]
    );
    base.extension_mut::<Expressions>()
        .unwrap()
        .insert(Expression::new("sad", vec![]));
    base.uninstall(&installed);
    assert_eq!(names(&base), ["happy", "sad"]);
    assert_eq!(
        base.extension::<Expressions>().unwrap().refs(),
        [IdRef::Param(pid("blush"))]
    );

    *addon.extension_mut::<Expressions>().unwrap() =
        Expressions::from_expressions([Expression::new("happy", vec![])]);
    assert!(matches!(
        base.install(&addon),
        Err(InstallError::ExtensionRefused { .. })
    ));
    assert_eq!(names(&base), ["happy", "sad"]);
}
