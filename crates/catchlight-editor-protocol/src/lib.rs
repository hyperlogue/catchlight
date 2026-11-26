//! Wire types for the catchlight editor.
//!
//! Ids travel as the strings the model file stores, never per-session
//! handles, so [`Command::RenameId`] and texture drops break addons that
//! named the old Id. An add names its new Id under the reply's word (`node`,
//! `param`, …), never `id`: [`Request`] flattens its command beside its own
//! correlation `id`.

pub use catchlight_core::{MotionResponse, ResponseInput, ResponseSource};
use serde::{Deserialize, Serialize};

use catchlight_core::extension::IdRef;
pub use catchlight_core::id::{ExtensionKey, NodeId, ParamId, SlotId, TexId};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionId(pub u64);

/// A review thread's identity within its session; never reused.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ThreadId(pub u64);

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
}

/// The correlation id alone, so a request whose body did not parse is still
/// answered against it.
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct RequestId {
    pub id: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Command {
    #[serde(rename = "session_create")]
    SessionNew {
        #[serde(default)]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<SessionSource>,
    },
    /// Open the `.clm` the server's store holds at `path`.
    SessionOpen {
        path: String,
    },
    SessionFork {
        session: SessionId,
        if_rev: u64,
        #[serde(default)]
        name: Option<String>,
    },
    ModelExport {
        session: SessionId,
        if_rev: u64,
    },
    SessionList,
    SessionClose {
        session: SessionId,
    },
    #[serde(rename = "session_save")]
    Save {
        session: SessionId,
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "manifest_export")]
    ExportManifest {
        session: SessionId,
        path: String,
    },
    #[serde(rename = "session_get")]
    Status {
        session: SessionId,
    },
    #[serde(rename = "model_check")]
    Check {
        session: SessionId,
    },
    #[serde(rename = "node_tree_get")]
    NodeTree {
        session: SessionId,
    },
    #[serde(rename = "node_get")]
    NodeInfo {
        session: SessionId,
        node: NodeId,
    },
    MeshGet {
        session: SessionId,
        node: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
    },
    ModelGet {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
    },
    GeometryGet {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(default)]
        pose: Vec<ParamPose>,
        nodes: Vec<NodeId>,
        fields: Vec<GeometryField>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vertices: Option<IndexRange>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        triangles: Option<IndexRange>,
    },
    NodeAdd {
        session: SessionId,
        parent: NodeId,
        kind: NodeKindArg,
        #[serde(default)]
        name: Option<String>,
        /// Absent generates one; an existing Id is [`ErrorCode::DuplicateId`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<NodeId>,
    },
    NodeSet {
        session: SessionId,
        node: NodeId,
        #[serde(flatten)]
        patch: NodePatch,
    },
    NodeReparent {
        session: SessionId,
        node: NodeId,
        to: NodeId,
    },
    /// Move a node within its parent's children (clamped to the end).
    NodeReorder {
        session: SessionId,
        node: NodeId,
        index: u32,
    },

    /// Deep-copy a node's subtree as its next sibling; copies get fresh Ids.
    NodeDuplicate {
        session: SessionId,
        node: NodeId,
    },
    /// Change an Id and every reference this model holds to it. Breaks addons:
    /// nothing outside the model is rewritten and no alias remains.
    #[serde(rename = "id_rename")]
    RenameId {
        session: SessionId,
        rename: Rename,
    },
    MaskAdd {
        session: SessionId,
        node: NodeId,
        source: NodeId,
        mode: MaskMode,
    },

    MaskDelete {
        session: SessionId,
        node: NodeId,
        index: u32,
    },
    MotionResponseSet {
        session: SessionId,
        node: NodeId,
        response: MotionResponse,
    },
    NodeDelete {
        session: SessionId,
        node: NodeId,
    },
    /// Add a texture from attachment `texture` and assign it to part `node` in
    /// one edit, dropping the old texture if nothing else draws it.
    TextureAdd {
        session: SessionId,
        node: NodeId,
        #[serde(default)]
        encoding: TextureEncoding,
        /// Absent generates one; an existing Id is [`ErrorCode::DuplicateId`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        texture: Option<TexId>,
    },
    TextureList {
        session: SessionId,
    },
    ParamAdd {
        session: SessionId,
        name: String,
        #[serde(default)]
        min: f32,
        #[serde(default = "one")]
        max: f32,
        #[serde(default)]
        default: f32,
        /// Absent generates one; an existing Id is [`ErrorCode::DuplicateId`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        param: Option<ParamId>,
    },
    ParamList {
        session: SessionId,
    },
    /// Change param metadata; absent = unchanged. Key positions are
    /// normalized, so a range change doesn't move them.
    ParamSet {
        session: SessionId,
        param: ParamId,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        min: Option<f32>,
        #[serde(default)]
        max: Option<f32>,
        #[serde(default)]
        default: Option<f32>,
    },
    ParamDelete {
        session: SessionId,
        param: ParamId,
    },
    /// Insert a distinct key position at normalized `value` in [0, 1].
    BindingKeyInsert {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        axis: ParamId,
        value: f32,
    },
    /// Remove a key position while retaining at least one position on the axis.
    BindingKeyDelete {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        axis: ParamId,
        index: u32,
    },
    /// Move a key position to normalized `value` in [0, 1], preserving
    /// strict ordering with its neighbors.
    BindingKeyMove {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        axis: ParamId,
        index: u32,
        value: f32,
    },

    /// Atomically execute a bounded list against one revision of the model.
    EditApply {
        session: SessionId,
        if_rev: u64,
        edits: Vec<EditOp>,
    },
    /// Evaluate the same list on a captured clone without publishing it.
    EditValidate {
        session: SessionId,
        if_rev: u64,
        edits: Vec<EditOp>,
    },
    BindingCellsGet {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        cells: Vec<[u32; 2]>,
        #[serde(default)]
        include_derived: bool,
        /// Optional page of each deform array; scalar reads reject this field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        vertices: Option<IndexRange>,
    },
    BindingCellsSet {
        session: SessionId,
        if_rev: u64,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        cells: Vec<BindingCellWrite>,
    },
    BindingCellsUnset {
        session: SessionId,
        if_rev: u64,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        cells: Vec<[u32; 2]>,
    },
    BindingAdd {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_positions: Option<Vec<Vec<f32>>>,
    },

    BindingDelete {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
    },
    #[serde(rename = "binding_interpolation_set")]
    BindingInterpolate {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        mode: Interpolate,
    },

    BindingList {
        session: SessionId,
        node: NodeId,
    },

    /// Replace a Part/MeshGroup mesh, re-fitting its deform bindings in the same
    /// step. Answers with the slots the new mesh emptied.
    MeshSet {
        session: SessionId,
        /// Refuse a stale draft or gesture while holding the session lock.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        node: NodeId,
        verts: Vec<[f32; 2]>,
        /// One `[u, v]` per vertex, as many as `verts`.
        uvs: Vec<[f32; 2]>,
        /// One `[a, b, c]` per triangle, each a `verts` index.
        indices: Vec<[u32; 3]>,
        origin: [f32; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deform_mapping: Option<Vec<Vec<VertexWeight>>>,
    },
    /// Trace a part's mesh from its texture's alpha and apply it like
    /// [`Command::MeshSet`].
    #[serde(rename = "mesh_generate")]
    MeshAuto {
        session: SessionId,
        node: NodeId,
        /// Absent is [`AutoMesh::Contour`] with every knob at its default.
        #[serde(default)]
        mode: AutoMesh,
    },

    SlotAdd {
        session: SessionId,
        node: NodeId,
        /// Absent generates one (`slot-<8 hex>`, re-drawn until it is free on
        /// the part). The reply names it either way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<SlotId>,
    },
    SlotFill {
        session: SessionId,
        node: NodeId,
        slot: SlotId,
        vertex: u32,
    },
    /// Unfill a slot. The weld pairs that name it keep it and skip it until it
    /// is filled again.
    SlotClear {
        session: SessionId,
        node: NodeId,
        slot: SlotId,
    },
    /// Remove a slot from a part, and with it the weld pairs that named it.
    SlotDelete {
        session: SessionId,
        node: NodeId,
        slot: SlotId,
    },
    #[serde(rename = "slot_list")]
    Slots {
        session: SessionId,
        node: NodeId,
    },
    #[serde(rename = "weld_list")]
    Welds {
        session: SessionId,
    },

    /// Weld two parts together, replacing any weld already pairing them. Each
    /// pair names a slot on `a` and a slot on `b`; empty `pairs` records the
    /// two parts as welded and joins nothing yet.
    WeldSet {
        session: SessionId,
        a: NodeId,
        b: NodeId,
        #[serde(default)]
        pairs: Vec<SlotPair>,
    },
    /// Unmake the weld pairing two parts, named in either order.
    WeldDelete {
        session: SessionId,
        a: NodeId,
        b: NodeId,
    },
    /// Add a spine node. Empty `joints`, or a joint repeating the point above
    /// it, is refused.
    SpineAdd {
        session: SessionId,
        parent: NodeId,
        #[serde(default)]
        name: Option<String>,
        /// The far end of each link, in the node's own space, root to tip.
        /// `joints[0]` ends the link that starts at the node itself.
        joints: Vec<[f32; 2]>,
        /// One param per link, in link order, `None` where a link is rigid.
        /// Absent binds none; present, it must be exactly as long as `joints`.
        #[serde(default)]
        targets: Option<Vec<Option<ParamId>>>,
        /// Absent generates one; an existing Id is [`ErrorCode::DuplicateId`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<NodeId>,
    },
    /// Change fields on a spine node; absent = unchanged.
    ///
    /// `joints` and `targets` in one command apply in that order, so `targets`
    /// is measured against the length `joints` just set rather than the one it
    /// replaced.
    SpineSet {
        session: SessionId,
        node: NodeId,
        /// The whole spine, root to tip. Targets follow the new length: a link
        /// past the new end loses its param, a new link arrives rigid.
        #[serde(default)]
        joints: Option<Vec<[f32; 2]>>,
        #[serde(default)]
        targets: Option<Vec<Option<ParamId>>>,
    },
    /// Rig a strand of art to a spine and its bend params in one edit, with no
    /// bindings. The spine goes between the part and its parent, keeping the
    /// part's world placement; a re-fit retains params of surviving links.
    SpineFit {
        session: SessionId,
        part: NodeId,
        /// How many links to divide the art into. Zero is refused.
        links: u32,
        /// The direction the strand hangs, in the part's own frame. Absent is
        /// straight down; the vector is normalised, and one too short to
        /// normalise is refused.
        #[serde(default)]
        axis: Option<[f32; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<NodeId>,
        #[serde(default)]
        name: Option<String>,
    },
    #[serde(rename = "edit_undo")]
    Undo {
        session: SessionId,
        if_rev: u64,
    },
    #[serde(rename = "edit_redo")]
    Redo {
        session: SessionId,
        if_rev: u64,
    },
    EditGoto {
        session: SessionId,
        if_rev: u64,
        revision: u64,
    },
    EditHistoryGet {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
    },
    /// Publish ephemeral view state (pose / camera / selection) — a separate
    /// path from the model: never bumps rev, never undone, never saved.
    PresenceSet {
        session: SessionId,
        #[serde(flatten)]
        presence: Presence,
    },
    /// Read the session's current shared presence (latest publisher wins).
    PresenceGet {
        session: SessionId,
    },

    /// Render one frame as the reply's PNG payload. An absent camera is the
    /// default, never a tab's last view.
    #[serde(rename = "preview_render")]
    Preview {
        session: SessionId,
        #[serde(default)]
        pose: Vec<ParamPose>,
        #[serde(default)]
        size: Option<[u32; 2]>,
        #[serde(default)]
        camera: Option<Camera>,
    },

    /// Import a `.clm` structure as JSON under the required `parent`, as one
    /// undoable edit. Attachment `structure` is the JSON; each `textures` entry
    /// arrives as `texture:<texture>`. Byte-extension markers are refused.
    #[serde(rename = "structure_json_import")]
    ImportJson {
        session: SessionId,
        parent: NodeId,
        if_rev: u64,
        #[serde(default)]
        textures: Vec<ImportTexture>,
    },
    /// Set the extension under `key`. The `catchlight.` prefix is
    /// [`ErrorCode::ReservedExtension`].
    ExtensionSet {
        session: SessionId,
        key: ExtensionKey,
        value: ExtensionSet,
    },
    /// Drop the extension under `key`; a missing key is
    /// [`ErrorCode::NoExtension`].
    ExtensionDelete {
        session: SessionId,
        key: ExtensionKey,
    },
    /// Every extension in key order; a byte value as its size and hash.
    #[serde(rename = "extension_list")]
    Extensions {
        session: SessionId,
    },
    /// One extension's value; bytes come back as the reply's payload.
    ExtensionGet {
        session: SessionId,
        key: ExtensionKey,
    },
    /// Every finding about the model's maintained extensions: references
    /// that no longer resolve, each extension's own check, and data this
    /// editor cannot read or check.
    ExtensionWarnings {
        session: SessionId,
    },

    /// Map a humanoid control onto a param, replacing its mapping. Without
    /// `stops`, a two-sided control spans the param's min, default and max,
    /// and a one-sided control rests at the default and reaches the end of
    /// the range farther from it. A control outside the standard list is
    /// [`ErrorCode::NonstandardControl`] unless `nonstandard`.
    HumanoidMap {
        session: SessionId,
        control: String,
        param: ParamId,
        /// The param values the control's -1, 0 and +1 land on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stops: Option<[f32; 3]>,
        #[serde(default)]
        nonstandard: bool,
    },
    /// Remove a control's mapping; removing the last removes the extension.
    HumanoidUnmap {
        session: SessionId,
        control: String,
    },
    /// The control mappings, and the standard controls.
    HumanoidGet {
        session: SessionId,
    },
    /// Create or replace an expression or viseme from its settings, kept in
    /// the order given. A control value must lie in its control's range.
    ExpressionSet {
        session: SessionId,
        group: ExpressionGroup,
        name: String,
        settings: Vec<ExpressionSetting>,
        /// Keep a control name outside the standard list.
        #[serde(default)]
        nonstandard: bool,
    },
    /// Remove an expression or viseme; removing the last of both groups
    /// removes the extension.
    ExpressionDelete {
        session: SessionId,
        group: ExpressionGroup,
        name: String,
    },
    /// Both groups with their settings, and the standard names.
    ExpressionList {
        session: SessionId,
    },
    /// Add the standard visemes with their default settings after the
    /// existing ones. An existing viseme of a standard name is skipped, or
    /// replaced in place when `replace`.
    VisemeAddStandard {
        session: SessionId,
        #[serde(default)]
        replace: bool,
    },
    /// The clips of `catchlight.animations`, with what each lane drives.
    ClipList {
        session: SessionId,
    },

    /// The session's review: every thread, and the agents attending to it.
    ReviewGet {
        session: SessionId,
    },
    /// Open a thread on `target` with its first comment. `pose` is what the
    /// author was looking at.
    ReviewThreadAdd {
        session: SessionId,
        target: ReviewTarget,
        author: ReviewAuthor,
        body: String,
        #[serde(default)]
        pose: Vec<ParamPose>,
        /// Images from [`Command::ReviewImageAdd`], each usable once.
        #[serde(default)]
        images: Vec<u64>,
        /// A retry key: the same author writing again under it gets the
        /// thread it opened back and opens nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// A saved version the comment points at, such as the one holding a
        /// fix; [`ErrorCode::NoVersion`] unless the session has it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u64>,
    },
    ReviewThreadSet {
        session: SessionId,
        thread: ThreadId,
        status: ThreadStatus,
    },
    ReviewThreadDelete {
        session: SessionId,
        thread: ThreadId,
    },
    /// Reply on a thread. An agent's reply releases its own claim on it.
    ReviewCommentAdd {
        session: SessionId,
        thread: ThreadId,
        author: ReviewAuthor,
        body: String,
        /// Images from [`Command::ReviewImageAdd`], each usable once.
        #[serde(default)]
        images: Vec<u64>,
        /// As for [`Command::ReviewThreadAdd`]: a repeat answers with the
        /// thread and adds nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// As for [`Command::ReviewThreadAdd`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u64>,
    },
    /// Removing a thread's only comment removes the thread.
    ReviewCommentDelete {
        session: SessionId,
        thread: ThreadId,
        comment: u64,
    },
    /// Hold attachment `image` (PNG, JPEG or WebP) for a comment about to be
    /// written. One no comment claims is dropped as newer ones arrive.
    ReviewImageAdd {
        session: SessionId,
    },
    /// An image's bytes, as the reply's payload.
    ReviewImageGet {
        session: SessionId,
        image: u64,
    },
    /// Hand every unsent human comment on an open thread to the agent.
    ReviewSubmit {
        session: SessionId,
    },
    /// An agent's heartbeat while it waits for comments: keeps it shown as
    /// listening and answers how many threads wait for it.
    ReviewPoll {
        session: SessionId,
        agent: ReviewIdentity,
    },
    /// Read the threads waiting for `agent`: handed over and claimed by no
    /// other agent. Changes nothing but presence, so a second fetch before
    /// [`Command::ReviewAck`] returns the same comments.
    ReviewFetch {
        session: SessionId,
        agent: ReviewIdentity,
    },
    /// Mark exactly the comments of a fetched `batch` delivered once the agent
    /// has them, and claim their threads for it where nobody else holds them.
    /// Refused with `batch_stale`, changing nothing, when any of them was
    /// deleted, resolved or delivered since.
    ReviewAck {
        session: SessionId,
        agent: ReviewIdentity,
        batch: String,
    },
    /// Take or renew a lease on each thread for `agent`; all or none. A thread
    /// another agent holds is refused with `thread_claimed`.
    ReviewClaim {
        session: SessionId,
        agent: ReviewIdentity,
        threads: Vec<ThreadId>,
    },
    /// Give up `agent`'s own claims on the threads; another's are left alone.
    ReviewRelease {
        session: SessionId,
        agent: ReviewIdentity,
        threads: Vec<ThreadId>,
    },

    /// Keep the session's model as it is now as the next numbered version.
    /// Moves no revision, writes no file and leaves `dirty` alone.
    VersionSave {
        session: SessionId,
        /// One line each; blank is absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        author: ReviewAuthor,
        /// As for [`Command::ReviewThreadAdd`]: the same author saving again
        /// under it with the same label and summary gets the version it saved
        /// back and saves nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// The session's versions, oldest first.
    VersionList {
        session: SessionId,
    },
    /// One version's `.clm`, byte for byte as it was saved, as the reply's
    /// payload.
    VersionGet {
        session: SessionId,
        version: u64,
    },
    /// Replace the session's model with a version's, as one undoable edit.
    VersionRestore {
        session: SessionId,
        version: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
    },
    /// A read-only session holding a version's model, for looking at it
    /// beside the live one; every edit to it is [`ErrorCode::ReadOnly`]. One
    /// already open on the same version is answered again. It closes with
    /// the session it came from.
    VersionOpen {
        session: SessionId,
        version: u64,
    },
}

/// What applying a [`Command`] does to the model it addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// Changes the model or which sessions exist; the only kind that moves `rev`.
    Edit,
    /// Publishes shared view state (pose, camera, selection); changes no model.
    Presence,
    /// Changes the session's review threads; changes no model.
    Review,
    /// Changes the session's saved versions; changes no model.
    Version,
    /// A pure function of the model, so a replica answers it locally.
    ReplicaQuery,
    /// Needs the editor's sessions, store or renderer. `export_manifest` is here
    /// because what it writes lands in the store, not the session.
    ServerQuery,
}

/// Every command's wire tag paired with its [`CommandKind`]. Hand-maintained;
/// `xtask generate` checks it against the enum.
pub const COMMAND_KINDS: &[(&str, CommandKind)] = &[
    ("model_export", CommandKind::ServerQuery),
    ("session_fork", CommandKind::Edit),
    ("geometry_get", CommandKind::ReplicaQuery),
    ("model_get", CommandKind::ReplicaQuery),
    ("mesh_get", CommandKind::ReplicaQuery),
    ("binding_cells_unset", CommandKind::Edit),
    ("binding_cells_set", CommandKind::Edit),
    ("binding_cells_get", CommandKind::ReplicaQuery),
    ("edit_validate", CommandKind::ServerQuery),
    ("edit_apply", CommandKind::Edit),
    ("session_create", CommandKind::Edit),
    ("session_open", CommandKind::Edit),
    ("session_list", CommandKind::ServerQuery),
    ("session_close", CommandKind::Edit),
    ("session_save", CommandKind::Edit),
    ("manifest_export", CommandKind::ServerQuery),
    ("session_get", CommandKind::ServerQuery),
    ("model_check", CommandKind::ReplicaQuery),
    ("node_tree_get", CommandKind::ReplicaQuery),
    ("node_get", CommandKind::ReplicaQuery),
    ("node_add", CommandKind::Edit),
    ("node_set", CommandKind::Edit),
    ("node_reparent", CommandKind::Edit),
    ("node_reorder", CommandKind::Edit),
    ("node_duplicate", CommandKind::Edit),
    ("id_rename", CommandKind::Edit),
    ("mask_add", CommandKind::Edit),
    ("mask_delete", CommandKind::Edit),
    ("motion_response_set", CommandKind::Edit),
    ("node_delete", CommandKind::Edit),
    ("texture_add", CommandKind::Edit),
    ("texture_list", CommandKind::ReplicaQuery),
    ("param_add", CommandKind::Edit),
    ("param_list", CommandKind::ReplicaQuery),
    ("param_set", CommandKind::Edit),
    ("param_delete", CommandKind::Edit),
    ("binding_key_insert", CommandKind::Edit),
    ("binding_key_delete", CommandKind::Edit),
    ("binding_key_move", CommandKind::Edit),
    ("binding_add", CommandKind::Edit),
    ("binding_delete", CommandKind::Edit),
    ("binding_interpolation_set", CommandKind::Edit),
    ("binding_list", CommandKind::ReplicaQuery),
    ("mesh_set", CommandKind::Edit),
    ("mesh_generate", CommandKind::Edit),
    ("slot_add", CommandKind::Edit),
    ("slot_fill", CommandKind::Edit),
    ("slot_clear", CommandKind::Edit),
    ("slot_delete", CommandKind::Edit),
    ("slot_list", CommandKind::ReplicaQuery),
    ("weld_list", CommandKind::ReplicaQuery),
    ("weld_set", CommandKind::Edit),
    ("weld_delete", CommandKind::Edit),
    ("spine_add", CommandKind::Edit),
    ("spine_set", CommandKind::Edit),
    ("spine_fit", CommandKind::Edit),
    ("edit_undo", CommandKind::Edit),
    ("edit_redo", CommandKind::Edit),
    ("edit_goto", CommandKind::Edit),
    ("edit_history_get", CommandKind::ServerQuery),
    ("presence_set", CommandKind::Presence),
    ("presence_get", CommandKind::ServerQuery),
    ("preview_render", CommandKind::ServerQuery),
    ("structure_json_import", CommandKind::Edit),
    ("extension_set", CommandKind::Edit),
    ("extension_delete", CommandKind::Edit),
    ("extension_list", CommandKind::ReplicaQuery),
    ("extension_get", CommandKind::ServerQuery),
    ("extension_warnings", CommandKind::ReplicaQuery),
    ("humanoid_map", CommandKind::Edit),
    ("humanoid_unmap", CommandKind::Edit),
    ("humanoid_get", CommandKind::ReplicaQuery),
    ("expression_set", CommandKind::Edit),
    ("expression_delete", CommandKind::Edit),
    ("expression_list", CommandKind::ReplicaQuery),
    ("viseme_add_standard", CommandKind::Edit),
    ("clip_list", CommandKind::ReplicaQuery),
    ("review_get", CommandKind::ServerQuery),
    ("review_thread_add", CommandKind::Review),
    ("review_thread_set", CommandKind::Review),
    ("review_thread_delete", CommandKind::Review),
    ("review_comment_add", CommandKind::Review),
    ("review_comment_delete", CommandKind::Review),
    ("review_image_add", CommandKind::Review),
    ("review_image_get", CommandKind::ServerQuery),
    ("review_submit", CommandKind::Review),
    ("review_poll", CommandKind::Review),
    ("review_fetch", CommandKind::Review),
    ("review_ack", CommandKind::Review),
    ("review_claim", CommandKind::Review),
    ("review_release", CommandKind::Review),
    ("version_save", CommandKind::Version),
    ("version_list", CommandKind::ServerQuery),
    ("version_get", CommandKind::ServerQuery),
    ("version_restore", CommandKind::Edit),
    ("version_open", CommandKind::Edit),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    Fixed(&'static str),
    /// `<family>:<suffix>`, any number; the command checks the suffixes.
    Family(&'static str),
    /// Exactly this name, needed or not as the command's own fields decide.
    Optional(&'static str),
}

impl Attachment {
    pub fn admits(&self, name: &str) -> bool {
        match self {
            Self::Fixed(fixed) | Self::Optional(fixed) => name == *fixed,
            Self::Family(family) => name
                .strip_prefix(family)
                .and_then(|rest| rest.strip_prefix(':'))
                .is_some_and(|suffix| !suffix.is_empty()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bytes {
    pub attachments: &'static [Attachment],
    pub payload: bool,
}

/// Every command that carries bytes; a command absent here carries none.
pub const COMMAND_BYTES: &[(&str, Bytes)] = &[
    (
        "review_image_add",
        Bytes {
            attachments: &[Attachment::Fixed("image")],
            payload: false,
        },
    ),
    (
        "review_image_get",
        Bytes {
            attachments: &[],
            payload: true,
        },
    ),
    (
        "texture_add",
        Bytes {
            attachments: &[Attachment::Fixed("texture")],
            payload: false,
        },
    ),
    (
        "session_create",
        Bytes {
            attachments: &[
                Attachment::Optional("model"),
                Attachment::Optional("structure"),
                Attachment::Optional("manifest"),
                Attachment::Family("texture"),
            ],
            payload: false,
        },
    ),
    (
        "structure_json_import",
        Bytes {
            attachments: &[
                Attachment::Fixed("structure"),
                Attachment::Family("texture"),
            ],
            payload: false,
        },
    ),
    (
        "preview_render",
        Bytes {
            attachments: &[],
            payload: true,
        },
    ),
    (
        "model_export",
        Bytes {
            attachments: &[],
            payload: true,
        },
    ),
    (
        "extension_set",
        Bytes {
            attachments: &[Attachment::Optional("value")],
            payload: false,
        },
    ),
    (
        "extension_get",
        Bytes {
            attachments: &[],
            payload: true,
        },
    ),
    (
        "version_get",
        Bytes {
            attachments: &[],
            payload: true,
        },
    ),
];

impl Command {
    /// The `cmd` tag. Exhaustive so [`COMMAND_KINDS`] stays checkable.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::EditApply { .. } => "edit_apply",
            Self::EditValidate { .. } => "edit_validate",
            Self::BindingCellsGet { .. } => "binding_cells_get",
            Self::BindingCellsSet { .. } => "binding_cells_set",
            Self::BindingCellsUnset { .. } => "binding_cells_unset",
            Self::MeshGet { .. } => "mesh_get",
            Self::ModelGet { .. } => "model_get",
            Self::GeometryGet { .. } => "geometry_get",
            Self::SessionFork { .. } => "session_fork",
            Self::ModelExport { .. } => "model_export",
            Self::SessionNew { .. } => "session_create",
            Self::SessionOpen { .. } => "session_open",
            Self::SessionList => "session_list",
            Self::SessionClose { .. } => "session_close",
            Self::Save { .. } => "session_save",
            Self::ExportManifest { .. } => "manifest_export",
            Self::Status { .. } => "session_get",
            Self::Check { .. } => "model_check",
            Self::NodeTree { .. } => "node_tree_get",
            Self::NodeInfo { .. } => "node_get",
            Self::NodeAdd { .. } => "node_add",
            Self::NodeSet { .. } => "node_set",
            Self::NodeReparent { .. } => "node_reparent",
            Self::NodeReorder { .. } => "node_reorder",
            Self::NodeDuplicate { .. } => "node_duplicate",
            Self::RenameId { .. } => "id_rename",
            Self::MaskAdd { .. } => "mask_add",
            Self::MaskDelete { .. } => "mask_delete",
            Self::MotionResponseSet { .. } => "motion_response_set",
            Self::NodeDelete { .. } => "node_delete",
            Self::TextureAdd { .. } => "texture_add",
            Self::TextureList { .. } => "texture_list",
            Self::ParamAdd { .. } => "param_add",
            Self::ParamList { .. } => "param_list",
            Self::ParamSet { .. } => "param_set",
            Self::ParamDelete { .. } => "param_delete",
            Self::BindingKeyInsert { .. } => "binding_key_insert",
            Self::BindingKeyDelete { .. } => "binding_key_delete",
            Self::BindingKeyMove { .. } => "binding_key_move",
            Self::BindingAdd { .. } => "binding_add",
            Self::BindingDelete { .. } => "binding_delete",
            Self::BindingInterpolate { .. } => "binding_interpolation_set",
            Self::BindingList { .. } => "binding_list",
            Self::MeshSet { .. } => "mesh_set",
            Self::MeshAuto { .. } => "mesh_generate",
            Self::SlotAdd { .. } => "slot_add",
            Self::SlotFill { .. } => "slot_fill",
            Self::SlotClear { .. } => "slot_clear",
            Self::SlotDelete { .. } => "slot_delete",
            Self::Slots { .. } => "slot_list",
            Self::Welds { .. } => "weld_list",
            Self::WeldSet { .. } => "weld_set",
            Self::WeldDelete { .. } => "weld_delete",
            Self::SpineAdd { .. } => "spine_add",
            Self::SpineSet { .. } => "spine_set",
            Self::SpineFit { .. } => "spine_fit",
            Self::Undo { .. } => "edit_undo",
            Self::Redo { .. } => "edit_redo",
            Self::EditGoto { .. } => "edit_goto",
            Self::EditHistoryGet { .. } => "edit_history_get",
            Self::PresenceSet { .. } => "presence_set",
            Self::PresenceGet { .. } => "presence_get",
            Self::Preview { .. } => "preview_render",
            Self::ImportJson { .. } => "structure_json_import",
            Self::ExtensionSet { .. } => "extension_set",
            Self::ExtensionDelete { .. } => "extension_delete",
            Self::Extensions { .. } => "extension_list",
            Self::ExtensionGet { .. } => "extension_get",
            Self::ExtensionWarnings { .. } => "extension_warnings",
            Self::HumanoidMap { .. } => "humanoid_map",
            Self::HumanoidUnmap { .. } => "humanoid_unmap",
            Self::HumanoidGet { .. } => "humanoid_get",
            Self::ExpressionSet { .. } => "expression_set",
            Self::ExpressionDelete { .. } => "expression_delete",
            Self::ExpressionList { .. } => "expression_list",
            Self::VisemeAddStandard { .. } => "viseme_add_standard",
            Self::ClipList { .. } => "clip_list",
            Self::ReviewGet { .. } => "review_get",
            Self::ReviewThreadAdd { .. } => "review_thread_add",
            Self::ReviewThreadSet { .. } => "review_thread_set",
            Self::ReviewThreadDelete { .. } => "review_thread_delete",
            Self::ReviewCommentAdd { .. } => "review_comment_add",
            Self::ReviewCommentDelete { .. } => "review_comment_delete",
            Self::ReviewImageAdd { .. } => "review_image_add",
            Self::ReviewImageGet { .. } => "review_image_get",
            Self::ReviewSubmit { .. } => "review_submit",
            Self::ReviewPoll { .. } => "review_poll",
            Self::ReviewFetch { .. } => "review_fetch",
            Self::ReviewAck { .. } => "review_ack",
            Self::ReviewClaim { .. } => "review_claim",
            Self::ReviewRelease { .. } => "review_release",
            Self::VersionSave { .. } => "version_save",
            Self::VersionList { .. } => "version_list",
            Self::VersionGet { .. } => "version_get",
            Self::VersionRestore { .. } => "version_restore",
            Self::VersionOpen { .. } => "version_open",
        }
    }

    /// What applying this command does to the session's model.
    ///
    /// A tag missing from [`COMMAND_KINDS`] reads as [`CommandKind::Edit`], the
    /// conservative fallback: a spurious redraw beats a lost notification.
    pub fn kind(&self) -> CommandKind {
        let tag = self.tag();
        COMMAND_KINDS
            .iter()
            .find(|(name, _)| *name == tag)
            .map_or(CommandKind::Edit, |(_, kind)| *kind)
    }

    pub fn bytes(&self) -> Option<&'static Bytes> {
        let tag = self.tag();
        COMMAND_BYTES
            .iter()
            .find(|(name, _)| *name == tag)
            .map(|(_, bytes)| bytes)
    }

    /// What bytes *this* command carries. Differs from [`Command::bytes`] only
    /// for an [`Attachment::Optional`], such as an `extension_set` of JSON.
    pub fn carries_bytes(&self) -> Option<&'static Bytes> {
        let carrying = match self {
            Self::SessionNew { source, .. } => source.is_some(),
            Self::ExtensionSet { value, .. } => matches!(
                value,
                ExtensionSet::Bytes | ExtensionSet::Maintained { value: None, .. }
            ),
            _ => true,
        };
        self.bytes().filter(|_| carrying)
    }

    /// The session this command addresses; `None` for editor-level commands.
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Self::SessionNew { .. } | Self::SessionOpen { .. } | Self::SessionList => None,
            Self::SessionClose { session }
            | Self::EditApply { session, .. }
            | Self::EditValidate { session, .. }
            | Self::BindingCellsGet { session, .. }
            | Self::BindingCellsSet { session, .. }
            | Self::BindingCellsUnset { session, .. }
            | Self::MeshGet { session, .. }
            | Self::ModelGet { session, .. }
            | Self::GeometryGet { session, .. }
            | Self::SessionFork { session, .. }
            | Self::ModelExport { session, .. }
            | Self::Save { session, .. }
            | Self::ExportManifest { session, .. }
            | Self::Status { session }
            | Self::Check { session }
            | Self::NodeTree { session }
            | Self::NodeInfo { session, .. }
            | Self::NodeAdd { session, .. }
            | Self::NodeSet { session, .. }
            | Self::NodeReparent { session, .. }
            | Self::NodeReorder { session, .. }
            | Self::NodeDuplicate { session, .. }
            | Self::RenameId { session, .. }
            | Self::MaskAdd { session, .. }
            | Self::MaskDelete { session, .. }
            | Self::MotionResponseSet { session, .. }
            | Self::NodeDelete { session, .. }
            | Self::TextureAdd { session, .. }
            | Self::TextureList { session }
            | Self::ParamAdd { session, .. }
            | Self::ParamList { session }
            | Self::ParamSet { session, .. }
            | Self::ParamDelete { session, .. }
            | Self::BindingKeyInsert { session, .. }
            | Self::BindingKeyDelete { session, .. }
            | Self::BindingKeyMove { session, .. }
            | Self::BindingAdd { session, .. }
            | Self::BindingDelete { session, .. }
            | Self::BindingInterpolate { session, .. }
            | Self::BindingList { session, .. }
            | Self::MeshSet { session, .. }
            | Self::MeshAuto { session, .. }
            | Self::SlotAdd { session, .. }
            | Self::SlotFill { session, .. }
            | Self::SlotClear { session, .. }
            | Self::SlotDelete { session, .. }
            | Self::Slots { session, .. }
            | Self::Welds { session }
            | Self::WeldSet { session, .. }
            | Self::WeldDelete { session, .. }
            | Self::SpineAdd { session, .. }
            | Self::SpineSet { session, .. }
            | Self::SpineFit { session, .. }
            | Self::Undo { session, .. }
            | Self::Redo { session, .. }
            | Self::EditGoto { session, .. }
            | Self::EditHistoryGet { session, .. }
            | Self::PresenceSet { session, .. }
            | Self::PresenceGet { session }
            | Self::Preview { session, .. }
            | Self::ImportJson { session, .. }
            | Self::ExtensionSet { session, .. }
            | Self::ExtensionDelete { session, .. }
            | Self::Extensions { session }
            | Self::ExtensionGet { session, .. }
            | Self::ExtensionWarnings { session }
            | Self::HumanoidMap { session, .. }
            | Self::HumanoidUnmap { session, .. }
            | Self::HumanoidGet { session }
            | Self::ExpressionSet { session, .. }
            | Self::ExpressionDelete { session, .. }
            | Self::ExpressionList { session }
            | Self::VisemeAddStandard { session, .. }
            | Self::ClipList { session }
            | Self::ReviewGet { session }
            | Self::ReviewThreadAdd { session, .. }
            | Self::ReviewThreadSet { session, .. }
            | Self::ReviewThreadDelete { session, .. }
            | Self::ReviewCommentAdd { session, .. }
            | Self::ReviewCommentDelete { session, .. }
            | Self::ReviewImageAdd { session }
            | Self::ReviewImageGet { session, .. }
            | Self::ReviewSubmit { session }
            | Self::ReviewPoll { session, .. }
            | Self::ReviewFetch { session, .. }
            | Self::ReviewAck { session, .. }
            | Self::ReviewClaim { session, .. }
            | Self::ReviewRelease { session, .. }
            | Self::VersionSave { session, .. }
            | Self::VersionList { session }
            | Self::VersionGet { session, .. }
            | Self::VersionRestore { session, .. }
            | Self::VersionOpen { session, .. } => Some(*session),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Rename {
    Node {
        from: NodeId,
        to: NodeId,
    },
    Param {
        from: ParamId,
        to: ParamId,
    },
    Texture {
        from: TexId,
        to: TexId,
    },
    /// A slot is scoped to its part, so this one names the part too. Every
    /// weld pair that named the old Id follows it.
    Slot {
        node: NodeId,
        from: SlotId,
        to: SlotId,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum GeometryField {
    Rest,
    Local,
    World,
    Uvs,
    Triangles,
}

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
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum BindingCellValue {
    Scalar(f32),
    Offsets(Vec<[f32; 2]>),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingCellWrite {
    pub cell: [u32; 2],
    pub value: BindingCellValue,
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

/// One convex source contribution to a new mesh vertex's authored deformation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct VertexWeight {
    pub vertex: u32,
    pub weight: f32,
}

/// Model edits accepted by an atomic batch. Created IDs are always explicit.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum EditOp {
    NodeAdd {
        parent: NodeId,
        kind: NodeKindArg,
        #[serde(default)]
        name: Option<String>,
        node: NodeId,
    },
    NodeSet {
        node: NodeId,
        #[serde(flatten)]
        patch: NodePatch,
    },
    NodeReparent {
        node: NodeId,
        to: NodeId,
    },
    NodeReorder {
        node: NodeId,
        index: u32,
    },
    MeshSet {
        node: NodeId,
        verts: Vec<[f32; 2]>,
        uvs: Vec<[f32; 2]>,
        indices: Vec<[u32; 3]>,
        origin: [f32; 2],
        #[serde(default)]
        deform_mapping: Option<Vec<Vec<VertexWeight>>>,
    },
    BindingKeyInsert {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        axis: ParamId,
        value: f32,
    },
    BindingKeyDelete {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        axis: ParamId,
        index: u32,
    },
    BindingKeyMove {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        axis: ParamId,
        index: u32,
        value: f32,
    },
    BindingAdd {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        #[serde(default)]
        key_positions: Option<Vec<Vec<f32>>>,
    },
    BindingCellsSet {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        cells: Vec<BindingCellWrite>,
    },
    BindingCellsUnset {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        cells: Vec<[u32; 2]>,
    },
    BindingDelete {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
    },
    BindingInterpolationSet {
        node: NodeId,
        #[serde(flatten)]
        params: BindingParams,
        target: BindingTarget,
        mode: Interpolate,
    },
    MaskAdd {
        node: NodeId,
        source: NodeId,
        mode: MaskMode,
    },
    MaskDelete {
        node: NodeId,
        index: u32,
    },
    SlotAdd {
        node: NodeId,
        slot: SlotId,
    },
    SlotFill {
        node: NodeId,
        slot: SlotId,
        vertex: u32,
    },
    SlotClear {
        node: NodeId,
        slot: SlotId,
    },
    SlotDelete {
        node: NodeId,
        slot: SlotId,
    },
    WeldSet {
        a: NodeId,
        b: NodeId,
        pairs: Vec<SlotPair>,
    },
    WeldDelete {
        a: NodeId,
        b: NodeId,
    },
}

/// Exactly one source and its declared attachments initialize a new session.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum SessionSource {
    Clm {},
    Json {
        #[serde(default)]
        textures: Vec<ImportTexture>,
    },
    Manifest {},
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

/// The param, or the pair of params, a binding is keyed by. With `param_y`
/// the binding owns two position axes and `cell` indexes both; without it
/// the grid is one row and `cell[1]` is 0.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingParams {
    pub param: ParamId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_y: Option<ParamId>,
}

impl BindingParams {
    pub fn one(param: ParamId) -> Self {
        Self {
            param,
            param_y: None,
        }
    }

    pub fn two(x: ParamId, y: ParamId) -> Self {
        Self {
            param: x,
            param_y: Some(y),
        }
    }
}

/// How [`Command::MeshAuto`] derives a mesh from a part's alpha. Absent knobs
/// take `catchlight-editor-core`'s defaults.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AutoMesh {
    Contour {
        /// Alpha strictly above this counts as solid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<u8>,
        /// Douglas-Peucker tolerance, in texels: higher is a coarser outline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        simplify: Option<f32>,
        /// Texels of outward dilation before tracing, so the outline clears
        /// the art's own edge.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        margin: Option<u32>,
        /// Interior fill-point spacing in texels; 0 is boundary only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spacing: Option<u32>,
        /// Ring scale factors about the outline's centroid: 0 the centroid, 1 the
        /// outline; above 1 clamps to 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rings: Option<Vec<f32>>,
        /// Texels: a free vertex closer than this to a placed one is dropped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_distance: Option<f32>,
        /// Texel x of a vertical mirror line for free vertices; the outline is not
        /// mirrored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mirror_x: Option<f32>,
    },
    /// A grid over the bounding box of the solid texels.
    Grid {
        /// Alpha strictly above this counts as solid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u32>,
        /// Grid lines as fractions of the solid bounding box (0 left, 1 right);
        /// present, replaces `cols` and `margin` on this axis.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axes_x: Option<Vec<f32>>,
        /// The same, down, replacing `rows`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axes_y: Option<Vec<f32>>,
        /// Fraction of the bounding box added outside it on both sides, when
        /// the lines come from `cols`/`rows`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        margin: Option<f32>,
    },
}

impl Default for AutoMesh {
    fn default() -> Self {
        Self::Contour {
            threshold: None,
            simplify: None,
            margin: None,
            spacing: None,
            rings: None,
            min_distance: None,
            mirror_x: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotAddr {
    pub node: NodeId,
    pub slot: SlotId,
}

/// `weight` is `a`'s share (`0..=1`) of the point both vertices are pulled to.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotPair {
    pub a: SlotId,
    pub b: SlotId,
    pub weight: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum NodeKindArg {
    MotionResponse,
    Group,
    Part,
    Composite,
    MeshGroup,
}

/// The kind reported by a node query. [`NodeKindArg`] selects a kind on creation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum NodeKind {
    Group,
    Part,
    Composite,
    MeshGroup,
    MotionResponse,
    Spine,
}

impl NodeKind {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Part => "part",
            Self::Composite => "composite",
            Self::MeshGroup => "mesh_group",
            Self::MotionResponse => "motion_response",
            Self::Spine => "spine",
        }
    }

    pub fn of(kind: &catchlight_core::ModelNodeKind) -> Self {
        use catchlight_core::ModelNodeKind as K;
        match kind {
            K::Group => Self::Group,
            K::Part(_) => Self::Part,
            K::Composite(_) => Self::Composite,
            K::MeshGroup(_) => Self::MeshGroup,
            K::MotionResponse(_) => Self::MotionResponse,
            K::Spine(_) => Self::Spine,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum MaskMode {
    Mask,
    Dodge,
}

impl MaskMode {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Mask => "mask",
            Self::Dodge => "dodge",
        }
    }
}

impl From<MaskMode> for catchlight_core::components::MaskMode {
    fn from(mode: MaskMode) -> Self {
        match mode {
            MaskMode::Mask => Self::Mask,
            MaskMode::Dodge => Self::DodgeMask,
        }
    }
}

impl From<catchlight_core::components::MaskMode> for MaskMode {
    fn from(mode: catchlight_core::components::MaskMode) -> Self {
        match mode {
            catchlight_core::components::MaskMode::Mask => Self::Mask,
            catchlight_core::components::MaskMode::DodgeMask => Self::Dodge,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Interpolate {
    Nearest,
    Stepped,
    Linear,
    Cubic,
}

impl Interpolate {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Stepped => "stepped",
            Self::Linear => "linear",
            Self::Cubic => "cubic",
        }
    }

    pub const ALL: [Self; 4] = [Self::Nearest, Self::Stepped, Self::Linear, Self::Cubic];
}

impl From<Interpolate> for catchlight_core::interpolate::InterpolateMode {
    fn from(mode: Interpolate) -> Self {
        match mode {
            Interpolate::Nearest => Self::Nearest,
            Interpolate::Stepped => Self::Stepped,
            Interpolate::Linear => Self::Linear,
            Interpolate::Cubic => Self::Cubic,
        }
    }
}

impl From<catchlight_core::interpolate::InterpolateMode> for Interpolate {
    fn from(mode: catchlight_core::interpolate::InterpolateMode) -> Self {
        use catchlight_core::interpolate::InterpolateMode as M;
        match mode {
            M::Nearest => Self::Nearest,
            M::Stepped => Self::Stepped,
            M::Linear => Self::Linear,
            M::Cubic => Self::Cubic,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum TextureEncoding {
    #[default]
    Png,
    Tga,
}

impl From<TextureEncoding> for catchlight_core::formats::clm::TextureEncoding {
    fn from(encoding: TextureEncoding) -> Self {
        match encoding {
            TextureEncoding::Png => Self::Png,
            TextureEncoding::Tga => Self::Tga,
        }
    }
}

impl From<catchlight_core::formats::clm::TextureEncoding> for TextureEncoding {
    fn from(encoding: catchlight_core::formats::clm::TextureEncoding) -> Self {
        use catchlight_core::formats::clm::TextureEncoding as E;
        match encoding {
            E::Png => Self::Png,
            E::Tga => Self::Tga,
        }
    }
}

/// What a texture's alpha channel means. Straight is the default; `.inx`
/// textures are premultiplied.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum TextureAlpha {
    #[default]
    Straight,
    PremultipliedSrgb,
}

impl From<TextureAlpha> for catchlight_core::formats::clm::TextureAlpha {
    fn from(alpha: TextureAlpha) -> Self {
        match alpha {
            TextureAlpha::Straight => Self::Straight,
            TextureAlpha::PremultipliedSrgb => Self::PremultipliedSrgb,
        }
    }
}

impl From<catchlight_core::formats::clm::TextureAlpha> for TextureAlpha {
    fn from(alpha: catchlight_core::formats::clm::TextureAlpha) -> Self {
        use catchlight_core::formats::clm::TextureAlpha as A;
        match alpha {
            A::Straight => Self::Straight,
            A::PremultipliedSrgb => Self::PremultipliedSrgb,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum BlendMode {
    Normal,
    Multiply,
    ColorDodge,
    LinearDodge,
    Screen,
    ClipToLower,
    SliceFromLower,
    Overlay,
    ColorBurn,
    LinearBurn,
    Darken,
    Lighten,
    Add,
    Inverse,
    Subtract,
}

impl BlendMode {
    /// `Snake_case`; the model file spells the same modes in `PascalCase`.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Multiply => "multiply",
            Self::ColorDodge => "color_dodge",
            Self::LinearDodge => "linear_dodge",
            Self::Screen => "screen",
            Self::ClipToLower => "clip_to_lower",
            Self::SliceFromLower => "slice_from_lower",
            Self::Overlay => "overlay",
            Self::ColorBurn => "color_burn",
            Self::LinearBurn => "linear_burn",
            Self::Darken => "darken",
            Self::Lighten => "lighten",
            Self::Add => "add",
            Self::Inverse => "inverse",
            Self::Subtract => "subtract",
        }
    }

    pub const ALL: [Self; 15] = [
        Self::Normal,
        Self::Multiply,
        Self::ColorDodge,
        Self::LinearDodge,
        Self::Screen,
        Self::ClipToLower,
        Self::SliceFromLower,
        Self::Overlay,
        Self::ColorBurn,
        Self::LinearBurn,
        Self::Darken,
        Self::Lighten,
        Self::Add,
        Self::Inverse,
        Self::Subtract,
    ];
}

impl From<BlendMode> for catchlight_core::components::BlendMode {
    fn from(mode: BlendMode) -> Self {
        match mode {
            BlendMode::Normal => Self::Normal,
            BlendMode::Multiply => Self::Multiply,
            BlendMode::ColorDodge => Self::ColorDodge,
            BlendMode::LinearDodge => Self::LinearDodge,
            BlendMode::Screen => Self::Screen,
            BlendMode::ClipToLower => Self::ClipToLower,
            BlendMode::SliceFromLower => Self::SliceFromLower,
            BlendMode::Overlay => Self::Overlay,
            BlendMode::ColorBurn => Self::ColorBurn,
            BlendMode::LinearBurn => Self::LinearBurn,
            BlendMode::Darken => Self::Darken,
            BlendMode::Lighten => Self::Lighten,
            BlendMode::Add => Self::Add,
            BlendMode::Inverse => Self::Inverse,
            BlendMode::Subtract => Self::Subtract,
        }
    }
}

impl From<catchlight_core::components::BlendMode> for BlendMode {
    fn from(mode: catchlight_core::components::BlendMode) -> Self {
        use catchlight_core::components::BlendMode as M;
        match mode {
            M::Normal => Self::Normal,
            M::Multiply => Self::Multiply,
            M::ColorDodge => Self::ColorDodge,
            M::LinearDodge => Self::LinearDodge,
            M::Screen => Self::Screen,
            M::ClipToLower => Self::ClipToLower,
            M::SliceFromLower => Self::SliceFromLower,
            M::Overlay => Self::Overlay,
            M::ColorBurn => Self::ColorBurn,
            M::LinearBurn => Self::LinearBurn,
            M::Darken => Self::Darken,
            M::Lighten => Self::Lighten,
            M::Add => Self::Add,
            M::Inverse => Self::Inverse,
            M::Subtract => Self::Subtract,
        }
    }
}

/// A property a binding drives with one number per cell.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ScalarTarget {
    Tx,
    Ty,
    Tz,
    Sx,
    Sy,
    Rx,
    Ry,
    Rz,
    ZOrder,
    Opacity,
    #[serde(rename = "tintr")]
    TintR,
    #[serde(rename = "tintg")]
    TintG,
    #[serde(rename = "tintb")]
    TintB,
    #[serde(rename = "screentintr")]
    ScreenTintR,
    #[serde(rename = "screentintg")]
    ScreenTintG,
    #[serde(rename = "screentintb")]
    ScreenTintB,
}

/// Any property a binding drives: one of [`ScalarTarget`]'s scalars, or the
/// per-vertex deform, which the deform commands author instead.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum BindingTarget {
    Tx,
    Ty,
    Tz,
    Sx,
    Sy,
    Rx,
    Ry,
    Rz,
    ZOrder,
    Opacity,
    #[serde(rename = "tintr")]
    TintR,
    #[serde(rename = "tintg")]
    TintG,
    #[serde(rename = "tintb")]
    TintB,
    #[serde(rename = "screentintr")]
    ScreenTintR,
    #[serde(rename = "screentintg")]
    ScreenTintG,
    #[serde(rename = "screentintb")]
    ScreenTintB,
    Deform,
}

impl ScalarTarget {
    pub fn wire_name(self) -> &'static str {
        catchlight_core::ScalarTarget::from(self).name()
    }
}

impl BindingTarget {
    pub fn wire_name(self) -> &'static str {
        catchlight_core::BindingTarget::from(self).name()
    }
}

/// Both target enums against the core ones, in one table, so a variant added
/// to any of the four cannot be spelled differently in the others.
macro_rules! scalar_targets {
    ($($variant:ident),* $(,)?) => {
        impl From<ScalarTarget> for catchlight_core::ScalarTarget {
            fn from(t: ScalarTarget) -> Self {
                match t { $(ScalarTarget::$variant => Self::$variant),* }
            }
        }

        impl From<catchlight_core::ScalarTarget> for ScalarTarget {
            fn from(t: catchlight_core::ScalarTarget) -> Self {
                match t { $(catchlight_core::ScalarTarget::$variant => Self::$variant),* }
            }
        }

        impl From<ScalarTarget> for BindingTarget {
            fn from(t: ScalarTarget) -> Self {
                match t { $(ScalarTarget::$variant => Self::$variant),* }
            }
        }

        impl BindingTarget {
            pub fn scalar(self) -> Option<ScalarTarget> {
                match self {
                    $(Self::$variant => Some(ScalarTarget::$variant),)*
                    Self::Deform => None,
                }
            }
        }

        impl From<BindingTarget> for catchlight_core::BindingTarget {
            fn from(t: BindingTarget) -> Self {
                match t {
                    $(BindingTarget::$variant => {
                        Self::Scalar(catchlight_core::ScalarTarget::$variant)
                    })*
                    BindingTarget::Deform => Self::Deform,
                }
            }
        }

        impl From<catchlight_core::BindingTarget> for BindingTarget {
            fn from(t: catchlight_core::BindingTarget) -> Self {
                match t {
                    $(catchlight_core::BindingTarget::Scalar(
                        catchlight_core::ScalarTarget::$variant,
                    ) => Self::$variant,)*
                    catchlight_core::BindingTarget::Deform => Self::Deform,
                }
            }
        }
    };
}

scalar_targets!(
    Tx,
    Ty,
    Tz,
    Sx,
    Sy,
    Rx,
    Ry,
    Rz,
    ZOrder,
    Opacity,
    TintR,
    TintG,
    TintB,
    ScreenTintR,
    ScreenTintG,
    ScreenTintB,
);

macro_rules! display_as_wire {
    ($($t:ty),* $(,)?) => {$(
        impl std::fmt::Display for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.wire_name())
            }
        }
    )*};
}

display_as_wire!(
    NodeKind,
    MaskMode,
    Interpolate,
    BlendMode,
    ScalarTarget,
    BindingTarget,
);

/// A present JSON value — `null` included — as `Some`, for a field whose
/// absence and whose `null` mean different things (the merge-patch idiom).
fn present_as_some<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(de).map(Some)
}

/// Fields to change on a node; absent = unchanged. Colour fields reach parts
/// and composites only.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NodePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_order: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Merge patch: absent leaves it, `null` draws none, an Id draws that one.
    #[serde(
        default,
        deserialize_with = "present_as_some",
        skip_serializing_if = "Option::is_none"
    )]
    // ts-rs would otherwise spell the double option `TexId | null | null`.
    #[cfg_attr(feature = "ts", ts(as = "Option<TexId>"))]
    pub texture: Option<Option<TexId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_to_root: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<BlendMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_tint: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_threshold: Option<f32>,
    /// Composite: forward mesh-group deformation to children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate_meshgroup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mg_translate_children: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ParamPose {
    pub param: ParamId,
    pub value: f32,
}

/// One texture an [`Command::ImportJson`] structure names, arriving as
/// attachment `texture:<texture>`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ImportTexture {
    pub texture: TexId,
    #[serde(default)]
    pub encoding: TextureEncoding,
    #[serde(default)]
    pub alpha: TextureAlpha,
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

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Camera {
    pub center: [f32; 2],
    pub height: f32,
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

fn one() -> f32 {
    1.0
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

fn yes() -> bool {
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

/// `$XDG_RUNTIME_DIR/catchlight-editor/server.sock`, else the temp dir.
#[cfg(unix)]
pub fn default_socket_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    base.join("catchlight-editor").join("server.sock")
}

/// Where a running editor writes its [`Connection`] for local clients;
/// `CATCHLIGHT_EDITOR_CONNECTION` overrides it. Never a shared temp dir: the
/// file holds a bearer token.
pub fn default_connection_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Some(path) = std::env::var_os("CATCHLIGHT_EDITOR_CONNECTION") {
        return Some(PathBuf::from(path));
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(base.join("catchlight-editor").join("connection.json"))
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// `http://127.0.0.1:<port>`, the base every route hangs off.
    pub url: String,
    pub token: String,
    /// The process that wrote the file, so it removes only its own.
    pub pid: u32,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_finds_its_own_bytes_in_the_table() {
        for (tag, _) in COMMAND_BYTES {
            assert!(
                COMMAND_KINDS.iter().any(|(known, _)| known == tag),
                "{tag} carries bytes but is not a command",
            );
        }
        let texture_add = Command::TextureAdd {
            session: SessionId(1),
            node: node("face"),
            encoding: TextureEncoding::Png,
            texture: None,
        };
        let bytes = texture_add.bytes().expect("texture_add carries bytes");
        assert_eq!(bytes.attachments, &[Attachment::Fixed("texture")]);
        assert!(!bytes.payload);
        assert!(Command::SessionList.bytes().is_none());
    }

    #[test]
    fn a_family_admits_its_own_suffixes_and_nothing_else() {
        let family = Attachment::Family("texture");
        assert!(family.admits("texture:images/face.png"));
        assert!(!family.admits("texture"), "a family needs a suffix");
        assert!(!family.admits("texture:"), "and not an empty one");
        assert!(!family.admits("textures:face"));
        assert!(Attachment::Fixed("model").admits("model"));
        assert!(!Attachment::Fixed("model").admits("model:one"));
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

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid id")
    }

    fn param(id: &str) -> ParamId {
        ParamId::new(id).expect("valid id")
    }

    #[test]
    fn request_tagging_roundtrips() {
        let req = Request {
            id: 7,
            command: Command::NodeAdd {
                session: SessionId(1),
                parent: node("root"),
                kind: NodeKindArg::Part,
                name: Some("Body".into()),
                node: None,
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains("\"cmd\":\"node_add\""));
        assert!(line.contains("\"id\":7"));
        assert!(line.contains("\"parent\":\"root\""), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back.command,
            Command::NodeAdd {
                kind: NodeKindArg::Part,
                ..
            }
        ));
    }

    #[test]
    fn ids_travel_as_the_plain_strings_a_model_file_stores() {
        let req = Request {
            id: 1,
            command: Command::BindingCellsSet {
                session: SessionId(2),
                params: BindingParams::two(param("head.x"), param("head.y")),
                node: node("root/part-3f9a2c1e"),
                target: BindingTarget::Tx,
                if_rev: 0,
                cells: vec![BindingCellWrite {
                    cell: [1, 2],
                    value: BindingCellValue::Scalar(0.5),
                }],
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains("\"param\":\"head.x\""), "{line}");
        assert!(line.contains("\"param_y\":\"head.y\""), "{line}");
        assert!(line.contains("\"node\":\"root/part-3f9a2c1e\""), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        match back.command {
            Command::BindingCellsSet { params, cells, .. } => {
                assert_eq!(params.param.as_str(), "head.x");
                assert_eq!(params.param_y.unwrap().as_str(), "head.y");
                assert_eq!(cells[0].cell, [1, 2]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_one_param_binding_leaves_the_second_param_off_the_wire() {
        let line = serde_json::to_string(&Request {
            id: 1,
            command: Command::BindingAdd {
                session: SessionId(1),
                params: BindingParams::one(param("pull")),
                key_positions: None,
                node: node("body"),
                target: BindingTarget::Tx,
            },
        })
        .unwrap();
        assert!(!line.contains("param_y"), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        match back.command {
            Command::BindingAdd { params, .. } => assert!(params.param_y.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_add_names_what_it_makes_rather_than_shadowing_the_requests_id() {
        let line = serde_json::to_string(&Request {
            id: 7,
            command: Command::NodeAdd {
                session: SessionId(1),
                parent: node("root"),
                kind: NodeKindArg::Part,
                name: None,
                node: Some(node("hair")),
            },
        })
        .unwrap();
        assert!(line.contains("\"id\":7"), "{line}");
        assert!(line.contains("\"node\":\"hair\""), "{line}");

        let back: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(back.id, 7);
        match back.command {
            Command::NodeAdd { node, .. } => assert_eq!(node.unwrap().as_str(), "hair"),
            other => panic!("{other:?}"),
        }

        let line = serde_json::to_string(&Request {
            id: 8,
            command: Command::ParamAdd {
                session: SessionId(1),
                name: "Pull".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                param: None,
            },
        })
        .unwrap();
        assert!(!line.contains("\"param\""), "{line}");
    }

    #[test]
    fn an_id_outside_the_charset_does_not_parse() {
        let line = r#"{"id":4,"cmd":"node_delete","session":1,"node":"a b"}"#;
        let err = serde_json::from_str::<Request>(line).unwrap_err();
        assert!(err.to_string().contains("invalid byte"), "{err}");
        let RequestId { id } = serde_json::from_str(line).unwrap();
        assert_eq!(id, 4);
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
            message: "part carries no such vertex slot".into(),
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

    #[test]
    fn a_read_that_needs_the_editor_is_not_a_replica_query() {
        let session = SessionId(1);
        assert_eq!(
            Command::NodeTree { session }.kind(),
            CommandKind::ReplicaQuery
        );
        assert_eq!(Command::Status { session }.kind(), CommandKind::ServerQuery);
        assert_eq!(
            Command::PresenceGet { session }.kind(),
            CommandKind::ServerQuery
        );
        assert!(
            serde_json::from_str::<Command>(
                r#"{"cmd":"scratch_deform","session":1,"node":"hair","offsets":[]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_command_names_the_session_it_addresses() {
        assert_eq!(
            Command::Undo {
                session: SessionId(5),
                if_rev: 0,
            }
            .session(),
            Some(SessionId(5))
        );
        assert_eq!(Command::SessionList.session(), None);
        assert_eq!(
            Command::SessionOpen {
                path: "m.clm".into()
            }
            .session(),
            None
        );
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

    /// serde's spelling and `wire_name` must agree.
    #[test]
    fn what_a_closed_set_prints_is_what_it_travels_as() {
        fn same<T: Serialize + std::fmt::Display + Copy>(values: &[T]) {
            for v in values {
                let json = serde_json::to_value(v).unwrap();
                assert_eq!(json, serde_json::json!(v.to_string()), "{v}");
            }
        }
        same(&[
            NodeKind::Group,
            NodeKind::Part,
            NodeKind::Composite,
            NodeKind::MeshGroup,
            NodeKind::MotionResponse,
            NodeKind::Spine,
        ]);
        same(&[MaskMode::Mask, MaskMode::Dodge]);
        same(&Interpolate::ALL);
        same(&BlendMode::ALL);
        same(&SCALAR_TARGETS);
        same(&BINDING_TARGETS);
    }

    #[test]
    fn a_binding_target_travels_under_the_models_own_spelling() {
        let words: Vec<String> = SCALAR_TARGETS.iter().map(ToString::to_string).collect();
        assert_eq!(
            words,
            [
                "tx",
                "ty",
                "tz",
                "sx",
                "sy",
                "rx",
                "ry",
                "rz",
                "z_order",
                "opacity",
                "tintr",
                "tintg",
                "tintb",
                "screentintr",
                "screentintg",
                "screentintb",
            ]
        );
        assert_eq!(BindingTarget::Deform.to_string(), "deform");
        assert_eq!(BindingTarget::from(ScalarTarget::Rz), BindingTarget::Rz);
    }

    const SCALAR_TARGETS: [ScalarTarget; 16] = [
        ScalarTarget::Tx,
        ScalarTarget::Ty,
        ScalarTarget::Tz,
        ScalarTarget::Sx,
        ScalarTarget::Sy,
        ScalarTarget::Rx,
        ScalarTarget::Ry,
        ScalarTarget::Rz,
        ScalarTarget::ZOrder,
        ScalarTarget::Opacity,
        ScalarTarget::TintR,
        ScalarTarget::TintG,
        ScalarTarget::TintB,
        ScalarTarget::ScreenTintR,
        ScalarTarget::ScreenTintG,
        ScalarTarget::ScreenTintB,
    ];

    const BINDING_TARGETS: [BindingTarget; 17] = [
        BindingTarget::Tx,
        BindingTarget::Ty,
        BindingTarget::Tz,
        BindingTarget::Sx,
        BindingTarget::Sy,
        BindingTarget::Rx,
        BindingTarget::Ry,
        BindingTarget::Rz,
        BindingTarget::ZOrder,
        BindingTarget::Opacity,
        BindingTarget::TintR,
        BindingTarget::TintG,
        BindingTarget::TintB,
        BindingTarget::ScreenTintR,
        BindingTarget::ScreenTintG,
        BindingTarget::ScreenTintB,
        BindingTarget::Deform,
    ];

    #[test]
    fn a_word_outside_a_closed_set_does_not_parse() {
        let line =
            r#"{"id":4,"cmd":"mask_add","session":1,"node":"a","source":"b","mode":"dodge_mask"}"#;
        let err = serde_json::from_str::<Request>(line).unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
        let RequestId { id } = serde_json::from_str(line).unwrap();
        assert_eq!(id, 4);
    }

    #[test]
    fn a_misspelt_request_field_is_refused() {
        let line = r#"{"id":4,"cmd":"mesh_set","session":1,"node":"a","verts":[],"uvs":[],"indices":[],"origin":[0,0],"ifrev":5}"#;
        let err = serde_json::from_str::<Request>(line).unwrap_err();
        assert!(err.to_string().contains("unknown field `ifrev`"), "{err}");
        let RequestId { id } = serde_json::from_str(line).unwrap();
        assert_eq!(id, 4);
    }

    #[test]
    fn a_misspelt_field_is_refused_inside_a_flattened_or_nested_payload() {
        for line in [
            r#"{"id":1,"cmd":"node_set","session":1,"node":"a","nmae":"b"}"#,
            r#"{"id":1,"cmd":"binding_add","session":1,"node":"a","target":"opacity","param":"p","paramy":"q"}"#,
            r#"{"id":1,"cmd":"edit_apply","session":1,"if_rev":1,"edits":[{"op":"node_set","node":"a","nmae":"b"}]}"#,
            r#"{"id":1,"cmd":"mesh_generate","session":1,"node":"a","mode":{"mode":"grid","colls":4}}"#,
        ] {
            let err = serde_json::from_str::<Request>(line).unwrap_err();
            assert!(err.to_string().contains("unknown field"), "{line}: {err}");
        }
    }

    #[test]
    fn a_reply_tolerates_a_field_it_does_not_know() {
        let line = r#"{"reply":"event","event":"sessions_changed","later":true}"#;
        serde_json::from_str::<Reply>(line).unwrap();
    }

    #[test]
    fn a_rename_names_the_kind_it_renames() {
        let s = serde_json::to_string(&Command::RenameId {
            session: SessionId(1),
            rename: Rename::Param {
                from: param("param-0001"),
                to: param("pull"),
            },
        })
        .unwrap();
        assert!(s.contains("\"kind\":\"param\""), "{s}");
        let back: Command = serde_json::from_str(&s).unwrap();
        match back {
            Command::RenameId {
                rename: Rename::Param { to, .. },
                ..
            } => assert_eq!(to.as_str(), "pull"),
            other => panic!("{other:?}"),
        }
    }
}
