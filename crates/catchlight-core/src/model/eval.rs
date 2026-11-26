//! Evaluating a model's bindings at a pose.

use std::collections::HashMap;

use crate::id::ParamId;
use crate::interpolate::{InterpolateMode, cubic, frac};

use super::{BindingKey, BindingTarget, DenseGrid, Model, ModelError};

/// A param the pose does not mention reads its default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pose(HashMap<ParamId, f32>);

impl Pose {
    pub fn new() -> Self {
        Self::default()
    }

    /// A non-finite `value` is ignored: the param keeps what it had.
    pub fn set(&mut self, param: ParamId, value: f32) {
        if !value.is_finite() {
            return;
        }
        self.0.insert(param, value);
    }

    pub fn get(&self, param: &ParamId) -> Option<f32> {
        self.0.get(param).copied()
    }

    pub fn remove(&mut self, param: &ParamId) {
        self.0.remove(param);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ParamId, f32)> {
        self.0.iter().map(|(k, &v)| (k, v))
    }
}

impl FromIterator<(ParamId, f32)> for Pose {
    fn from_iter<T: IntoIterator<Item = (ParamId, f32)>>(iter: T) -> Self {
        Self(iter.into_iter().filter(|(_, v)| v.is_finite()).collect())
    }
}

struct Located {
    lo: usize,
    hi: usize,
    frac: f32,
}

trait Sample: Clone {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self;
    fn spline(p: [&Self; 4], t: f32) -> Self;
}

impl Sample for f32 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a + (b - a) * t
    }

    fn spline(p: [&Self; 4], t: f32) -> Self {
        cubic(*p[0], *p[1], *p[2], *p[3], t)
    }
}

impl Sample for Vec<f32> {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let n = a.len().max(b.len());
        (0..n)
            .map(|i| {
                let (x, y) = (
                    a.get(i).copied().unwrap_or(0.0),
                    b.get(i).copied().unwrap_or(0.0),
                );
                x + (y - x) * t
            })
            .collect()
    }

    fn spline(p: [&Self; 4], t: f32) -> Self {
        let n = p.iter().map(|v| v.len()).max().unwrap_or(0);
        (0..n)
            .map(|i| {
                let at = |k: usize| p[k].get(i).copied().unwrap_or(0.0);
                cubic(at(0), at(1), at(2), at(3), t)
            })
            .collect()
    }
}

impl Model {
    pub fn eval_scalar(&self, key: &BindingKey, pose: &Pose) -> Option<f32> {
        if !matches!(key.target, BindingTarget::Scalar(_)) {
            return None;
        }
        let (w, h) = self.binding_grid(key).ok()?;
        match &**self.binding_dense(key)? {
            DenseGrid::Scalar(dense) => self.interpolate(key, dense, w as usize, h as usize, pose),
            DenseGrid::Deform(_) => None,
        }
    }

    /// Flat `[dx, dy, …]`.
    pub fn eval_deform(&self, key: &BindingKey, pose: &Pose) -> Option<Vec<f32>> {
        if key.target != BindingTarget::Deform {
            return None;
        }
        let (w, h) = self.binding_grid(key).ok()?;
        match &**self.binding_dense(key)? {
            DenseGrid::Deform(dense) => self.interpolate(key, dense, w as usize, h as usize, pose),
            DenseGrid::Scalar(_) => None,
        }
    }

    fn locate(&self, param: &ParamId, axis: &[f32], pose: &Pose) -> Result<Located, ModelError> {
        let p = self.param(param).ok_or(ModelError::UnknownParam)?;
        if axis.is_empty() {
            return Ok(Located {
                lo: 0,
                hi: 0,
                frac: 0.0,
            });
        }
        let value = pose.get(param).unwrap_or(p.default);
        let span = p.max - p.min;
        let normed = if span.abs() > 1e-9 {
            ((value.clamp(p.min, p.max) - p.min) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (lo, hi) = crate::interpolate::binding_bracket(
            axis,
            normed,
            value == 0. && self.response_owner(param).is_some(),
        );
        Ok(Located {
            lo,
            hi,
            frac: frac(normed, axis[lo], axis[hi]),
        })
    }

    fn interpolate<T: Sample>(
        &self,
        key: &BindingKey,
        dense: &[T],
        w: usize,
        h: usize,
        pose: &Pose,
    ) -> Option<T> {
        if w == 0 || h == 0 || dense.len() < w * h {
            return None;
        }
        let (axis_x, axis_y) = self.binding_axes(key).ok()?;
        let x = self.locate(key.params.x(), &axis_x, pose).ok()?;
        let y = match key.params.y() {
            Some(p) => self.locate(p, &axis_y, pose).ok()?,
            None => Located {
                lo: 0,
                hi: 0,
                frac: 0.0,
            },
        };
        let (x0, x1) = (x.lo.min(w - 1), x.hi.min(w - 1));
        let (y0, y1) = (y.lo.min(h - 1), y.hi.min(h - 1));
        let (fx, fy) = (x.frac, y.frac);
        let at = |cx: usize, cy: usize| &dense[cy * w + cx];

        let mode = self.binding(key)?.interpolate_mode();
        Some(match mode {
            InterpolateMode::Nearest => {
                let cx = if fx < 0.5 { x0 } else { x1 };
                let cy = if fy < 0.5 { y0 } else { y1 };
                at(cx, cy).clone()
            }
            InterpolateMode::Stepped => {
                // At the very top of an axis the bracket stops advancing; without this the
                // last cell would be unreachable.
                let cx = if fx >= 1.0 && x1 + 1 >= w { x1 } else { x0 };
                let cy = if fy >= 1.0 && y1 + 1 >= h { y1 } else { y0 };
                at(cx, cy).clone()
            }
            InterpolateMode::Linear => {
                let top = T::lerp(at(x0, y0), at(x1, y0), fx);
                let bottom = T::lerp(at(x0, y1), at(x1, y1), fx);
                T::lerp(&top, &bottom, fy)
            }
            InterpolateMode::Cubic => {
                // Clamping the outer indices degrades edge cells to linear.
                let cx = [x0.saturating_sub(1), x0, x1, (x1 + 1).min(w - 1)];
                let cy = [y0.saturating_sub(1), y0, y1, (y1 + 1).min(h - 1)];
                let row = |r: usize| {
                    T::spline([at(cx[0], r), at(cx[1], r), at(cx[2], r), at(cx[3], r)], fx)
                };
                let rows = [row(cy[0]), row(cy[1]), row(cy[2]), row(cy[3])];
                T::spline([&rows[0], &rows[1], &rows[2], &rows[3]], fy)
            }
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::formats::clm::{ClmIndices, ClmMesh};
    use crate::id::{Name, SeededHex};
    use crate::model::{
        BindingParams, ModelNode, ModelNodeKind, ModelParam, ModelPart, ScalarTarget,
    };

    pub(super) fn fixture() -> (Model, BindingKey, ParamId, ParamId) {
        let mut hex = SeededHex::new(13);
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let node = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh {
                        verts: vec![0.0, 0.0, 1.0, 0.0],
                        uvs: vec![0.0; 4],
                        indices: ClmIndices::U16(Vec::new()),
                        origin: [0.0, 0.0],
                    })),
                ),
                &mut hex,
            )
            .unwrap();
        let mut add = |name: &str, model: &mut Model| {
            model
                .add_param(
                    ModelParam {
                        name: Name::truncated(name),
                        min: 0.0,
                        max: 1.0,
                        default: 0.0,
                    },
                    &mut hex,
                )
                .unwrap()
        };
        let x = add("head.x", &mut model);
        let y = add("head.y", &mut model);
        let key = BindingKey::pair(x.clone(), y.clone(), node, BindingTarget::Deform);
        model
            .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]; 2])
            .unwrap();
        (model, key, x, y)
    }

    fn pose(x: (&ParamId, f32), y: (&ParamId, f32)) -> Pose {
        [(x.0.clone(), x.1), (y.0.clone(), y.1)]
            .into_iter()
            .collect()
    }

    #[test]
    fn a_two_param_deform_binding_is_bilinear_over_its_grid() {
        let (mut model, key, x, y) = fixture();
        model
            .set_deform_vertices(&key, [0, 0], vec![0.0, 0.0, 0.0, 0.0])
            .unwrap();
        model
            .set_deform_vertices(&key, [2, 0], vec![10.0, 0.0, 10.0, 0.0])
            .unwrap();
        model
            .set_deform_vertices(&key, [0, 2], vec![0.0, 20.0, 0.0, 20.0])
            .unwrap();
        model
            .set_deform_vertices(&key, [2, 2], vec![10.0, 20.0, 10.0, 20.0])
            .unwrap();

        assert_eq!(
            model
                .eval_deform(&key, &pose((&x, 1.0), (&y, 0.0)))
                .unwrap(),
            vec![10.0, 0.0, 10.0, 0.0],
        );
        assert_eq!(
            model
                .eval_deform(&key, &pose((&x, 0.0), (&y, 1.0)))
                .unwrap(),
            vec![0.0, 20.0, 0.0, 20.0],
        );
        let mid = model
            .eval_deform(&key, &pose((&x, 0.5), (&y, 0.5)))
            .unwrap();
        assert!((mid[0] - 5.0).abs() < 1e-5, "{mid:?}");
        assert!((mid[1] - 10.0).abs() < 1e-5, "{mid:?}");
    }

    #[test]
    fn each_param_drives_its_own_axis() {
        let (mut model, key, x, y) = fixture();
        model.reset_binding_key(&key, [0, 0]).unwrap();
        model
            .set_deform_vertices(&key, [2, 0], vec![4.0, 0.0, 0.0, 0.0])
            .unwrap();
        let only_x = model
            .eval_deform(&key, &pose((&x, 1.0), (&y, 0.0)))
            .unwrap();
        let only_y = model
            .eval_deform(&key, &pose((&x, 0.0), (&y, 1.0)))
            .unwrap();
        assert!((only_x[0] - 4.0).abs() < 1e-5, "{only_x:?}");
        assert!(only_y[0].abs() < 1e-5, "{only_y:?}");
    }

    #[test]
    fn a_one_param_scalar_binding_is_linear() {
        let (mut model, _, x, _) = fixture();
        let node = model
            .nodes_in_order()
            .into_iter()
            .find(|id| Some(id) != model.root())
            .unwrap();
        let key = BindingKey::new(x.clone(), node, BindingTarget::Scalar(ScalarTarget::ZOrder));
        model
            .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        model.set_binding_key(&key, [0, 0], -2.0).unwrap();
        model.set_binding_key(&key, [2, 0], 6.0).unwrap();
        assert!(matches!(key.params, BindingParams::One(_)));

        let at = |v: f32| {
            model
                .eval_scalar(&key, &std::iter::once((x.clone(), v)).collect())
                .unwrap()
        };
        assert!((at(0.0) + 2.0).abs() < 1e-5);
        assert!((at(1.0) - 6.0).abs() < 1e-5);
        assert!((at(0.5) - 2.0).abs() < 1e-5, "{}", at(0.5));
        assert!(at(0.25).abs() < 1e-5, "{}", at(0.25));
    }
}

#[cfg(test)]
mod memo_tests {
    use super::tests::*;
    use super::*;
    use crate::formats::clm::{ClmIndices, ClmMesh};
    use crate::model::DenseGrid;

    /// The memo must be invalidated by authored cells, key positions, and the
    /// mesh a deform's identity is sized to.
    #[test]
    fn the_dense_grid_is_rebuilt_when_what_it_came_from_moves() {
        let (mut model, key, x, _) = fixture();
        model.reset_binding_key(&key, [0, 0]).unwrap();
        model
            .set_deform_vertices(&key, [2, 0], vec![4.0, 0.0, 0.0, 0.0])
            .unwrap();
        let at = |m: &Model| m.eval_deform(&key, &std::iter::once((x.clone(), 0.25)).collect());

        let before = at(&model).unwrap();
        assert_eq!(at(&model).unwrap(), before, "a second read is the memo");

        model
            .set_deform_vertices(&key, [1, 0], vec![9.0, 0.0, 0.0, 0.0])
            .unwrap();
        let after_cell = at(&model).unwrap();
        assert_ne!(after_cell, before, "the new keypoint reached the grid");

        model.key_move(&key, &x, 1, 0.9).unwrap();
        assert_ne!(at(&model).unwrap(), after_cell, "moving the key re-derived");

        let node = key.node.clone();
        model
            .set_node_mesh(
                &node,
                ClmMesh {
                    verts: vec![0.0, 0.0, 1.0, 0.0, 2.0, 0.0],
                    uvs: vec![0.0; 6],
                    indices: ClmIndices::U16(Vec::new()),
                    origin: [0.0, 0.0],
                },
            )
            .unwrap();
        assert_eq!(
            at(&model).unwrap().len(),
            6,
            "the grid follows the new vertex count",
        );
    }

    #[test]
    fn a_snapshot_keeps_the_grid_it_was_taken_with() {
        let (mut model, key, x, _) = fixture();
        model.reset_binding_key(&key, [0, 0]).unwrap();
        model
            .set_deform_vertices(&key, [2, 0], vec![4.0, 0.0, 0.0, 0.0])
            .unwrap();
        let pose: Pose = std::iter::once((x, 1.0)).collect();
        let snapshot = model.clone();
        let before = snapshot.eval_deform(&key, &pose).unwrap();

        model
            .set_deform_vertices(&key, [2, 0], vec![-4.0, 0.0, 0.0, 0.0])
            .unwrap();

        assert_eq!(snapshot.eval_deform(&key, &pose).unwrap(), before);
        assert_eq!(
            model.eval_deform(&key, &pose).unwrap(),
            vec![-4.0, 0.0, 0.0, 0.0],
        );
    }

    #[test]
    fn an_unauthored_binding_derives_the_identity_grid() {
        let (mut model, key, _, _) = fixture();
        model.add_binding(&key).unwrap();
        match &**model.binding_dense(&key).unwrap() {
            DenseGrid::Deform(cells) => {
                assert_eq!(cells.len(), 9);
                assert!(cells.iter().all(|c| c == &vec![0.0; 4]));
            }
            DenseGrid::Scalar(_) => panic!("a deform binding derives a deform grid"),
        }
    }
}
