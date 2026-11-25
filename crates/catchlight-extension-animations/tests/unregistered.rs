#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A program that never registers `Animations`, as the registry is
//! process-wide: its raw slot is maintained by the core alone.

use catchlight_core::extension::{Extension, IdRef, RemovedRef};
use catchlight_core::{InterpolateMode, Model, ModelParam, Name, ParamId};
use catchlight_extension_animations::{codes, Animations, Clip, Keyframe, Lane};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

#[test]
fn an_unregistered_reader_keeps_the_removal_marker_until_a_typed_write() {
    let mut model = Model::new();
    model
        .add_param_with_id(
            pid("eye"),
            ModelParam::new(Name::truncated("eye"), -1.0, 1.0, 0.0),
        )
        .unwrap();
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([Clip {
        lanes: vec![Lane {
            param: pid("eye"),
            interpolation: InterpolateMode::Linear,
            keyframes: vec![Keyframe {
                frame: 0,
                value: 1.0,
            }],
        }],
        ..Clip::new("talk")
    }]);
    // Through a file, so the slot is raw.
    let mut model = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    model.delete_param(&pid("eye")).unwrap();
    assert_eq!(
        model.extensions().refs(Animations::KEY).unwrap(),
        [IdRef::Removed(RemovedRef::Param(pid("eye")))]
    );
    let warnings: Vec<_> = Animations::check(model.extension::<Animations>().unwrap(), &model)
        .into_iter()
        .map(|w| w.code)
        .collect();
    assert_eq!(warnings, [codes::REMOVED_PARAM]);
    assert!(model
        .extension_mut::<Animations>()
        .unwrap()
        .clip("talk")
        .unwrap()
        .lanes
        .is_empty());
}
