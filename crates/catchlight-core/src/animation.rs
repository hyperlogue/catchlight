use crate::params::{InterpolateMode, ParamAxis};

/// A single keyframe on an animation lane. `frame` is an integer frame
/// index rather than time; the lane's `timestep` converts frames to seconds.
#[derive(Debug, Clone)]
pub struct Keyframe {
    pub frame: i32,
    pub value: f32,
}

/// One lane of an animation drives a single axis of a single parameter.
#[derive(Debug, Clone)]
pub struct AnimationLane {
    pub param_id: u32,
    pub axis: ParamAxis,
    pub keyframes: Vec<Keyframe>,
    pub interpolation: InterpolateMode,
}

impl AnimationLane {
    /// Interpolated value at fractional frame `t`. Out-of-range `t`
    /// clamps to the first/last keyframe (no extrapolation).
    pub fn value_at(&self, t: f32) -> f32 {
        value_at(&self.keyframes, self.interpolation, t)
    }
}

/// One lane of a [`PuppetAnimation`]: the param it drives and the values it
/// holds over time. A param is a scalar, so a lane drives a whole param rather
/// than one axis of a two-dimensional one — the difference from
/// [`AnimationLane`], which is keyed by the legacy uuid namespace.
#[derive(Debug, Clone)]
pub struct PuppetLane {
    pub param: crate::id::ParamId,
    pub keyframes: Vec<Keyframe>,
    pub interpolation: InterpolateMode,
}

impl PuppetLane {
    /// Interpolated value at fractional frame `t`, clamped at both ends.
    pub fn value_at(&self, t: f32) -> f32 {
        value_at(&self.keyframes, self.interpolation, t)
    }
}

/// The one keyframe interpolator both lane types use.
fn value_at(keyframes: &[Keyframe], interpolation: InterpolateMode, t: f32) -> f32 {
    {
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
                        crate::params::cubic(p0, p1, p2, p3, frac)
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub name: String,
    /// Seconds per frame. Defaults to 0.0166s (about 60 fps).
    pub timestep: f32,
    /// Length in frames. A player that advances past `length` rolls
    /// back to the loop region's start (looping) unless a caller
    /// tracks play state itself.
    pub length: i32,
    /// Frame where the lead-in ends / looping restarts. -1 (or any
    /// value outside (0, length-1)) means the loop region starts at 0.
    /// The lead-in plays once before the first wrap.
    pub lead_in: i32,
    /// Frame where the lead-out starts / looping wraps. -1 (or any
    /// value outside (0, length-1)) means the loop region ends at
    /// `length`.
    pub lead_out: i32,
    pub lanes: Vec<AnimationLane>,
}

impl Animation {
    /// A lead-in/out affects the loop region only when it lies strictly
    /// inside the clip.
    pub(crate) fn loop_region(&self) -> (i32, i32) {
        loop_region(self.length, self.lead_in, self.lead_out)
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

/// A named, timed sequence of param values: an optional lead-in played once,
/// then a body that repeats. The form a [`crate::puppet::Puppet`] plays —
/// [`Animation`] is the same shape keyed by the legacy uuid namespace.
#[derive(Debug, Clone)]
pub struct PuppetAnimation {
    pub name: String,
    /// Seconds per frame. Defaults to 0.0166s (about 60 fps).
    pub timestep: f32,
    /// Length in frames.
    pub length: i32,
    /// Frame where the lead-in ends and looping restarts, or -1 for none.
    pub lead_in: i32,
    /// Frame where the lead-out starts and looping wraps, or -1 for none.
    pub lead_out: i32,
    pub lanes: Vec<PuppetLane>,
}

impl PuppetAnimation {
    pub(crate) fn loop_region(&self) -> (i32, i32) {
        loop_region(self.length, self.lead_in, self.lead_out)
    }

    /// The play form of one of a model's clips. The wire type and this one
    /// hold the same fields; they are separate only because one is what the
    /// file stores and the other is what a puppet plays.
    pub(crate) fn from_clm(clip: &crate::formats::clm::ClmAnimation) -> Self {
        Self {
            name: clip.name.clone(),
            timestep: clip.timestep,
            length: clip.length,
            lead_in: clip.lead_in,
            lead_out: clip.lead_out,
            lanes: clip
                .lanes
                .iter()
                .map(|lane| PuppetLane {
                    param: lane.param.clone(),
                    interpolation: lane.interpolation,
                    keyframes: lane
                        .keyframes
                        .iter()
                        .map(|k| Keyframe {
                            frame: k.frame,
                            value: k.value,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

impl Default for PuppetAnimation {
    fn default() -> Self {
        Self {
            name: String::new(),
            timestep: 1.0 / 60.0,
            length: 0,
            lead_in: -1,
            lead_out: -1,
            lanes: Vec::new(),
        }
    }
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            name: String::new(),
            timestep: 1.0 / 60.0,
            length: 0,
            lead_in: -1,
            lead_out: -1,
            lanes: Vec::new(),
        }
    }
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

    fn lane(kfs: Vec<(i32, f32)>, mode: InterpolateMode) -> AnimationLane {
        AnimationLane {
            param_id: 0,
            axis: ParamAxis::X,
            keyframes: kfs
                .into_iter()
                .map(|(frame, value)| Keyframe { frame, value })
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
