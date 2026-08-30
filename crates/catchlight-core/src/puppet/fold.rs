//! Folding a pose through a model's bindings into the arena.
//!
//! One binding, one write: a scalar target adds to or multiplies the node
//! property it names, and a deform target fills the binding's own slot in the
//! node's deform stack, which `combine` later sums. A binding is skipped
//! outright when every cell with non-zero interpolation weight holds the
//! target's identity — the fold is then provably a no-op, and skipping keeps
//! the deform source inactive so nothing downstream re-uploads it.
//!
//! Two params interpolate bilinearly over the product of their key positions;
//! one param is the same evaluation with the second axis collapsed to a single
//! position. That is the same arithmetic [`crate::model::Model::eval_scalar`]
//! does, in the same four modes, over the same grid — the model evaluates one
//! binding for the editor, this evaluates all of them into a frame.

use glam::Vec2;

use crate::components::NodeKind;
use crate::deform::DeformSource;
use crate::model::{BindingTarget, DenseGrid, ScalarTarget};
use crate::params::{cubic, InterpolateMode};

use super::bake::BakedBinding;
use super::{Located, Puppet};

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The bracket cell a `Stepped` binding holds: the lower one, except at the
/// very top of an axis, where the bracket stops advancing and the last cell
/// would otherwise be unreachable.
fn stepped_cell(
    b: &BakedBinding,
    x: (usize, usize),
    y: (usize, usize),
    f: (f32, f32),
) -> (usize, usize) {
    let cx = if f.0 >= 1.0 && x.1 + 1 >= b.width {
        x.1
    } else {
        x.0
    };
    let cy = if f.1 >= 1.0 && y.1 + 1 >= b.height {
        y.1
    } else {
        y.0
    };
    (cx, cy)
}

/// Whether every cell the interpolation actually weighs is the identity, so
/// the whole binding can be skipped.
fn interpolated_cell_zero(
    b: &BakedBinding,
    x: (usize, usize),
    y: (usize, usize),
    f: (f32, f32),
) -> bool {
    let (x0, x1) = x;
    let (y0, y1) = y;
    let (fx, fy) = f;
    match b.mode {
        InterpolateMode::Nearest => {
            let cx = if fx < 0.5 { x0 } else { x1 };
            let cy = if fy < 0.5 { y0 } else { y1 };
            b.cell_zero_at(cx, cy)
        }
        InterpolateMode::Stepped => {
            let (cx, cy) = stepped_cell(b, x, y, f);
            b.cell_zero_at(cx, cy)
        }
        InterpolateMode::Linear => {
            if fx < 1.0 && fy < 1.0 && !b.cell_zero_at(x0, y0) {
                return false;
            }
            if fx > 0.0 && fy < 1.0 && !b.cell_zero_at(x1, y0) {
                return false;
            }
            if fx < 1.0 && fy > 0.0 && !b.cell_zero_at(x0, y1) {
                return false;
            }
            if fx > 0.0 && fy > 0.0 && !b.cell_zero_at(x1, y1) {
                return false;
            }
            true
        }
        // Cubic mixes the bracket's neighbours through the tangent terms, so a
        // corner-only test can wrongly skip a binding whose inner corners are
        // identity but whose neighbours bend the curve off it. Skip only when
        // the whole 4x4 neighbourhood is identity; `cell_zero_at` clamps the
        // out-of-range outer indices.
        InterpolateMode::Cubic => {
            let cx = [x0.saturating_sub(1), x0, x1, x1 + 1];
            let cy = [y0.saturating_sub(1), y0, y1, y1 + 1];
            !cy.iter()
                .any(|&r| cx.iter().any(|&c| !b.cell_zero_at(c, r)))
        }
    }
}

impl Puppet {
    pub(super) fn fold_binding(&mut self, b: &BakedBinding) {
        if b.width == 0 || b.height == 0 {
            return;
        }
        let x = self.located_at(b.x);
        let y = match b.y {
            Some(slot) => self.located_at(slot),
            None => Located::REST,
        };
        // The bracket comes from the param's key-position count; a well-formed
        // binding's grid has exactly those dimensions. Clamp to the grid's own
        // dims so a malformed model evaluates approximately instead of
        // indexing out of bounds.
        let xs = (x.lo.min(b.width - 1), x.hi.min(b.width - 1));
        let ys = (y.lo.min(b.height - 1), y.hi.min(b.height - 1));
        let f = (x.frac, y.frac);
        if interpolated_cell_zero(b, xs, ys, f) {
            return;
        }
        match (&*b.grid, b.target) {
            (DenseGrid::Scalar(dense), BindingTarget::Scalar(target)) => {
                if dense.len() < b.width * b.height {
                    return;
                }
                let value = sample(b, dense, xs, ys, f);
                self.write_scalar(b, target, value);
            }
            (DenseGrid::Deform(dense), BindingTarget::Deform) => {
                if dense.len() < b.width * b.height {
                    return;
                }
                self.write_deform(b, dense, xs, ys, f, Vec2::new(x.value, y.value));
            }
            // A grid whose shape disagrees with the target is a model bug; the
            // fold renders the rest of the frame rather than refusing it.
            _ => {}
        }
    }

    fn located_at(&self, slot: u32) -> Located {
        self.located
            .get(slot as usize)
            .copied()
            .unwrap_or(Located::REST)
    }

    fn write_scalar(&mut self, b: &BakedBinding, target: ScalarTarget, value: f32) {
        let Some(node) = self.arena.get_mut(b.node) else {
            return;
        };
        let mut moved_transform = true;
        match target {
            ScalarTarget::Tx => node.transform.translation.x += value,
            ScalarTarget::Ty => node.transform.translation.y += value,
            ScalarTarget::Sx => node.transform.scale.x *= value,
            ScalarTarget::Sy => node.transform.scale.y *= value,
            ScalarTarget::Rx => node.transform.rotation.x += value,
            ScalarTarget::Ry => node.transform.rotation.y += value,
            ScalarTarget::Rz => node.transform.rotation.z += value,
            other => {
                moved_transform = false;
                match &mut node.kind {
                    NodeKind::Part(p) => write_colour(
                        other,
                        &mut p.opacity,
                        &mut p.tint,
                        &mut p.screen_tint,
                        value,
                    ),
                    NodeKind::Composite(c) => write_colour(
                        other,
                        &mut c.opacity,
                        &mut c.tint,
                        &mut c.screen_tint,
                        value,
                    ),
                    NodeKind::SimplePhysics(p) => match other {
                        ScalarTarget::OutputScaleX => p.offset_output_scale.x *= value,
                        ScalarTarget::OutputScaleY => p.offset_output_scale.y *= value,
                        _ => {}
                    },
                    // A mesh group is never drawn, so it has no colour to fold
                    // into; a z order on it orders nothing.
                    _ => {}
                }
                if other == ScalarTarget::ZOrder {
                    node.z_order += value;
                }
            }
        }
        if moved_transform {
            self.arena.mark_transform_dirty(b.node);
        }
    }

    fn write_deform(
        &mut self,
        b: &BakedBinding,
        dense: &[Vec<f32>],
        x: (usize, usize),
        y: (usize, usize),
        f: (f32, f32),
        memo_key: Vec2,
    ) {
        let Some(node) = self.arena.get_mut(b.node) else {
            return;
        };
        let stack = match &mut node.kind {
            NodeKind::Part(p) => &mut p.deform_stack,
            NodeKind::MeshGroup(mg) => &mut mg.deform_stack,
            _ => return,
        };
        let vert_len = stack.vert_count;
        // A grid whose longest cell is shorter than the target's mesh cannot
        // be folded into it; short cells within a grid read as zero offsets.
        if dense.iter().map(Vec::len).max().unwrap_or(0) < vert_len * 2 {
            return;
        }
        let Some(out) = stack.param_buf_mut(b.source, memo_key) else {
            return;
        };
        let (x0, x1) = x;
        let (y0, y1) = y;
        let (fx, fy) = f;
        let w = b.width;
        let at = |cx: usize, cy: usize| &dense[cy * w + cx];
        let comp = |cell: &Vec<f32>, i: usize| {
            Vec2::new(
                cell.get(2 * i).copied().unwrap_or(0.0),
                cell.get(2 * i + 1).copied().unwrap_or(0.0),
            )
        };
        match b.mode {
            InterpolateMode::Cubic => {
                let cx = [x0.saturating_sub(1), x0, x1, (x1 + 1).min(w - 1)];
                let cy = [y0.saturating_sub(1), y0, y1, (y1 + 1).min(b.height - 1)];
                let cells: [[&Vec<f32>; 4]; 4] =
                    std::array::from_fn(|k| std::array::from_fn(|j| at(cx[j], cy[k])));
                for (i, o) in out.iter_mut().enumerate().take(vert_len) {
                    let row = |k: usize| {
                        cubic(
                            comp(cells[k][0], i),
                            comp(cells[k][1], i),
                            comp(cells[k][2], i),
                            comp(cells[k][3], i),
                            fx,
                        )
                    };
                    *o = cubic(row(0), row(1), row(2), row(3), fy);
                }
            }
            InterpolateMode::Nearest => {
                let src = at(
                    if fx < 0.5 { x0 } else { x1 },
                    if fy < 0.5 { y0 } else { y1 },
                );
                for (i, o) in out.iter_mut().enumerate().take(vert_len) {
                    *o = comp(src, i);
                }
            }
            InterpolateMode::Stepped => {
                let (sx, sy) = stepped_cell(b, x, y, f);
                let src = at(sx, sy);
                for (i, o) in out.iter_mut().enumerate().take(vert_len) {
                    *o = comp(src, i);
                }
            }
            InterpolateMode::Linear => {
                let (a, b2, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
                for (i, o) in out.iter_mut().enumerate().take(vert_len) {
                    let (a, b2, c, d) = (comp(a, i), comp(b2, i), comp(c, i), comp(d, i));
                    let top = a + (b2 - a) * fx;
                    let bot = c + (d - c) * fx;
                    *o = top + (bot - top) * fy;
                }
            }
        }
    }
}

fn write_colour(
    target: ScalarTarget,
    opacity: &mut f32,
    tint: &mut glam::Vec3,
    screen_tint: &mut glam::Vec3,
    value: f32,
) {
    match target {
        ScalarTarget::Opacity => *opacity *= value,
        ScalarTarget::TintR => tint.x *= value,
        ScalarTarget::TintG => tint.y *= value,
        ScalarTarget::TintB => tint.z *= value,
        ScalarTarget::ScreenTintR => screen_tint.x += value,
        ScalarTarget::ScreenTintG => screen_tint.y += value,
        ScalarTarget::ScreenTintB => screen_tint.z += value,
        _ => {}
    }
}

/// One scalar cell value, interpolated in the binding's own mode.
fn sample(
    b: &BakedBinding,
    dense: &[f32],
    x: (usize, usize),
    y: (usize, usize),
    f: (f32, f32),
) -> f32 {
    let (x0, x1) = x;
    let (y0, y1) = y;
    let (fx, fy) = f;
    let w = b.width;
    let at = |cx: usize, cy: usize| dense[cy * w + cx];
    match b.mode {
        InterpolateMode::Nearest => at(
            if fx < 0.5 { x0 } else { x1 },
            if fy < 0.5 { y0 } else { y1 },
        ),
        InterpolateMode::Stepped => {
            let (cx, cy) = stepped_cell(b, x, y, f);
            at(cx, cy)
        }
        InterpolateMode::Linear => {
            let top = lerp(at(x0, y0), at(x1, y0), fx);
            let bottom = lerp(at(x0, y1), at(x1, y1), fx);
            lerp(top, bottom, fy)
        }
        InterpolateMode::Cubic => {
            // The spline reaches one cell past the bracket on each side;
            // clamping the outer indices makes edge cells degrade to linear
            // rather than index out of the grid.
            let cx = [x0.saturating_sub(1), x0, x1, (x1 + 1).min(w - 1)];
            let cy = [y0.saturating_sub(1), y0, y1, (y1 + 1).min(b.height - 1)];
            let row = |r: usize| cubic(at(cx[0], r), at(cx[1], r), at(cx[2], r), at(cx[3], r), fx);
            cubic(row(cy[0]), row(cy[1]), row(cy[2]), row(cy[3]), fy)
        }
    }
}

/// The deform-stack slot a binding writes. One per binding, so two bindings on
/// one node never share a buffer however they are keyed.
pub(super) fn deform_source(index: usize) -> DeformSource {
    DeformSource::Param(index as u32)
}
