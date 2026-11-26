//! Folding a pose through a model's bindings into the arena.

use glam::Vec2;

use crate::components::NodeKind;
use crate::deform::DeformSource;
use crate::interpolate::{GridPoint, InterpolateMode, Stencil, bicubic, bilinear};
use crate::model::{BindingTarget, DenseGrid, ScalarTarget};

use super::bake::BakedBinding;
use super::{Located, Puppet};

fn interpolated_cell_zero(b: &BakedBinding, point: &GridPoint) -> bool {
    let ((x0, x1), (y0, y1), (fx, fy)) = (point.x, point.y, point.f);
    match b.mode {
        InterpolateMode::Nearest => {
            let (cx, cy) = point.nearest_cell();
            b.cell_zero_at(cx, cy)
        }
        InterpolateMode::Stepped => {
            let (cx, cy) = point.stepped_cell(b.width, b.height);
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
        // Cubic reads the bracket's neighbours through the tangents, so only the
        // whole 4x4 neighbourhood being identity proves a no-op.
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
        let x = self.located_at(b.x, &b.key_positions[0]);
        let y = match b.y {
            Some(slot) => self.located_at(slot, &b.key_positions[1]),
            None => Located::REST,
        };
        let point = GridPoint::new(
            (x.lo, x.hi, x.frac),
            (y.lo, y.hi, y.frac),
            b.width,
            b.height,
        );
        if interpolated_cell_zero(b, &point) {
            return;
        }
        match (&*b.grid, b.target) {
            (DenseGrid::Scalar(dense), BindingTarget::Scalar(target)) => {
                if dense.len() < b.width * b.height {
                    return;
                }
                let value = Stencil::new(b.mode, b.width, b.height, &point, dense).sample();
                self.write_scalar(b, target, value);
            }
            (DenseGrid::Deform(dense), BindingTarget::Deform) => {
                if dense.len() < b.width * b.height {
                    return;
                }
                self.write_deform(b, dense, &point, Vec2::new(x.value, y.value));
            }
            // A shape mismatch is a model bug; render the rest of the frame.
            _ => {}
        }
    }

    fn located_at(&self, slot: u32, axis: &[f32]) -> Located {
        let input = self
            .located
            .get(slot as usize)
            .copied()
            .unwrap_or(Located::REST);
        let (lo, hi, frac) = crate::interpolate::locate(
            axis,
            input.normalized,
            input.value == 0. && self.responses.owns(slot),
        );
        Located {
            lo,
            hi,
            frac,
            ..input
        }
    }

    fn write_scalar(&mut self, b: &BakedBinding, target: ScalarTarget, value: f32) {
        let Some(node) = self.arena.get_mut(b.node) else {
            return;
        };
        match target {
            ScalarTarget::Tx => node.transform.translation.x += value,
            ScalarTarget::Ty => node.transform.translation.y += value,
            ScalarTarget::Tz => node.transform.translation.z += value,
            ScalarTarget::Sx => node.transform.scale.x *= value,
            ScalarTarget::Sy => node.transform.scale.y *= value,
            ScalarTarget::Rx => node.transform.rotation.x += value,
            ScalarTarget::Ry => node.transform.rotation.y += value,
            ScalarTarget::Rz => node.transform.rotation.z += value,
            other => {
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
                    _ => {}
                }
                if other == ScalarTarget::ZOrder {
                    node.z_order += value;
                }
            }
        }
        if target.moves_geometry() {
            self.arena.mark_transform_dirty(b.node);
        }
    }

    fn write_deform(
        &mut self,
        b: &BakedBinding,
        dense: &[Vec<f32>],
        point: &GridPoint,
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
        let vert_len = stack.vert_count();
        if dense.iter().map(Vec::len).max().unwrap_or(0) < vert_len * 2 {
            return;
        }
        let Some(out) = stack.param_buf_mut(b.source, memo_key) else {
            return;
        };
        let comp = |cell: &Vec<f32>, i: usize| {
            Vec2::new(
                cell.get(2 * i).copied().unwrap_or(0.0),
                cell.get(2 * i + 1).copied().unwrap_or(0.0),
            )
        };
        let out = out.iter_mut().enumerate().take(vert_len);
        match Stencil::new(b.mode, b.width, b.height, point, dense) {
            Stencil::One(cell) => {
                for (i, o) in out {
                    *o = comp(cell, i);
                }
            }
            Stencil::Linear(cells, f) => {
                for (i, o) in out {
                    *o = bilinear(cells.map(|c| comp(c, i)), f);
                }
            }
            Stencil::Cubic(cells, f) => {
                for (i, o) in out {
                    *o = bicubic(
                        std::array::from_fn(|k| std::array::from_fn(|j| comp(cells[k][j], i))),
                        f,
                    );
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

/// One per binding, so two bindings on one node never share a buffer.
pub(super) const fn deform_source(index: usize) -> DeformSource {
    DeformSource::Param(index as u32)
}
