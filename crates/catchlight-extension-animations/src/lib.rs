//! The official `catchlight.animations` extension: a model's named clips of
//! param keyframes, and [`AnimationPlayer`], which plays them on a puppet.

mod player;
mod sample;

pub use player::{AnimationPlayer, AnimationSample};

use catchlight_core::extension::{
    DecodeError, Extension, IdRef, MergeRefused, Payload, RawExtension, Receipt, RemovedRef,
    Warning,
};
use catchlight_core::{InterpolateMode, Model, ParamId};
use serde::{Deserialize, Serialize};

/// The warning codes [`Animations`] raises from its check.
pub mod codes {
    pub const DRIVEN_PARAM: &str = "catchlight.animations/driven-param";
}

/// A named, timed sequence of param values: an optional lead-in played once,
/// then the loop region between it and an optional lead-out, repeated.
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

/// One clip's track over a single param.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lane {
    pub param: ParamId,
    pub interpolation: InterpolateMode,
    /// In frame order; [`Animations::insert`] sorts them.
    #[serde(default)]
    pub keyframes: Vec<Keyframe>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyframe {
    pub frame: u32,
    pub value: f32,
}

/// The clips a model carries, no two sharing a name. Lanes name their param
/// only through the reference table, which the core keeps current across
/// param renames and removals.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Animations {
    refs: Vec<IdRef>,
    clips: Vec<stored::Clip>,
}

/// The payload, CBOR-encoded. Lanes are columnar, and values are stored as
/// little-endian bytes so every f32 round-trips bit for bit.
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
        pub(crate) param: u32,
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
    pub fn clips(&self) -> Vec<Clip> {
        self.clips.iter().map(|c| self.resolve(c)).collect()
    }

    /// Adds `clip`, or replaces the one of the same name in place and returns
    /// it. Keyframes are sorted into frame order.
    pub fn insert(&mut self, mut clip: Clip) -> Option<Clip> {
        for lane in &mut clip.lanes {
            lane.keyframes.sort_by_key(|k| k.frame);
        }
        let stored = stored::Clip {
            name: clip.name,
            timestep: clip.timestep,
            length: clip.length,
            lead_in: clip.lead_in,
            lead_out: clip.lead_out,
            lanes: clip
                .lanes
                .into_iter()
                .map(|lane| stored::Lane {
                    param: self.intern(IdRef::Param(lane.param)),
                    interpolation: lane.interpolation,
                    frames: lane.keyframes.iter().map(|k| k.frame).collect(),
                    values: lane.keyframes.iter().map(|k| k.value).collect(),
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
                    let IdRef::Param(param) = self.refs.get(lane.param as usize)? else {
                        return None;
                    };
                    Some(Lane {
                        param: param.clone(),
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
        let mut clips = std::mem::take(&mut self.clips);
        for lane in clips.iter_mut().flat_map(|c| &mut c.lanes) {
            lane.param = self.intern(old[lane.param as usize].clone());
        }
        self.clips = clips;
    }

    fn lanes(&self) -> impl Iterator<Item = (&stored::Clip, &IdRef)> {
        self.clips.iter().flat_map(move |clip| {
            clip.lanes
                .iter()
                .map(move |lane| (clip, &self.refs[lane.param as usize]))
        })
    }
}

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
                match raw.refs.get(lane.param as usize) {
                    Some(IdRef::Param(_) | IdRef::Removed(RemovedRef::Param(_))) => {}
                    _ => {
                        return Err(DecodeError::new(format!(
                            "a lane of clip {:?} names reference {}, which is not a param",
                            clip.name, lane.param
                        )))
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

    fn refs_mut(&mut self) -> &mut Vec<IdRef> {
        &mut self.refs
    }

    fn check(&self, model: &Model) -> Vec<Warning> {
        self.lanes()
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
            .collect()
    }

    fn reconcile(&mut self, _model: &Model) {
        let refs = &self.refs;
        for clip in &mut self.clips {
            clip.lanes
                .retain(|lane| !refs[lane.param as usize].is_removed());
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
                lane.param = merged.intern(addon.refs[lane.param as usize].clone());
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
