//! Playing clips on a puppet from outside it: each frame the host advances
//! the player with its own inputs, then ticks the puppet.

use std::sync::Arc;

use catchlight_core::{Model, ParamId, Puppet};
use catchlight_extension_expressions::{BlendScratch, Expressions, Weights};
use catchlight_extension_humanoid::{ControlValues, Humanoid};

use crate::sample::same_sample;
use crate::{Animations, Clip, LaneTarget};

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
    /// `false` once [`AnimationPlayer::release`] let the clip run out.
    pub looping: bool,
}

/// A program's live inputs for one frame: control values, such as tracking,
/// and the weights of expressions and visemes. Any may be empty. A playing
/// clip's lane on the same control, expression or viseme wins.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveInputs {
    pub controls: ControlValues,
    pub expressions: Weights,
    pub visemes: Weights,
}

/// The model data a frame applies controls, expressions and visemes
/// through. Either may be absent, and then what needs it does nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rig<'a> {
    pub humanoid: Option<&'a Humanoid>,
    pub expressions: Option<&'a Expressions>,
}

impl<'a> Rig<'a> {
    /// The model's humanoid and expressions data, each left out when the
    /// model has none or it cannot be read.
    pub fn of(model: &'a Model) -> Self {
        Self {
            humanoid: model.extension::<Humanoid>().ok(),
            expressions: model.extension::<Expressions>().ok(),
        }
    }
}

/// A param the control mappings or param settings wrote: its value before
/// they did, and what the puppet held after.
#[derive(Debug, Clone, PartialEq)]
struct Blended {
    param: ParamId,
    base: f32,
    written: f32,
}

/// Working space an advance reuses, so playback allocates nothing once it
/// has grown to fit the model.
#[derive(Debug, Clone, Default)]
struct Scratch {
    /// The live inputs with the clip's lanes over them, when the clip or the
    /// rig changes them.
    controls: ControlValues,
    expressions: Weights,
    visemes: Weights,
    pose: PoseScratch,
}

#[derive(Debug, Clone, Default)]
struct PoseScratch {
    /// Each param the mappings or param settings may write, with its value
    /// before this frame wrote it.
    before: Vec<(ParamId, Option<f32>)>,
    /// Each param this frame blends, with the base it blends from.
    bases: Vec<(ParamId, f32)>,
    /// Last frame's [`AnimationPlayer::blended`].
    previous: Vec<Blended>,
    blend: BlendScratch,
}

/// Plays one clip at a time on a puppet, together with the live inputs.
/// Cloning shares the clips.
///
/// ```no_run
/// # use catchlight_core::{Model, Puppet};
/// # use catchlight_extension_animations::{AnimationPlayer, LiveInputs, Rig};
/// # fn frame(model: &Model, puppet: &mut Puppet, player: &mut AnimationPlayer, dt: f32) {
/// player.advance_with(puppet, dt, Rig::of(model), &LiveInputs::default());
/// puppet.tick(model, dt);
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct AnimationPlayer {
    clips: Arc<[Clip]>,
    /// Params whose lane jumps where the loop wraps.
    wrap_cuts: Vec<ParamId>,
    /// Whether a control, expression or viseme lane jumps where the loop
    /// wraps.
    wrap_cuts_named: bool,
    state: Option<PlayState>,
    sampled: Option<(PlayState, f32)>,
    /// What last frame's mappings and param settings wrote, so a held weight
    /// does not compound from frame to frame and a param returns to its base
    /// once nothing sets it.
    blended: Vec<Blended>,
    scratch: Scratch,
}

impl AnimationPlayer {
    pub fn new(clips: impl Into<Arc<[Clip]>>) -> Self {
        Self {
            clips: clips.into(),
            ..Self::default()
        }
    }

    /// The clips `animations` holds; nothing plays.
    #[must_use]
    pub fn from_animations(animations: &Animations) -> Self {
        Self::new(animations.clips().collect::<Arc<[Clip]>>())
    }

    #[must_use]
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// Stops whatever was playing.
    pub fn set_clips(&mut self, clips: impl Into<Arc<[Clip]>>) {
        self.clips = clips.into();
        self.stop();
    }

    /// Takes new clips, as after the model changed: the playing clip keeps
    /// playing from where it was when a clip of its name remains, and
    /// otherwise stops.
    pub fn replace_clips(&mut self, clips: impl Into<Arc<[Clip]>>) {
        let resume = self.state.and_then(|state| {
            let name = self.clips.get(state.index)?.name.clone();
            Some((name, state, self.sampled))
        });
        self.clips = clips.into();
        self.stop();
        let Some((name, state, sampled)) = resume else {
            return;
        };
        if !self.play(&name) {
            return;
        }
        let Some(index) = self.state.map(|s| s.index) else {
            return;
        };
        self.state = Some(PlayState { index, ..state });
        // Not a start: the clip carries on without marking jumps.
        self.sampled = sampled.map(|(s, frame)| (PlayState { index, ..s }, frame));
    }

    /// Starts the clip named `name` from its first frame. `false` when no clip
    /// has that name.
    pub fn play(&mut self, name: &str) -> bool {
        let Some(index) = self.clips.iter().position(|c| c.name == name) else {
            return false;
        };
        self.wrap_cuts.clear();
        self.wrap_cuts_named = false;
        let clip = &self.clips[index];
        if clip.length > 0 {
            let (begin, end) = clip.loop_region();
            for lane in &clip.lanes {
                if !lane.jumps_at_wrap(i64::from(begin), i64::from(end)) {
                    continue;
                }
                match &lane.target {
                    LaneTarget::Param(param) => self.wrap_cuts.push(param.clone()),
                    _ => self.wrap_cuts_named = true,
                }
            }
        }
        self.sampled = None;
        self.state = Some(PlayState {
            index,
            time: 0.0,
            looping: true,
        });
        true
    }

    /// Lets the playing clip run out: at the loop's end playback goes on into
    /// the lead-out, plays it once and holds the clip's last frame. A clip
    /// without a lead-out holds the loop's last frame.
    pub const fn release(&mut self) {
        if let Some(state) = &mut self.state {
            state.looping = false;
        }
    }

    pub const fn stop(&mut self) {
        self.state = None;
        self.sampled = None;
    }

    /// The sample the most recent advance used, after wrapping and clamping.
    /// `None` after play, stop or new clips until the next advance.
    #[must_use]
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

    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.state.is_some()
    }

    #[must_use]
    pub fn playing(&self) -> Option<&str> {
        let state = self.state?;
        Some(self.clips.get(state.index)?.name.as_str())
    }

    /// [`Self::advance_with`] with no humanoid or expressions data and no
    /// live inputs: only the playing clip's param lanes act.
    pub fn advance(&mut self, puppet: &mut Puppet, dt: f32) -> bool {
        self.advance_with(puppet, dt, Rig::default(), &LiveInputs::default())
    }

    /// Steps the playing clip by `dt` seconds and poses `puppet` for this
    /// frame. Call it every frame, playing or not, before the puppet's tick.
    ///
    /// In order: the live `inputs`, with the clip's control, expression
    /// and viseme lanes replacing the live value for the same name; the
    /// control settings of expressions, then of visemes; the control
    /// mappings; the param settings of expressions, then of visemes; the
    /// clip's param lanes. Params a motion response drives are left alone.
    /// Returns whether any param moved.
    pub fn advance_with(
        &mut self,
        puppet: &mut Puppet,
        dt: f32,
        rig: Rig<'_>,
        inputs: &LiveInputs,
    ) -> bool {
        let _span = tracing::trace_span!("advance_animation").entered();
        if !dt.is_finite() {
            return false;
        }
        let clips = Arc::clone(&self.clips);
        let (sample, cut) = self.step(&clips, puppet, dt);
        let Self {
            scratch, blended, ..
        } = self;

        // The live inputs are copied only when something writes over them.
        let lanes = sample.map_or(&[][..], |(clip, _)| clip.lanes.as_slice());
        let mut own_controls = rig.expressions.is_some();
        let (mut own_expressions, mut own_visemes) = (false, false);
        for lane in lanes {
            match lane.target {
                LaneTarget::Param(_) => {}
                LaneTarget::Control(_) => own_controls = true,
                LaneTarget::Expression(_) => own_expressions = true,
                LaneTarget::Viseme(_) => own_visemes = true,
            }
        }
        if own_controls {
            scratch.controls.clone_from(&inputs.controls);
        }
        if own_expressions {
            scratch.expressions.clone_from(&inputs.expressions);
        }
        if own_visemes {
            scratch.visemes.clone_from(&inputs.visemes);
        }
        if let Some((clip, frame)) = sample {
            for lane in &clip.lanes {
                match &lane.target {
                    LaneTarget::Param(_) => {}
                    LaneTarget::Control(name) => scratch.controls.set(name, lane.value_at(frame)),
                    LaneTarget::Expression(name) => {
                        scratch.expressions.set(name, lane.value_at(frame));
                    }
                    LaneTarget::Viseme(name) => scratch.visemes.set(name, lane.value_at(frame)),
                }
            }
        }
        let expressions = if own_expressions {
            &scratch.expressions
        } else {
            &inputs.expressions
        };
        let visemes = if own_visemes {
            &scratch.visemes
        } else {
            &inputs.visemes
        };
        if let Some(data) = rig.expressions {
            data.blend_controls(
                expressions,
                visemes,
                &mut scratch.controls,
                &mut scratch.pose.blend,
            );
        }
        let controls = if own_controls {
            &scratch.controls
        } else {
            &inputs.controls
        };
        let pose = &mut scratch.pose;
        Self::pose(blended, pose, puppet, rig, controls, expressions, visemes);
        let mut changed = false;
        for (param, value) in &pose.before {
            let now = puppet.param_value_posed(param);
            if now == *value {
                continue;
            }
            changed = true;
            let (Some(now), Some(limits)) = (now, puppet.param_limits(param)) else {
                continue;
            };
            let was = value.unwrap_or(limits.default);
            if cut && !same_sample(was, now, limits.min.abs().max(limits.max.abs())) {
                puppet.mark_jump(param);
            }
        }

        if let Some((clip, frame)) = sample {
            for lane in &clip.lanes {
                let LaneTarget::Param(param) = &lane.target else {
                    continue;
                };
                if puppet.is_driven(param) {
                    continue;
                }
                let value = lane.value_at(frame);
                if puppet.param_value_posed(param) != Some(value) {
                    puppet.set_param_value(param, value);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Steps the playing clip and marks the jumps its param lanes make. Gives
    /// the clip and fractional frame to sample, and whether its control,
    /// expression and viseme lanes are discontinuous this frame: on a start,
    /// played backwards, or across a loop wrap one of them jumps at.
    fn step<'c>(
        &mut self,
        clips: &'c [Clip],
        puppet: &mut Puppet,
        dt: f32,
    ) -> (Option<(&'c Clip, f32)>, bool) {
        let Some(mut state) = self.state else {
            return (None, false);
        };
        let Some(clip) = clips.get(state.index) else {
            self.state = None;
            return (None, false);
        };
        let params = || {
            clip.lanes
                .iter()
                .filter_map(|lane| Some((lane, lane.param()?)))
        };
        let named = clip.lanes.iter().any(|lane| lane.param().is_none());
        let mut cut = false;
        if dt < 0. {
            // Played backwards: every lane is a discontinuity.
            for (_, param) in params() {
                puppet.mark_jump(param);
            }
            cut = named;
        } else if self.sampled.is_none() {
            for (lane, param) in params() {
                let Some(limits) = puppet.param_limits(param) else {
                    continue;
                };
                let base = puppet.param_value_posed(param).unwrap_or(limits.default);
                if !same_sample(
                    base,
                    lane.value_at(0.),
                    limits.min.abs().max(limits.max.abs()),
                ) {
                    puppet.mark_jump(param);
                }
            }
            cut = named;
        }
        state.time += dt;
        let mut frame = 0.0;
        if clip.length > 0 && clip.timestep > 0.0 {
            // The lead-in plays once; the loop region then repeats. Every
            // frame lasts one timestep, the region's last one included, so
            // the wrap is decided on the clock, not on a rounded frame: a
            // step shorter than half a timestep must still get past it.
            let (loop_begin, loop_end) = clip.loop_region();
            let last = if state.looping {
                let begin = loop_begin as f32 * clip.timestep;
                let end = loop_end as f32 * clip.timestep;
                // Steps that sum to the end land a rounding error to either
                // side of it; those wrap onto the first frame exactly.
                let slack = clip.timestep * 1e-3;
                let over = state.time - end;
                if over >= -slack {
                    for param in &self.wrap_cuts {
                        puppet.mark_jump(param);
                    }
                    cut |= self.wrap_cuts_named;
                    state.time = begin
                        + if over > slack {
                            over % (end - begin)
                        } else {
                            0.0
                        };
                }
                loop_end - 1
            } else {
                state.time = state.time.min((clip.length - 1) as f32 * clip.timestep);
                clip.length - 1
            };
            frame = (state.time / clip.timestep).min(last as f32);
        }
        self.sampled = Some((state, frame));
        self.state = Some(state);
        (Some((clip, frame)), cut)
    }

    /// Applies the control mappings, then blends the param settings over
    /// them, from a base that does not drift: a param that still holds what
    /// last frame left blends from last frame's base, and one that something
    /// else moved since blends from its new value. A param that a control
    /// maps onto blends from what the mapping wrote. A param neither sets any
    /// more returns to its base. Fills the scratch's `before` with each param
    /// written.
    fn pose(
        blended: &mut Vec<Blended>,
        scratch: &mut PoseScratch,
        puppet: &mut Puppet,
        rig: Rig<'_>,
        controls: &ControlValues,
        expressions: &Weights,
        visemes: &Weights,
    ) {
        let PoseScratch {
            before,
            bases,
            previous,
            blend,
        } = scratch;
        std::mem::swap(blended, previous);
        blended.clear();
        before.clear();
        bases.clear();
        let posed = |puppet: &Puppet, param: &ParamId| {
            puppet
                .param_value_posed(param)
                .or_else(|| puppet.param_limits(param).map(|l| l.default))
        };
        let base_of = |puppet: &Puppet, param: &ParamId| {
            let current = posed(puppet, param)?;
            Some(match previous.iter().find(|b| b.param == *param) {
                Some(b) if b.written == current => b.base,
                _ => current,
            })
        };
        if let Some(humanoid) = rig.humanoid {
            for (name, mapping) in humanoid.mappings() {
                if controls.get(name).is_none()
                    || puppet.is_driven(&mapping.param)
                    || before.iter().any(|(p, _)| *p == mapping.param)
                {
                    continue;
                }
                before.push((
                    mapping.param.clone(),
                    puppet.param_value_posed(&mapping.param),
                ));
                if let Some(base) = base_of(puppet, &mapping.param) {
                    bases.push((mapping.param, base));
                }
            }
            humanoid.pose(controls, puppet);
        }
        let mapped = bases.len();
        let values = match rig.expressions {
            Some(data) => data.param_values(expressions, visemes, blend, |param| {
                if puppet.is_driven(param) {
                    return None;
                }
                if bases[..mapped].iter().any(|(p, _)| p == param) {
                    return posed(puppet, param);
                }
                let base = base_of(puppet, param)?;
                bases.push((param.clone(), base));
                Some(base)
            }),
            None => &[],
        };
        let note = |before: &mut Vec<(ParamId, Option<f32>)>, puppet: &Puppet, param: &ParamId| {
            if !before.iter().any(|(p, _)| p == param) {
                before.push((param.clone(), puppet.param_value_posed(param)));
            }
        };
        for b in previous.iter() {
            if bases.iter().any(|(p, _)| *p == b.param) || puppet.is_driven(&b.param) {
                continue;
            }
            if posed(puppet, &b.param) == Some(b.written) {
                note(before, puppet, &b.param);
                puppet.set_param_value(&b.param, b.base);
            }
        }
        for (param, value) in values {
            note(before, puppet, param);
            puppet.set_param_value(param, *value);
        }
        for (param, base) in bases.drain(..) {
            if let Some(written) = posed(puppet, &param) {
                blended.push(Blended {
                    param,
                    base,
                    written,
                });
            }
        }
    }
}
