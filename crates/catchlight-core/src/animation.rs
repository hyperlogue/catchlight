//! Playing an animation: what a clip's lanes read at a frame.
//!
//! A loop region is derived: `lead_in` / `lead_out` affect it only when they
//! lie strictly inside the clip, and a lead-out before the lead-in falls back
//! to the whole length.

use std::sync::Arc;

use crate::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
use crate::id::ParamId;
use crate::interpolate::InterpolateMode;
use crate::puppet::Puppet;

/// Scale comparisons to the authored units and allow for f32 sampling error.
fn same_sample(a: f32, b: f32, scale: f32) -> bool {
    (a - b).abs() <= 8. * f32::EPSILON * a.abs().max(b.abs()).max(scale.abs())
}

impl ClmLane {
    /// A loop may omit the duplicate closing sample; allow one frame of travel
    /// across the seam.
    pub(crate) fn jumps_at_wrap(&self, begin: i32, end: i32) -> bool {
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
            let lo = begin.max(a.frame);
            let hi = (end - 1).min(b.frame);
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
            let start = (f64::from(a.frame) + span * middle - 0.5).floor() as i32;
            [lo, hi - 1, start, start.saturating_add(1)]
                .into_iter()
                .filter(|&f| f >= lo && f < hi)
                .any(|f| {
                    let step = (self.value_at((f + 1) as f32) - self.value_at(f as f32)).abs();
                    step >= delta || same_sample(step, delta, adjacent)
                })
        })
    }

    /// `t` is a fractional frame, clamped at both ends.
    pub fn value_at(&self, t: f32) -> f32 {
        value_at(&self.keyframes, self.interpolation, t)
    }
}

impl ClmAnimation {
    pub(crate) fn loop_region(&self) -> (i32, i32) {
        loop_region(self.length, self.lead_in, self.lead_out)
    }
}

/// Reads `keyframes` in frame order, which the model and reader enforce.
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
            // i64: the full i32 frame span overflows the subtraction.
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
                    crate::interpolate::cubic(p0, p1, p2, p3, frac)
                }
            }
        }
    }
}

fn loop_region(length: i32, lead_in: i32, lead_out: i32) -> (i32, i32) {
    // saturating_add: lead_out comes verbatim from the file.
    let has_lead_in = lead_in > 0 && lead_in.saturating_add(1) < length;
    let has_lead_out = lead_out > 0 && lead_out.saturating_add(1) < length;
    let begin = if has_lead_in { lead_in } else { 0 };
    let end = if has_lead_out { lead_out } else { length };
    // lead_out < lead_in would wedge playback; fall back to the whole clip.
    if begin >= end {
        return (0, length);
    }
    (begin, end)
}

#[derive(Debug, Clone, Copy)]
struct PlayState {
    index: usize,
    /// Seconds from the start of the clip.
    time: f32,
    looping: bool,
}

/// `time` is the runtime's f32 playback clock in seconds and can wrap while
/// an export frame index keeps increasing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationSample<'a> {
    pub index: usize,
    pub name: &'a str,
    pub timestep: f32,
    pub time: f32,
    pub frame: f32,
    pub looping: bool,
}

/// Plays clips on a puppet through its public API alone: the host advances
/// the player, then ticks the puppet.
#[derive(Debug, Clone, Default)]
pub(crate) struct AnimationPlayer {
    clips: Arc<Vec<ClmAnimation>>,
    /// Params whose lane jumps where the loop wraps, by Id so a rebake can
    /// reassign slots during playback.
    wrap_cuts: Vec<ParamId>,
    state: Option<PlayState>,
    sampled: Option<(PlayState, f32)>,
}

impl AnimationPlayer {
    pub(crate) fn clips(&self) -> &[ClmAnimation] {
        &self.clips
    }

    pub(crate) fn set_clips(&mut self, clips: Vec<ClmAnimation>) {
        self.clips = Arc::new(clips);
        self.state = None;
        self.sampled = None;
    }

    pub(crate) fn play(&mut self, name: &str) -> bool {
        let Some(index) = self.clips.iter().position(|a| a.name == name) else {
            return false;
        };
        self.wrap_cuts.clear();
        let clip = &self.clips[index];
        if clip.length > 0 {
            let (begin, end) = clip.loop_region();
            self.wrap_cuts.extend(
                clip.lanes
                    .iter()
                    .filter(|lane| lane.jumps_at_wrap(begin, end))
                    .map(|lane| lane.param.clone()),
            );
        }
        self.sampled = None;
        self.state = Some(PlayState {
            index,
            time: 0.0,
            looping: true,
        });
        true
    }

    pub(crate) fn stop(&mut self) {
        self.state = None;
        self.sampled = None;
    }

    /// The clip sample the most recent advance used, after wrapping and
    /// clamping. Cleared by play, stop or replacing the clips until the next
    /// advance.
    pub(crate) fn sample(&self) -> Option<AnimationSample<'_>> {
        let (state, frame) = self.sampled?;
        let clip = self.clips.get(state.index)?;
        Some(AnimationSample {
            index: state.index,
            name: &clip.name,
            timestep: clip.timestep,
            time: state.time,
            frame,
            looping: state.looping,
        })
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.state.is_some()
    }

    pub(crate) fn playing(&self) -> Option<&str> {
        let state = self.state?;
        Some(self.clips.get(state.index)?.name.as_str())
    }

    /// `dt` is in seconds. Returns whether any param moved.
    pub(crate) fn advance(&mut self, puppet: &mut Puppet, dt: f32) -> bool {
        let _span = tracing::trace_span!("advance_animation").entered();
        if !dt.is_finite() {
            return false;
        }
        let Some(mut state) = self.state else {
            return false;
        };
        let clips = Arc::clone(&self.clips);
        let Some(clip) = clips.get(state.index) else {
            self.state = None;
            return false;
        };
        if dt < 0. {
            // Played backwards: every lane is a discontinuity.
            for lane in &clip.lanes {
                puppet.mark_jump(&lane.param);
            }
        } else if self.sampled.is_none() {
            for lane in &clip.lanes {
                let Some(limits) = puppet.param_limits(&lane.param) else {
                    continue;
                };
                let base = puppet
                    .param_value_posed(&lane.param)
                    .unwrap_or(limits.default);
                if !same_sample(
                    base,
                    lane.value_at(0.),
                    limits.min.abs().max(limits.max.abs()),
                ) {
                    puppet.mark_jump(&lane.param);
                }
            }
        }
        state.time += dt;
        // The lead-in plays once; a loop snaps back to the loop region's start.
        if clip.length > 0 && clip.timestep > 0.0 {
            let (loop_begin, loop_end) = clip.loop_region();
            let frame = (state.time / clip.timestep).round() as i64;
            if state.looping && frame >= loop_end as i64 {
                for param in &self.wrap_cuts {
                    puppet.mark_jump(param);
                }
                state.time = loop_begin as f32 * clip.timestep;
            }
            let frame = (state.time / clip.timestep).round() as i64;
            if frame + 1 >= clip.length as i64 {
                state.time = (clip.length - 1) as f32 * clip.timestep;
            }
        }
        let frame = if clip.timestep > 0.0 {
            state.time / clip.timestep
        } else {
            0.0
        };
        self.sampled = Some((state, frame));
        let mut changed = false;
        for lane in &clip.lanes {
            if puppet.is_driven(&lane.param) {
                continue;
            }
            let value = lane.value_at(frame);
            if puppet.param_value_posed(&lane.param) != Some(value) {
                puppet.set_param_value(&lane.param, value);
                changed = true;
            }
        }
        self.state = Some(state);
        changed
    }
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
                    let sample = |f: i32| l.value_at(f as f32);
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
        let l = lane(
            vec![(i32::MIN, -1.0), (i32::MAX, 1.0)],
            InterpolateMode::Linear,
        );
        let v = l.value_at(0.0);
        assert!(v.is_finite());
        assert!(
            v.abs() < 1e-3,
            "t = 0 sits mid-span, so linear gives ~0: {v}"
        );
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
        assert!(
            (mid - 0.5625).abs() < 1e-5,
            "cubic midpoint {} should be Catmull-Rom 0.5625",
            mid
        );
    }
}
