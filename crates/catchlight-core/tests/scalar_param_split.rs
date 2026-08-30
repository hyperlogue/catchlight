#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The legacy bridge: a 2-D arena param becomes two scalar params plus a
//! two-param binding in a [`Model`].
//!
//! That is only lossless if the pair evaluates to what the 2-D param
//! evaluated to. The runtime that read the 2-D param is gone, so the
//! comparison is against a reference interpolator written out longhand
//! below — normalize the pose into the param's box, bracket it against the
//! key positions, interpolate the binding's dense grid — which is the whole
//! definition of a two-axis param and shares no code with the model's own
//! evaluator.

use catchlight_core::formats::clm::{
    ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmMesh, ClmPhysics, ClmTransform,
};
use catchlight_core::formats::legacy::{
    LegacyBinding, LegacyDocument, LegacyFile, LegacyNode, LegacyNodeKind, LegacyParam, LegacyPart,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::model::DenseGrid;
use catchlight_core::{BindingKey, BindingParams, BindingTarget, Model, Pose, ScalarTarget};

/// One part under the root, driven by one 2-D param whose z-order binding is
/// authored at four scattered cells of a 3x3 grid (so the fill has work to do).
fn two_dimensional_file(mode: InterpolateMode) -> LegacyFile {
    let part = LegacyPart {
        mesh: ClmMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 0.0, 1.0],
            uvs: vec![0.0; 6],
            indices: ClmIndices::U16(vec![0, 1, 2]),
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
    let cell = |x: u32, y: u32, value: f32| ClmCell { x, y, value };
    LegacyFile {
        doc: LegacyDocument {
            physics: ClmPhysics::default(),
            nodes: vec![
                LegacyNode {
                    parent: None,
                    name: "Root".into(),
                    enabled: true,
                    z_order: 0.0,
                    transform: ClmTransform {
                        translation: [0.0; 3],
                        rotation: [0.0; 3],
                        scale: [1.0, 1.0],
                    },
                    lock_to_root: false,
                    kind: LegacyNodeKind::Group,
                },
                LegacyNode {
                    parent: Some(0),
                    name: "Body".into(),
                    enabled: true,
                    z_order: 3.0,
                    transform: ClmTransform {
                        translation: [0.0; 3],
                        rotation: [0.0; 3],
                        scale: [1.0, 1.0],
                    },
                    lock_to_root: false,
                    kind: LegacyNodeKind::Part(part),
                },
            ],
            params: vec![LegacyParam {
                name: "Head".into(),
                is_vec2: true,
                min: [-1.0, -2.0],
                max: [1.0, 2.0],
                defaults: [0.0, 0.0],
                axis_points_x: vec![0.0, 0.25, 1.0],
                axis_points_y: vec![0.0, 0.5, 1.0],
                bindings: vec![LegacyBinding {
                    node: 1,
                    interpolate_mode: mode,
                    values: ClmBindingValues::ZOrder(ClmCells {
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

/// What a two-axis param at `(x, y)` is *defined* to produce, computed here
/// rather than asked of the model: normalize into the box the arena param
/// declared, bracket against its key positions, interpolate the binding's
/// dense grid in the authored mode.
///
/// `file` supplies the param's box and key positions (the model's, split in
/// two, must agree with them); the grid comes from the model because deriving
/// the unauthored cells is [`catchlight_core::fill`]'s job and has its own
/// tests.
fn reference_contribution(
    file: &LegacyFile,
    model: &Model,
    key: &BindingKey,
    mode: InterpolateMode,
    x: f32,
    y: f32,
) -> f32 {
    let p = &file.doc.params[0];
    let (w, h) = model.binding_grid(key).unwrap();
    let (w, h) = (w as usize, h as usize);
    let DenseGrid::Scalar(grid) = &**model.binding_dense(key).unwrap() else {
        panic!("the fixture binds z order, which is a scalar target");
    };
    assert_eq!(grid.len(), w * h);

    let norm = |v: f32, min: f32, max: f32| ((v - min) / (max - min)).clamp(0.0, 1.0);
    let tx = norm(x, p.min[0], p.max[0]);
    let ty = if p.is_vec2 {
        norm(y, p.min[1], p.max[1])
    } else {
        0.0
    };

    let axis_x = &p.axis_points_x;
    let axis_y = &p.axis_points_y;
    let (x0, x1, fx) = locate(axis_x, tx);
    let (y0, y1, fy) = locate(axis_y, ty);
    let at = |i: usize, j: usize| grid[j * w + i];

    match mode {
        InterpolateMode::Nearest => at(
            if fx < 0.5 { x0 } else { x1 },
            if fy < 0.5 { y0 } else { y1 },
        ),
        // Hold the bracket's lower cell, except at the very top of an axis,
        // where the bracket stops advancing and the last cell would otherwise
        // be unreachable.
        InterpolateMode::Stepped => at(
            if fx >= 1.0 && x1 + 1 >= w { x1 } else { x0 },
            if fy >= 1.0 && y1 + 1 >= h { y1 } else { y0 },
        ),
        InterpolateMode::Linear => {
            let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
            let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
            top + (bottom - top) * fy
        }
        InterpolateMode::Cubic => {
            // Catmull-Rom over the bracket plus one cell each side, with the
            // outer indices clamped so an edge cell degrades to linear.
            let cx = [x0.saturating_sub(1), x0, x1, (x1 + 1).min(w - 1)];
            let cy = [y0.saturating_sub(1), y0, y1, (y1 + 1).min(h - 1)];
            let row =
                |j: usize| catmull_rom(at(cx[0], j), at(cx[1], j), at(cx[2], j), at(cx[3], j), fx);
            catmull_rom(row(cy[0]), row(cy[1]), row(cy[2]), row(cy[3]), fy)
        }
    }
}

/// The bracketing key positions around `t` and how far between them it sits.
fn locate(axis: &[f32], t: f32) -> (usize, usize, f32) {
    let last = axis.len().saturating_sub(1);
    if last == 0 {
        return (0, 0, 0.0);
    }
    let (i0, i1) = (0..last)
        .find(|&i| t >= axis[i] && t <= axis[i + 1])
        .map(|i| (i, i + 1))
        .unwrap_or(if t < axis[0] {
            (0, 1)
        } else {
            (last - 1, last)
        });
    let (a, b) = (axis[i0], axis[i1]);
    let f = if (b - a).abs() < 1e-9 {
        0.0
    } else {
        ((t - a) / (b - a)).clamp(0.0, 1.0)
    };
    (i0, i1, f)
}

fn catmull_rom(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let (m1, m2) = ((p2 - p0) * 0.5, (p3 - p1) * 0.5);
    let (t2, t3) = (t * t, t * t * t);
    p1 * (2.0 * t3 - 3.0 * t2 + 1.0)
        + p2 * (-2.0 * t3 + 3.0 * t2)
        + m1 * (t3 - 2.0 * t2 + t)
        + m2 * (t3 - t2)
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
        let model = Model::from_legacy(&file).unwrap();
        let key = split_binding(&model);

        // A grid of poses across (and just past) the param's box.
        for i in 0..=8 {
            for j in 0..=8 {
                let x = -1.5 + 3.0 * i as f32 / 8.0;
                let y = -3.0 + 6.0 * j as f32 / 8.0;
                let expected = reference_contribution(&file, &model, &key, mode, x, y);
                let model_value = model_contribution(&model, &key, x, y);
                assert!(
                    (expected - model_value).abs() < 1e-4,
                    "{mode:?} at ({x}, {y}): expected {expected}, model {model_value}",
                );
            }
        }
    }
}

/// The split has to survive a round trip in both directions: back out to the
/// arena the legacy runtime reads, and out to `.clm` and in again.
#[test]
fn splitting_and_re_pairing_is_stable() {
    let file = two_dimensional_file(InterpolateMode::Linear);
    let model = Model::from_legacy(&file).unwrap();
    assert_eq!(model.to_legacy().unwrap(), file);

    let bytes = model.to_clm_bytes().unwrap();
    let reopened = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(reopened.to_clm_bytes().unwrap(), bytes);
    assert_eq!(reopened.to_legacy().unwrap(), file);
}

/// A one-param binding is the same evaluation with the second axis collapsed,
/// which is what makes a scalar param a strict simplification rather than a
/// loss.
#[test]
fn a_one_dimensional_param_still_evaluates_as_it_did() {
    let mut file = two_dimensional_file(InterpolateMode::Linear);
    file.doc.params[0].is_vec2 = false;
    file.doc.params[0].axis_points_y = vec![0.0];
    let ClmBindingValues::ZOrder(cells) = &mut file.doc.params[0].bindings[0].values else {
        panic!("the fixture binds z order");
    };
    cells.cells.retain(|c| c.y == 0);

    let model = Model::from_legacy(&file).unwrap();
    let key = model.bindings().next().unwrap().key().clone();
    let BindingParams::One(param) = key.params.clone() else {
        panic!("a 1-D param keeps a one-param binding");
    };

    for i in 0..=8 {
        let x = -1.5 + 3.0 * i as f32 / 8.0;
        let expected = reference_contribution(&file, &model, &key, InterpolateMode::Linear, x, 0.0);
        let pose: Pose = [(param.clone(), x)].into_iter().collect();
        let model_value = model.eval_scalar(&key, &pose).unwrap();
        assert!(
            (expected - model_value).abs() < 1e-4,
            "at {x}: expected {expected}, model {model_value}",
        );
    }
}

/// A param the pose does not mention reads its default, so a partial pose is a
/// legal pose and evaluating one is not an error.
#[test]
fn an_unposed_param_reads_its_default() {
    let mut file = two_dimensional_file(InterpolateMode::Linear);
    file.doc.params[0].defaults = [1.0, 2.0];
    let model = Model::from_legacy(&file).unwrap();
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
    let model = Model::from_legacy(&file).unwrap();
    let key = split_binding(&model);
    assert!(model.eval_deform(&key, &Pose::new()).is_none());

    let missing = BindingKey::new(
        model.param_ids()[0].clone(),
        model.root().unwrap().clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    assert!(model.eval_scalar(&missing, &Pose::new()).is_none());
}
