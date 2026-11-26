#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]
//! A program that never registers `Expressions`, as the registry is
//! process-wide: it carries the data as stored.

use catchlight_core::extension::{self, Extension, IdRef, RemovedRef};
use catchlight_core::{Model, ModelParam, Name, ParamId};
use catchlight_extension_expressions::{Expression, Expressions, Group, Setting};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

#[test]
fn an_unregistered_program_carries_the_expressions_unchanged() {
    let mut model = Model::new();
    model
        .add_param_with_id(
            pid("tears"),
            ModelParam::new(Name::truncated("tears"), 0.0, 1.0, 0.0),
        )
        .unwrap();
    *model.extension_or_default::<Expressions>().unwrap() =
        Expressions::from_expressions([Expression::new(
            "crying",
            vec![
                Setting::control("ear_droop", 0.5),
                Setting::param(pid("tears"), 1.0),
            ],
        )]);
    let bytes = model.to_clm_bytes().unwrap();
    let mut model = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(model.to_clm_bytes().unwrap(), bytes);

    model.delete_param(&pid("tears")).unwrap();
    assert_eq!(
        model.extensions().refs(Expressions::KEY).unwrap(),
        [IdRef::Removed(RemovedRef::Param(pid("tears")))]
    );
    let codes: Vec<_> = model
        .extension_warnings()
        .into_iter()
        .map(|w| w.warning.code)
        .collect();
    assert_eq!(
        codes,
        [extension::codes::REMOVED, extension::codes::NOT_CHECKED]
    );
    let crying = model
        .extension_mut::<Expressions>()
        .unwrap()
        .get(Group::Expression, "crying");
    assert_eq!(
        crying.unwrap().settings,
        [Setting::control("ear_droop", 0.5)]
    );
}
