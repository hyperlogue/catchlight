#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]
//! A program that never registers `Humanoid`, as the registry is
//! process-wide: it carries the data as stored.

use catchlight_core::extension::{self, Extension, IdRef, RemovedRef};
use catchlight_core::{Model, ModelParam, Name, ParamId};
use catchlight_extension_humanoid::{ControlMapping, Humanoid};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

#[test]
fn an_unregistered_program_carries_the_mappings_unchanged() {
    let mut model = Model::new();
    for p in ["jaw", "tail"] {
        model
            .add_param_with_id(pid(p), ModelParam::new(Name::truncated(p), -1.0, 1.0, 0.0))
            .unwrap();
    }
    let humanoid = model.extension_or_default::<Humanoid>().unwrap();
    humanoid.set(
        "mouth_open",
        ControlMapping {
            param: pid("jaw"),
            stops: [0.0, 0.0, 1.0],
        },
    );
    humanoid.set(
        "tail_wag",
        ControlMapping {
            param: pid("tail"),
            stops: [1.0, 0.25, -1.0],
        },
    );
    let bytes = model.to_clm_bytes().unwrap();
    let mut model = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(model.to_clm_bytes().unwrap(), bytes);

    model.delete_param(&pid("jaw")).unwrap();
    assert_eq!(
        model.extensions().refs(Humanoid::KEY).unwrap(),
        [
            IdRef::Removed(RemovedRef::Param(pid("jaw"))),
            IdRef::Param(pid("tail"))
        ]
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
    let humanoid = model.extension_mut::<Humanoid>().unwrap();
    assert!(humanoid.mapping("mouth_open").is_none());
    assert_eq!(humanoid.len(), 1);
}
