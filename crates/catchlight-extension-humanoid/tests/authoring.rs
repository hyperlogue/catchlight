#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The checked edits a tool makes on a person's behalf: what each does, and
//! each request it refuses.

use catchlight_core::extension::{Extension, ExtensionError, Unread};
use catchlight_core::formats::clm::{self, ClmExtension, ClmPayload};
use catchlight_core::id::ExtensionKey;
use catchlight_core::{Model, ModelParam, Name, ParamId};
use catchlight_extension_humanoid::authoring::{
    map, standard_control, unmap, AuthoringError, Mapped,
};
use catchlight_extension_humanoid::{ControlMapping, Humanoid};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}

/// `jaw` in 0..1 resting at 0.2, `turn` in -2..2 resting at -2.
fn rig() -> Model {
    let mut model = Model::new();
    for (p, min, max, default) in [
        ("jaw", 0.0, 1.0, 0.2),
        ("turn", -2.0, 2.0, -2.0),
        ("lid", 0.0, 1.0, 1.0),
    ] {
        model
            .add_param_with_id(
                pid(p),
                ModelParam::new(Name::truncated(p), min, max, default),
            )
            .unwrap();
    }
    model
}

fn mapping(param: &str, stops: [f32; 3]) -> ControlMapping {
    ControlMapping {
        param: pid(param),
        stops,
    }
}

#[test]
fn default_stops_follow_the_controls_sidedness_and_explicit_stops_are_kept() {
    let mut model = rig();
    let mapped = map(&mut model, "head_yaw", &pid("turn"), None, false).unwrap();
    assert_eq!(
        mapped,
        Mapped {
            mapping: mapping("turn", [-2.0, -2.0, 2.0]),
            replaced: None,
        }
    );
    let mapped = map(&mut model, "mouth_open", &pid("jaw"), None, false).unwrap();
    assert_eq!(mapped.mapping.stops, [0.2, 0.2, 1.0], "one-sided");
    let mapped = map(&mut model, "eye_left_squint", &pid("lid"), None, false).unwrap();
    assert_eq!(
        mapped.mapping.stops,
        [1.0, 1.0, 0.0],
        "a one-sided control reaches the end farther from the default"
    );
    let mapped = map(
        &mut model,
        "head_yaw",
        &pid("turn"),
        Some([1.0, 0.0, -1.0]),
        false,
    )
    .unwrap();
    assert_eq!(
        mapped.replaced,
        Some(mapping("turn", [-2.0, -2.0, 2.0])),
        "a second map replaces and returns the first"
    );
    let humanoid = model.extension::<Humanoid>().unwrap();
    assert_eq!(
        humanoid.mapping("head_yaw"),
        Some(mapping("turn", [1.0, 0.0, -1.0]))
    );
    assert_eq!(humanoid.len(), 3);
}

#[test]
fn a_nonstandard_name_is_refused_with_the_nearest_unless_allowed() {
    let mut model = rig();
    let err = map(&mut model, "head_yaww", &pid("turn"), None, false).unwrap_err();
    assert_eq!(
        err,
        AuthoringError::NonstandardControl {
            name: "head_yaww".into(),
            nearest: Some("head_yaw"),
        }
    );
    assert!(
        err.to_string().contains("did you mean \"head_yaw\""),
        "{err}"
    );
    assert!(model.extensions().is_empty(), "a refusal changes nothing");

    let mapped = map(&mut model, "tail_wag", &pid("jaw"), None, true).unwrap();
    assert_eq!(
        mapped.mapping.stops,
        [0.0, 0.2, 1.0],
        "a nonstandard control is two-sided"
    );

    assert_eq!(
        standard_control("", true).unwrap_err(),
        AuthoringError::EmptyControlName
    );
    assert_eq!(
        standard_control("mouth_open", false).unwrap().unwrap().name,
        "mouth_open"
    );
    assert_eq!(standard_control("tail_wag", true).unwrap(), None);
}

#[test]
fn a_near_miss_suggests_the_standard_name() {
    let near = |n| match standard_control(n, false) {
        Err(AuthoringError::NonstandardControl { nearest, .. }) => nearest,
        other => panic!("{n} is refused: {other:?}"),
    };
    assert_eq!(near("mouth_opn"), Some("mouth_open"));
    assert_eq!(near("head_yaww"), Some("head_yaw"));
    assert_eq!(near("tail_wag_speed"), None);
}

#[test]
fn a_missing_param_and_a_non_finite_stop_are_refused() {
    let mut model = rig();
    assert_eq!(
        map(&mut model, "head_yaw", &pid("ghost"), None, false).unwrap_err(),
        AuthoringError::NoParam(pid("ghost"))
    );
    assert!(matches!(
        map(
            &mut model,
            "head_yaw",
            &pid("turn"),
            Some([0.0, f32::NAN, 1.0]),
            false
        )
        .unwrap_err(),
        AuthoringError::NonFiniteStop { .. }
    ));
    assert!(model.extensions().is_empty());
}

#[test]
fn unmap_returns_the_mapping_and_the_last_one_takes_the_extension_with_it() {
    let mut model = rig();
    map(&mut model, "head_yaw", &pid("turn"), None, false).unwrap();
    map(&mut model, "mouth_open", &pid("jaw"), None, false).unwrap();

    assert_eq!(
        unmap(&mut model, "head_yaw").unwrap(),
        mapping("turn", [-2.0, -2.0, 2.0])
    );
    assert!(model.extensions().contains_key(Humanoid::KEY));
    unmap(&mut model, "mouth_open").unwrap();
    assert!(!model.extensions().contains_key(Humanoid::KEY));
}

#[test]
fn unmapping_what_is_not_mapped_is_refused() {
    let mut model = rig();
    assert_eq!(
        unmap(&mut model, "head_yaw").unwrap_err(),
        AuthoringError::NotMapped("head_yaw".into()),
        "with no humanoid data at all"
    );
    map(&mut model, "mouth_open", &pid("jaw"), None, false).unwrap();
    assert_eq!(
        unmap(&mut model, "head_yaw").unwrap_err(),
        AuthoringError::NotMapped("head_yaw".into())
    );
}

#[test]
fn data_this_build_cannot_read_is_refused_rather_than_overwritten() {
    let mut file = clm::decode(&rig().to_clm_bytes().unwrap()).unwrap();
    file.doc.extensions.insert(
        ExtensionKey::new(Humanoid::KEY).unwrap(),
        ClmExtension::Maintained {
            version: Humanoid::VERSION + 1,
            refs: Vec::new(),
            payload: ClmPayload::Json(serde_json::json!({"future": true})),
        },
    );
    let mut model = Model::from_clm_bytes(&file.encode().unwrap()).unwrap();
    let newer = AuthoringError::Unreadable(ExtensionError::Unread(Unread::Newer {
        version: Humanoid::VERSION + 1,
        readable: Humanoid::VERSION,
    }));
    assert_eq!(
        map(&mut model, "head_yaw", &pid("turn"), None, false).unwrap_err(),
        newer
    );
    assert_eq!(unmap(&mut model, "head_yaw").unwrap_err(), newer);
}
