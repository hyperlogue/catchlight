#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The checked edits a tool makes on a person's behalf: what each does, and
//! each request it refuses.

use catchlight_core::extension::{Extension, ExtensionError, Unread};
use catchlight_core::formats::clm::{self, ClmExtension, ClmPayload};
use catchlight_core::id::ExtensionKey;
use catchlight_core::{Model, ModelParam, Name, ParamId};
use catchlight_extension_expressions::authoring::{
    add_standard_visemes, remove, set, AuthoringError,
};
use catchlight_extension_expressions::{
    standard_visemes, Expression, Expressions, Group, Setting, VISEMES,
};
use catchlight_extension_humanoid::authoring::AuthoringError as ControlError;

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

fn rig() -> Model {
    let mut model = Model::new();
    model
        .add_param_with_id(
            pid("tears"),
            ModelParam::new(Name::truncated("tears"), 0.0, 1.0, 0.0),
        )
        .unwrap();
    model
}

#[test]
fn set_keeps_settings_in_order_and_returns_what_it_replaced() {
    let mut model = rig();
    let settings = vec![
        Setting::param(pid("tears"), 0.8),
        Setting::control("mouth_smile", -0.5),
    ];
    let replaced = set(
        &mut model,
        Group::Expression,
        "sad",
        settings.clone(),
        false,
    )
    .unwrap();
    assert_eq!(replaced, None);
    let data = model.extension::<Expressions>().unwrap();
    assert_eq!(
        data.get(Group::Expression, "sad"),
        Some(Expression::new("sad", settings.clone()))
    );
    assert_eq!(data.len(Group::Viseme), 0, "only the named group changed");

    let replaced = set(
        &mut model,
        Group::Expression,
        "sad",
        vec![Setting::control("mouth_open", 0.3)],
        false,
    )
    .unwrap();
    assert_eq!(replaced, Some(Expression::new("sad", settings)));
}

#[test]
fn an_empty_name_or_no_settings_is_refused() {
    let mut model = rig();
    let one = vec![Setting::control("mouth_open", 0.3)];
    let err = set(&mut model, Group::Viseme, "", one, false).unwrap_err();
    assert_eq!(err, AuthoringError::EmptyName(Group::Viseme));
    assert_eq!(err.to_string(), "a viseme name must not be empty");
    assert_eq!(
        set(&mut model, Group::Expression, "sad", Vec::new(), false).unwrap_err(),
        AuthoringError::NoSettings {
            group: Group::Expression,
            name: "sad".into(),
        }
    );
    assert!(model.extensions().is_empty(), "a refusal changes nothing");
}

#[test]
fn a_setting_is_checked_against_its_target() {
    let mut model = rig();
    let refused = |model: &mut Model, setting: Setting, nonstandard: bool| {
        set(model, Group::Expression, "x", vec![setting], nonstandard).unwrap_err()
    };
    assert_eq!(
        refused(&mut model, Setting::control("mouth_smil", 0.5), false),
        AuthoringError::Control(ControlError::NonstandardControl {
            name: "mouth_smil".into(),
            nearest: Some("mouth_smile"),
        })
    );
    assert_eq!(
        refused(&mut model, Setting::control("", 0.5), true),
        AuthoringError::Control(ControlError::EmptyControlName)
    );
    assert_eq!(
        refused(&mut model, Setting::control("mouth_open", -0.5), false),
        AuthoringError::ControlValue {
            control: "mouth_open".into(),
            value: -0.5,
            min: 0.0,
            max: 1.0,
        },
        "a one-sided control takes 0..1"
    );
    assert!(matches!(
        refused(&mut model, Setting::control("tail_wag", 1.5), true),
        AuthoringError::ControlValue { min, max, .. } if min == -1.0 && max == 1.0
    ));
    assert!(matches!(
        refused(
            &mut model,
            Setting::control("mouth_open", f32::INFINITY),
            false
        ),
        AuthoringError::ControlValue { .. }
    ));
    assert!(matches!(
        refused(&mut model, Setting::param(pid("tears"), f32::NAN), false),
        AuthoringError::ParamValue { .. }
    ));
    assert_eq!(
        refused(&mut model, Setting::param(pid("ghost"), 1.0), false),
        AuthoringError::NoParam(pid("ghost"))
    );
    assert!(model.extensions().is_empty());

    set(
        &mut model,
        Group::Expression,
        "wag",
        vec![Setting::control("tail_wag", 0.5)],
        true,
    )
    .unwrap();
}

#[test]
fn remove_returns_the_member_and_the_last_one_takes_the_extension_with_it() {
    let mut model = rig();
    let happy = Expression::new("happy", vec![Setting::control("mouth_smile", 1.0)]);
    let aa = Expression::new("aa", vec![Setting::control("mouth_open", 1.0)]);
    set(
        &mut model,
        Group::Expression,
        "happy",
        happy.settings.clone(),
        false,
    )
    .unwrap();
    set(&mut model, Group::Viseme, "aa", aa.settings.clone(), false).unwrap();

    assert_eq!(
        remove(&mut model, Group::Expression, "happy").unwrap(),
        happy
    );
    assert!(model.extensions().contains_key(Expressions::KEY));
    assert_eq!(remove(&mut model, Group::Viseme, "aa").unwrap(), aa);
    assert!(!model.extensions().contains_key(Expressions::KEY));
}

#[test]
fn removing_what_is_not_there_is_refused() {
    let mut model = rig();
    let missing = AuthoringError::NoMember {
        group: Group::Viseme,
        name: "aa".into(),
    };
    assert_eq!(
        remove(&mut model, Group::Viseme, "aa").unwrap_err(),
        missing
    );
    set(
        &mut model,
        Group::Expression,
        "aa",
        vec![Setting::control("mouth_open", 1.0)],
        false,
    )
    .unwrap();
    assert_eq!(
        remove(&mut model, Group::Viseme, "aa").unwrap_err(),
        missing,
        "an expression of that name is not a viseme"
    );
}

#[test]
fn adding_the_standard_visemes_skips_or_replaces_existing_ones() {
    let mut model = rig();
    let own = vec![Setting::control("mouth_open", 0.1)];
    set(&mut model, Group::Viseme, "aa", own.clone(), false).unwrap();

    let report = add_standard_visemes(&mut model, false).unwrap();
    assert_eq!(report.skipped, ["aa"]);
    assert!(report.replaced.is_empty());
    assert_eq!(report.added.len(), VISEMES.len() - 1);
    let data = model.extension::<Expressions>().unwrap();
    assert_eq!(
        data.get(Group::Viseme, "aa"),
        Some(Expression::new("aa", own))
    );
    assert_eq!(
        data.names(Group::Viseme).next(),
        Some("aa"),
        "kept in place"
    );

    let report = add_standard_visemes(&mut model, true).unwrap();
    assert!(report.added.is_empty() && report.skipped.is_empty());
    assert_eq!(report.replaced.len(), VISEMES.len());
    let standard_aa = standard_visemes().into_iter().find(|v| v.name == "aa");
    assert_eq!(
        model
            .extension::<Expressions>()
            .unwrap()
            .get(Group::Viseme, "aa"),
        standard_aa
    );
}

#[test]
fn data_this_build_cannot_read_is_refused_rather_than_overwritten() {
    let mut file = clm::decode(&rig().to_clm_bytes().unwrap()).unwrap();
    file.doc.extensions.insert(
        ExtensionKey::new(Expressions::KEY).unwrap(),
        ClmExtension::Maintained {
            version: Expressions::VERSION + 1,
            refs: Vec::new(),
            payload: ClmPayload::Json(serde_json::json!({"future": true})),
        },
    );
    let mut model = Model::from_clm_bytes(&file.encode().unwrap()).unwrap();
    let newer = AuthoringError::Unreadable(ExtensionError::Unread(Unread::Newer {
        version: Expressions::VERSION + 1,
        readable: Expressions::VERSION,
    }));
    let one = vec![Setting::control("mouth_open", 0.3)];
    assert_eq!(
        set(&mut model, Group::Viseme, "aa", one, false).unwrap_err(),
        newer
    );
    assert_eq!(remove(&mut model, Group::Viseme, "aa").unwrap_err(), newer);
    assert_eq!(add_standard_visemes(&mut model, false).unwrap_err(), newer);
}
