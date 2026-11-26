//! Review threads, saved versions and shared presence.

use serde::{Deserialize, Serialize};

use crate::*;

/// Ephemeral shared view state, kept off the model path: never an edit, never
/// saved.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Presence {
    #[serde(default)]
    pub pose: Vec<ParamPose>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<NodeId>,
}

/// What a review thread is about. Anything with an Id can be one; `model` is
/// a remark on the whole character.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ReviewTarget {
    Model,
    Node { node: NodeId },
    Param { param: ParamId },
    Texture { texture: TexId },
    Slot { node: NodeId, slot: SlotId },
}

/// Who wrote a comment. Declared by the client, not verified: every client of
/// a local editor is already trusted with the model.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ReviewAuthor {
    /// Both as the host knows the person, if it does.
    Human {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Agent {
        id: String,
        name: String,
    },
}

/// An agent: `id` is stable for as long as it works (one harness session),
/// and is what claims, presence and retry keys go by; `name` is only shown.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReviewIdentity {
    pub id: String,
    pub name: String,
}

/// A thread another agent holds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReviewHeld {
    pub thread: ThreadId,
    pub holder: ReviewIdentity,
}

/// How far a human comment has travelled toward the agent. An agent's own
/// comment is always `delivered`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ReviewDelivery {
    /// Written, and held until the human sends the review.
    Unsent,
    /// Handed over; no agent has taken it yet.
    Sent,
    Delivered,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ThreadStatus {
    Open,
    Resolved,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReviewComment {
    /// Unique within the session, across threads.
    pub comment: u64,
    pub author: ReviewAuthor,
    pub body: String,
    pub delivery: ReviewDelivery,
    /// The session revision the author was looking at.
    pub rev: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ReviewImage>,
    /// The saved version the comment points at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

/// An image on a review comment; its bytes come from
/// [`Command::ReviewImageGet`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReviewImage {
    /// Unique within the session.
    pub image: u64,
    /// `image/png`, `image/jpeg` or `image/webp`, read from the bytes.
    pub media_type: String,
    pub byte_length: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReviewThread {
    pub thread: ThreadId,
    pub target: ReviewTarget,
    /// The model no longer holds what the thread is about.
    pub target_missing: bool,
    pub status: ThreadStatus,
    /// The pose the thread was opened at.
    pub pose: Vec<ParamPose>,
    /// Never empty; the first is the opening comment.
    pub comments: Vec<ReviewComment>,
    /// The agent whose claim on the thread is live: it is working on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<ReviewIdentity>,
}

/// An agent recently heard from on this session's review.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReviewAgent {
    pub id: String,
    pub name: String,
    /// It polled recently enough to be waiting for comments now.
    pub listening: bool,
}

/// A session's review threads: shared, kept off the model path like
/// [`Presence`] — never an edit, never undone, never in the model file.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Review {
    /// Oldest first.
    pub threads: Vec<ReviewThread>,
    /// Every agent recently heard from, most recent last.
    #[serde(default)]
    pub agents: Vec<ReviewAgent>,
    #[serde(default)]
    pub kept: Kept,
    /// Why, when `kept` is `unreadable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_problem: Option<String>,
}

/// How long what the editor keeps beside a model, its review or its
/// versions, outlives the session. Says nothing about where it is kept: that
/// is the editor's choice.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Kept {
    /// Kept with the model: it is there when the model is next opened.
    Stored,
    /// The model has no file yet; it is kept once the model is saved.
    Unsaved,
    /// This editor keeps nothing: it lasts until the session closes.
    #[default]
    Session,
    /// What was kept earlier could not be read. It is left as it was and
    /// what is written now lasts until the session closes.
    Unreadable,
}

/// A snapshot of a session's model, kept beside it. Not a revision: a
/// revision counts the session's edits and is gone when it closes, while a
/// version is numbered per model and outlives the session as its review does.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct VersionInfo {
    /// 1, 2, 3… within the model; never reused.
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub author: ReviewAuthor,
    /// Milliseconds since the Unix epoch; 0 where the editor has no clock.
    pub time: u64,
    /// The session revision it was saved at.
    pub rev: u64,
    pub byte_length: u64,
    /// Lowercase hex of the `.clm` bytes' SHA-256.
    pub sha256: String,
}

/// What a read-only session opened by [`Command::VersionOpen`] shows.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct VersionViewing {
    /// The session whose version it is.
    pub session: SessionId,
    pub version: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Camera {
    pub center: [f32; 2],
    pub height: f32,
}
