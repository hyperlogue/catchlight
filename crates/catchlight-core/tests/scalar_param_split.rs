#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The `.clp` v0 bridge, checked against the runtime that still reads v0.
//!
//! A 2-D param in a file becomes two scalar params plus a two-param binding in
//! a [`Model`]. That is only lossless if the pair evaluates to what the 2-D
//! param evaluated to, so this drives the *same file* two ways — through the
//! legacy runtime's `Param::apply` and through `Model::eval_scalar` — and
//! compares them across the grid.

use catchlight_core::formats::clp::{
    ClpBinding, ClpBindingValues, ClpCell, ClpCells, ClpDocument, ClpFile, ClpIndices, ClpMesh,
    ClpNode, ClpNodeKind, ClpParam, ClpPart, ClpPhysics, ClpTransform, FORMAT_VERSION,
};
use catchlight_core::params::InterpolateMode;
use catchlight_core::{BindingKey, BindingParams, BindingTarget, Model, Pose, ScalarTarget};

/// One part under the root, driven by one 2-D param whose z-order binding is
/// authored at four scattered cells of a 3x3 grid (so the fill has work to do).
fn two_dimensional_file(mode: InterpolateMode) -> ClpFile {
    let part = ClpPart {
        mesh: ClpMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 0.0, 1.0],
            uvs: vec![0.0; 6],
            indices: ClpIndices::U16(vec![0, 1, 2]),
            origin: [0.0, 0.0],
        },
        albedo: u32::MAX,
        opacity: 1.0,
        blend_mode: catchlight_core::components::BlendMode::Normal,
        tint: [1.0; 3],
        screen_tint: [0.0; 3],
        masks: Vec::new(),
        mask_threshold: 0.5,
    };
    let cell = |x: u32, y: u32, value: f32| ClpCell { x, y, value };
    ClpFile {
        version: FORMAT_VERSION,
        doc: ClpDocument {
            physics: ClpPhysics::default(),
            nodes: vec![
                ClpNode {
                    parent: None,
                    name: "Root".into(),
                    enabled: true,
                    z_order: 0.0,
                    transform: ClpTransform {
                        translation: [0.0; 3],
                        rotation: [0.0; 3],
                        scale: [1.0, 1.0],
                    },
                    lock_to_root: false,
                    kind: ClpNodeKind::Group,
                },
                ClpNode {
                    parent: Some(0),
                    name: "Body".into(),
                    enabled: true,
                    z_order: 3.0,
                    transform: ClpTransform {
                        translation: [0.0; 3],
                        rotation: [0.0; 3],
                        scale: [1.0, 1.0],
                    },
                    lock_to_root: false,
                    kind: ClpNodeKind::Part(part),
                },
            ],
            params: vec![ClpParam {
                name: "Head".into(),
                is_vec2: true,
                min: [-1.0, -2.0],
                max: [1.0, 2.0],
                defaults: [0.0, 0.0],
                axis_points_x: vec![0.0, 0.25, 1.0],
                axis_points_y: vec![0.0, 0.5, 1.0],
                bindings: vec![ClpBinding {
                    node: 1,
                    interpolate_mode: mode,
                    values: ClpBindingValues::ZOrder(ClpCells {
                        cells: vec![
                            cell(0, 0, -4.0),
                            cell(2, 0, 6.0),
                            cell(1, 2, 2.5),
                            cell(2, 2, -1.5),
                        ],
                    }),
                }],
            }],
            welds: Vec::new(),
        },
        textures: Vec::new(),
    }
}

/// The z order the legacy runtime's 2-D param folds onto the part at `(x, y)`,
/// as a delta from the part's authored z order.
fn runtime_contribution(file: &ClpFile, x: f32, y: f32) -> f32 {
    let mut puppet = catchlight_core::from_clp(file, 0).unwrap();
    puppet.apply_pose_overlay(&[("Head", glam::Vec2::new(x, y))]);
    puppet.reset_dynamic_state();
    puppet.apply_params();
    let node = puppet
        .node_for_uuid(1)
        .and_then(|id| puppet.get(id))
        .expect("the part is in the arena");
    node.z_order - node.base_z_order
}

fn model_contribution(model: &Model, key: &BindingKey, x: f32, y: f32) -> f32 {
    let params = key.params.clone();
    let BindingParams::Two(px, py) = params else {
        panic!("the split should have produced a two-param binding");
    };
    let pose: Pose = [(px, x), (py, y)].into_iter().collect();
    model
        .eval_scalar(key, &pose)
        .expect("the binding evaluates")
}

fn split_binding(model: &Model) -> BindingKey {
    let key = model
        .bindings()
        .next()
        .expect("the binding survived the split")
        .key()
        .clone();
    assert!(
        matches!(key.params, BindingParams::Two(_, _)),
        "a 2-D param's binding must span both halves",
    );
    key
}

#[test]
fn a_split_two_dimensional_param_evaluates_as_it_did() {
    for mode in [
        InterpolateMode::Linear,
        InterpolateMode::Nearest,
        InterpolateMode::Stepped,
        InterpolateMode::Cubic,
    ] {
        let file = two_dimensional_file(mode);
        let model = Model::from_clp_file(&file).unwrap();
        let key = split_binding(&model);

        // A grid of poses across (and just past) the param's box.
        for i in 0..=8 {
            for j in 0..=8 {
                let x = -1.5 + 3.0 * i as f32 / 8.0;
                let y = -3.0 + 6.0 * j as f32 / 8.0;
                let runtime = runtime_contribution(&file, x, y);
                let model_value = model_contribution(&model, &key, x, y);
                assert!(
                    (runtime - model_value).abs() < 1e-4,
                    "{mode:?} at ({x}, {y}): runtime {runtime}, model {model_value}",
                );
            }
        }
    }
}

/// The split has to survive a save: an editor that opened an imported model
/// and saved it without editing must write the same bytes back.
#[test]
fn splitting_and_re_pairing_is_byte_stable() {
    let file = two_dimensional_file(InterpolateMode::Linear);
    let model = Model::from_clp_file(&file).unwrap();
    assert_eq!(model.flatten().unwrap(), file);

    let bytes = model.to_clp_bytes().unwrap();
    let reopened = Model::from_clp_bytes(&bytes).unwrap();
    assert_eq!(reopened.to_clp_bytes().unwrap(), bytes);
}

/// A one-param binding is the same evaluation with the second axis collapsed,
/// which is what makes a scalar param a strict simplification rather than a
/// loss.
#[test]
fn a_one_dimensional_param_still_evaluates_as_it_did() {
    let mut file = two_dimensional_file(InterpolateMode::Linear);
    file.doc.params[0].is_vec2 = false;
    file.doc.params[0].axis_points_y = vec![0.0];
    let ClpBindingValues::ZOrder(cells) = &mut file.doc.params[0].bindings[0].values else {
        panic!("the fixture binds z order");
    };
    cells.cells.retain(|c| c.y == 0);

    let model = Model::from_clp_file(&file).unwrap();
    let key = model.bindings().next().unwrap().key().clone();
    let BindingParams::One(param) = key.params.clone() else {
        panic!("a 1-D param keeps a one-param binding");
    };

    for i in 0..=8 {
        let x = -1.5 + 3.0 * i as f32 / 8.0;
        let runtime = runtime_contribution(&file, x, 0.0);
        let pose: Pose = [(param.clone(), x)].into_iter().collect();
        let model_value = model.eval_scalar(&key, &pose).unwrap();
        assert!(
            (runtime - model_value).abs() < 1e-4,
            "at {x}: runtime {runtime}, model {model_value}",
        );
    }
}

/// A param the pose does not mention reads its default, so a partial pose is a
/// legal pose and evaluating one is not an error.
#[test]
fn an_unposed_param_reads_its_default() {
    let mut file = two_dimensional_file(InterpolateMode::Linear);
    file.doc.params[0].defaults = [1.0, 2.0];
    let model = Model::from_clp_file(&file).unwrap();
    let key = split_binding(&model);
    let BindingParams::Two(px, py) = key.params.clone() else {
        unreachable!()
    };

    let full: Pose = [(px, 1.0), (py, 2.0)].into_iter().collect();
    assert_eq!(
        model.eval_scalar(&key, &Pose::new()),
        model.eval_scalar(&key, &full),
    );
}

/// Nothing in the model is addressed by a target it does not have.
#[test]
fn evaluating_the_wrong_target_is_none() {
    let file = two_dimensional_file(InterpolateMode::Linear);
    let model = Model::from_clp_file(&file).unwrap();
    let key = split_binding(&model);
    assert!(model.eval_deform(&key, &Pose::new()).is_none());

    let missing = BindingKey::new(
        model.param_ids()[0].clone(),
        model.root().clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    assert!(model.eval_scalar(&missing, &Pose::new()).is_none());
}
