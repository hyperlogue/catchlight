//! Playing clips on a puppet from outside it: the host advances the player,
//! then ticks the puppet.

use std::sync::Arc;

use catchlight_core::{ParamId, Puppet};

use crate::sample::same_sample;
use crate::{Animations, Clip};

#[derive(Debug, Clone, Copy)]
struct PlayState {
    index: usize,
    /// Seconds from the start of the clip.
    time: f32,
    looping: bool,
}

/// The clip sample an advance used. `time` is the runtime's f32 playback
/// clock in seconds and can wrap while an export frame index keeps
/// increasing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationSample<'a> {
    pub index: usize,
    pub name: &'a str,
    pub timestep: f32,
    pub time: f32,
    pub frame: f32,
    pub looping: bool,
}

/// Plays one clip at a time on a puppet. Cloning shares the clips.
///
/// ```no_run
/// # use catchlight_core::{Model, Puppet};
/// # use catchlight_extension_animations::AnimationPlayer;
/// # fn frame(model: &Model, puppet: &mut Puppet, player: &mut AnimationPlayer, dt: f32) {
/// player.advance(puppet, dt);
/// puppet.tick(model, dt);
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct AnimationPlayer {
    clips: Arc<[Clip]>,
    /// Params whose lane jumps where the loop wraps.
    wrap_cuts: Vec<ParamId>,
    state: Option<PlayState>,
    sampled: Option<(PlayState, f32)>,
}

impl AnimationPlayer {
    pub fn new(clips: impl Into<Arc<[Clip]>>) -> Self {
        Self {
            clips: clips.into(),
            ..Self::default()
        }
    }

    /// The clips `animations` holds; nothing plays.
    pub fn from_animations(animations: &Animations) -> Self {
        Self::new(animations.clips())
    }

    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// Stops whatever was playing.
    pub fn set_clips(&mut self, clips: impl Into<Arc<[Clip]>>) {
        self.clips = clips.into();
        self.stop();
    }

    /// Starts the clip named `name` from its first frame. `false` when no clip
    /// has that name.
    pub fn play(&mut self, name: &str) -> bool {
        let Some(index) = self.clips.iter().position(|c| c.name == name) else {
            return false;
        };
        self.wrap_cuts.clear();
        let clip = &self.clips[index];
        if clip.length > 0 {
            let (begin, end) = clip.loop_region();
            self.wrap_cuts.extend(
                clip.lanes
                    .iter()
                    .filter(|lane| lane.jumps_at_wrap(i64::from(begin), i64::from(end)))
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

    pub fn stop(&mut self) {
        self.state = None;
        self.sampled = None;
    }

    /// The sample the most recent advance used, after wrapping and clamping.
    /// `None` after play, stop or new clips until the next advance.
    pub fn sample(&self) -> Option<AnimationSample<'_>> {
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

    pub fn is_playing(&self) -> bool {
        self.state.is_some()
    }

    pub fn playing(&self) -> Option<&str> {
        let state = self.state?;
        Some(self.clips.get(state.index)?.name.as_str())
    }

    /// Steps the playing clip by `dt` seconds and poses `puppet` with it,
    /// leaving params a motion response drives alone. Call before the
    /// puppet's tick. Returns whether any param moved.
    pub fn advance(&mut self, puppet: &mut Puppet, dt: f32) -> bool {
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
            if state.looping && frame >= i64::from(loop_end) {
                for param in &self.wrap_cuts {
                    puppet.mark_jump(param);
                }
                state.time = loop_begin as f32 * clip.timestep;
            }
            let frame = (state.time / clip.timestep).round() as i64;
            if frame + 1 >= i64::from(clip.length) {
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
