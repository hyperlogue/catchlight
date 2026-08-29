//! 2D parameters and the bindings they drive.
//!
//! **Param ids vs node ids.** `Param.id` and `AnimationLane { param_id, axis }`
//! (the latter in `crate::animation`) are the param namespace. The **node**
//! namespace is still spelled `uuid` (`Puppet::uuid_to_node`, `node_for_uuid`,
//! `insert_child(.., uuid: Option<u32>)`) and is a plain `u32` inherited from
//! inochi2d — not a UUID. Several param write paths (`set_param_value`,
//! `param_value`) also still name their argument `uuid`; it is a `Param.id`.
//!
//! **Colour targets only reach drawables.** `Opacity`, `Tint*` and
//! `ScreenTint*` fold into a part's or a composite's colour. A mesh group is
//! never drawn and has no colour, so a binding aiming one of those at a mesh
//! group has nowhere to land: the loader rejects it
//! ([`MeshGroupColorBindingError`]) instead of silently dropping it.

use crate::components::{NodeIdx, NodeKind};
use crate::deform::{DeformShapeError, DeformSource};
use glam::Vec2;

/// Which component of a `Vec2` param a binding or animation lane drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ParamAxis {
    X,
    Y,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpolateMode {
    Nearest,
    Linear,
    /// Hold the previous keyframe / axis-point value until the next one.
    /// The reference supports Stepped only on animation lanes (its binding
    /// interpolator aborts on it); for bindings catchlight holds the
    /// bracket's lower cell, the direct analogue of the lane semantics.
    Stepped,
    Cubic,
}

/// A 2D parameter drives bindings on one or more nodes. Callers set a
/// value (inside the param's [min, max] box); `apply` walks every
/// binding, bilinearly interpolates the stored values at the four
/// axis-point corners that bracket the value, and writes the result
/// into the target node.
///
/// For 1D params `axis_points_y` has a single element; the second
/// axis is effectively a no-op and bilinear collapses to linear.
#[derive(Debug, Clone)]
pub struct Param {
    pub id: u32,
    pub name: String,
    pub is_vec2: bool,
    pub min: Vec2,
    pub max: Vec2,
    pub defaults: Vec2,
    pub axis_points_x: Vec<f32>,
    pub axis_points_y: Vec<f32>,
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub node: NodeIdx,
    pub interpolate_mode: InterpolateMode,
    pub values: BindingValues,
    /// Per-cell flag: true when the cell contributes nothing (every Vec2
    /// is `Vec2::ZERO` for Deform; the f32 is `0.0` for additive scalar
    /// kinds; the f32 is `1.0` for multiplicative kinds — Scale,
    /// Opacity). Computed once at load. `Param::apply` AND-folds the 4
    /// corner cells around the current value to skip the per-vertex /
    /// per-target work when the bilinear output is guaranteed identity.
    pub cell_zero: Matrix<bool>,
}

impl Binding {
    pub fn new(node: NodeIdx, interpolate_mode: InterpolateMode, values: BindingValues) -> Self {
        let cell_zero = compute_cell_zero(&values);
        Self {
            node,
            interpolate_mode,
            values,
            cell_zero,
        }
    }

    pub fn cell_zero(&self, x: usize, y: usize) -> bool {
        let m = &self.cell_zero;
        if m.width == 0 || m.height == 0 {
            return false;
        }
        let cx = x.min(m.width - 1);
        let cy = y.min(m.height - 1);
        m.data[cy * m.width + cx]
    }

    /// Stepped holds the bracket's *lower* cell — except at the very top of an
    /// axis, where `bracket` returns `(last - 1, last)` for every value from
    /// the previous axis point onward, which would leave the final cell
    /// unreachable. `AnimationLane::value_at` holds the last keyframe the same
    /// way; this is the binding-side half of that.
    pub(crate) fn stepped_cell(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        fx: f32,
        fy: f32,
    ) -> (usize, usize) {
        let m = &self.cell_zero;
        let x = if fx >= 1.0 && x1 + 1 >= m.width {
            x1
        } else {
            x0
        };
        let y = if fy >= 1.0 && y1 + 1 >= m.height {
            y1
        } else {
            y0
        };
        (x, y)
    }

    fn interpolated_cell_zero(
        &self,
        x0: usize,
        x1: usize,
        y0: usize,
        y1: usize,
        fx: f32,
        fy: f32,
    ) -> bool {
        match self.interpolate_mode {
            InterpolateMode::Nearest => {
                let x = if fx < 0.5 { x0 } else { x1 };
                let y = if fy < 0.5 { y0 } else { y1 };
                self.cell_zero(x, y)
            }
            InterpolateMode::Stepped => {
                let (x, y) = self.stepped_cell(x0, x1, y0, y1, fx, fy);
                self.cell_zero(x, y)
            }
            InterpolateMode::Linear => {
                if fx < 1.0 && fy < 1.0 && !self.cell_zero(x0, y0) {
                    return false;
                }
                if fx > 0.0 && fy < 1.0 && !self.cell_zero(x1, y0) {
                    return false;
                }
                if fx < 1.0 && fy > 0.0 && !self.cell_zero(x0, y1) {
                    return false;
                }
                if fx > 0.0 && fy > 0.0 && !self.cell_zero(x1, y1) {
                    return false;
                }
                true
            }
            // Cubic mixes the bracket's neighbours via the tangent terms,
            // so a corner-only test can wrongly skip a binding whose inner
            // corners are identity but whose neighbours bend the curve off
            // it. Skip only when the whole 4x4 neighbourhood is identity;
            // `cell_zero` clamps the out-of-range outer indices.
            InterpolateMode::Cubic => {
                let cx = [x0.saturating_sub(1), x0, x1, x1 + 1];
                let cy = [y0.saturating_sub(1), y0, y1, y1 + 1];
                !cy.iter()
                    .any(|&y| cx.iter().any(|&x| !self.cell_zero(x, y)))
            }
        }
    }
}

fn compute_cell_zero(values: &BindingValues) -> Matrix<bool> {
    fn from_scalar(m: &Matrix<f32>, identity: f32) -> Matrix<bool> {
        Matrix {
            width: m.width,
            height: m.height,
            data: m.data.iter().map(|v| *v == identity).collect(),
        }
    }
    match values {
        BindingValues::Deform(dm) => Matrix {
            width: dm.width,
            height: dm.height,
            data: (0..dm.width * dm.height)
                .map(|cell| {
                    let start = cell * dm.vert_count;
                    dm.data[start..start + dm.vert_count]
                        .iter()
                        .all(|v| *v == Vec2::ZERO)
                })
                .collect(),
        },
        BindingValues::ZOrder(m)
        | BindingValues::TransformTX(m)
        | BindingValues::TransformTY(m)
        | BindingValues::TransformRX(m)
        | BindingValues::TransformRY(m)
        | BindingValues::TransformRZ(m)
        | BindingValues::ScreenTintR(m)
        | BindingValues::ScreenTintG(m)
        | BindingValues::ScreenTintB(m) => from_scalar(m, 0.0),
        BindingValues::TransformSX(m)
        | BindingValues::TransformSY(m)
        | BindingValues::Opacity(m)
        | BindingValues::TintR(m)
        | BindingValues::TintG(m)
        | BindingValues::TintB(m)
        | BindingValues::OutputScaleX(m)
        | BindingValues::OutputScaleY(m) => from_scalar(m, 1.0),
    }
}

/// Each `Matrix` is indexed `[x][y]`.
/// Dimensions must equal (axis_points_x.len(), axis_points_y.len()).
#[derive(Debug, Clone)]
pub enum BindingValues {
    ZOrder(Matrix<f32>),
    TransformTX(Matrix<f32>),
    TransformTY(Matrix<f32>),
    TransformSX(Matrix<f32>),
    TransformSY(Matrix<f32>),
    TransformRX(Matrix<f32>),
    TransformRY(Matrix<f32>),
    TransformRZ(Matrix<f32>),
    /// Per-vertex offsets keyed by (x_axis, y_axis), stored flat with a
    /// uniform per-cell stride so the fold walks contiguous memory. See
    /// [`DeformMatrix`].
    Deform(DeformMatrix),
    /// Multiplicative opacity factor folded into the target Part's or
    /// Composite's `opacity` field.
    Opacity(Matrix<f32>),
    /// Multiplicative per-channel tint factors (identity 1.0), folded
    /// into the target Part's or Composite's `tint`.
    TintR(Matrix<f32>),
    TintG(Matrix<f32>),
    TintB(Matrix<f32>),
    /// Additive per-channel screen-tint offsets (identity 0.0), folded
    /// into the target Part's or Composite's `screen_tint`.
    ScreenTintR(Matrix<f32>),
    ScreenTintG(Matrix<f32>),
    ScreenTintB(Matrix<f32>),
    /// Multiplicative factors (identity 1.0) folded into a SimplePhysics
    /// node's `offset_output_scale` (`offsetOutputScale *= value`,
    /// reset to (1,1) each frame in `beginUpdate`).
    OutputScaleX(Matrix<f32>),
    OutputScaleY(Matrix<f32>),
}

/// A binding aims a colour target (`Opacity`, `Tint*`, `ScreenTint*`) at a
/// mesh group, which has no colour to fold it into. The loader rejects the
/// file rather than drop the binding: a model that keys a mesh group's opacity
/// is broken in a way its author has to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("param {param}: {target} binding targets mesh group node {node}, which has no colour")]
pub struct MeshGroupColorBindingError {
    /// Index of the offending binding's param in the file's param table.
    pub param: u32,
    /// Index of the targeted mesh group in the file's node arena.
    pub node: u32,
    /// The binding's colour target: `opacity`, `tint.r`, `screen_tint.b`, ….
    pub target: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct Matrix<T> {
    pub width: usize,
    pub height: usize,
    pub data: Vec<T>,
}

impl<T: Clone + Default> Matrix<T> {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![T::default(); width * height],
        }
    }
}

impl<T> Matrix<T> {
    pub fn get(&self, x: usize, y: usize) -> &T {
        &self.data[y * self.width + x]
    }
}

/// Flat storage for a Deform binding's per-cell, per-vertex offsets: one
/// allocation instead of a `Vec` per axis-grid cell, so the hot fold walks
/// contiguous memory. Cell `(x, y)` occupies
/// `data[((y * width + x) * vert_count)..][..vert_count]`.
#[derive(Debug, Clone)]
pub struct DeformMatrix {
    pub width: usize,
    pub height: usize,
    /// Uniform per-cell vertex count; a well-formed rig's cells all hold
    /// the target's vertex count. Ragged imports are zero-padded to the
    /// longest cell (see [`DeformMatrix::from_cells`]).
    pub vert_count: usize,
    data: Vec<Vec2>,
}

impl DeformMatrix {
    /// Build from ragged per-cell offsets laid out `cells[y * width + x]`.
    /// `vert_count` becomes the longest cell and shorter cells are
    /// zero-padded to that stride, so a malformed rig with ragged lengths
    /// renders approximately (padded with zero offsets) instead of the
    /// fold skipping brackets — the render-don't-panic stance the rest of
    /// this module takes for malformed input.
    pub fn from_cells(
        width: usize,
        height: usize,
        cells: Vec<Vec<Vec2>>,
    ) -> Result<Self, DeformShapeError> {
        let expected = width
            .checked_mul(height)
            .ok_or(DeformShapeError::MatrixDimensionsOverflow { width, height })?;
        if cells.len() != expected {
            return Err(DeformShapeError::MatrixCellCount {
                width,
                height,
                expected,
                actual: cells.len(),
            });
        }
        let vert_count = cells.iter().map(Vec::len).max().unwrap_or(0);
        if cells.iter().any(|c| c.len() != vert_count) {
            tracing::warn!(
                "deform binding has ragged cell lengths (padding short cells to {})",
                vert_count
            );
        }
        let storage_len =
            expected
                .checked_mul(vert_count)
                .ok_or(DeformShapeError::MatrixStorageOverflow {
                    cells: expected,
                    vert_count,
                })?;
        let mut data = Vec::with_capacity(storage_len);
        for cell in &cells {
            let base = data.len();
            data.extend_from_slice(cell);
            data.resize(base + vert_count, Vec2::ZERO);
        }
        Ok(Self {
            width,
            height,
            vert_count,
            data,
        })
    }

    pub fn cell(&self, x: usize, y: usize) -> &[Vec2] {
        let start = (y * self.width + x) * self.vert_count;
        &self.data[start..start + self.vert_count]
    }

    /// All offsets, flat. Per-offset operations (all-zero test, Y
    /// reflection) don't care about cell boundaries, so the importer edits
    /// this directly.
    pub fn offsets(&self) -> &[Vec2] {
        &self.data
    }

    pub fn offsets_mut(&mut self) -> &mut [Vec2] {
        &mut self.data
    }
}

fn bracket(axis: &[f32], t: f32) -> (usize, usize) {
    let last = axis.len().saturating_sub(1);
    if last == 0 {
        return (0, 0);
    }
    for i in 0..last {
        if t >= axis[i] && t <= axis[i + 1] {
            return (i, i + 1);
        }
    }
    if t < axis[0] {
        (0, 1)
    } else {
        (last - 1, last)
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Uniform Catmull-Rom: the cubic Hermite from `p1`→`p2` (t in [0,1])
/// whose endpoint tangents are the central differences of the four
/// samples. Matches inmath's `cubic`, which the reference uses for both
/// Cubic parameter bindings and Cubic animation lanes — `AnimationLane`
/// shares this single copy. With a collapsed neighbourhood (p0==p1,
/// p2==p3) the tangent terms cancel and it reduces to `lerp`, so
/// boundary cells degrade to linear.
pub(crate) fn cubic<T>(p0: T, p1: T, p2: T, p3: T, t: f32) -> T
where
    T: Copy
        + std::ops::Add<Output = T>
        + std::ops::Sub<Output = T>
        + std::ops::Mul<f32, Output = T>,
{
    let m1 = (p2 - p0) * 0.5;
    let m2 = (p3 - p1) * 0.5;
    let t2 = t * t;
    let t3 = t2 * t;
    p1 * (2.0 * t3 - 3.0 * t2 + 1.0)
        + p2 * (-2.0 * t3 + 3.0 * t2)
        + m1 * (t3 - 2.0 * t2 + t)
        + m2 * (t3 - t2)
}

impl Param {
    /// Normalize `val` to each axis's [0, 1] range, clamped. Returns
    /// the normalized point plus the 4 bracketing indices in
    /// axis_points_x / axis_points_y.
    fn locate(&self, val: Vec2) -> (Vec2, (usize, usize), (usize, usize)) {
        let clamped = val.clamp(self.min, self.max);
        let span = self.max - self.min;
        let normed = Vec2::new(
            if span.x.abs() > 1e-9 {
                (clamped.x - self.min.x) / span.x
            } else {
                0.0
            },
            if span.y.abs() > 1e-9 {
                (clamped.y - self.min.y) / span.y
            } else {
                0.0
            },
        );
        let (x0, x1) = bracket(&self.axis_points_x, normed.x);
        let (y0, y1) = bracket(&self.axis_points_y, normed.y);
        (normed, (x0, x1), (y0, y1))
    }

    /// Fractional position of `t` inside [a, b]. Returns 0 if
    /// a == b so a degenerate (single-axis-point) axis doesn't NaN.
    fn frac(t: f32, a: f32, b: f32) -> f32 {
        if (b - a).abs() < 1e-9 {
            0.0
        } else {
            ((t - a) / (b - a)).clamp(0.0, 1.0)
        }
    }

    /// Apply this parameter at `val` to every binding. Pushes Deform
    /// contributions into the target node's DeformStack; non-Deform
    /// binding kinds are parsed but not yet wired through here.
    pub fn apply(&self, val: Vec2, puppet: &mut crate::puppet::Puppet) {
        self.apply_filtered(val, puppet, |_| true);
    }

    /// `apply` restricted to bindings whose values pass `include`. Lets
    /// the physics pre-pass skip Deform/Opacity/Tint work that the
    /// resets before the final apply would discard anyway.
    pub(crate) fn apply_filtered(
        &self,
        val: Vec2,
        puppet: &mut crate::puppet::Puppet,
        include: impl Fn(&BindingValues) -> bool,
    ) {
        // A well-formed param has at least one point per axis (the
        // importer guarantees this). Guard hand-built or malformed
        // params so the axis indexing below can't panic.
        if self.axis_points_x.is_empty() || self.axis_points_y.is_empty() {
            return;
        }
        let (normed, (x0, x1), (y0, y1)) = self.locate(val);
        let fx = Self::frac(normed.x, self.axis_points_x[x0], self.axis_points_x[x1]);
        let fy = Self::frac(normed.y, self.axis_points_y[y0], self.axis_points_y[y1]);

        for binding in &self.bindings {
            if !include(&binding.values) {
                continue;
            }
            // Bracket indices come from the param's axis-point counts; a
            // well-formed binding's value matrix has exactly those
            // dimensions. Clamp to the matrix's own dims so a malformed
            // import (mismatched or empty matrix) renders approximately
            // instead of panicking on an out-of-bounds index. `cell_zero`
            // mirrors the value matrix dimensions for every binding kind.
            let (mw, mh) = (binding.cell_zero.width, binding.cell_zero.height);
            if mw == 0 || mh == 0 {
                continue;
            }
            let (x0, x1) = (x0.min(mw - 1), x1.min(mw - 1));
            let (y0, y1) = (y0.min(mh - 1), y1.min(mh - 1));
            // Cubic samples the bracket's neighbours; clamp the outer
            // indices to the matrix so edge cells reuse the boundary
            // value (the spline then collapses to linear there).
            let cx = [x0.saturating_sub(1), x0, x1, (x1 + 1).min(mw - 1)];
            let cy = [y0.saturating_sub(1), y0, y1, (y1 + 1).min(mh - 1)];

            // Skip the binding entirely when every cell with non-zero
            // interpolation weight is identity, so the source stays
            // inactive and combine_deforms doesn't see it.
            if binding.interpolated_cell_zero(x0, x1, y0, y1, fx, fy) {
                continue;
            }
            let scalar = |m: &Matrix<f32>| -> f32 {
                match binding.interpolate_mode {
                    InterpolateMode::Nearest => {
                        let x = if fx < 0.5 { x0 } else { x1 };
                        let y = if fy < 0.5 { y0 } else { y1 };
                        *m.get(x, y)
                    }
                    InterpolateMode::Stepped => {
                        let (x, y) = binding.stepped_cell(x0, x1, y0, y1, fx, fy);
                        *m.get(x, y)
                    }
                    InterpolateMode::Linear => {
                        let top = lerp(*m.get(x0, y0), *m.get(x1, y0), fx);
                        let bot = lerp(*m.get(x0, y1), *m.get(x1, y1), fx);
                        lerp(top, bot, fy)
                    }
                    InterpolateMode::Cubic => {
                        let row = |y: usize| {
                            cubic(
                                *m.get(cx[0], y),
                                *m.get(cx[1], y),
                                *m.get(cx[2], y),
                                *m.get(cx[3], y),
                                fx,
                            )
                        };
                        cubic(row(cy[0]), row(cy[1]), row(cy[2]), row(cy[3]), fy)
                    }
                }
            };
            match &binding.values {
                BindingValues::Deform(dm) => {
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    let stack = match &mut node.kind {
                        NodeKind::Part(p) => &mut p.deform_stack,
                        NodeKind::MeshGroup(mg) => &mut mg.deform_stack,
                        _ => continue,
                    };

                    let vert_len = stack.vert_count;

                    // The uniform stride collapses the old per-cell length
                    // guard to one check: a binding whose cells are all
                    // shorter than the target is skipped (as before), while
                    // ragged cells were zero-padded at construction and now
                    // render approximately instead of skipping per bracket.
                    if dm.vert_count < vert_len {
                        continue;
                    }

                    match binding.interpolate_mode {
                        InterpolateMode::Cubic => {
                            // Per-vertex bicubic over the 4x4 neighbourhood.
                            let cells: [[&[Vec2]; 4]; 4] = std::array::from_fn(|k| {
                                std::array::from_fn(|j| dm.cell(cx[j], cy[k]))
                            });
                            let Some(out) = stack.param_buf_mut(DeformSource::Param(self.id), val)
                            else {
                                continue;
                            };
                            for i in 0..vert_len {
                                let row = |k: usize| {
                                    cubic(
                                        cells[k][0][i],
                                        cells[k][1][i],
                                        cells[k][2][i],
                                        cells[k][3][i],
                                        fx,
                                    )
                                };
                                out[i] = cubic(row(0), row(1), row(2), row(3), fy);
                            }
                        }
                        InterpolateMode::Nearest => {
                            let src = match (fx < 0.5, fy < 0.5) {
                                (true, true) => dm.cell(x0, y0),
                                (false, true) => dm.cell(x1, y0),
                                (true, false) => dm.cell(x0, y1),
                                (false, false) => dm.cell(x1, y1),
                            };
                            let Some(out) = stack.param_buf_mut(DeformSource::Param(self.id), val)
                            else {
                                continue;
                            };
                            out.copy_from_slice(&src[..vert_len]);
                        }
                        InterpolateMode::Stepped => {
                            let (sx, sy) = binding.stepped_cell(x0, x1, y0, y1, fx, fy);
                            let src = dm.cell(sx, sy);
                            let Some(out) = stack.param_buf_mut(DeformSource::Param(self.id), val)
                            else {
                                continue;
                            };
                            out.copy_from_slice(&src[..vert_len]);
                        }
                        InterpolateMode::Linear => {
                            let a = dm.cell(x0, y0);
                            let b = dm.cell(x1, y0);
                            let c = dm.cell(x0, y1);
                            let d = dm.cell(x1, y1);
                            let Some(out) = stack.param_buf_mut(DeformSource::Param(self.id), val)
                            else {
                                continue;
                            };
                            for i in 0..vert_len {
                                let top = a[i] + (b[i] - a[i]) * fx;
                                let bot = c[i] + (d[i] - c[i]) * fx;
                                out[i] = top + (bot - top) * fy;
                            }
                        }
                    }
                }
                BindingValues::ZOrder(m) => {
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.z_order += scalar(m);
                }
                BindingValues::TransformTX(m) => {
                    let s = scalar(m);
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.transform.translation.x += s;
                    puppet.mark_transform_dirty(binding.node);
                }
                BindingValues::TransformTY(m) => {
                    let s = scalar(m);
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.transform.translation.y += s;
                    puppet.mark_transform_dirty(binding.node);
                }
                BindingValues::TransformSX(m) => {
                    let s = scalar(m);
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.transform.scale.x *= s;
                    puppet.mark_transform_dirty(binding.node);
                }
                BindingValues::TransformSY(m) => {
                    let s = scalar(m);
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.transform.scale.y *= s;
                    puppet.mark_transform_dirty(binding.node);
                }
                BindingValues::TransformRX(m) => {
                    let s = scalar(m);
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.transform.rotation.x += s;
                    puppet.mark_transform_dirty(binding.node);
                }
                BindingValues::TransformRY(m) => {
                    let s = scalar(m);
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.transform.rotation.y += s;
                    puppet.mark_transform_dirty(binding.node);
                }
                BindingValues::TransformRZ(m) => {
                    let s = scalar(m);
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    node.transform.rotation.z += s;
                    puppet.mark_transform_dirty(binding.node);
                }
                BindingValues::Opacity(m) => {
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    let factor = scalar(m);
                    match &mut node.kind {
                        NodeKind::Part(p) => p.opacity *= factor,
                        NodeKind::Composite(c) => c.opacity *= factor,
                        _ => {}
                    }
                }
                BindingValues::TintR(m) | BindingValues::TintG(m) | BindingValues::TintB(m) => {
                    let factor = scalar(m);
                    let channel = match &binding.values {
                        BindingValues::TintR(_) => 0,
                        BindingValues::TintG(_) => 1,
                        _ => 2,
                    };
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    let tint = match &mut node.kind {
                        NodeKind::Part(p) => &mut p.tint,
                        NodeKind::Composite(c) => &mut c.tint,
                        _ => continue,
                    };
                    tint[channel] *= factor;
                }
                BindingValues::ScreenTintR(m)
                | BindingValues::ScreenTintG(m)
                | BindingValues::ScreenTintB(m) => {
                    let offset = scalar(m);
                    let channel = match &binding.values {
                        BindingValues::ScreenTintR(_) => 0,
                        BindingValues::ScreenTintG(_) => 1,
                        _ => 2,
                    };
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    let screen_tint = match &mut node.kind {
                        NodeKind::Part(p) => &mut p.screen_tint,
                        NodeKind::Composite(c) => &mut c.screen_tint,
                        _ => continue,
                    };
                    screen_tint[channel] += offset;
                }
                BindingValues::OutputScaleX(m) | BindingValues::OutputScaleY(m) => {
                    let factor = scalar(m);
                    let is_x = matches!(&binding.values, BindingValues::OutputScaleX(_));
                    let Some(node) = puppet.get_mut(binding.node) else {
                        continue;
                    };
                    let NodeKind::SimplePhysics(p) = &mut node.kind else {
                        continue;
                    };
                    if is_x {
                        p.offset_output_scale.x *= factor;
                    } else {
                        p.offset_output_scale.y *= factor;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deform_matrix_rejects_invalid_shape() {
        let err = DeformMatrix::from_cells(2, 2, vec![Vec::new(); 3]).unwrap_err();
        assert_eq!(
            err,
            DeformShapeError::MatrixCellCount {
                width: 2,
                height: 2,
                expected: 4,
                actual: 3,
            }
        );

        let err = DeformMatrix::from_cells(usize::MAX, 2, Vec::new()).unwrap_err();
        assert_eq!(
            err,
            DeformShapeError::MatrixDimensionsOverflow {
                width: usize::MAX,
                height: 2,
            }
        );
    }

    #[test]
    fn bracket_picks_surrounding_indices() {
        let axis = [0.0, 0.5, 1.0];
        assert_eq!(bracket(&axis, 0.25), (0, 1));
        assert_eq!(bracket(&axis, 0.75), (1, 2));
        assert_eq!(bracket(&axis, 0.0), (0, 1));
        assert_eq!(bracket(&axis, 1.0), (1, 2));
    }

    #[test]
    fn bracket_single_point_axis_returns_degenerate() {
        let axis = [0.0];
        assert_eq!(bracket(&axis, 0.7), (0, 0));
    }

    #[test]
    fn locate_normalizes_and_brackets_on_2d_param() {
        let p = Param {
            id: 0,
            name: String::new(),
            is_vec2: true,
            min: Vec2::new(-1.0, 0.0),
            max: Vec2::new(1.0, 10.0),
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![],
        };
        // (0.0, 5.0) — midpoint on X, midpoint on Y
        let (normed, (x0, x1), (y0, y1)) = p.locate(Vec2::new(0.0, 5.0));
        assert!((normed.x - 0.5).abs() < 1e-6);
        assert!((normed.y - 0.5).abs() < 1e-6);
        assert_eq!((x0, x1), (0, 1)); // 0.5 sits on axis-point boundary; bracket picks leftmost
        assert_eq!((y0, y1), (0, 1));
    }

    #[test]
    fn locate_clamps_out_of_range() {
        let p = Param {
            id: 0,
            name: String::new(),
            is_vec2: true,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![],
        };
        let (normed, _, _) = p.locate(Vec2::new(2.0, -5.0));
        assert_eq!(normed, Vec2::new(1.0, 0.0));
    }

    #[test]
    fn apply_with_empty_axis_does_not_panic() {
        use crate::puppet::Puppet;
        use crate::Node;

        // A malformed param with a zero-point x-axis must not panic when
        // apply indexes axis_points_x; it should no-op instead.
        let mut puppet = Puppet::new();
        let node = Node {
            z_order: 1.0,
            base_z_order: 1.0,
            ..Default::default()
        };
        let id = puppet.insert_child(puppet.root(), node, Some(3));
        let param = Param {
            id: 1,
            name: "p".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                id,
                InterpolateMode::Linear,
                BindingValues::ZOrder(Matrix {
                    width: 2,
                    height: 1,
                    data: vec![0.0, 5.0],
                }),
            )],
        };
        puppet.set_params(vec![param]);
        puppet.set_param_value(1, Vec2::new(0.5, 0.0));
        puppet.reset_dynamic_state();
        puppet.apply_params();
        assert_eq!(
            puppet.get(id).unwrap().z_order,
            1.0,
            "empty axis must no-op"
        );
    }

    #[test]
    fn apply_with_matrix_smaller_than_axes_clamps_without_panic() {
        use crate::puppet::Puppet;
        use crate::Node;

        // 3 x-axis points but a 2-wide value matrix (dimension mismatch).
        // At the far end the bracket selects (1, 2); both must clamp to
        // the matrix width (1) instead of indexing out of bounds.
        let mut puppet = Puppet::new();
        let id = puppet.insert_child(puppet.root(), Node::default(), Some(4));
        let param = Param {
            id: 1,
            name: "p".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                id,
                InterpolateMode::Linear,
                BindingValues::ZOrder(Matrix {
                    width: 2,
                    height: 1,
                    data: vec![0.0, 10.0],
                }),
            )],
        };
        puppet.set_params(vec![param]);
        puppet.set_param_value(1, Vec2::new(1.0, 0.0));
        puppet.reset_dynamic_state();
        puppet.apply_params();
        let z = puppet.get(id).unwrap().z_order;
        assert!(
            (z - 10.0).abs() < 1e-5,
            "expected clamped cell (10.0), got {}",
            z
        );
    }

    #[test]
    fn apply_cubic_curves_away_from_linear() {
        use crate::puppet::Puppet;
        use crate::Node;

        // 1D param with a peaked value matrix [0, 1, 0]. Sampled at x=0.25
        // (mid of the first segment), a Catmull-Rom through the three cells
        // pulls above the straight 0->1 chord. The neighbourhood is
        // p=[v0, v0, v1, v2]=[0,0,1,0] with t=0.5, giving 0.5625; a linear
        // binding at the same point gives exactly 0.5. This pins that Cubic
        // is no longer silently linear.
        let build = |mode| {
            let mut puppet = Puppet::new();
            let id = puppet.insert_child(puppet.root(), Node::default(), Some(7));
            let param = Param {
                id: 1,
                name: "p".into(),
                is_vec2: false,
                min: Vec2::ZERO,
                max: Vec2::ONE,
                defaults: Vec2::ZERO,
                axis_points_x: vec![0.0, 0.5, 1.0],
                axis_points_y: vec![0.0],
                bindings: vec![Binding::new(
                    id,
                    mode,
                    BindingValues::ZOrder(Matrix {
                        width: 3,
                        height: 1,
                        data: vec![0.0, 1.0, 0.0],
                    }),
                )],
            };
            puppet.set_params(vec![param]);
            puppet.set_param_value(1, Vec2::new(0.25, 0.0));
            puppet.reset_dynamic_state();
            puppet.apply_params();
            puppet.get(id).unwrap().z_order
        };

        let linear = build(InterpolateMode::Linear);
        let cubic = build(InterpolateMode::Cubic);
        assert!(
            (linear - 0.5).abs() < 1e-5,
            "linear should be 0.5, got {linear}"
        );
        assert!(
            (cubic - 0.5625).abs() < 1e-5,
            "cubic should be 0.5625, got {cubic}"
        );
    }

    #[test]
    fn apply_skips_binding_when_corner_cells_are_zero() {
        use crate::components::{Mesh, MeshIndices, Node, NodeKind, PartData};
        use crate::deform::DeformStack;
        use crate::puppet::Puppet;

        let mut puppet = Puppet::new();
        let mesh = Mesh::new(
            vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
            vec![Vec2::ZERO; 3],
            MeshIndices::U16(vec![0, 1, 2]),
            Vec2::ZERO,
        );
        let part = PartData {
            mesh,
            deform_stack: DeformStack::new(3),
            ..Default::default()
        };
        let part_id = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Part(Box::new(part)),
                ..Default::default()
            },
            Some(7),
        );

        // 2x3 deform matrix: rows 0 and 1 (the corners around y=0) are
        // all-zero; row 2 carries a non-zero offset. At default value
        // (0,0) the bracket selects rows 0 and 1, so the binding should
        // be skipped and the source must NOT be active.
        let deform = DeformMatrix::from_cells(
            2,
            3,
            vec![
                vec![Vec2::ZERO, Vec2::ZERO, Vec2::ZERO],
                vec![Vec2::ZERO, Vec2::ZERO, Vec2::ZERO],
                vec![Vec2::ZERO, Vec2::ZERO, Vec2::ZERO],
                vec![Vec2::ZERO, Vec2::ZERO, Vec2::ZERO],
                vec![Vec2::new(5.0, 0.0), Vec2::ZERO, Vec2::ZERO],
                vec![Vec2::new(5.0, 0.0), Vec2::ZERO, Vec2::ZERO],
            ],
        )
        .unwrap();
        let param = Param {
            id: 1,
            name: "p".into(),
            is_vec2: true,
            min: Vec2::ZERO,
            max: Vec2::new(1.0, 2.0),
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 0.5, 1.0],
            bindings: vec![Binding::new(
                part_id,
                InterpolateMode::Linear,
                BindingValues::Deform(deform),
            )],
        };
        puppet.set_params(vec![param]);
        // value (0.5, 0) — y=0 on the axis, so bracket picks (y0=0, y1=1),
        // and both corner rows of the deform matrix are zero.
        puppet.set_param_value(1, Vec2::new(0.5, 0.0));
        puppet.reset_deforms();
        puppet.apply_params();

        let stack = match &puppet.get(part_id).unwrap().kind {
            NodeKind::Part(p) => &p.deform_stack,
            _ => panic!(),
        };
        assert!(
            !stack.is_active(),
            "skip should leave the param source inactive"
        );

        // value (0.5, 2) — normed.y=1 brackets (y0=1, y1=2). Now row
        // 2 is non-zero so the binding must NOT be skipped.
        puppet.set_param_value(1, Vec2::new(0.5, 2.0));
        puppet.reset_deforms();
        puppet.apply_params();
        let stack = match &puppet.get(part_id).unwrap().kind {
            NodeKind::Part(p) => &p.deform_stack,
            _ => panic!(),
        };
        assert!(stack.is_active(), "non-zero corners must reactivate");
    }

    #[test]
    fn apply_skips_zero_weight_nonzero_cells_on_axis_point() {
        use crate::components::{Node, NodeKind, PartData};
        use crate::deform::DeformStack;
        use crate::puppet::Puppet;

        let mut puppet = Puppet::new();
        let part_id = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Part(Box::new(PartData {
                    deform_stack: DeformStack::new(1),
                    ..Default::default()
                })),
                ..Default::default()
            },
            Some(8),
        );

        let deform = DeformMatrix::from_cells(
            3,
            1,
            vec![
                vec![Vec2::new(10.0, 0.0)],
                vec![Vec2::ZERO],
                vec![Vec2::new(20.0, 0.0)],
            ],
        )
        .unwrap();
        let param = Param {
            id: 1,
            name: "p".into(),
            is_vec2: false,
            min: Vec2::new(-1.0, 0.0),
            max: Vec2::new(1.0, 1.0),
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                part_id,
                InterpolateMode::Linear,
                BindingValues::Deform(deform),
            )],
        };
        puppet.set_params(vec![param]);

        puppet.set_param_value(1, Vec2::new(0.0, 0.0));
        puppet.reset_deforms();
        puppet.apply_params();
        let stack = match &puppet.get(part_id).unwrap().kind {
            NodeKind::Part(p) => &p.deform_stack,
            _ => panic!(),
        };
        assert!(
            !stack.is_active(),
            "non-zero cells with zero interpolation weight must not activate the source"
        );

        puppet.set_param_value(1, Vec2::new(1.0, 0.0));
        puppet.reset_deforms();
        puppet.apply_params();
        let stack = match &puppet.get(part_id).unwrap().kind {
            NodeKind::Part(p) => &p.deform_stack,
            _ => panic!(),
        };
        assert!(
            stack.is_active(),
            "selected non-zero cell must still activate"
        );
    }

    #[test]
    fn tint_multiplies_screen_tint_adds_and_reset_prevents_drift() {
        use crate::components::{Node, NodeKind, PartData};
        use crate::puppet::Puppet;
        use crate::Vec3;

        let mut puppet = Puppet::new();
        let part_id = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Part(Box::new(PartData {
                    tint: Vec3::ONE,
                    base_tint: Vec3::ONE,
                    ..Default::default()
                })),
                ..Default::default()
            },
            Some(8),
        );

        // 1D param, x in [0, 1]. tint.r runs 1.0 -> 0.0 (multiply, rest=1),
        // screenTint.r runs 0.0 -> 1.0 (add, rest=0); mirrors the reference rig's
        // "Lighting - Red" layout and exercises both fold kinds at once.
        let tint_r = Matrix {
            width: 2,
            height: 1,
            data: vec![1.0, 0.0],
        };
        let screen_r = Matrix {
            width: 2,
            height: 1,
            data: vec![0.0, 1.0],
        };
        let param = Param {
            id: 1,
            name: "p".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::new(1.0, 1.0),
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![
                Binding::new(
                    part_id,
                    InterpolateMode::Linear,
                    BindingValues::TintR(tint_r),
                ),
                Binding::new(
                    part_id,
                    InterpolateMode::Linear,
                    BindingValues::ScreenTintR(screen_r),
                ),
            ],
        };
        puppet.set_params(vec![param]);

        let part_state = |p: &Puppet| -> (Vec3, Vec3) {
            match &p.get(part_id).unwrap().kind {
                NodeKind::Part(part) => (part.tint, part.screen_tint),
                _ => panic!(),
            }
        };

        // Extreme: tint.r = 1.0 * 0.0 = 0.0, screen_tint.r = 0.0 + 1.0 = 1.0.
        puppet.set_param_value(1, Vec2::new(1.0, 0.0));
        puppet.reset_dynamic_state();
        puppet.apply_params();
        let (tint, screen_tint) = part_state(&puppet);
        assert_eq!(tint, Vec3::new(0.0, 1.0, 1.0));
        assert_eq!(screen_tint, Vec3::new(1.0, 0.0, 0.0));

        // Mid value across two frames must give the same result — the
        // per-frame reset-to-base is what stops `tint *= 0.5` compounding
        // to 0.25 on frame two.
        for _ in 0..2 {
            puppet.set_param_value(1, Vec2::new(0.5, 0.0));
            puppet.reset_dynamic_state();
            puppet.apply_params();
            let (tint, screen_tint) = part_state(&puppet);
            assert_eq!(tint, Vec3::new(0.5, 1.0, 1.0));
            assert_eq!(screen_tint, Vec3::new(0.5, 0.0, 0.0));
        }
    }
}
