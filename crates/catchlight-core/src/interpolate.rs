//! How a value is read between the cells of a binding's grid.

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
