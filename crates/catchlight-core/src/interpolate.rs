//! How a value is read between the cells of a binding's grid.
//!
//! A binding holds a value at each of its params' key positions; a pose lands
//! between them. [`InterpolateMode`] is the rule the author picked for that,
//! and the free functions here are the one implementation of it — shared by
//! [`Model::eval_scalar`](crate::Model::eval_scalar) (what a model reports at
//! a pose) and
//! [`crate::puppet::Puppet`]'s fold (what a tick evaluates), so the two can
//! never drift.

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

/// The pair of key-position indices that bracket `t`. Shared with
/// `crate::model::eval` so a Model and a puppet locate a pose identically.
pub(crate) fn bracket(axis: &[f32], t: f32) -> (usize, usize) {
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

/// Fractional position of `t` inside `[a, b]`. Returns 0 when a == b, so a
/// degenerate (single-key-position) axis doesn't NaN. Shared with
/// `crate::model::eval`.
pub(crate) fn frac(t: f32, a: f32, b: f32) -> f32 {
    if (b - a).abs() < 1e-9 {
        0.0
    } else {
        ((t - a) / (b - a)).clamp(0.0, 1.0)
    }
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
    fn bracket_single_point_axis_returns_degenerate() {
        let axis = [0.0];
        assert_eq!(bracket(&axis, 0.7), (0, 0));
    }
}
