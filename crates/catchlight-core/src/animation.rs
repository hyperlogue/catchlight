//! Playing an animation: what a clip's lanes read at a frame.
//!
//! There is one animation type, not two. A clip is
//! [`ClmAnimation`](crate::formats::clm::ClmAnimation) wherever it appears —
//! on the model, on the wire, and on the puppet playing it — the same way a
//! mesh is a `ClmMesh` and a binding's cells are `ClmBindingValues`. This
//! module is the play behaviour over that type; `formats::clm` is its shape.
//!
//! **A loop region is derived, not authored.** `lead_in` / `lead_out` affect
//! it only when they lie strictly inside the clip, and a clip whose lead-out
//! precedes its lead-in falls back to the whole length rather than wedging
//! playback at one frame.

use crate::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
use crate::interpolate::InterpolateMode;

impl ClmLane {
    /// Interpolated value at fractional frame `t`, clamped at both ends.
    pub fn value_at(&self, t: f32) -> f32 {
        value_at(&self.keyframes, self.interpolation, t)
    }
}

impl ClmAnimation {
    pub(crate) fn loop_region(&self) -> (i32, i32) {
        loop_region(self.length, self.lead_in, self.lead_out)
    }
}

/// The keyframe interpolator.
fn value_at(keyframes: &[ClmKeyframe], interpolation: InterpolateMode, t: f32) -> f32 {
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
            let span = (b.frame - a.frame) as f32;
            let frac = ((t - a.frame as f32) / span).clamp(0.0, 1.0);
            match interpolation {
                InterpolateMode::Nearest => {
                    if frac < 0.5 {
                        a.value
                    } else {
                        b.value
                    }
                }
                InterpolateMode::Stepped => a.value,
                InterpolateMode::Linear => a.value + (b.value - a.value) * frac,
                InterpolateMode::Cubic => {
                    let p0 = kfs[i.saturating_sub(2)].value;
                    let p1 = a.value;
                    let p2 = b.value;
                    let p3 = kfs[(i + 1).min(kfs.len() - 1)].value;
                    crate::interpolate::cubic(p0, p1, p2, p3, frac)
                }
            }
        }
    }
}

/// The frames a looping player wraps between.
fn loop_region(length: i32, lead_in: i32, lead_out: i32) -> (i32, i32) {
    // saturating_add: lead_in/lead_out come verbatim from the model file,
    // so `+ 1` on i32::MAX would overflow (panic in debug, wrap to a
    // negative that passes the `< length` test in release).
    let has_lead_in = lead_in > 0 && lead_in.saturating_add(1) < length;
    let has_lead_out = lead_out > 0 && lead_out.saturating_add(1) < length;
    let begin = if has_lead_in { lead_in } else { 0 };
    let end = if has_lead_out { lead_out } else { length };
    // A file with lead_out < lead_in would give begin > end, which wedges
    // playback at `begin` forever; fall back to the whole clip.
    if begin >= end {
        return (0, length);
    }
    (begin, end)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnimationPlayState {
    pub index: usize,
    /// Current playback time in seconds from the start of the clip.
    pub time: f32,
    pub looping: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(kfs: Vec<(i32, f32)>, mode: InterpolateMode) -> ClmLane {
        ClmLane {
            param: crate::id::ParamId::new("p").unwrap(),
            keyframes: kfs
                .into_iter()
                .map(|(frame, value)| ClmKeyframe { frame, value })
                .collect(),
            interpolation: mode,
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
        // Matches the reference: at the exact keyframe the previous
        // value still holds; the switch happens just past it.
        assert_eq!(l.value_at(10.0), 1.0);
        assert_eq!(l.value_at(10.1), 5.0);
        assert_eq!(l.value_at(19.0), 5.0);
        assert_eq!(l.value_at(25.0), 9.0);
    }

    #[test]
    fn cubic_is_plain_catmull_rom() {
        // Keyframes (0,0) (1,1) (2,0) sampled mid-first-segment: the
        // neighbourhood is p=[0,0,1,0] at t=0.5, and uniform Catmull-Rom
        // gives 0.5625 — same value `Param::apply` produces for the same
        // cells, pinning that lanes and bindings share one spline.
        let l = lane(vec![(0, 0.0), (1, 1.0), (2, 0.0)], InterpolateMode::Cubic);
        let mid = l.value_at(0.5);
        assert!(
            (mid - 0.5625).abs() < 1e-5,
            "cubic midpoint {} should be Catmull-Rom 0.5625",
            mid
        );
    }
}
