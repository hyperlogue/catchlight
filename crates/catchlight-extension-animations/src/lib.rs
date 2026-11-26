//! The official `catchlight.animations` extension: a model's named clips of
//! keyframes on params, humanoid controls, expressions and visemes, and
//! [`AnimationPlayer`], which plays them on a puppet each frame together with
//! the host's own control values and weights.
//!
//! ```no_run
//! # use catchlight_core::{Model, Puppet};
//! # use catchlight_extension_animations::{AnimationPlayer, LiveInputs, Rig};
//! # use catchlight_extension_humanoid::controls_from_arkit;
//! # fn frame(
//! #     model: &Model,
//! #     puppet: &mut Puppet,
//! #     player: &mut AnimationPlayer,
//! #     tracking: impl Fn(&str) -> f32,
//! #     dt: f32,
//! # ) {
//! let mut inputs = LiveInputs {
//!     controls: controls_from_arkit(tracking),
//!     ..LiveInputs::default()
//! };
//! inputs.expressions.set("happy", 0.6);
//! inputs.visemes.set("aa", 1.0);
//! player.advance_with(puppet, dt, Rig::of(model), &inputs);
//! puppet.tick(model, dt);
//! # }
//! ```

mod player;
mod sample;

pub use player::{AnimationPlayer, AnimationSample, LiveInputs, Rig};

use std::collections::HashMap;

use catchlight_core::extension::{
    DecodeError, Extension, ExtensionError, IdRef, MergeRefused, Payload, RawExtension, Receipt,
    RemovedRef, Warning,
};
use catchlight_core::{InterpolateMode, Model, ParamId};
use catchlight_extension_expressions::{Expressions, Group};
use catchlight_extension_humanoid::Humanoid;
use serde::{Deserialize, Serialize};

/// The warning codes [`Animations`] raises from its check.
pub mod codes {
    pub const DRIVEN_PARAM: &str = "catchlight.animations/driven-param";
    pub const UNMAPPED_CONTROL: &str = "catchlight.animations/unmapped-control";
    pub const MISSING_EXPRESSION: &str = "catchlight.animations/missing-expression";
    pub const MISSING_VISEME: &str = "catchlight.animations/missing-viseme";
}

/// A named, timed sequence of values: an optional lead-in played once, then
/// the loop region between it and an optional lead-out, repeated until
/// [`AnimationPlayer::release`] lets the lead-out play.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub name: String,
    /// Seconds per frame.
    pub timestep: f32,
    /// In frames.
    pub length: u32,
    /// The frame where the lead-in ends and the loop restarts. Ignored unless
    /// it lies strictly inside the clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_in: Option<u32>,
    /// The frame where the loop wraps and the lead-out starts. Ignored unless
    /// it lies strictly inside the clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_out: Option<u32>,
    #[serde(default)]
    pub lanes: Vec<Lane>,
}

impl Clip {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            timestep: 1.0 / 60.0,
            length: 0,
            lead_in: None,
            lead_out: None,
            lanes: Vec::new(),
        }
    }
}

/// One clip's track over a single target. Written as JSON with exactly one
/// of `param`, `control`, `expression` or `viseme` naming it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LaneFields", into = "LaneFields")]
pub struct Lane {
    pub target: LaneTarget,
    pub interpolation: InterpolateMode,
    /// In frame order, which sampling relies on.
    keyframes: Vec<Keyframe>,
}

impl Lane {
    /// Sorts `keyframes` into frame order, keeping the given order of
    /// keyframes on the same frame.
    pub fn new(
        target: LaneTarget,
        interpolation: InterpolateMode,
        mut keyframes: Vec<Keyframe>,
    ) -> Self {
        keyframes.sort_by_key(|k| k.frame);
        Self {
            target,
            interpolation,
            keyframes,
        }
    }

    /// In frame order.
    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    /// The param a param lane drives.
    pub fn param(&self) -> Option<&ParamId> {
        match &self.target {
            LaneTarget::Param(param) => Some(param),
            _ => None,
        }
    }
}

/// What a lane drives. A control, expression or viseme is named by string,
/// so the lane plays on any model that has it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LaneTarget {
    /// The value is a raw param value.
    Param(ParamId),
    /// A humanoid control; the value is in the control's range.
    Control(String),
    /// The value is the expression's weight, 0..1.
    Expression(String),
    /// The value is the viseme's weight, 0..1.
    Viseme(String),
}

impl std::fmt::Display for LaneTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Param(param) => write!(f, "param {:?}", param.as_str()),
            Self::Control(name) => write!(f, "control {name:?}"),
            Self::Expression(name) => write!(f, "expression {name:?}"),
            Self::Viseme(name) => write!(f, "viseme {name:?}"),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    param: Option<ParamId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viseme: Option<String>,
    interpolation: InterpolateMode,
    #[serde(default)]
    keyframes: Vec<Keyframe>,
}

impl TryFrom<LaneFields> for Lane {
    type Error = String;

    fn try_from(f: LaneFields) -> Result<Self, String> {
        let target = match (f.param, f.control, f.expression, f.viseme) {
            (Some(param), None, None, None) => LaneTarget::Param(param),
            (None, Some(name), None, None) => LaneTarget::Control(name),
            (None, None, Some(name), None) => LaneTarget::Expression(name),
            (None, None, None, Some(name)) => LaneTarget::Viseme(name),
            _ => {
                return Err(
                    "a lane names exactly one of param, control, expression or viseme".into(),
                );
            }
        };
        Ok(Self::new(target, f.interpolation, f.keyframes))
    }
}

impl From<Lane> for LaneFields {
    fn from(lane: Lane) -> Self {
        let mut out = Self {
            param: None,
            control: None,
            expression: None,
            viseme: None,
            interpolation: lane.interpolation,
            keyframes: lane.keyframes,
        };
        match lane.target {
            LaneTarget::Param(param) => out.param = Some(param),
            LaneTarget::Control(name) => out.control = Some(name),
            LaneTarget::Expression(name) => out.expression = Some(name),
            LaneTarget::Viseme(name) => out.viseme = Some(name),
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    pub frame: u32,
    pub value: f32,
}

/// The clips a model carries, no two sharing a name. Param lanes name their
/// param only through the reference table, which the core keeps current
/// across param renames and removals.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Animations {
    refs: Vec<IdRef>,
    clips: Vec<stored::Clip>,
}

/// The payload, CBOR-encoded. Lanes are columnar, and values are stored as
/// little-endian bytes so every f32 round-trips bit for bit. A lane carries
/// exactly one of its four target fields.
mod stored {
    use catchlight_core::InterpolateMode;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct Clip {
        pub(crate) name: String,
        pub(crate) timestep: f32,
        pub(crate) length: u32,
        pub(crate) lead_in: Option<u32>,
        pub(crate) lead_out: Option<u32>,
        pub(crate) lanes: Vec<Lane>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub(crate) struct Lane {
        /// An index into the reference table.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) param: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) control: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) expression: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub(crate) viseme: Option<String>,
        pub(crate) interpolation: InterpolateMode,
        pub(crate) frames: Vec<u32>,
        #[serde(with = "f32_bytes")]
        pub(crate) values: Vec<f32>,
    }

    mod f32_bytes {
        use serde::{Deserialize, Deserializer, Serializer};

        pub(crate) fn serialize<S: Serializer>(values: &[f32], s: S) -> Result<S::Ok, S::Error> {
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            serde_bytes::serialize(&bytes, s)
        }

        pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f32>, D::Error> {
            let bytes = serde_bytes::ByteBuf::deserialize(d)?;
            if bytes.len() % 4 != 0 {
                return Err(serde::de::Error::custom(
                    "lane values are not a whole number of f32s",
                ));
            }
            Ok(bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect())
        }
    }
}

impl Animations {
    pub fn from_clips(clips: impl IntoIterator<Item = Clip>) -> Self {
        let mut out = Self::default();
        for clip in clips {
            out.insert(clip);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.clips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.clips.iter().map(|c| c.name.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.position(name).is_some()
    }

    /// A lane whose param was removed is left out until reconcile drops it.
    pub fn clip(&self, name: &str) -> Option<Clip> {
        self.position(name).map(|i| self.resolve(&self.clips[i]))
    }

    /// Every clip in order, as [`Self::clip`] reads it.
    pub fn clips(&self) -> impl Iterator<Item = Clip> {
        self.clips.iter().map(|c| self.resolve(c))
    }

    /// Adds `clip`, or replaces the one of the same name in place and returns
    /// it.
    pub fn insert(&mut self, clip: Clip) -> Option<Clip> {
        let stored = stored::Clip {
            name: clip.name,
            timestep: clip.timestep,
            length: clip.length,
            lead_in: clip.lead_in,
            lead_out: clip.lead_out,
            lanes: clip
                .lanes
                .into_iter()
                .map(|lane| {
                    let mut stored = stored::Lane {
                        param: None,
                        control: None,
                        expression: None,
                        viseme: None,
                        interpolation: lane.interpolation,
                        frames: lane.keyframes.iter().map(|k| k.frame).collect(),
                        values: lane.keyframes.iter().map(|k| k.value).collect(),
                    };
                    match lane.target {
                        LaneTarget::Param(param) => {
                            stored.param = Some(self.intern(IdRef::Param(param)));
                        }
                        LaneTarget::Control(name) => stored.control = Some(name),
                        LaneTarget::Expression(name) => stored.expression = Some(name),
                        LaneTarget::Viseme(name) => stored.viseme = Some(name),
                    }
                    stored
                })
                .collect(),
        };
        let previous = match self.position(&stored.name) {
            Some(i) => {
                let old = std::mem::replace(&mut self.clips[i], stored);
                Some(self.resolve(&old))
            }
            None => {
                self.clips.push(stored);
                None
            }
        };
        self.compact();
        previous
    }

    pub fn remove(&mut self, name: &str) -> Option<Clip> {
        let i = self.position(name)?;
        let clip = self.clips.remove(i);
        let out = self.resolve(&clip);
        self.compact();
        Some(out)
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.name == name)
    }

    fn resolve(&self, clip: &stored::Clip) -> Clip {
        Clip {
            name: clip.name.clone(),
            timestep: clip.timestep,
            length: clip.length,
            lead_in: clip.lead_in,
            lead_out: clip.lead_out,
            lanes: clip
                .lanes
                .iter()
                .filter_map(|lane| {
                    Some(Lane {
                        target: self.target(lane)?,
                        interpolation: lane.interpolation,
                        keyframes: lane
                            .frames
                            .iter()
                            .zip(&lane.values)
                            .map(|(&frame, &value)| Keyframe { frame, value })
                            .collect(),
                    })
                })
                .collect(),
        }
    }

    /// `None` for a param lane whose param was removed.
    fn target(&self, lane: &stored::Lane) -> Option<LaneTarget> {
        Some(
            match (&lane.param, &lane.control, &lane.expression, &lane.viseme) {
                (Some(index), ..) => match self.refs.get(*index as usize)? {
                    IdRef::Param(param) => LaneTarget::Param(param.clone()),
                    _ => return None,
                },
                (_, Some(name), ..) => LaneTarget::Control(name.clone()),
                (_, _, Some(name), _) => LaneTarget::Expression(name.clone()),
                (_, _, _, Some(name)) => LaneTarget::Viseme(name.clone()),
                _ => return None,
            },
        )
    }

    fn intern(&mut self, r: IdRef) -> u32 {
        let at = self.refs.iter().position(|x| *x == r).unwrap_or_else(|| {
            self.refs.push(r);
            self.refs.len() - 1
        });
        at as u32
    }

    /// Keeps exactly the references some lane uses, in first-use order.
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.refs);
        let mut at = HashMap::new();
        for lane in self.clips.iter_mut().flat_map(|c| &mut c.lanes) {
            if let Some(param) = &mut lane.param {
                *param = renumber(&mut self.refs, &mut at, &old[*param as usize]);
            }
        }
    }

    fn param_lanes(&self) -> impl Iterator<Item = (&stored::Clip, &IdRef)> {
        self.clips.iter().flat_map(move |clip| {
            clip.lanes
                .iter()
                .filter_map(move |lane| Some((clip, &self.refs[lane.param? as usize])))
        })
    }

    /// The names a clip's control, expression and viseme lanes need the
    /// model's other extensions to have; skipped when those cannot be read.
    fn check_names(&self, model: &Model) -> Vec<Warning> {
        let mut out = Vec::new();
        let humanoid = model.extension::<Humanoid>();
        let expressions = model.extension::<Expressions>();
        for clip in &self.clips {
            let name = &clip.name;
            for lane in &clip.lanes {
                if let Some(control) = &lane.control {
                    let mapped = match &humanoid {
                        Ok(h) => h.mapping(control).is_some(),
                        Err(ExtensionError::Missing) => false,
                        Err(_) => continue,
                    };
                    if !mapped {
                        out.push(Warning::new(
                            codes::UNMAPPED_CONTROL,
                            format!(
                                "clip {name:?} has a lane on control {control:?}, which the \
                                 model maps onto no param, so the lane does nothing"
                            ),
                        ));
                    }
                }
                for (member, group, code) in [
                    (
                        &lane.expression,
                        Group::Expression,
                        codes::MISSING_EXPRESSION,
                    ),
                    (&lane.viseme, Group::Viseme, codes::MISSING_VISEME),
                ] {
                    let Some(member) = member else { continue };
                    let present = match &expressions {
                        Ok(e) => e.contains(group, member),
                        Err(ExtensionError::Missing) => false,
                        Err(_) => continue,
                    };
                    if !present {
                        let noun = group.noun();
                        out.push(Warning::new(
                            code,
                            format!(
                                "clip {name:?} has a lane on {noun} {member:?}, which the model \
                                 lacks, so the lane does nothing"
                            ),
                        ));
                    }
                }
            }
        }
        out
    }
}

/// `r`'s index in `refs`, appending it on first use; `at` indexes `refs`.
fn renumber(refs: &mut Vec<IdRef>, at: &mut HashMap<IdRef, u32>, r: &IdRef) -> u32 {
    *at.entry(r.clone()).or_insert_with(|| {
        refs.push(r.clone());
        (refs.len() - 1) as u32
    })
}

impl catchlight_core::extension::sealed::Sealed for Animations {}
impl catchlight_core::extension::Official for Animations {}

impl Extension for Animations {
    const KEY: &'static str = "catchlight.animations";
    const VERSION: u32 = 1;

    fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
        let Payload::Bytes(bytes) = &raw.payload else {
            return Err(DecodeError::new("the payload is JSON, not CBOR bytes"));
        };
        let clips: Vec<stored::Clip> =
            ciborium::from_reader(&bytes[..]).map_err(DecodeError::new)?;
        for (i, clip) in clips.iter().enumerate() {
            if clips[..i].iter().any(|c| c.name == clip.name) {
                return Err(DecodeError::new(format!(
                    "two clips are named {:?}",
                    clip.name
                )));
            }
            for lane in &clip.lanes {
                let targets = usize::from(lane.param.is_some())
                    + usize::from(lane.control.is_some())
                    + usize::from(lane.expression.is_some())
                    + usize::from(lane.viseme.is_some());
                if targets != 1 {
                    return Err(DecodeError::new(format!(
                        "a lane of clip {:?} names {targets} targets, not one",
                        clip.name
                    )));
                }
                if let Some(param) = lane.param {
                    match raw.refs.get(param as usize) {
                        Some(IdRef::Param(_) | IdRef::Removed(RemovedRef::Param(_))) => {}
                        _ => {
                            return Err(DecodeError::new(format!(
                                "a lane of clip {:?} names reference {param}, which is not a \
                                 param",
                                clip.name
                            )));
                        }
                    }
                }
                if lane.frames.len() != lane.values.len() {
                    return Err(DecodeError::new(format!(
                        "a lane of clip {:?} has {} frames and {} values",
                        clip.name,
                        lane.frames.len(),
                        lane.values.len()
                    )));
                }
                if lane.frames.windows(2).any(|w| w[0] > w[1]) {
                    return Err(DecodeError::new(format!(
                        "a lane of clip {:?} has keyframes out of frame order",
                        clip.name
                    )));
                }
            }
        }
        Ok(Self {
            refs: raw.refs,
            clips,
        })
    }

    fn encode(&self) -> RawExtension {
        let mut bytes = Vec::new();
        // Writing to a Vec cannot fail, and every field serializes.
        let _ = ciborium::into_writer(&self.clips, &mut bytes);
        RawExtension {
            version: Self::VERSION,
            refs: self.refs.clone(),
            payload: Payload::Bytes(bytes.into()),
        }
    }

    fn refs(&self) -> &[IdRef] {
        &self.refs
    }

    fn refs_mut(&mut self) -> &mut [IdRef] {
        &mut self.refs
    }

    fn check(&self, model: &Model) -> Vec<Warning> {
        let mut out: Vec<Warning> = self
            .param_lanes()
            .filter(|(_, r)| matches!(r, IdRef::Param(id) if model.response_owner(id).is_some()))
            .map(|(clip, r)| {
                Warning::new(
                    codes::DRIVEN_PARAM,
                    format!(
                        "clip {:?} has a lane on {r}, which a motion response drives, so the \
                         lane never plays",
                        clip.name
                    ),
                )
                .with_subjects(vec![r.clone()])
            })
            .collect();
        // An addon's lanes may name what only the base has.
        if !model.is_fragment() {
            out.extend(self.check_names(model));
        }
        out
    }

    fn reconcile(&mut self, _model: &Model) {
        let refs = &self.refs;
        for clip in &mut self.clips {
            clip.lanes
                .retain(|lane| lane.param.is_none_or(|p| !refs[p as usize].is_removed()));
        }
        self.compact();
    }

    fn merge_addon(&self, addon: &Self) -> Result<(Self, Receipt), MergeRefused> {
        let mut merged = self.clone();
        let mut added = Vec::with_capacity(addon.clips.len());
        for clip in &addon.clips {
            if self.contains(&clip.name) {
                return Err(MergeRefused::Conflict(format!(
                    "the model already has an animation named {:?}",
                    clip.name
                )));
            }
            let mut clip = clip.clone();
            for lane in &mut clip.lanes {
                if let Some(param) = &mut lane.param {
                    *param = merged.intern(addon.refs[*param as usize].clone());
                }
            }
            added.push(clip.name.clone());
            merged.clips.push(clip);
        }
        merged.compact();
        Ok((merged, Receipt::new(added)))
    }

    fn unmerge_addon(&mut self, receipt: &Receipt) {
        let Some(added) = receipt.get::<Vec<String>>() else {
            return;
        };
        self.clips.retain(|c| !added.contains(&c.name));
        self.compact();
    }
}
