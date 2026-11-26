//! Extensions and what the official ones expose.

use serde::{Deserialize, Serialize};

use catchlight_core::extension::IdRef;

use crate::*;

/// What [`Command::ExtensionSet`] sets: inline JSON, or bytes attached as
/// `value`. Tagged by `kind` because a byte marker is itself a JSON object.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExtensionSet {
    Json {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        value: serde_json::Value,
    },
    Bytes,
    /// A maintained value: its payload is `value` inline, or the `value`
    /// attachment when `value` is absent. A reference the model lacks is kept
    /// and reported by [`Command::ExtensionWarnings`], never refused.
    Maintained {
        version: u32,
        #[serde(default)]
        refs: Vec<ExtensionRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        value: Option<serde_json::Value>,
    },
}

/// One entry of a maintained extension's reference table, in the file's own
/// form: `{"param": "eye_l"}`, `{"slot": ["head", "slot-1a2b3c4d"]}`, or
/// `{"removed": {"param": "eye_l"}}` for an Id the model no longer holds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExtensionRef {
    Node(NodeId),
    Param(ParamId),
    Texture(TexId),
    /// A part's node, then the slot on it.
    Slot((NodeId, SlotId)),
    /// Never itself `removed`.
    #[expect(
        clippy::use_self,
        reason = "the Python generator reads type names from this source literally"
    )]
    Removed(Box<ExtensionRef>),
}

impl From<&IdRef> for ExtensionRef {
    fn from(r: &IdRef) -> Self {
        match r {
            IdRef::Node(id) => Self::Node(id.clone()),
            IdRef::Param(id) => Self::Param(id.clone()),
            IdRef::Texture(id) => Self::Texture(id.clone()),
            IdRef::Slot(node, slot) => Self::Slot((node.clone(), slot.clone())),
            IdRef::Removed(gone) => Self::Removed(Box::new(Self::from(&IdRef::from(gone.clone())))),
        }
    }
}

/// A [`ExtensionRef::Removed`] that wraps another removed reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a removed reference cannot itself be removed")]
pub struct NestedRemoved;

impl TryFrom<ExtensionRef> for IdRef {
    type Error = NestedRemoved;

    fn try_from(r: ExtensionRef) -> Result<Self, NestedRemoved> {
        Ok(match r {
            ExtensionRef::Node(id) => Self::Node(id),
            ExtensionRef::Param(id) => Self::Param(id),
            ExtensionRef::Texture(id) => Self::Texture(id),
            ExtensionRef::Slot((node, slot)) => Self::Slot(node, slot),
            ExtensionRef::Removed(inner) => match Self::try_from(*inner)? {
                Self::Removed(_) => return Err(NestedRemoved),
                live => live.removed(),
            },
        })
    }
}

/// What an extension holds; bytes are reported as their marker's size and
/// hash.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExtensionValueInfo {
    Json {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        value: serde_json::Value,
    },
    Bytes {
        size: u32,
        /// Lowercase hex of the content hash the marker carries.
        hash: String,
    },
    /// A maintained extension's envelope: its payload as inline `value`, or
    /// as the `size` and `hash` of its bytes.
    Maintained {
        version: u32,
        /// Its reference table, which the core keeps current.
        refs: Vec<ExtensionRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        value: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hash: Option<String>,
    },
}

/// Which of the two groups of the official expressions extension.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExpressionGroup {
    Expression,
    Viseme,
}

/// One setting of an expression or viseme: a value for a humanoid `control`,
/// in the control's range, or a raw value for one of the model's own params.
/// Exactly one of `control` and `param`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ExpressionSetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<ParamId>,
    pub value: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ExpressionInfo {
    pub name: String,
    pub settings: Vec<ExpressionSetting>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ControlMappingInfo {
    pub control: String,
    pub param: ParamId,
    /// The param values the control's -1, 0 and +1 land on.
    pub stops: [f32; 3],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StandardControlInfo {
    pub name: String,
    /// Runs 0..1 rather than -1..1.
    pub one_sided: bool,
}

/// One clip of `catchlight.animations`: an optional lead-in played once, then
/// the loop region up to an optional lead-out, repeated.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ClipInfo {
    pub name: String,
    /// Seconds per frame.
    pub timestep: f32,
    /// In frames.
    pub length: u32,
    /// The frame where the lead-in ends and the loop restarts; ignored unless
    /// strictly inside the clip.
    pub lead_in: Option<u32>,
    /// The frame where the loop wraps and the lead-out starts; ignored unless
    /// strictly inside the clip.
    pub lead_out: Option<u32>,
    /// In the clip's order.
    pub lanes: Vec<LaneInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LaneInfo {
    pub target: LaneTargetInfo,
    pub keyframes: u32,
}

/// What a lane drives: one of the model's params by Id, or a humanoid
/// control, an expression or a viseme by name.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum LaneTargetInfo {
    Param { param: ParamId },
    Control { name: String },
    Expression { name: String },
    Viseme { name: String },
}

/// A finding about one extension, never a reason to refuse anything.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ExtensionWarningInfo {
    pub key: ExtensionKey,
    /// Namespaced by its owner, as in `catchlight/dangling-reference` or
    /// `catchlight.humanoid/shared-param`.
    pub code: String,
    pub message: String,
    /// The references the finding concerns; may be empty.
    pub subjects: Vec<ExtensionRef>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ExtensionInfo {
    pub key: ExtensionKey,
    pub value: ExtensionValueInfo,
}
