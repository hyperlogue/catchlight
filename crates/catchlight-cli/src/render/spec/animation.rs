//! Remote serde adapters add strict input-field checking and generated schema
//! without changing the clip shape `catchlight.animations` stores. A lane
//! names its target by exactly one of `param`, `control`, `expression` or
//! `viseme`, as the clip type itself does.

use super::bad;
use crate::Error;
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::ParamId;
use catchlight_extension_animations::{Clip, Keyframe, Lane, LaneTarget};
use catchlight_extension_expressions::Group;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
#[serde(deny_unknown_fields)]
#[schemars(rename = "Lane", transform = one_target)]
struct LaneSchema {
    /// A param id; the values are raw param values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<super::ModelIdSchema>")]
    param: Option<ParamId>,
    /// A humanoid control the model maps; the values are in the control's
    /// range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control: Option<String>,
    /// An expression the model has; the values are its weight, 0..1. While
    /// the clip plays the lane replaces the request's weight for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expression: Option<String>,
    /// A viseme the model has; the values are its weight, 0..1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    viseme: Option<String>,
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

/// Exactly one target field.
fn one_target(schema: &mut schemars::Schema) {
    let one = |field: &str| serde_json::json!({"required": [field]});
    schema.insert(
        "oneOf".into(),
        serde_json::json!([
            one("param"),
            one("control"),
            one("expression"),
            one("viseme")
        ]),
    );
}

impl TryFrom<LaneSchema> for Lane {
    type Error = &'static str;
    fn try_from(l: LaneSchema) -> Result<Self, Self::Error> {
        let target = match (l.param, l.control, l.expression, l.viseme) {
            (Some(id), None, None, None) => LaneTarget::Param(id),
            (None, Some(name), None, None) => LaneTarget::Control(name),
            (None, None, Some(name), None) => LaneTarget::Expression(name),
            (None, None, None, Some(name)) => LaneTarget::Viseme(name),
            _ => return Err("a lane names exactly one of param, control, expression or viseme"),
        };
        // Lane::new would sort them, and a spec plays its lanes as given.
        if l.keyframes.windows(2).any(|w| w[0].frame > w[1].frame) {
            return Err("animation lane keyframes must be in frame order");
        }
        Ok(Lane::new(target, l.interpolation, l.keyframes))
    }
}
impl From<&Lane> for LaneSchema {
    fn from(lane: &Lane) -> Self {
        let mut out = Self {
            param: None,
            control: None,
            expression: None,
            viseme: None,
            interpolation: lane.interpolation,
            keyframes: lane.keyframes().to_vec(),
        };
        match &lane.target {
            LaneTarget::Param(id) => out.param = Some(id.clone()),
            LaneTarget::Control(name) => out.control = Some(name.clone()),
            LaneTarget::Expression(name) => out.expression = Some(name.clone()),
            LaneTarget::Viseme(name) => out.viseme = Some(name.clone()),
        }
        out
    }
}

mod lanes {
    use super::*;
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Lane>, D::Error> {
        Vec::<LaneSchema>::deserialize(d)?
            .into_iter()
            .map(|l| Lane::try_from(l).map_err(serde::de::Error::custom))
            .collect()
    }
    pub fn serialize<S: serde::Serializer>(items: &[Lane], s: S) -> Result<S::Ok, S::Error> {
        items
            .iter()
            .map(LaneSchema::from)
            .collect::<Vec<_>>()
            .serialize(s)
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
/// params, the controls it maps and its expressions and visemes.
pub(super) fn validate(model: &catchlight_core::Model, clip: &Clip) -> Result<(), Error> {
    if !clip.timestep.is_finite() || clip.timestep <= 0.0 || clip.length == 0 {
        return Err(bad(
            "animation timestep and length must be positive and finite",
        ));
    }
    for lane in &clip.lanes {
        if lane.keyframes().iter().any(|k| !k.value.is_finite()) {
            return Err(bad("animation keyframe values must be finite"));
        }
        match &lane.target {
            LaneTarget::Param(param) => {
                if model.param(param).is_none() {
                    return Err(bad(format!(
                        "animation lane names param {:?}, which the model lacks",
                        param.as_str()
                    )));
                }
            }
            LaneTarget::Control(name) => {
                let controls = BTreeMap::from([(name.clone(), 0.0)]);
                super::validate_controls(model, &controls).map_err(in_lane)?;
            }
            LaneTarget::Expression(name) => lane_member(model, Group::Expression, name)?,
            LaneTarget::Viseme(name) => lane_member(model, Group::Viseme, name)?,
        }
    }
    Ok(())
}

fn lane_member(model: &catchlight_core::Model, group: Group, name: &str) -> Result<(), Error> {
    let weights = BTreeMap::from([(name.to_owned(), 0.0)]);
    super::validate_weights(model, group, &weights).map_err(in_lane)
}

fn in_lane(e: Error) -> Error {
    match e {
        Error::Render(message) => bad(format!("animation lane: {message}")),
        e => e,
    }
}
