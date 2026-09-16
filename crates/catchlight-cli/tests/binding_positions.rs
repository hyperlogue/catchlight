#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! CLI inspection keeps binding-owned axes separate while sampling every knot.
use catchlight_core::{
    BindingKey, BindingTarget, Model, ModelParam, Name, NodeId, ParamId, ScalarTarget,
};

#[test]
fn pose_discovery_unions_independent_axes_without_reauthoring_them() {
    let mut model = Model::new();
    let param = ParamId::new("drive").unwrap();
    let unused = ParamId::new("unused").unwrap();
    for id in [&param, &unused] {
        model
            .add_param_with_id(
                id.clone(),
                ModelParam::new(Name::new("Drive").unwrap(), 0.0, 1.0, 0.0),
            )
            .unwrap();
    }
    let a = BindingKey::new(
        param.clone(),
        NodeId::new("root").unwrap(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    let b = BindingKey::new(
        param.clone(),
        NodeId::new("root").unwrap(),
        BindingTarget::Scalar(ScalarTarget::Ty),
    );
    model
        .add_binding_with_positions(&a, vec![vec![0.0, 0.25, 1.0]])
        .unwrap();
    model
        .add_binding_with_positions(&b, vec![vec![0.0, 0.75, 1.0]])
        .unwrap();
    let before = model.to_clm_bytes().unwrap();
    let dump = catchlight_cli::poses::build(&model);
    assert_eq!(
        dump.params
            .iter()
            .find(|p| p.id == param)
            .unwrap()
            .key_positions,
        vec![0.0, 0.25, 0.75, 1.0]
    );
    assert_eq!(
        dump.params
            .iter()
            .find(|p| p.id == unused)
            .unwrap()
            .key_positions,
        vec![0.0, 1.0]
    );
    assert_eq!(model.to_clm_bytes().unwrap(), before);
}

#[test]
fn diff_reports_positions_as_a_binding_change() {
    let mut model = Model::new();
    let param = ParamId::new("drive").unwrap();
    model
        .add_param_with_id(
            param.clone(),
            ModelParam::new(Name::new("Drive").unwrap(), 0.0, 1.0, 0.0),
        )
        .unwrap();
    let key = BindingKey::new(
        param,
        NodeId::new("root").unwrap(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    model
        .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
        .unwrap();
    let before = model.to_clm_file().unwrap();
    let mut after = before.clone();
    after.doc.bindings[0].key_positions[0][1] = 0.75;
    let lines = catchlight_cli::diff::diff(&before, &after);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("binding") && lines[0].contains("key_positions"),
        "{lines:?}"
    );
}
