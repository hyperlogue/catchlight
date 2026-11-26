//! What a clip's lanes read at a frame.
//!
//! A loop region is derived: a lead-in or lead-out affects it only when it
//! lies strictly inside the clip, and a lead-out before the lead-in falls back
//! to the whole length.

use catchlight_core::InterpolateMode;
use catchlight_core::interpolate::cubic;

use crate::{Clip, Keyframe, Lane};

/// Scale comparisons to the authored units and allow for f32 sampling error.
pub(crate) fn same_sample(a: f32, b: f32, scale: f32) -> bool {
    (a - b).abs() <= 8. * f32::EPSILON * a.abs().max(b.abs()).max(scale.abs())
}

impl Lane {
    /// `t` is a fractional frame, clamped at both ends.
    pub fn value_at(&self, t: f32) -> f32 {
        value_at(&self.keyframes, self.interpolation, t)
    }

    /// A loop may omit the duplicate closing sample; allow one frame of travel
    /// across the seam.
    pub(crate) fn jumps_at_wrap(&self, begin: i64, end: i64) -> bool {
        let first = self.value_at(begin as f32);
        let last = self.value_at((end - 1) as f32);
        let delta = (first - last).abs();
        let entering = (last - self.value_at((end - 2).max(begin) as f32)).abs();
        let leaving = (self.value_at((begin + 1).min(end - 1) as f32) - first).abs();
        let adjacent = entering.max(leaving);
        // A smooth curve can travel fastest on the closing interval. Allow
        // half a neighbouring step of curvature before treating it as a cut.
        if same_sample(first, self.value_at(end as f32), adjacent)
            || delta <= 1.5 * adjacent
            || same_sample(first, last, adjacent)
        {
            return false;
        }
        !self.keyframes.windows(2).enumerate().any(|(i, keys)| {
            let (a, b) = (&keys[0], &keys[1]);
            let (a_frame, b_frame) = (i64::from(a.frame), i64::from(b.frame));
            let lo = begin.max(a_frame);
            let hi = (end - 1).min(b_frame);
            if lo >= hi {
                return false;
            }
            let span = f64::from(b.frame) - f64::from(a.frame);
            // For a cubic segment the one-frame difference is quadratic in the starting
            // frame: its extremum straddles the cubic's inflection point.
            let middle = match self.interpolation {
                InterpolateMode::Nearest => 0.5,
                InterpolateMode::Cubic => {
                    let before = self.keyframes[i.saturating_sub(1)].value;
                    let after = self.keyframes[(i + 2).min(self.keyframes.len() - 1)].value;
                    let d = f64::from(b.value) - f64::from(a.value);
                    let m1 = (f64::from(b.value) - f64::from(before)) * 0.5;
                    let m2 = (f64::from(after) - f64::from(a.value)) * 0.5;
                    let cubic = m1 + m2 - 2. * d;
                    if cubic == 0. {
                        0.
                    } else {
                        (2. * m1 + m2 - 3. * d) / (3. * cubic)
                    }
                }
                _ => 0.,
            };
            let start = (f64::from(a.frame) + span * middle - 0.5).floor() as i64;
            [lo, hi - 1, start, start.saturating_add(1)]
                .into_iter()
                .filter(|&f| f >= lo && f < hi)
                .any(|f| {
                    let step = (self.value_at((f + 1) as f32) - self.value_at(f as f32)).abs();
                    step >= delta || same_sample(step, delta, adjacent)
                })
        })
    }
}

impl Clip {
    /// The frames `[begin, end)` the clip repeats after its lead-in.
    pub fn loop_region(&self) -> (u32, u32) {
        let inside = |frame: Option<u32>| {
            frame.filter(|&f| f > 0 && u64::from(f) + 1 < u64::from(self.length))
        };
        let begin = inside(self.lead_in).unwrap_or(0);
        let end = inside(self.lead_out).unwrap_or(self.length);
        // A lead-out before the lead-in would wedge playback.
        if begin >= end {
            return (0, self.length);
        }
        (begin, end)
    }
}

/// Reads `keyframes` in frame order, which [`Lane::new`] puts them in.
fn value_at(keyframes: &[Keyframe], interpolation: InterpolateMode, t: f32) -> f32 {
    match keyframes {
        [] => 0.0,
        [k] => k.value,
        kfs => {
            if t <= kfs[0].frame as f32 {
                return kfs[0].value;
            }
            if t >= kfs[kfs.len() - 1].frame as f32 {
                return kfs[kfs.len() - 1].value;
            }
            let i = kfs
                .partition_point(|k| (k.frame as f32) < t)
                .min(kfs.len() - 1)
                .max(1);
            let a = &kfs[i - 1];
            let b = &kfs[i];
            if b.frame == a.frame {
                return b.value;
            }
            let span = (i64::from(b.frame) - i64::from(a.frame)) as f32;
            let frac = ((t - a.frame as f32) / span).clamp(0.0, 1.0);
            match interpolation {
                InterpolateMode::Nearest => {
                    if frac < 0.5 {
                        a.value
                    } else {
                        b.value
                    }
                }
                InterpolateMode::Stepped => {
                    if frac >= 1.0 {
                        b.value
                    } else {
                        a.value
                    }
                }
                InterpolateMode::Linear => a.value + (b.value - a.value) * frac,
                InterpolateMode::Cubic => {
                    let p0 = kfs[i.saturating_sub(2)].value;
                    let p1 = a.value;
                    let p2 = b.value;
                    let p3 = kfs[(i + 1).min(kfs.len() - 1)].value;
                    cubic(p0, p1, p2, p3, frac)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catchlight_core::ParamId;

    fn lane(kfs: Vec<(u32, f32)>, mode: InterpolateMode) -> Lane {
        Lane {
            target: crate::LaneTarget::Param(ParamId::new("p").unwrap()),
            keyframes: kfs
                .into_iter()
                .map(|(frame, value)| Keyframe { frame, value })
                .collect(),
            interpolation: mode,
        }
    }

    #[test]
    fn sparse_loop_cut_detection_agrees_with_scanning_every_frame() {
        for mode in [
            InterpolateMode::Linear,
            InterpolateMode::Cubic,
            InterpolateMode::Stepped,
            InterpolateMode::Nearest,
        ] {
            let l = lane(
                vec![(0, 0.), (3, 0.2), (11, -1.), (29, 1.), (34, 0.1)],
                mode,
            );
            for begin in 0..32 {
                for end in begin + 2..=35 {
                    let sample = |f: i64| l.value_at(f as f32);
                    let first = sample(begin);
                    let last = sample(end - 1);
                    let adjacent = (last - sample(end - 2))
                        .abs()
                        .max((sample(begin + 1) - first).abs());
                    let delta = (first - last).abs();
                    let biggest = (begin..end - 1)
                        .map(|i| (sample(i + 1) - sample(i)).abs())
                        .fold(0.0_f32, f32::max);
                    let cut = !same_sample(first, sample(end), adjacent)
                        && !same_sample(first, last, adjacent)
                        && delta > 1.5 * adjacent
                        && delta > biggest
                        && !same_sample(delta, biggest, adjacent);
                    assert_eq!(l.jumps_at_wrap(begin, end), cut, "{mode:?} {begin}..{end}");
                }
            }
        }
    }

    #[test]
    fn empty_lane_returns_zero() {
        let l = lane(vec![], InterpolateMode::Linear);
        assert_eq!(l.value_at(0.0), 0.0);
        assert_eq!(l.value_at(100.0), 0.0);
    }

    #[test]
    fn single_keyframe_is_constant() {
        let l = lane(vec![(5, 42.0)], InterpolateMode::Linear);
        assert_eq!(l.value_at(0.0), 42.0);
        assert_eq!(l.value_at(5.0), 42.0);
        assert_eq!(l.value_at(100.0), 42.0);
    }

    #[test]
    fn linear_interpolates_between_keyframes() {
        let l = lane(vec![(0, 0.0), (10, 100.0)], InterpolateMode::Linear);
        assert!((l.value_at(0.0) - 0.0).abs() < 1e-5);
        assert!((l.value_at(5.0) - 50.0).abs() < 1e-5);
        assert!((l.value_at(10.0) - 100.0).abs() < 1e-5);
    }

    #[test]
    fn out_of_range_clamps_to_endpoints() {
        let l = lane(vec![(0, 10.0), (10, 20.0)], InterpolateMode::Linear);
        assert_eq!(l.value_at(-5.0), 10.0);
        assert_eq!(l.value_at(20.0), 20.0);
    }

    #[test]
    fn extreme_frames_interpolate_without_overflow() {
        let l = lane(vec![(0, -1.0), (u32::MAX, 1.0)], InterpolateMode::Linear);
        let v = l.value_at(u32::MAX as f32 / 2.0);
        assert!(v.is_finite());
        assert!(v.abs() < 1e-3, "mid-span, so linear gives ~0: {v}");
    }

    #[test]
    fn nearest_snaps_to_closer_keyframe() {
        let l = lane(vec![(0, 0.0), (10, 100.0)], InterpolateMode::Nearest);
        assert_eq!(l.value_at(4.0), 0.0);
        assert_eq!(l.value_at(6.0), 100.0);
    }

    #[test]
    fn stepped_holds_previous_value_until_next_keyframe() {
        let l = lane(
            vec![(0, 1.0), (10, 5.0), (20, 9.0)],
            InterpolateMode::Stepped,
        );
        assert_eq!(l.value_at(0.0), 1.0);
        assert_eq!(l.value_at(9.9), 1.0);
        assert_eq!(l.value_at(10.0), 5.0);
        assert_eq!(l.value_at(10.1), 5.0);
        assert_eq!(l.value_at(19.0), 5.0);
        assert_eq!(l.value_at(25.0), 9.0);
    }

    #[test]
    fn cubic_is_plain_catmull_rom() {
        // Uniform Catmull-Rom at p=[0,0,1,0], t=0.5 gives 0.5625, the same as a
        // Cubic binding's fold over the same cells.
        let l = lane(vec![(0, 0.0), (1, 1.0), (2, 0.0)], InterpolateMode::Cubic);
        let mid = l.value_at(0.5);
        assert!((mid - 0.5625).abs() < 1e-5, "cubic midpoint {mid}");
    }

    #[test]
    fn a_lead_out_before_the_lead_in_loops_the_whole_clip() {
        let mut clip = Clip::new("c");
        clip.length = 10;
        clip.lead_in = Some(6);
        clip.lead_out = Some(3);
        assert_eq!(clip.loop_region(), (0, 10));
        clip.lead_out = Some(8);
        assert_eq!(clip.loop_region(), (6, 8));
        clip.lead_in = Some(9);
        assert_eq!(clip.loop_region(), (0, 8));
    }
}
