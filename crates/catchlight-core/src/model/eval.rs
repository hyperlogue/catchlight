//! Evaluating a model's bindings at a pose.
//!
//! A [`Pose`] is a plain map from [`ParamId`] to a value — that is what scalar
//! params buy. Evaluation locates each of a binding's one or two params on its
//! key positions, then interpolates the binding's dense grid: linearly along
//! one param, bilinearly across two, and the same four modes the runtime folds
//! with. `bracket` and `frac` come from [`crate::params`], so a Model and a
//! puppet locate the same pose in the same place.

use std::collections::HashMap;

use crate::fill::derive_dense;
use crate::id::ParamId;
use crate::params::{bracket, cubic, frac, InterpolateMode};

use super::binding::{deform_cells, scalar_cells};
use super::{BindingKey, BindingTarget, Model, ModelError};

/// An assignment of values to a model's params. A param the pose does not
/// mention reads its default, so a partial pose is a legal pose.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pose(HashMap<ParamId, f32>);

impl Pose {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, param: ParamId, value: f32) {
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
        Self(iter.into_iter().collect())
    }
}

/// Where a pose sits on one param's key positions: the bracketing indices and
/// how far between them it fell.
struct Located {
    lo: usize,
    hi: usize,
    frac: f32,
}

/// A value a binding grid can hold and interpolation can mix.
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
    /// The value a scalar binding contributes at `pose`, or `None` when the
    /// key names no binding (or names a deform one).
    pub fn eval_scalar(&self, key: &BindingKey, pose: &Pose) -> Option<f32> {
        let BindingTarget::Scalar(target) = key.target else {
            return None;
        };
        let binding = self.binding(key)?;
        let (w, h) = self.binding_grid(key).ok()?;
        let (axis_x, axis_y) = self.binding_axes(key).ok()?;
        let cells: Vec<((u32, u32), f32)> = scalar_cells(binding.values())?
            .iter()
            .map(|c| ((c.x, c.y), c.value))
            .collect();
        let dense = derive_dense(
            w as usize,
            h as usize,
            axis_x,
            axis_y,
            &cells,
            &target.identity(),
        );
        self.interpolate(key, &dense, w as usize, h as usize, pose)
    }

    /// The per-vertex offsets a deform binding contributes at `pose`, flat
    /// `[dx, dy, …]`, or `None` when the key names no deform binding.
    pub fn eval_deform(&self, key: &BindingKey, pose: &Pose) -> Option<Vec<f32>> {
        if key.target != BindingTarget::Deform {
            return None;
        }
        let binding = self.binding(key)?;
        let (w, h) = self.binding_grid(key).ok()?;
        let (axis_x, axis_y) = self.binding_axes(key).ok()?;
        let identity = vec![0.0f32; self.deform_len(&key.node)];
        let cells: Vec<((u32, u32), Vec<f32>)> = deform_cells(binding.values())?
            .iter()
            .map(|c| ((c.x, c.y), c.value.clone()))
            .collect();
        let dense = derive_dense(w as usize, h as usize, axis_x, axis_y, &cells, &identity);
        self.interpolate(key, &dense, w as usize, h as usize, pose)
    }

    /// Where `pose` puts a param on its own key positions.
    fn locate(&self, param: &ParamId, pose: &Pose) -> Result<Located, ModelError> {
        let p = self.param(param).ok_or(ModelError::UnknownParam)?;
        if p.key_positions.is_empty() {
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
        let (lo, hi) = bracket(&p.key_positions, normed);
        Ok(Located {
            lo,
            hi,
            frac: frac(normed, p.key_positions[lo], p.key_positions[hi]),
        })
    }

    /// Interpolate a dense grid at `pose`, in the binding's own mode.
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
        let x = self.locate(key.params.x(), pose).ok()?;
        let y = match key.params.y() {
            Some(p) => self.locate(p, pose).ok()?,
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
                // Hold the bracket's lower cell, except at the very top of an
                // axis, where the bracket stops advancing and the last cell
                // would otherwise be unreachable.
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
                // The spline reaches one cell past the bracket on each side;
                // clamping the outer indices makes edge cells degrade to
                // linear rather than index out of the grid.
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
mod tests {
    use super::*;
    use crate::formats::clp::{ClpIndices, ClpMesh};
    use crate::id::{Name, SeededHex};
    use crate::model::{
        BindingParams, ModelNode, ModelNodeKind, ModelParam, ModelPart, ScalarTarget,
    };

    /// A part driven by two params, each with key positions at 0 / 0.5 / 1 over
    /// the range [0, 1].
    fn rig() -> (Model, BindingKey, ParamId, ParamId) {
        let mut hex = SeededHex::new(13);
        let mut model = Model::new();
        let root = model.root().clone();
        let node = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClpMesh {
                        verts: vec![0.0, 0.0, 1.0, 0.0],
                        uvs: vec![0.0; 4],
                        indices: ClpIndices::U16(Vec::new()),
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
                        key_positions: vec![0.0, 0.5, 1.0],
                    },
                    &mut hex,
                )
                .unwrap()
        };
        let x = add("head.x", &mut model);
        let y = add("head.y", &mut model);
        let key = BindingKey::pair(x.clone(), y.clone(), node, BindingTarget::Deform);
        (model, key, x, y)
    }

    fn pose(x: (&ParamId, f32), y: (&ParamId, f32)) -> Pose {
        [(x.0.clone(), x.1), (y.0.clone(), y.1)]
            .into_iter()
            .collect()
    }

    /// The whole point of a two-param binding: the corners are authored
    /// independently and the middle is the bilinear blend of all four.
    #[test]
    fn a_two_param_deform_binding_is_bilinear_over_its_grid() {
        let (mut model, key, x, y) = rig();
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

        // Each corner evaluates to what was authored there.
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
        // The centre is the blend of all four.
        let mid = model
            .eval_deform(&key, &pose((&x, 0.5), (&y, 0.5)))
            .unwrap();
        assert!((mid[0] - 5.0).abs() < 1e-5, "{mid:?}");
        assert!((mid[1] - 10.0).abs() < 1e-5, "{mid:?}");
    }

    /// One param moving must move the value on its own axis and nothing else —
    /// the pair is a grid, not two independent contributions.
    #[test]
    fn each_param_drives_its_own_axis() {
        let (mut model, key, x, y) = rig();
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

    /// A scalar binding on one param is the same evaluation with the second
    /// axis collapsed.
    #[test]
    fn a_one_param_scalar_binding_is_linear() {
        let (mut model, _, x, _) = rig();
        let node = model
            .nodes_in_order()
            .into_iter()
            .find(|id| id != model.root())
            .unwrap();
        let key = BindingKey::new(x.clone(), node, BindingTarget::Scalar(ScalarTarget::ZOrder));
        model.set_binding_key(&key, [0, 0], -2.0).unwrap();
        model.set_binding_key(&key, [2, 0], 6.0).unwrap();
        assert!(matches!(key.params, BindingParams::One(_)));

        let at = |v: f32| {
            model
                .eval_scalar(&key, &[(x.clone(), v)].into_iter().collect())
                .unwrap()
        };
        assert!((at(0.0) + 2.0).abs() < 1e-5);
        assert!((at(1.0) - 6.0).abs() < 1e-5);
        // The middle key position derives to the midpoint (2.0), so a quarter
        // of the way along lands halfway between -2 and 2.
        assert!((at(0.5) - 2.0).abs() < 1e-5, "{}", at(0.5));
        assert!(at(0.25).abs() < 1e-5, "{}", at(0.25));
    }
}
