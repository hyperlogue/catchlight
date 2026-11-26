//! What the editor answers and announces.

use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ModelTextureHeader {
    pub id: TexId,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
}

/// Selected arrays retain their authored indices; range starts identify pages.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct GeometryNode {
    pub node: NodeId,
    pub origin: [f32; 2],
    /// Column-major matrix, including the evaluated ancestors.
    pub local_to_world: [f32; 16],
    pub vertex_count: u32,
    pub triangle_count: u32,
    pub vertices: IndexRange,
    pub triangle_range: IndexRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest: Option<Vec<[f32; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<Vec<[f32; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<Vec<[f32; 3]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uvs: Option<Vec<[f32; 2]>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triangles: Option<Vec<[u32; 3]>>,
}

/// A half-open page in authored order. Counts never imply truncation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IndexRange {
    pub start: u32,
    pub count: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingCellRead {
    pub cell: [u32; 2],
    pub authored: bool,
    pub value: Option<BindingCellValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<BindingCellValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionOrigin {
    pub session: SessionId,
    pub rev: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ModelFormat {
    Clm,
}

/// The server's answer. `Ok`/`Err` carry the request's `id`; `Event` is
/// unsolicited (a model changed on a session this client observes).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "reply", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Reply {
    Ok {
        id: u64,
        /// The addressed session's revision after this command; absent for
        /// editor-level commands and `session_close`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<u64>,
        body: ResponseBody,
    },
    /// `code` is what a client branches on; `message` is for a person.
    Err {
        id: u64,
        code: ErrorCode,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        op_index: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<LimitInfo>,
    },
    Event(Event),
}

/// Why a command was refused; clients branch on this, not the message.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ErrorCode {
    /// The session changed after a draft or recording gesture began.
    RevisionConflict,
    NoSession,
    /// The session was closed because an edit failed partway through it;
    /// its model may be half-changed. Open its file again.
    SessionPoisoned,
    NoNode,
    NoParam,
    NoTexture,
    /// The request did not parse: bad JSON, an unknown command, or a string
    /// that is not a valid Id.
    BadRequest,
    /// A target that does not fit what it names, such as `deform` where only a
    /// scalar can go.
    BadTarget,
    NothingToUndo,
    NothingToRedo,
    RevisionUnavailable,
    RevisionExhausted,
    LimitExceeded,
    /// A save with no path, on a session that has no file of its own.
    NoSavePath,
    UnknownSlot,
    DuplicateId,
    DuplicateSlot,
    WeldUnknownSlot,
    WeldSlotPairedTwice,
    /// A weld's two ends are the same node, or one of them is not a part.
    BadWeldEnd,
    /// Those two parts are already welded; one weld records a part pair.
    DuplicateWeld,
    /// A weight is a share of a meeting point, so it must be within `0..=1`.
    WeldWeightOutOfRange,
    UnknownWeld,
    /// The texture has no pixel above the alpha threshold.
    NothingToMesh,
    /// This needs a complete model and the session holds an addon fragment.
    Fragment,
    Edit,
    Manifest,
    Io,
    Image,
    Preview,
    /// The command needs a filesystem or a GPU and this build has neither
    /// (wasm).
    NativeOnly,
    /// A command carrying bytes was sent over a transport that cannot carry
    /// them; send it over one that can.
    BulkOverHttp,
    NoExtension,
    NoThread,
    NoComment,
    /// No such review image, or a comment already holds it.
    NoImage,
    /// A fetched batch changed before it was acknowledged: fetch again.
    BatchStale,
    /// Another agent holds a live claim on the thread.
    ThreadClaimed,
    /// A retry key the same author already used for different content.
    KeyConflict,
    /// `catchlight.` is the format's own prefix: a reader accepts a key under
    /// it, and nothing outside the format may author one.
    ReservedExtension,
    NoVersion,
    /// The session shows a saved version and takes no edits.
    ReadOnly,
    /// A control name outside the humanoid standard; the command takes
    /// `nonstandard` to keep it anyway.
    NonstandardControl,
    /// An official extension's data cannot be read by this editor: written
    /// by a newer version, failing to decode, or a plain value under its
    /// key. It is kept as stored, so it is neither shown nor edited.
    ExtensionUnread,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "result", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ResponseBody {
    MeshInfo {
        node: NodeId,
        mesh: MeshInfo,
    },
    ModelStructure {
        /// The current core `ClmStructure` JSON, including authored cells and extension markers.
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        structure: serde_json::Value,
        textures: Vec<ModelTextureHeader>,
    },
    GeometrySample {
        pose: Vec<ParamPose>,
        nodes: Vec<GeometryNode>,
    },
    EditHistory {
        root: u64,
        current: u64,
        pruned: bool,
        entries: Vec<EditHistoryEntry>,
    },
    EditResults {
        changed: bool,
        #[expect(
            clippy::use_self,
            reason = "the Python generator reads type names from this source literally"
        )]
        results: Vec<ResponseBody>,
    },
    BindingCells {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        width: u32,
        height: u32,
        interpolate: Interpolate,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vertex_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vertices: Option<IndexRange>,
        cells: Vec<BindingCellRead>,
    },
    Empty,
    Session {
        session: SessionId,
    },
    SessionFork {
        session: SessionId,
        source: SessionOrigin,
    },
    ModelExport {
        format: ModelFormat,
        byte_length: u64,
        sha256: String,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
        /// New for every editor process. Session ids restart with the
        /// process, so a client that remembers one checks it names this.
        instance: String,
    },
    Node {
        node: NodeId,
        /// Textures the edit deleted because no part draws them any more.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<TexId>,
    },
    Param {
        param: ParamId,
    },
    Slot {
        slot: SlotAddr,
    },
    Params {
        params: Vec<ParamInfo>,
    },
    Bindings {
        bindings: Vec<BindingInfo>,
    },
    Texture {
        texture: TexId,
        /// Textures the edit deleted because no part draws them any more.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<TexId>,
    },
    Textures {
        textures: Vec<TexInfo>,
    },
    Tree {
        root: TreeNode,
    },
    /// Boxed: by far the largest reply, and every reply pays [`Reply`]'s size.
    NodeInfo {
        node: Box<NodeInfo>,
    },
    Status {
        status: StatusInfo,
    },
    Warnings {
        warnings: Vec<String>,
    },
    Preview {
        preview: PreviewInfo,
    },
    Saved {
        path: String,
    },
    Presence {
        presence: Option<Presence>,
    },
    Slots {
        slots: Vec<SlotInfo>,
    },
    Welds {
        welds: Vec<WeldInfo>,
    },
    /// The slots a mesh edit emptied on `node`, in the part's slot order.
    Emptied {
        node: NodeId,
        slots: Vec<SlotId>,
    },
    SpineFit {
        node: NodeId,
        /// The param each link's bend is read from, in link order.
        params: Vec<ParamId>,
    },
    Extensions {
        extensions: Vec<ExtensionInfo>,
    },
    /// For a byte value the bytes are the reply's payload.
    Extension {
        key: ExtensionKey,
        value: ExtensionValueInfo,
    },
    ExtensionWarnings {
        warnings: Vec<ExtensionWarningInfo>,
    },
    HumanoidMapped {
        mapping: ControlMappingInfo,
        /// The mapping this one replaced.
        replaced: Option<ControlMappingInfo>,
    },
    /// The mapping a [`Command::HumanoidUnmap`] removed.
    HumanoidUnmapped {
        mapping: ControlMappingInfo,
    },
    Humanoid {
        /// In control name order.
        mappings: Vec<ControlMappingInfo>,
        /// Every standard control, in the standard's order.
        controls: Vec<StandardControlInfo>,
    },
    /// What a [`Command::ExpressionSet`] stored.
    Expression {
        group: ExpressionGroup,
        expression: ExpressionInfo,
        /// The member of the same name this one replaced.
        replaced: Option<ExpressionInfo>,
    },
    /// What a [`Command::ExpressionDelete`] removed.
    ExpressionDeleted {
        group: ExpressionGroup,
        expression: ExpressionInfo,
    },
    Expressions {
        /// In author order, as are the visemes.
        expressions: Vec<ExpressionInfo>,
        visemes: Vec<ExpressionInfo>,
        /// The standard expression names, each an emotion.
        emotion_names: Vec<String>,
        viseme_names: Vec<String>,
    },
    /// By viseme name, in the standard order.
    VisemesAdded {
        added: Vec<String>,
        replaced: Vec<String>,
        skipped: Vec<String>,
    },
    Clips {
        /// In the model's order.
        clips: Vec<ClipInfo>,
    },
    Review {
        review: Review,
    },
    /// The thread a [`Command::ReviewThreadAdd`] opened.
    ReviewThread {
        thread: ReviewThread,
    },
    /// The image a [`Command::ReviewImageAdd`] holds, or the one whose bytes
    /// are this reply's payload.
    ReviewImage {
        image: ReviewImage,
    },
    /// How many threads wait for the polling agent.
    ReviewPending {
        pending: u32,
        /// The `batch` a [`Command::ReviewFetch`] would return now: it
        /// changes exactly when other comments wait. Absent when none do.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<String>,
    },
    /// The threads waiting for the agent, whole, oldest first. The comments
    /// handed over read `sent`; `batch` acknowledges exactly those.
    ReviewFetched {
        threads: Vec<ReviewThread>,
        /// Opaque; absent when nothing waits.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        batch: Option<String>,
    },
    /// What a [`Command::ReviewAck`] delivered and claimed.
    ReviewAcked {
        /// Now held by the acknowledging agent.
        claimed: Vec<ThreadId>,
        /// Delivered, but another agent holds them.
        held: Vec<ReviewHeld>,
    },
    /// Held by the agent for `lease_seconds` unless claimed again.
    ReviewClaimed {
        threads: Vec<ThreadId>,
        lease_seconds: u32,
    },
    /// The version saved, or the one whose `.clm` is this reply's payload.
    Version {
        version: VersionInfo,
    },
    Versions {
        /// Oldest first.
        versions: Vec<VersionInfo>,
        kept: Kept,
        /// Why, when `kept` is `unreadable`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kept_problem: Option<String>,
    },
}

/// A retained authored state, identified by its creation revision.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EditHistoryEntry {
    pub revision: u64,
    pub parent: Option<u64>,
    pub redo: Option<u64>,
    /// Retained publication aliases, including the entry's creation revision.
    pub revisions: Vec<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct LimitInfo {
    /// Stable budget name, such as `request_bytes` or `derived_cell_vertices`.
    pub resource: String,
    /// Maximum permitted count in the resource's units.
    pub limit: u64,
    /// Observed count; byte counting may stop at the first excess.
    pub requested: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Event {
    /// The model changed; `rev` is the session's new revision. Observers
    /// re-read when their last-seen rev is older.
    ModelChanged { session: SessionId, rev: u64 },
    /// The set of open sessions changed; re-read [`Command::SessionList`].
    SessionsChanged,
    /// The session's review changed; re-read [`Command::ReviewGet`].
    ReviewChanged { session: SessionId },
    /// The session's versions changed; re-read [`Command::VersionList`].
    VersionsChanged { session: SessionId },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionInfo {
    pub session: SessionId,
    pub title: String,
    #[serde(default)]
    pub file: Option<String>,
    pub dirty: bool,
    pub rev: u64,
    pub node_count: u32,
    /// Set on a read-only session showing a saved version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewing: Option<VersionViewing>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StatusInfo {
    pub title: String,
    pub node_count: u32,
    pub param_count: u32,
    pub texture_count: u32,
    pub dirty: bool,
    pub rev: u64,
    #[serde(default)]
    pub undo_steps: u32,
    #[serde(default)]
    pub redo_steps: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TexInfo {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub sha256: String,
    pub id: TexId,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ParamInfo {
    pub id: ParamId,
    pub name: String,
    pub min: f32,
    pub max: f32,
    #[serde(default)]
    pub default: f32,
    /// Normalized keys a new binding on this axis starts with.
    pub initial_positions: Vec<f32>,
    #[serde(default)]
    pub bindings: u32,
    /// The motion response whose output this is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driven_by: Option<NodeId>,
}

/// Compact identity contribution; deform identity does not repeat vertex zeros.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum BindingIdentity {
    Scalar { scalar: f32 },
    Deform { offset: [f32; 2], vertex_count: u32 },
}

/// One binding as a panel draws it.
///
/// The grid is `[y][x]`, the transpose of the `cell: [x, y]` binding commands
/// take. A `null` in `keys` is an unauthored cell, not a zero; a `deform`
/// binding's cells are all `null`, so read [`Self::authored`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingInfo {
    /// One ordered normalized axis per input, owned only by this binding.
    pub key_positions: Vec<Vec<f32>>,
    pub identity: BindingIdentity,
    pub target: BindingTarget,
    pub param: ParamId,
    /// The param along the grid's y axis. Absent when the grid is one row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_y: Option<ParamId>,
    pub interpolate: Interpolate,
    pub width: u32,
    pub height: u32,
    pub keys: Vec<Vec<Option<f32>>>,
    pub authored: Vec<Vec<bool>>,
}

/// One slot; `vertex` absent means unfilled, and the weld pairs naming it
/// skip it.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotInfo {
    pub id: SlotId,
    #[serde(default)]
    pub vertex: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct WeldInfo {
    pub a: NodeId,
    pub b: NodeId,
    pub pairs: Vec<SlotPair>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TreeNode {
    pub id: NodeId,
    pub name: String,
    pub kind: NodeKind,
    pub z_order: f32,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[expect(
        clippy::use_self,
        reason = "the Python generator reads type names from this source literally"
    )]
    pub children: Vec<TreeNode>,
}

const fn yes() -> bool {
    true
}

/// One node in full. Settable fields carry [`NodePatch`]'s names so an
/// inspector can send a value straight back; fields a node's kind lacks are
/// absent.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NodeInfo {
    pub id: NodeId,
    pub kind: NodeKind,
    /// Absent on the root, which is the one node with no parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<NodeId>,
    pub name: String,
    pub translate: [f32; 3],
    pub rotate: [f32; 3],
    pub scale: [f32; 2],
    pub z_order: f32,
    pub enabled: bool,
    pub lock_to_root: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<BlendMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_tint: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_threshold: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<TexId>,
    /// Absent on kinds without a mesh, so `0` means an empty mesh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triangle_count: Option<u32>,
    /// Composite: forward mesh-group deformation to children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate_meshgroup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mg_translate_children: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_response: Option<MotionResponse>,
    /// The existing spine that fitting this mesh will remeasure, across any response chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fitted_spine: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spine: Option<SpineInfo>,
    /// Ordered clipping rules. Empty on nodes that do not draw.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<MaskInfo>,
}

/// A mesh's authored vertices, UVs, triangles and origin. A client may send
/// these fields directly to `mesh_set`; posing never changes this read.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MeshInfo {
    pub verts: Vec<[f32; 2]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<[u32; 3]>,
    pub origin: [f32; 2],
}

/// One ordered clipping rule, addressed by its position in `NodeInfo::masks`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MaskInfo {
    pub source: NodeId,
    pub mode: MaskMode,
}

/// A spine; `joints` and `targets` are always the same length, `null` for a
/// rigid link.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SpineInfo {
    pub joints: Vec<[f32; 2]>,
    pub targets: Vec<Option<ParamId>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
/// The size of a [`Command::Preview`] PNG, which is the reply's payload.
pub struct PreviewInfo {
    pub width: u32,
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid id")
    }

    #[test]
    fn the_new_refusals_travel_as_their_own_words() {
        for (code, word) in [
            (ErrorCode::BulkOverHttp, "\"bulk_over_http\""),
            (ErrorCode::RevisionUnavailable, "\"revision_unavailable\""),
            (ErrorCode::LimitExceeded, "\"limit_exceeded\""),
        ] {
            let json = serde_json::to_string(&code).expect("a code serializes");
            assert_eq!(json, word);
            let back: ErrorCode = serde_json::from_str(&json).expect("and parses back");
            assert_eq!(back, code);
        }
    }

    #[test]
    fn reply_variants_roundtrip() {
        let ok = Reply::Ok {
            id: 7,
            rev: Some(4),
            body: ResponseBody::Session {
                session: SessionId(3),
            },
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"reply\":\"ok\""));
        assert!(s.contains("\"rev\":4"), "{s}");
        assert!(s.contains("\"result\":\"session\""));
        let evt = Reply::Event(Event::ModelChanged {
            session: SessionId(3),
            rev: 9,
        });
        let s = serde_json::to_string(&evt).unwrap();
        let back: Reply = serde_json::from_str(&s).unwrap();
        assert!(matches!(
            back,
            Reply::Event(Event::ModelChanged { rev: 9, .. })
        ));
    }

    #[test]
    fn an_error_reply_carries_a_code_a_client_can_match_on() {
        let s = serde_json::to_string(&Reply::Err {
            id: 3,
            code: ErrorCode::UnknownSlot,
            message: r#"part "body/arm" carries no vertex slot "elbow""#.into(),
            op_index: None,
            limit: None,
        })
        .unwrap();
        assert!(s.contains("\"code\":\"unknown_slot\""), "{s}");
        let back: Reply = serde_json::from_str(&s).unwrap();
        assert!(matches!(
            back,
            Reply::Err {
                code: ErrorCode::UnknownSlot,
                ..
            }
        ));
    }

    #[test]
    fn a_reply_that_names_no_session_carries_no_revision() {
        let s = serde_json::to_string(&Reply::Ok {
            id: 1,
            rev: None,
            body: ResponseBody::Sessions {
                sessions: Vec::new(),
                instance: "a1".into(),
            },
        })
        .unwrap();
        assert!(!s.contains("rev"), "{s}");
        let back: Reply = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, Reply::Ok { rev: None, .. }));
    }

    /// [`NodeInfo`]'s settable fields must parse as a [`NodePatch`] for the
    /// inspector round trip.
    #[test]
    fn a_node_info_reply_parses_as_the_patch_that_would_restore_it() {
        let info = NodeInfo {
            id: node("root/part-1"),
            kind: NodeKind::Part,
            parent: Some(node("root")),
            name: "Body".into(),
            translate: [1.0, 2.0, 3.0],
            rotate: [0.25, 0.5, 0.75],
            scale: [2.0, 3.0],
            z_order: 4.0,
            enabled: false,
            lock_to_root: true,
            opacity: Some(0.5),
            blend_mode: Some(BlendMode::Multiply),
            tint: Some([0.1, 0.2, 0.3]),
            screen_tint: Some([0.4, 0.5, 0.6]),
            mask_threshold: Some(0.75),
            texture: Some(TexId::new("tex-1").unwrap()),
            vertex_count: Some(4),
            triangle_count: Some(2),
            propagate_meshgroup: None,
            mg_translate_children: None,
            motion_response: None,
            spine: None,
            fitted_spine: None,
            masks: Vec::new(),
        };
        let line = serde_json::to_string(&info).unwrap();
        let mut settable: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&line).unwrap();
        for read_only in [
            "id",
            "kind",
            "parent",
            "vertex_count",
            "triangle_count",
            "masks",
        ] {
            settable.remove(read_only);
        }
        let patch: NodePatch = serde_json::from_value(settable.into()).unwrap();

        assert_eq!(patch.name.as_deref(), Some("Body"));
        assert_eq!(patch.translate, Some([1.0, 2.0, 3.0]));
        assert_eq!(patch.rotate, Some([0.25, 0.5, 0.75]));
        assert_eq!(patch.scale, Some([2.0, 3.0]));
        assert_eq!(patch.z_order, Some(4.0));
        assert_eq!(patch.enabled, Some(false));
        assert_eq!(patch.lock_to_root, Some(true));
        assert_eq!(patch.opacity, Some(0.5));
        assert_eq!(patch.blend_mode, Some(BlendMode::Multiply));
        assert_eq!(patch.tint, Some([0.1, 0.2, 0.3]));
        assert_eq!(patch.screen_tint, Some([0.4, 0.5, 0.6]));
        assert_eq!(patch.mask_threshold, Some(0.75));
        assert_eq!(
            patch
                .texture
                .as_ref()
                .map(|t| t.as_ref().map(TexId::as_str)),
            Some(Some("tex-1")),
        );
        assert!(line.contains("\"vertex_count\":4"), "{line}");
        assert!(line.contains("\"triangle_count\":2"), "{line}");
        assert!(!line.contains("mg_translate_children"), "{line}");
        assert_eq!(patch.mg_translate_children, None);
        assert_eq!(patch.propagate_meshgroup, None);
        assert!(!line.contains("motion_response"), "{line}");
    }

    #[test]
    fn a_node_info_reply_names_the_node_its_kind_and_its_parent() {
        let body = ResponseBody::NodeInfo {
            node: Box::new(NodeInfo {
                id: node("root/part-1"),
                kind: NodeKind::Part,
                parent: Some(node("root")),
                name: "Body".into(),
                translate: [0.0; 3],
                rotate: [0.0; 3],
                scale: [1.0, 1.0],
                z_order: 0.0,
                enabled: true,
                lock_to_root: false,
                opacity: Some(1.0),
                blend_mode: Some(BlendMode::Normal),
                tint: Some([1.0; 3]),
                screen_tint: Some([0.0; 3]),
                mask_threshold: Some(0.5),
                texture: None,
                motion_response: None,
                spine: None,
                fitted_spine: None,
                masks: Vec::new(),
                vertex_count: Some(0),
                triangle_count: Some(0),
                propagate_meshgroup: None,
                mg_translate_children: None,
            }),
        };
        let s = serde_json::to_string(&body).unwrap();
        assert!(s.contains("\"result\":\"node_info\""), "{s}");
        assert!(s.contains("\"id\":\"root/part-1\""), "{s}");
        assert!(s.contains("\"parent\":\"root\""), "{s}");
        assert!(!s.contains("texture"), "{s}");

        assert_eq!(
            Command::NodeInfo {
                session: SessionId(1),
                node: node("root/part-1"),
            }
            .kind(),
            CommandKind::ReplicaQuery,
        );
    }

    #[test]
    fn a_reply_tolerates_a_field_it_does_not_know() {
        let line = r#"{"reply":"event","event":"sessions_changed","later":true}"#;
        serde_json::from_str::<Reply>(line).unwrap();
    }
}
