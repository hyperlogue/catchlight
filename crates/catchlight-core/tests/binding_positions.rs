#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Binding-owned sampling grids preserve independent authoring over shared
//! inputs. Version-2 container fixtures exercise binding positions and asset
//! preservation through decoding.

use catchlight_core::formats::{clm, container};
use catchlight_core::id::SeededHex;
use catchlight_core::{
    BindingKey, BindingTarget, ExtensionKey, ExtensionValue, InterpolateMode, Model, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelTexture, Name, NodeId, ParamId, Pose, Puppet,
    ScalarTarget,
};
use serde_json::{json, Value};

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
    let cells = catchlight_core::scalar_cells(model.binding(&binding).unwrap().values()).unwrap();
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
    assert!(
        catchlight_core::scalar_cells(model.binding(&binding).unwrap().values())
            .unwrap()
            .is_empty()
    );
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
    // Exact creation is idempotent and cannot replace an existing grid.
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
    let cells = catchlight_core::scalar_cells(model.binding(&tx).unwrap().values()).unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0].x, 1);
    assert_eq!(model.eval_scalar(&tx, &Pose::new()), Some(8.0));
    let deform = BindingKey::new(param, node, BindingTarget::Deform);
    model.set_deform_vertices(&deform, [1, 0], vec![]).unwrap();
    let cells = catchlight_core::deform_cells(model.binding(&deform).unwrap().values()).unwrap();
    assert_eq!(cells.len(), 1);
    assert!(cells[0].value.is_empty());
    model.unset_binding_key(&deform, [1, 0]).unwrap();
    assert!(
        catchlight_core::deform_cells(model.binding(&deform).unwrap().values())
            .unwrap()
            .is_empty()
    );
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

/// Rewrite only the Structure section into the historical v2 shape, retaining
/// every encoded asset section byte for byte.
fn v2_bytes(bytes: &[u8], axes: Value, omit_params: bool) -> Vec<u8> {
    let parsed = container::read(bytes, &clm::MAGIC).unwrap();
    let mut structure: Value = ciborium::from_reader(parsed.section(0).unwrap()).unwrap();
    for param in structure["params"].as_array_mut().unwrap() {
        param["key_positions"] = axes.clone();
    }
    if omit_params {
        structure["params"] = json!([]);
    }
    for binding in structure["bindings"].as_array_mut().unwrap() {
        binding.as_object_mut().unwrap().remove("key_positions");
    }
    let mut payload = Vec::new();
    ciborium::into_writer(&structure, &mut payload).unwrap();
    let sections: Vec<_> = parsed
        .sections
        .iter()
        .map(|s| container::Section {
            kind: s.kind,
            data: if s.kind == 0 { &payload } else { s.data },
        })
        .collect();
    container::write(&clm::MAGIC, 2, &sections)
}

#[test]
fn v2_migration_preserves_samples_assets_and_structure_feed() {
    let (mut model, param, node) = fixture();
    let tx = key(&param, &node, ScalarTarget::Tx);
    model
        .add_binding_with_positions(&tx, vec![vec![0.0, 0.25, 1.0]])
        .unwrap();
    model.set_binding_key(&tx, [0, 0], -2.0).unwrap();
    model.set_binding_key(&tx, [2, 0], 6.0).unwrap();
    let mut ids = SeededHex::new(84);
    model
        .add_texture(
            &node,
            ModelTexture {
                encoding: clm::TextureEncoding::Tga,
                alpha: clm::TextureAlpha::Straight,
                data: vec![3, 1, 4, 1, 5].into(),
            },
            &mut ids,
        )
        .unwrap();
    model
        .set_extension(
            ExtensionKey::new("tests.binary").unwrap(),
            ExtensionValue::Bytes(vec![9, 2, 6].into()),
        )
        .unwrap();
    let bytes = v2_bytes(
        &model.to_clm_bytes().unwrap(),
        json!([0.0, 0.25, 1.0]),
        false,
    );
    let migrated = Model::from_clm_bytes(&bytes).unwrap();
    assert!(model.authored_eq(&migrated).unwrap());
    for value in [0.0, 0.125, 0.25, 0.75, 1.0] {
        let pose = [(param.clone(), value)].into_iter().collect();
        assert_eq!(
            model.eval_scalar(&tx, &pose),
            migrated.eval_scalar(&tx, &pose)
        );
    }
    let out = migrated.to_clm_bytes().unwrap();
    assert_eq!(container::read(&out, &clm::MAGIC).unwrap().version, 3);
    assert_eq!(out, model.to_clm_bytes().unwrap());

    let feed = v2_bytes(
        &model.to_structure_bytes().unwrap(),
        json!([0.0, 0.25, 1.0]),
        false,
    );
    let decoded = clm::decode_structure(&feed).unwrap();
    assert_eq!(decoded.doc, model.to_clm_structure().unwrap());
    assert_eq!(clm::structure_texture_ids(&feed).unwrap(), decoded.textures);
}

#[test]
fn legacy_empty_axis_is_constant_and_missing_base_axis_is_an_error() {
    let (mut model, param, node) = fixture();
    let tx = key(&param, &node, ScalarTarget::Tx);
    model
        .add_binding_with_positions(&tx, vec![vec![0.0]])
        .unwrap();
    model.set_binding_key(&tx, [0, 0], 7.0).unwrap();
    let bytes = model.to_clm_bytes().unwrap();
    let migrated = Model::from_clm_bytes(&v2_bytes(&bytes, json!([]), false)).unwrap();
    assert_eq!(migrated.binding(&tx).unwrap().key_positions(), &[vec![0.0]]);
    assert_eq!(
        migrated.eval_scalar(&tx, &[(param.clone(), 0.7)].into_iter().collect()),
        Some(7.0)
    );
    assert!(
        matches!(clm::decode(&v2_bytes(&bytes, json!([]), true)), Err(clm::ClmError::LegacyBindingAxisMissing { node: n, param: p }) if n == node.to_string() && p == param.to_string())
    );
}

#[test]
fn legacy_shared_axes_are_preflighted_before_expansion_without_double_charge() {
    use catchlight_core::load_budget::{
        charge_clm_structure, LoadBudget, LoadLimits, LoadResource,
    };
    let (mut model, param, node) = fixture();
    for target in [ScalarTarget::Tx, ScalarTarget::Ty] {
        model.add_binding(&key(&param, &node, target)).unwrap();
    }
    let axes = json!([0.0, 0.25, 0.5, 0.75, 1.0]);
    let full = v2_bytes(&model.to_clm_bytes().unwrap(), axes.clone(), false);
    let feed = v2_bytes(&model.to_structure_bytes().unwrap(), axes, false);
    let budget = |cells| {
        LoadBudget::new(LoadLimits {
            binding_cells: cells,
            ..LoadLimits::default()
        })
    };
    // Each five-position source axis expands to five cells in each binding.
    for structure_only in [false, true] {
        let mut too_small = budget(9);
        let result = if structure_only {
            clm::decode_structure_with_budget(&feed, &mut too_small).map(|file| file.doc)
        } else {
            clm::decode_with_budget(&full, &mut too_small).map(|file| file.doc)
        };
        assert!(
            matches!(result, Err(clm::ClmError::LoadLimit(error)) if error.resource == "binding cells" && error.got == 10)
        );
        let mut exact = budget(10);
        let doc = if structure_only {
            clm::decode_structure_with_budget(&feed, &mut exact)
                .unwrap()
                .doc
        } else {
            clm::decode_with_budget(&full, &mut exact).unwrap().doc
        };
        charge_clm_structure(&doc, &mut exact).unwrap();
        assert!(exact.charge(LoadResource::BindingCells, 1).is_err());
    }
    let mut used = budget(10);
    used.charge(LoadResource::BindingCells, 1).unwrap();
    assert!(clm::decode_with_budget(&full, &mut used).is_err());
}
