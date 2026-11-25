//! Remote serde adapters add strict input-field checking and generated schema
//! without changing the clip shape `catchlight.animations` stores.

use super::bad;
use crate::Error;
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::ParamId;
use catchlight_extension_animations::{Clip, Keyframe, Lane};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(transparent)]
pub struct NativeAnimation(
    #[serde(with = "Animation")]
    #[schemars(with = "Animation")]
    pub Clip,
);

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "Clip", deny_unknown_fields)]
struct Animation {
    #[serde(default)]
    name: String,
    /// Seconds per animation/export frame. Finite and positive.
    #[schemars(transform = super::positive_number)]
    timestep: f32,
    /// Clip length in frames, positive for rendering.
    #[schemars(range(min = 1))]
    length: u32,
    /// Native looping lead-in frame; absent for no lead-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lead_in: Option<u32>,
    /// Native looping lead-out frame; absent for no lead-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lead_out: Option<u32>,
    #[serde(default, with = "lanes")]
    #[schemars(with = "Vec<LaneSchema>")]
    lanes: Vec<Lane>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "Lane", deny_unknown_fields)]
#[schemars(rename = "Lane")]
struct LaneSchema {
    #[schemars(with = "super::ModelIdSchema")]
    param: ParamId,
    #[serde(with = "Interpolation")]
    #[schemars(with = "Interpolation")]
    interpolation: InterpolateMode,
    #[serde(default, with = "keyframes")]
    #[schemars(with = "Vec<KeyframeSchema>")]
    keyframes: Vec<Keyframe>,
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
#[serde(remote = "Keyframe", deny_unknown_fields)]
#[schemars(rename = "Keyframe")]
struct KeyframeSchema {
    frame: u32,
    value: f32,
}

mod lanes {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Item(#[serde(with = "LaneSchema")] Lane);
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Lane>, D::Error> {
        Ok(Vec::<Item>::deserialize(d)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
    pub fn serialize<S: serde::Serializer>(items: &[Lane], s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Ref<'a>(#[serde(with = "LaneSchema")] &'a Lane);
        items.iter().map(Ref).collect::<Vec<_>>().serialize(s)
    }
}
mod keyframes {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Item(#[serde(with = "KeyframeSchema")] Keyframe);
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Keyframe>, D::Error> {
        Ok(Vec::<Item>::deserialize(d)?
            .into_iter()
            .map(|v| v.0)
            .collect())
    }
    pub fn serialize<S: serde::Serializer>(items: &[Keyframe], s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Ref<'a>(#[serde(with = "KeyframeSchema")] &'a Keyframe);
        items.iter().map(Ref).collect::<Vec<_>>().serialize(s)
    }
}

/// A spec clip plays on the model as given, so it must name the model's
/// params and keep each lane in frame order.
pub(super) fn validate(model: &catchlight_core::Model, clip: &Clip) -> Result<(), Error> {
    if !clip.timestep.is_finite() || clip.timestep <= 0.0 || clip.length == 0 {
        return Err(bad(
            "animation timestep and length must be positive and finite",
        ));
    }
    for lane in &clip.lanes {
        if lane.keyframes.iter().any(|k| !k.value.is_finite()) {
            return Err(bad("animation keyframe values must be finite"));
        }
        if model.param(&lane.param).is_none() {
            return Err(bad(format!(
                "animation lane names param {:?}, which the model lacks",
                lane.param.as_str()
            )));
        }
        if lane.keyframes.windows(2).any(|w| w[0].frame > w[1].frame) {
            return Err(bad("animation lane keyframes must be in frame order"));
        }
    }
    Ok(())
}
