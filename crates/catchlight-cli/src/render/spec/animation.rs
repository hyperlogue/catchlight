//! Remote serde adapters add strict input-field checking and generated schema
//! without changing the native clip shape or introducing new playback types.

use super::bad;
use crate::Error;
use catchlight_core::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::{Model, ParamId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(transparent)]
pub struct NativeAnimation(
    #[serde(with = "Animation")]
    #[schemars(with = "Animation")]
    pub ClmAnimation,
);

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "ClmAnimation", deny_unknown_fields)]
struct Animation {
    #[serde(default)]
    name: String,
    /// Seconds per animation/export frame. Finite and positive.
    #[schemars(transform = super::positive_number)]
    timestep: f32,
    /// Clip length in frames, positive for rendering.
    #[schemars(range(min = 1))]
    length: i32,
    /// Native looping lead-in frame, or -1 for no lead-in.
    lead_in: i32,
    /// Native looping lead-out frame, or -1 for no lead-out.
    lead_out: i32,
    #[serde(default, with = "lanes")]
    #[schemars(with = "Vec<Lane>")]
    lanes: Vec<ClmLane>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "ClmLane", deny_unknown_fields)]
struct Lane {
    #[schemars(with = "super::ModelIdSchema")]
    param: ParamId,
    #[serde(with = "Interpolation")]
    #[schemars(with = "Interpolation")]
    interpolation: InterpolateMode,
    #[serde(default, with = "keyframes")]
    #[schemars(with = "Vec<Keyframe>")]
    keyframes: Vec<ClmKeyframe>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "InterpolateMode")]
enum Interpolation {
    Nearest,
    Linear,
    Stepped,
    Cubic,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "ClmKeyframe", deny_unknown_fields)]
struct Keyframe {
    frame: i32,
    value: f32,
}

mod lanes {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Item(#[serde(with = "Lane")] ClmLane);
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<ClmLane>, D::Error> {
        Ok(Vec::<Item>::deserialize(d)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
    pub fn serialize<S: serde::Serializer>(items: &[ClmLane], s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Ref<'a>(#[serde(with = "Lane")] &'a ClmLane);
        items.iter().map(Ref).collect::<Vec<_>>().serialize(s)
    }
}
mod keyframes {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Item(#[serde(with = "Keyframe")] ClmKeyframe);
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<ClmKeyframe>, D::Error> {
        Ok(Vec::<Item>::deserialize(d)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
    pub fn serialize<S: serde::Serializer>(items: &[ClmKeyframe], s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Ref<'a>(#[serde(with = "Keyframe")] &'a ClmKeyframe);
        items.iter().map(Ref).collect::<Vec<_>>().serialize(s)
    }
}

pub(super) fn validate(model: &Model, clip: &ClmAnimation) -> Result<(), Error> {
    if !clip.timestep.is_finite() || clip.timestep <= 0.0 || clip.length <= 0 {
        return Err(bad(
            "animation timestep and length must be positive and finite",
        ));
    }
    if clip
        .lanes
        .iter()
        .flat_map(|l| &l.keyframes)
        .any(|k| !k.value.is_finite())
    {
        return Err(bad("animation keyframe values must be finite"));
    }
    // Reuse core's lane-reference and ordering validation.
    let mut checked = model.clone();
    checked.set_animations(vec![clip.clone()])?;
    Ok(())
}
