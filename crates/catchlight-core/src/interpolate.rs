//! How a value is read between the cells of a binding's grid.

use std::borrow::Borrow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterpolateMode {
    Nearest,
    Linear,
    /// Hold a key's value from its own position until the next key.
    Stepped,
    Cubic,
}

/// The pair of key-position indices that bracket `t`. Each interval is
/// closed below and open above, except the last, so a pose exactly on an
/// interior key is the lower end of its bracket (what `Stepped` relies on).
pub(crate) fn bracket(axis: &[f32], t: f32) -> (usize, usize) {
    let last = axis.len().saturating_sub(1);
    if last == 0 {
        return (0, 0);
    }
    for i in 0..last {
        let below_next = if i + 1 == last {
            t <= axis[i + 1]
        } else {
            t < axis[i + 1]
        };
        if t >= axis[i] && below_next {
            return (i, i + 1);
        }
    }
    if t < axis[0] {
        (0, 1)
    } else {
        (last - 1, last)
    }
}

/// A normalized neutral key may round one f32 step differently in another client.
pub(crate) fn neutral_key(axis: &[f32], zero: f32) -> Option<usize> {
    axis.iter().position(|&p| p == zero).or_else(|| {
        axis.iter()
            .position(|&p| p >= zero.next_down() && p <= zero.next_up())
    })
}

pub(crate) fn binding_bracket(
    axis: &[f32],
    normalized: f32,
    neutral_output: bool,
) -> (usize, usize) {
    if neutral_output && let Some(index) = neutral_key(axis, normalized) {
        return (index, index);
    }
    bracket(axis, normalized)
}

/// `(lo, hi, frac)` for a normalized value on a non-empty key axis.
pub(crate) fn locate(axis: &[f32], normalized: f32, neutral_output: bool) -> (usize, usize, f32) {
    let (lo, hi) = binding_bracket(axis, normalized, neutral_output);
    (lo, hi, frac(normalized, axis[lo], axis[hi]))
}

/// Returns 0 when a == b, so a single-key-position axis doesn't NaN.
pub(crate) fn frac(t: f32, a: f32, b: f32) -> f32 {
    if (b - a).abs() < 1e-9 {
        0.0
    } else {
        ((t - a) / (b - a)).clamp(0.0, 1.0)
    }
}

/// Uniform Catmull-Rom from `p1`→`p2` (`t` in `[0,1]`). With a collapsed
/// neighbourhood (p0==p1, p2==p3) it reduces to `lerp`.
pub fn cubic<T>(p0: T, p1: T, p2: T, p3: T, t: f32) -> T
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

/// A param value's position in its range, `0..=1`; a degenerate range reads 0.
pub(crate) fn normalize(value: f32, min: f32, max: f32) -> f32 {
    let span = max - min;
    if span.abs() > 1e-9 {
        ((value.clamp(min, max) - min) / span).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Where a pose reads a binding's grid: the bracketing cells on each axis,
/// already clamped to the grid, and the fraction between them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GridPoint {
    pub(crate) x: (usize, usize),
    pub(crate) y: (usize, usize),
    pub(crate) f: (f32, f32),
}

impl GridPoint {
    /// `(lo, hi, frac)` per axis, clamped to a `w` x `h` grid so a malformed
    /// model reads approximately instead of out of bounds. `w` and `h` are
    /// at least 1.
    pub(crate) fn new(x: (usize, usize, f32), y: (usize, usize, f32), w: usize, h: usize) -> Self {
        Self {
            x: (x.0.min(w - 1), x.1.min(w - 1)),
            y: (y.0.min(h - 1), y.1.min(h - 1)),
            f: (x.2, y.2),
        }
    }

    pub(crate) fn nearest_cell(&self) -> (usize, usize) {
        let ((x0, x1), (y0, y1), (fx, fy)) = (self.x, self.y, self.f);
        (
            if fx < 0.5 { x0 } else { x1 },
            if fy < 0.5 { y0 } else { y1 },
        )
    }

    /// The lower cell, except at the very top of an axis, where the last cell
    /// would otherwise be unreachable.
    pub(crate) fn stepped_cell(&self, w: usize, h: usize) -> (usize, usize) {
        let ((x0, x1), (y0, y1), (fx, fy)) = (self.x, self.y, self.f);
        (
            if fx >= 1.0 && x1 + 1 >= w { x1 } else { x0 },
            if fy >= 1.0 && y1 + 1 >= h { y1 } else { y0 },
        )
    }
}

/// A value a binding cell holds, as interpolation combines it.
pub(crate) trait Sample: Clone {
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

impl Sample for glam::Vec2 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        *a + (*b - *a) * t
    }

    fn spline(p: [&Self; 4], t: f32) -> Self {
        cubic(*p[0], *p[1], *p[2], *p[3], t)
    }
}

/// The cells one read of a grid combines, and the weights it combines them
/// with: everything that depends on the binding and the pose, worked out once
/// so a deform can combine per vertex without redoing it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Stencil<C> {
    /// Nearest and Stepped read a single cell.
    One(C),
    /// `[x0y0, x1y0, x0y1, x1y1]`, then `(fx, fy)`.
    Linear([C; 4], (f32, f32)),
    /// Catmull-Rom over the 4x4 neighbourhood, rows by y, then `(fx, fy)`.
    Cubic([[C; 4]; 4], (f32, f32)),
}

impl<'a, C> Stencil<&'a C> {
    /// Reads a `w` x `h` row-major `grid` at `point`; `grid` holds at least
    /// `w * h` cells.
    pub(crate) fn new(
        mode: InterpolateMode,
        w: usize,
        h: usize,
        point: &GridPoint,
        grid: &'a [C],
    ) -> Self {
        let ((x0, x1), (y0, y1), f) = (point.x, point.y, point.f);
        let at = |cx: usize, cy: usize| &grid[cy * w + cx];
        match mode {
            InterpolateMode::Nearest => {
                let (cx, cy) = point.nearest_cell();
                Self::One(at(cx, cy))
            }
            InterpolateMode::Stepped => {
                let (cx, cy) = point.stepped_cell(w, h);
                Self::One(at(cx, cy))
            }
            InterpolateMode::Linear => {
                Self::Linear([at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1)], f)
            }
            InterpolateMode::Cubic => {
                // Clamping the outer indices degrades edge cells to linear.
                let cx = [x0.saturating_sub(1), x0, x1, (x1 + 1).min(w - 1)];
                let cy = [y0.saturating_sub(1), y0, y1, (y1 + 1).min(h - 1)];
                Self::Cubic(cy.map(|r| cx.map(|c| at(c, r))), f)
            }
        }
    }
}

impl<T: Sample> Stencil<&T> {
    pub(crate) fn sample(&self) -> T {
        match *self {
            Self::One(cell) => cell.clone(),
            Self::Linear(cells, f) => bilinear(cells, f),
            Self::Cubic(cells, f) => bicubic(cells, f),
        }
    }
}

/// `[x0y0, x1y0, x0y1, x1y1]`: x first, then y.
pub(crate) fn bilinear<T: Sample, R: Borrow<T>>(p: [R; 4], (fx, fy): (f32, f32)) -> T {
    let top = T::lerp(p[0].borrow(), p[1].borrow(), fx);
    let bottom = T::lerp(p[2].borrow(), p[3].borrow(), fx);
    T::lerp(&top, &bottom, fy)
}

// Without the hint the per-vertex deform loop passes the 4x4 neighbourhood
// through memory, which costs it about half its speed.
/// Rows by y: each row along x, then the rows along y.
#[inline]
pub(crate) fn bicubic<T: Sample, R: Borrow<T>>(p: [[R; 4]; 4], (fx, fy): (f32, f32)) -> T {
    let row = |r: &[R; 4]| {
        T::spline(
            [r[0].borrow(), r[1].borrow(), r[2].borrow(), r[3].borrow()],
            fx,
        )
    };
    let rows = [row(&p[0]), row(&p[1]), row(&p[2]), row(&p[3])];
    T::spline([&rows[0], &rows[1], &rows[2], &rows[3]], fy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_picks_surrounding_indices() {
        let axis = [0.0, 0.5, 1.0];
        assert_eq!(bracket(&axis, 0.25), (0, 1));
        assert_eq!(bracket(&axis, 0.75), (1, 2));
        assert_eq!(bracket(&axis, 0.0), (0, 1));
        assert_eq!(bracket(&axis, 1.0), (1, 2));
    }

    #[test]
    fn an_interior_key_is_the_lower_end_of_its_bracket() {
        let axis = [0.0, 0.25, 0.5, 1.0];
        assert_eq!(bracket(&axis, 0.25), (1, 2));
        assert_eq!(bracket(&axis, 0.5), (2, 3));
        assert_eq!(frac(0.5, axis[2], axis[3]), 0.0);
    }

    #[test]
    fn bracket_single_point_axis_returns_degenerate() {
        let axis = [0.0];
        assert_eq!(bracket(&axis, 0.7), (0, 0));
    }
}
