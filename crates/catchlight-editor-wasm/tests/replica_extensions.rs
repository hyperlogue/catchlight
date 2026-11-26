//! A tab that only holds replicas checks the official extensions as the
//! editor does. Its own process, so nothing else has registered them first.

use catchlight_core::{Model, ModelParam, Name, ParamId};
use catchlight_editor_wasm::ReplicaState;
use catchlight_extension_humanoid::{ControlMapping, Humanoid};
use serde_json::{Value, json};

#[test]
fn a_replica_reports_official_extensions_as_checked() {
    let mut model = Model::new();
    let jaw = ParamId::new("jaw").unwrap();
    model
        .add_param_with_id(
            jaw.clone(),
            ModelParam::new(Name::truncated("jaw"), 0.0, 1.0, 0.0),
        )
        .unwrap();
    model.extension_or_default::<Humanoid>().unwrap().set(
        "tail_wag",
        ControlMapping {
            param: jaw,
            stops: [0.0, 0.0, 1.0],
        },
    );
    let structure = model.to_structure_bytes().unwrap();

    let mut replica = ReplicaState::new();
    assert!(replica.apply_structure(&structure, 1).unwrap());
    let reply: Value = serde_json::from_str(
        &replica.query(&json!({"id": 1, "cmd": "extension_warnings", "session": 1}).to_string()),
    )
    .unwrap();
    assert_eq!(reply["reply"], "ok", "{reply}");
    let codes: Vec<&str> = reply["body"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        ["catchlight.humanoid/nonstandard-control"],
        "checked by the extension's own rules, not reported as unchecked"
    );
}
