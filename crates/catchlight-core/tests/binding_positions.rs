#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Binding-owned sampling grids preserve independent authoring over shared
//! inputs.

use catchlight_core::formats::clm;
use catchlight_core::id::SeededHex;
use catchlight_core::{
    BindingKey, BindingTarget, InterpolateMode, Model, ModelNode, ModelNodeKind, ModelParam,
    ModelPart, Name, NodeId, ParamId, Pose, Puppet, ScalarTarget,
};

fn fixture() -> (Model, ParamId, NodeId) {
    let mut model = Model::new();
    let mut ids = SeededHex::new(83);
    let root = model.root().unwrap().clone();
    let node = model
        .add_node(
            &root,
            ModelNode::new(
                "part",
                ModelNodeKind::Part(ModelPart::new(clm::ClmMesh::default())),
            ),
            &mut ids,
        )
        .unwrap();
    let param = model
        .add_param(
            ModelParam::new(Name::truncated("drive"), 0.0, 1.0, 0.0),
            &mut ids,
        )
        .unwrap();
    (model, param, node)
}

fn key(param: &ParamId, node: &NodeId, target: ScalarTarget) -> BindingKey {
    BindingKey::new(param.clone(), node.clone(), BindingTarget::Scalar(target))
}

#[test]
fn shared_param_has_independent_positions_in_model_and_puppet() {
    let (mut model, param, node) = fixture();
    let tx = key(&param, &node, ScalarTarget::Tx);
    let ty = key(&param, &node, ScalarTarget::Ty);
    for (key, middle) in [(&tx, 0.25), (&ty, 0.75)] {
        model
            .add_binding_with_positions(key, vec![vec![0.0, middle, 1.0]])
            .unwrap();
        for (cell, value) in [(0, 0.0), (1, 10.0), (2, 0.0)] {
            model.set_binding_key(key, [cell, 0], value).unwrap();
        }
    }
    let pose: Pose = [(param.clone(), 0.25)].into_iter().collect();
    assert_eq!(model.eval_scalar(&tx, &pose), Some(10.0));
    assert!((model.eval_scalar(&ty, &pose).unwrap() - 10.0 / 3.0).abs() < 1e-5);
    let mut puppet = Puppet::new(&model);
    puppet.apply_pose(&pose);
    puppet.tick(&model, 0.0);
    let transform = puppet
        .get(puppet.node_idx(&node).unwrap())
        .unwrap()
        .transform;
    assert!((transform.translation.x - 10.0).abs() < 1e-5);
    assert!((transform.translation.y - 10.0 / 3.0).abs() < 1e-5);

    let untouched = model.binding(&ty).unwrap().values().clone();
    model.key_move(&tx, &param, 1, 0.5).unwrap();
    assert_eq!(
        model.binding(&ty).unwrap().key_positions(),
        &[vec![0.0, 0.75, 1.0]]
    );
    assert_eq!(model.binding(&ty).unwrap().values(), &untouched);
    assert_eq!(model.eval_scalar(&tx, &pose), Some(5.0));
    puppet.tick(&model, 0.0);
    assert!(
        (puppet
            .get(puppet.node_idx(&node).unwrap())
            .unwrap()
            .transform
            .translation
            .x
            - 5.0)
            .abs()
            < 1e-5
    );
    model.key_delete(&tx, &param, 1).unwrap();
    assert_eq!(model.key_count(&tx, &param).unwrap(), 2);
    assert_eq!(model.key_count(&ty, &param).unwrap(), 3);
}

#[test]
fn two_param_axis_edits_keep_other_bindings_holes_and_values() {
    let (mut model, x, node) = fixture();
    let y = ParamId::new("cross").unwrap();
    model
        .add_param_with_id(
            y.clone(),
            ModelParam::new(Name::truncated("cross"), 0.0, 1.0, 0.0),
        )
        .unwrap();
    let binding = BindingKey::pair(
        x.clone(),
        y.clone(),
        node.clone(),
        BindingTarget::Scalar(ScalarTarget::Opacity),
    );
    model
        .add_binding_with_positions(&binding, vec![vec![0.2, 0.8], vec![0.0, 0.5, 1.0]])
        .unwrap();
    model.set_binding_key(&binding, [1, 2], 0.4).unwrap();
    model
        .set_binding_interpolate(&binding, InterpolateMode::Cubic)
        .unwrap();
    assert_eq!(model.key_insert(&binding, &y, 0.25).unwrap(), 1);
    let cells = model
        .binding(&binding)
        .unwrap()
        .values()
        .scalar_cells()
        .unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!((cells[0].x, cells[0].y, cells[0].value), (1, 3, 0.4));
    model.key_delete(&binding, &x, 0).unwrap();
    model.key_move(&binding, &x, 0, 0.6).unwrap();
    model.key_insert(&binding, &x, 0.0).unwrap();
    model.key_insert(&binding, &x, 1.0).unwrap();
    assert_eq!(
        model.binding(&binding).unwrap().key_positions()[0],
        vec![0.0, 0.6, 1.0]
    );
    assert_eq!(
        model.binding(&binding).unwrap().interpolate_mode(),
        InterpolateMode::Cubic
    );
    assert_eq!(model.binding_grid(&binding).unwrap(), (3, 4));
    model.key_delete(&binding, &y, 3).unwrap();
    assert!(model
        .binding(&binding)
        .unwrap()
        .values()
        .scalar_cells()
        .unwrap()
        .is_empty());
}

#[test]
fn invalid_axes_and_key_edits_leave_authored_state_unchanged() {
    let (mut model, param, node) = fixture();
    let tx = key(&param, &node, ScalarTarget::Tx);
    for positions in [
        vec![],
        vec![vec![]],
        vec![vec![0.5, 0.5]],
        vec![vec![1.0, 0.0]],
        vec![vec![f32::NAN]],
        vec![vec![-0.1]],
        vec![vec![1.1]],
        vec![vec![0.0], vec![0.0]],
    ] {
        let before = model.clone();
        let generation = model.generation();
        assert!(model.add_binding_with_positions(&tx, positions).is_err());
        assert!(model.authored_eq(&before).unwrap());
        assert_eq!(model.generation(), generation);
    }
    model
        .add_binding_with_positions(&tx, vec![vec![0.5]])
        .unwrap();
    let before = model.clone();
    for value in [f32::NAN, f32::INFINITY, -1.0, 2.0] {
        assert!(model.key_move(&tx, &param, 0, value).is_err());
        assert!(model.key_insert(&tx, &param, value).is_err());
        assert!(model.authored_eq(&before).unwrap());
    }
    assert!(model.key_delete(&tx, &param, 0).is_err());
    assert!(model.key_move(&tx, &param, usize::MAX, 0.4).is_err());
    assert!(model.key_insert(&tx, &param, 0.5).is_err());
    assert!(model.authored_eq(&before).unwrap());
    model
        .add_binding_with_positions(&tx, vec![vec![0.0, 1.0]])
        .unwrap();
    assert!(model.authored_eq(&before).unwrap());
}

#[test]
fn raw_writes_author_only_the_selected_scalar_or_empty_deform_cell() {
    let (mut model, param, node) = fixture();
    let tx = key(&param, &node, ScalarTarget::Tx);
    model.set_binding_key(&tx, [1, 0], 8.0).unwrap();
    let cells = model.binding(&tx).unwrap().values().scalar_cells().unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].x, 1);
    assert_eq!(model.eval_scalar(&tx, &Pose::new()), Some(8.0));
    let deform = BindingKey::new(param, node, BindingTarget::Deform);
    model.set_deform_vertices(&deform, [1, 0], vec![]).unwrap();
    let cells = model
        .binding(&deform)
        .unwrap()
        .values()
        .deform_cells()
        .unwrap();
    assert_eq!(cells.len(), 1);
    assert!(cells[0].value.is_empty());
    model.unset_binding_key(&deform, [1, 0]).unwrap();
    assert!(model
        .binding(&deform)
        .unwrap()
        .values()
        .deform_cells()
        .unwrap()
        .is_empty());
}

#[test]
fn grid_budget_and_file_validation_use_binding_axes() {
    let (mut model, x, node) = fixture();
    let y = ParamId::new("cross").unwrap();
    model
        .add_param_with_id(
            y.clone(),
            ModelParam::new(Name::truncated("cross"), 0.0, 1.0, 0.0),
        )
        .unwrap();
    let binding = BindingKey::pair(x.clone(), y, node, BindingTarget::Scalar(ScalarTarget::Tx));
    let axis: Vec<_> = (0..256).map(|i| i as f32 / 255.0).collect();
    model
        .add_binding_with_positions(&binding, vec![axis.clone(), axis])
        .unwrap();
    assert_eq!(model.binding_grid(&binding).unwrap(), (256, 256));
    let before = model.clone();
    assert!(model.key_insert(&binding, &x, 0.5).is_err());
    assert!(model.authored_eq(&before).unwrap());

    let file = model.to_clm_file().unwrap();
    assert!(Model::from_clm_file(&file).is_ok());
    for bad_axes in [
        vec![vec![]],
        vec![vec![0.0], vec![1.0, 0.0]],
        vec![vec![0.0], vec![f32::NAN]],
    ] {
        let mut bad = file.clone();
        bad.doc.bindings[0].key_positions = bad_axes;
        assert!(Model::from_clm_file(&bad).is_err());
    }
    let mut bad = file.clone();
    bad.doc.bindings[0].values = clm::ClmBindingValues::TransformTX(clm::ClmCells {
        cells: vec![clm::ClmCell {
            x: 256,
            y: 0,
            value: 1.0,
        }],
    });
    assert!(Model::from_clm_file(&bad).is_err());
    bad.doc.bindings[0].values = clm::ClmBindingValues::TransformTX(clm::ClmCells {
        cells: vec![
            clm::ClmCell {
                x: 0,
                y: 0,
                value: 1.0
            };
            2
        ],
    });
    assert!(Model::from_clm_file(&bad).is_err());
}
