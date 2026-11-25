#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `catchlight.expressions` as a maintained extension: its codec, the core's
//! upkeep of its reference table, its check, and addon merging.

use catchlight_core::extension::{register, Extension, IdRef, Payload, RawExtension, RemovedRef};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    ExtensionValue, InstallError, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart, Name,
    NodeId, ParamId,
};
use catchlight_extension_expressions::{Expression, Expressions, Group, Setting};
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

fn names(model: &Model, group: Group) -> Vec<String> {
    model
        .extension::<Expressions>()
        .unwrap()
        .names(group)
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
    assert_eq!(back.all(Group::Expression), [crying, happy]);
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
        .get(Group::Expression, "odd")
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
        .insert(
            Group::Expression,
            Expression::new("a", vec![Setting::param(pid("blush"), 1.0)]),
        )
        .unwrap();
    assert_eq!(old.settings, [Setting::param(pid("tears"), 1.0)]);
    assert_eq!(
        expressions.names(Group::Expression).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(expressions.refs(), [IdRef::Param(pid("blush"))]);
    expressions.remove(Group::Expression, "a").unwrap();
    assert!(expressions.refs().is_empty());
    assert!(expressions.get(Group::Expression, "a").is_none());
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
            .get(Group::Expression, name)
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
    assert!(removed
        .get(Group::Expression, "a")
        .unwrap()
        .settings
        .is_empty());
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
    assert_eq!(names(&base, Group::Expression), ["happy", "tipped", "aa"]);
    assert_eq!(
        base.extension::<Expressions>()
            .unwrap()
            .get(Group::Expression, "tipped")
            .unwrap()
            .settings,
        [Setting::param(pid("tears"), 1.0)]
    );
    base.extension_mut::<Expressions>()
        .unwrap()
        .insert(Group::Expression, Expression::new("sad", vec![]));
    base.uninstall(&installed);
    assert_eq!(names(&base, Group::Expression), ["happy", "sad"]);
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
    assert_eq!(names(&base, Group::Expression), ["happy", "sad"]);
}

fn both_groups() -> Expressions {
    Expressions::from_groups(
        [
            Expression::new("happy", vec![Setting::param(pid("blush"), 0.3)]),
            Expression::new("aa", vec![Setting::control("mouth_smile", 0.5)]),
        ],
        [
            Expression::new(
                "aa",
                vec![
                    Setting::control("mouth_open", 1.0),
                    Setting::param(pid("tears"), 0.25),
                ],
            ),
            Expression::new("oh", vec![Setting::control("mouth_funnel", 1.0)]),
        ],
    )
}

#[test]
fn both_groups_round_trip_through_a_file_and_share_one_table() {
    let mut model = rig();
    *model.extension_or_default::<Expressions>().unwrap() = both_groups();
    let Some(ExtensionValue::Maintained(raw)) = model.extensions().get(Expressions::KEY) else {
        panic!("not maintained");
    };
    assert_eq!(
        raw.refs,
        [IdRef::Param(pid("blush")), IdRef::Param(pid("tears"))]
    );
    let Payload::Json(json) = &raw.payload else {
        panic!("not JSON");
    };
    assert_eq!(json["expressions"].as_array().unwrap().len(), 2);
    assert_eq!(
        json["visemes"][0]["settings"][1],
        serde_json::json!({"param": 1, "value": 0.25})
    );
    let reopened = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let back = reopened.extension::<Expressions>().unwrap();
    assert_eq!(back, &both_groups());
    assert_eq!(back.all(Group::Viseme), both_groups().all(Group::Viseme));
}

#[test]
fn an_expression_and_a_viseme_of_one_name_coexist() {
    let mut expressions = both_groups();
    assert!(expressions.contains(Group::Expression, "aa"));
    assert!(expressions.contains(Group::Viseme, "aa"));
    assert_ne!(
        expressions.get(Group::Expression, "aa"),
        expressions.get(Group::Viseme, "aa")
    );
    expressions.remove(Group::Viseme, "aa").unwrap();
    assert!(expressions.contains(Group::Expression, "aa"));
    assert_eq!(expressions.names(Group::Viseme).collect::<Vec<_>>(), ["oh"]);
    assert_eq!(expressions.refs(), [IdRef::Param(pid("blush"))]);
    assert!(expressions.remove(Group::Viseme, "happy").is_none());
    assert_eq!(expressions.len(Group::Expression), 2);
}

#[test]
fn two_visemes_of_one_name_do_not_decode() {
    let raw = RawExtension {
        version: 1,
        refs: vec![],
        payload: Payload::Json(serde_json::json!({
            "expressions": [{"name": "aa", "settings": []}],
            "visemes": [{"name": "aa", "settings": []}, {"name": "aa", "settings": []}],
        })),
    };
    let error = Expressions::decode(raw).unwrap_err();
    assert!(error.to_string().contains("two visemes"), "{error}");
}

#[test]
fn check_warns_of_a_viseme_the_way_it_warns_of_an_expression() {
    register::<Expressions>().unwrap();
    register::<Humanoid>().unwrap();
    let mut model = rig();
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set("mouth_smile", map("smile"));
    let bad = || {
        vec![
            Expression::new("mapped", vec![Setting::param(pid("smile"), 1.0)]),
            Expression::new("unmapped", vec![Setting::control("mouth_open", 0.5)]),
            Expression::new("nonstandard", vec![Setting::control("ear_droop", 0.5)]),
            Expression::new("range", vec![Setting::control("mouth_smile", 1.5)]),
            Expression::new("nan", vec![Setting::param(pid("blush"), f32::NAN)]),
            Expression::new(
                "twice",
                vec![
                    Setting::param(pid("blush"), 0.1),
                    Setting::param(pid("blush"), 0.2),
                ],
            ),
        ]
    };
    let expected = [
        "catchlight.expressions/duplicate-target",
        "catchlight.expressions/mapped-param",
        "catchlight.expressions/non-finite-value",
        "catchlight.expressions/nonstandard-control",
        // `ear_droop` is unmapped too.
        "catchlight.expressions/unmapped-control",
        "catchlight.expressions/unmapped-control",
        "catchlight.expressions/value-out-of-range",
    ];
    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_groups([], bad());
    assert_eq!(codes(&model), expected);
    let messages: Vec<String> = model
        .extension_warnings()
        .into_iter()
        .map(|w| w.warning.message)
        .collect();
    assert!(
        messages.iter().all(|m| m.starts_with("viseme ")),
        "{messages:?}"
    );

    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_groups(bad(), bad());
    let mut twice: Vec<&str> = expected.iter().chain(&expected).copied().collect();
    twice.sort_unstable();
    assert_eq!(codes(&model), twice);
}

#[test]
fn reconcile_drops_a_removed_params_settings_from_both_groups() {
    let mut model = rig();
    *model.extension_or_default::<Expressions>().unwrap() = Expressions::from_groups(
        [Expression::new(
            "cry",
            vec![Setting::param(pid("tears"), 1.0)],
        )],
        [Expression::new(
            "aa",
            vec![
                Setting::param(pid("tears"), 0.5),
                Setting::control("mouth_open", 1.0),
            ],
        )],
    );
    model.delete_param(&pid("tears")).unwrap();
    let expressions = model.extension::<Expressions>().unwrap();
    assert!(expressions
        .get(Group::Expression, "cry")
        .unwrap()
        .settings
        .is_empty());
    assert_eq!(
        expressions.get(Group::Viseme, "aa").unwrap().settings,
        [Setting::control("mouth_open", 1.0)]
    );
    assert!(expressions.refs().is_empty());
}

#[test]
fn an_addons_visemes_merge_beside_its_expressions_clash_per_group_and_leave_on_uninstall() {
    register::<Expressions>().unwrap();
    let mut base = rig();
    let mut addon = base.extract(&[NodeId::new("hat").unwrap()]);
    base.delete_node(&NodeId::new("hat").unwrap()).unwrap();
    *base.extension_or_default::<Expressions>().unwrap() = Expressions::from_groups(
        [Expression::new("aa", vec![])],
        [Expression::new(
            "oh",
            vec![Setting::param(pid("blush"), 1.0)],
        )],
    );
    // An addon viseme `aa` beside the base's expression `aa` is no clash.
    *addon.extension_or_default::<Expressions>().unwrap() = Expressions::from_groups(
        [Expression::new("wink", vec![])],
        [Expression::new(
            "aa",
            vec![Setting::param(pid("tears"), 1.0)],
        )],
    );
    let installed = base.install(&addon).unwrap();
    assert_eq!(names(&base, Group::Expression), ["aa", "wink"]);
    assert_eq!(names(&base, Group::Viseme), ["oh", "aa"]);
    assert_eq!(
        base.extension::<Expressions>()
            .unwrap()
            .get(Group::Viseme, "aa")
            .unwrap()
            .settings,
        [Setting::param(pid("tears"), 1.0)]
    );
    base.uninstall(&installed);
    assert_eq!(names(&base, Group::Expression), ["aa"]);
    assert_eq!(names(&base, Group::Viseme), ["oh"]);
    assert_eq!(
        base.extension::<Expressions>().unwrap().refs(),
        [IdRef::Param(pid("blush"))]
    );

    *addon.extension_mut::<Expressions>().unwrap() =
        Expressions::from_groups([], [Expression::new("oh", vec![])]);
    let Err(InstallError::ExtensionRefused { .. }) = base.install(&addon) else {
        panic!("a taken viseme name installs");
    };
    assert_eq!(names(&base, Group::Viseme), ["oh"]);
}
