//! The commands a client sends, with what each does to the model and the
//! bytes it carries.

use serde::{Deserialize, Serialize};

use crate::*;

/// Declares [`Command`] and the tables read from it. Each variant carries,
/// in this order, its docs, its `#[serde(rename)]` wire tag and
/// `#[command(<CommandKind>)]`, with `, attach(<Attachment>, ...)` for the
/// attachments it takes or `, payload` when its reply carries bytes.
///
/// The enum stays written out so `xtask generate` can parse it with `syn`.
macro_rules! commands {
    (@bytes attach($($attachment:ident($name:literal)),+ $(,)?)) => {
        CommandBytes {
            attachments: &[$(Attachment::$attachment($name)),+],
            payload: false,
        }
    };
    (@bytes payload) => {
        CommandBytes {
            attachments: &[],
            payload: true,
        }
    };
    (@carried) => { None };
    (@carried $($bytes:tt)+) => { Some(&commands!(@bytes $($bytes)+)) };
    (
        $(#[$meta:meta])*
        pub enum Command {
            $(
                $(#[doc = $doc:literal])*
                #[serde(rename = $tag:literal)]
                #[command($kind:ident $(, $($bytes:tt)+)?)]
                $variant:ident $({ $($fields:tt)* })?,
            )*
        }
    ) => {
        $(#[$meta])*
        pub enum Command {
            $(
                $(#[doc = $doc])*
                #[serde(rename = $tag)]
                $variant $({ $($fields)* })?,
            )*
        }

        /// Every command's wire tag paired with its [`CommandKind`].
        pub const COMMAND_KINDS: &[(&str, CommandKind)] = &[$(($tag, CommandKind::$kind)),*];

        /// Every command that carries bytes; a command absent here carries none.
        pub const COMMAND_BYTES: &[(&str, CommandBytes)] = &[
            $($(($tag, commands!(@bytes $($bytes)+)),)?)*
        ];

        impl Command {
            /// The `cmd` tag.
            pub const fn tag(&self) -> &'static str {
                match self {
                    $(Self::$variant { .. } => $tag,)*
                }
            }

            /// What applying this command does to the session's model.
            pub const fn kind(&self) -> CommandKind {
                match self {
                    $(Self::$variant { .. } => CommandKind::$kind,)*
                }
            }

            /// The bytes a command of this tag may carry.
            pub const fn bytes(&self) -> Option<&'static CommandBytes> {
                match self {
                    $(Self::$variant { .. } => commands!(@carried $($($bytes)+)?),)*
                }
            }
        }
    };
}

commands! {
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Command {
    #[serde(rename = "session_create")]
    #[command(
        Edit,
        attach(
            Optional("model"),
            Optional("structure"),
            Optional("manifest"),
            Family("texture"),
        )
    )]
    SessionNew {
        #[serde(default)]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<SessionSource>,
    },
    /// Open the `.clm` the server's store holds at `path`.
    #[serde(rename = "session_open")]
    #[command(Edit)]
    SessionOpen {
        path: String,
    },
    #[serde(rename = "session_fork")]
    #[command(Edit)]
    SessionFork {
        session: SessionId,
        if_rev: u64,
        #[serde(default)]
        name: Option<String>,
    },
    #[serde(rename = "model_export")]
    #[command(ServerQuery, payload)]
    ModelExport {
        session: SessionId,
        if_rev: u64,
    },
    #[serde(rename = "session_list")]
    #[command(ServerQuery)]
    SessionList,
    #[serde(rename = "session_close")]
    #[command(Edit)]
    SessionClose {
        session: SessionId,
    },
    #[serde(rename = "session_save")]
    #[command(Edit)]
    Save {
        session: SessionId,
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "manifest_export")]
    #[command(ServerQuery)]
    ExportManifest {
        session: SessionId,
        path: String,
    },
    #[serde(rename = "session_get")]
    #[command(ServerQuery)]
    Status {
        session: SessionId,
    },
    #[serde(rename = "model_check")]
    #[command(ReplicaQuery)]
    Check {
        session: SessionId,
    },
    #[serde(rename = "node_tree_get")]
    #[command(ReplicaQuery)]
    NodeTree {
        session: SessionId,
    },
    #[serde(rename = "node_get")]
    #[command(ReplicaQuery)]
    NodeInfo {
        session: SessionId,
        node: NodeId,
    },
    #[serde(rename = "mesh_get")]
    #[command(ReplicaQuery)]
    MeshGet {
        session: SessionId,
        node: NodeId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
    },
    #[serde(rename = "model_get")]
    #[command(ReplicaQuery)]
    ModelGet {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
    },
    #[serde(rename = "geometry_get")]
    #[command(ReplicaQuery)]
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
    #[serde(rename = "node_add")]
    #[command(Edit)]
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
    #[serde(rename = "node_set")]
    #[command(Edit)]
    NodeSet {
        session: SessionId,
        node: NodeId,
        #[serde(flatten)]
        patch: NodePatch,
    },
    #[serde(rename = "node_reparent")]
    #[command(Edit)]
    NodeReparent {
        session: SessionId,
        #[serde(flatten)]
        edit: NodeReparentArgs,
    },
    /// Move a node within its parent's children (clamped to the end).
    #[serde(rename = "node_reorder")]
    #[command(Edit)]
    NodeReorder {
        session: SessionId,
        #[serde(flatten)]
        edit: NodeReorderArgs,
    },

    /// Deep-copy a node's subtree as its next sibling; copies get fresh Ids.
    #[serde(rename = "node_duplicate")]
    #[command(Edit)]
    NodeDuplicate {
        session: SessionId,
        node: NodeId,
    },
    /// Change an Id and every reference this model holds to it. Breaks addons:
    /// nothing outside the model is rewritten and no alias remains.
    #[serde(rename = "id_rename")]
    #[command(Edit)]
    RenameId {
        session: SessionId,
        rename: Rename,
    },
    #[serde(rename = "mask_add")]
    #[command(Edit)]
    MaskAdd {
        session: SessionId,
        #[serde(flatten)]
        edit: MaskAddArgs,
    },

    #[serde(rename = "mask_delete")]
    #[command(Edit)]
    MaskDelete {
        session: SessionId,
        #[serde(flatten)]
        edit: MaskDeleteArgs,
    },
    #[serde(rename = "motion_response_set")]
    #[command(Edit)]
    MotionResponseSet {
        session: SessionId,
        node: NodeId,
        response: MotionResponse,
    },
    #[serde(rename = "node_delete")]
    #[command(Edit)]
    NodeDelete {
        session: SessionId,
        node: NodeId,
    },
    /// Add a texture from attachment `texture` and assign it to part `node` in
    /// one edit, dropping the old texture if nothing else draws it.
    #[serde(rename = "texture_add")]
    #[command(Edit, attach(Fixed("texture")))]
    TextureAdd {
        session: SessionId,
        node: NodeId,
        #[serde(default)]
        encoding: TextureEncoding,
        /// Absent generates one; an existing Id is [`ErrorCode::DuplicateId`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        texture: Option<TexId>,
    },
    #[serde(rename = "texture_list")]
    #[command(ReplicaQuery)]
    TextureList {
        session: SessionId,
    },
    #[serde(rename = "param_add")]
    #[command(Edit)]
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
    #[serde(rename = "param_list")]
    #[command(ReplicaQuery)]
    ParamList {
        session: SessionId,
    },
    /// Change param metadata; absent = unchanged. Key positions are
    /// normalized, so a range change doesn't move them.
    #[serde(rename = "param_set")]
    #[command(Edit)]
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
    #[serde(rename = "param_delete")]
    #[command(Edit)]
    ParamDelete {
        session: SessionId,
        param: ParamId,
    },
    /// Insert a distinct key position at normalized `value` in [0, 1].
    #[serde(rename = "binding_key_insert")]
    #[command(Edit)]
    BindingKeyInsert {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingKeyInsertArgs,
    },
    /// Remove a key position while retaining at least one position on the axis.
    #[serde(rename = "binding_key_delete")]
    #[command(Edit)]
    BindingKeyDelete {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingKeyDeleteArgs,
    },
    /// Move a key position to normalized `value` in [0, 1], preserving
    /// strict ordering with its neighbors.
    #[serde(rename = "binding_key_move")]
    #[command(Edit)]
    BindingKeyMove {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingKeyMoveArgs,
    },

    /// Atomically execute a bounded list against one revision of the model.
    #[serde(rename = "edit_apply")]
    #[command(Edit)]
    EditApply {
        session: SessionId,
        if_rev: u64,
        edits: Vec<EditOp>,
    },
    /// Evaluate the same list on a captured clone without publishing it.
    #[serde(rename = "edit_validate")]
    #[command(ServerQuery)]
    EditValidate {
        session: SessionId,
        if_rev: u64,
        edits: Vec<EditOp>,
    },
    #[serde(rename = "binding_cells_get")]
    #[command(ReplicaQuery)]
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
    #[serde(rename = "binding_cells_set")]
    #[command(Edit)]
    BindingCellsSet {
        session: SessionId,
        if_rev: u64,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingCellsSetArgs,
    },
    #[serde(rename = "binding_cells_unset")]
    #[command(Edit)]
    BindingCellsUnset {
        session: SessionId,
        if_rev: u64,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingCellsUnsetArgs,
    },
    #[serde(rename = "binding_add")]
    #[command(Edit)]
    BindingAdd {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingAddArgs,
    },

    #[serde(rename = "binding_delete")]
    #[command(Edit)]
    BindingDelete {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingDeleteArgs,
    },
    #[serde(rename = "binding_interpolation_set")]
    #[command(Edit)]
    BindingInterpolationSet {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        #[serde(flatten)]
        edit: BindingInterpolationSetArgs,
    },

    #[serde(rename = "binding_list")]
    #[command(ReplicaQuery)]
    BindingList {
        session: SessionId,
        node: NodeId,
    },

    /// Replace a Part/MeshGroup mesh, re-fitting its deform bindings in the same
    /// step. Answers with the slots the new mesh emptied.
    #[serde(rename = "mesh_set")]
    #[command(Edit)]
    MeshSet {
        session: SessionId,
        /// Refuse a stale draft or gesture while holding the session lock.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
        #[serde(flatten)]
        edit: MeshSetArgs,
    },
    /// Trace a part's mesh from its texture's alpha and apply it like
    /// [`Command::MeshSet`].
    #[serde(rename = "mesh_generate")]
    #[command(Edit)]
    MeshAuto {
        session: SessionId,
        node: NodeId,
        /// Absent is [`AutoMesh::Contour`] with every knob at its default.
        #[serde(default)]
        mode: AutoMesh,
    },

    #[serde(rename = "slot_add")]
    #[command(Edit)]
    SlotAdd {
        session: SessionId,
        node: NodeId,
        /// Absent generates one (`slot-<8 hex>`, re-drawn until it is free on
        /// the part). The reply names it either way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<SlotId>,
    },
    #[serde(rename = "slot_fill")]
    #[command(Edit)]
    SlotFill {
        session: SessionId,
        #[serde(flatten)]
        edit: SlotFillArgs,
    },
    /// Unfill a slot. The weld pairs that name it keep it and skip it until it
    /// is filled again.
    #[serde(rename = "slot_clear")]
    #[command(Edit)]
    SlotClear {
        session: SessionId,
        #[serde(flatten)]
        edit: SlotClearArgs,
    },
    /// Remove a slot from a part, and with it the weld pairs that named it.
    #[serde(rename = "slot_delete")]
    #[command(Edit)]
    SlotDelete {
        session: SessionId,
        #[serde(flatten)]
        edit: SlotDeleteArgs,
    },
    #[serde(rename = "slot_list")]
    #[command(ReplicaQuery)]
    Slots {
        session: SessionId,
        node: NodeId,
    },
    #[serde(rename = "weld_list")]
    #[command(ReplicaQuery)]
    Welds {
        session: SessionId,
    },

    /// Weld two parts together, replacing any weld already pairing them. Each
    /// pair names a slot on `a` and a slot on `b`; empty `pairs` records the
    /// two parts as welded and joins nothing yet.
    #[serde(rename = "weld_set")]
    #[command(Edit)]
    WeldSet {
        session: SessionId,
        #[serde(flatten)]
        edit: WeldSetArgs,
    },
    /// Unmake the weld pairing two parts, named in either order.
    #[serde(rename = "weld_delete")]
    #[command(Edit)]
    WeldDelete {
        session: SessionId,
        #[serde(flatten)]
        edit: WeldDeleteArgs,
    },
    /// Add a spine node. Empty `joints`, or a joint repeating the point above
    /// it, is refused.
    #[serde(rename = "spine_add")]
    #[command(Edit)]
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
    #[serde(rename = "spine_set")]
    #[command(Edit)]
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
    #[serde(rename = "spine_fit")]
    #[command(Edit)]
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
    #[command(Edit)]
    Undo {
        session: SessionId,
        if_rev: u64,
    },
    #[serde(rename = "edit_redo")]
    #[command(Edit)]
    Redo {
        session: SessionId,
        if_rev: u64,
    },
    #[serde(rename = "edit_goto")]
    #[command(Edit)]
    EditGoto {
        session: SessionId,
        if_rev: u64,
        revision: u64,
    },
    #[serde(rename = "edit_history_get")]
    #[command(ServerQuery)]
    EditHistoryGet {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        if_rev: Option<u64>,
    },
    /// Publish ephemeral view state (pose / camera / selection) — a separate
    /// path from the model: never bumps rev, never undone, never saved.
    #[serde(rename = "presence_set")]
    #[command(Presence)]
    PresenceSet {
        session: SessionId,
        #[serde(flatten)]
        presence: Presence,
    },
    /// Read the session's current shared presence (latest publisher wins).
    #[serde(rename = "presence_get")]
    #[command(ServerQuery)]
    PresenceGet {
        session: SessionId,
    },

    /// Render one frame as the reply's PNG payload. An absent camera is the
    /// default, never a tab's last view.
    #[serde(rename = "preview_render")]
    #[command(ServerQuery, payload)]
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
    #[command(Edit, attach(Fixed("structure"), Family("texture")))]
    ImportJson {
        session: SessionId,
        parent: NodeId,
        if_rev: u64,
        #[serde(default)]
        textures: Vec<ImportTexture>,
    },
    /// Set the extension under `key`. The `catchlight.` prefix is
    /// [`ErrorCode::ReservedExtension`].
    #[serde(rename = "extension_set")]
    #[command(Edit, attach(Optional("value")))]
    ExtensionSet {
        session: SessionId,
        key: ExtensionKey,
        value: ExtensionSet,
    },
    /// Drop the extension under `key`; a missing key is
    /// [`ErrorCode::NoExtension`].
    #[serde(rename = "extension_delete")]
    #[command(Edit)]
    ExtensionDelete {
        session: SessionId,
        key: ExtensionKey,
    },
    /// Every extension in key order; a byte value as its size and hash.
    #[serde(rename = "extension_list")]
    #[command(ReplicaQuery)]
    Extensions {
        session: SessionId,
    },
    /// One extension's value; bytes come back as the reply's payload.
    #[serde(rename = "extension_get")]
    #[command(ServerQuery, payload)]
    ExtensionGet {
        session: SessionId,
        key: ExtensionKey,
    },
    /// Every finding about the model's maintained extensions: references
    /// that no longer resolve, each extension's own check, and data this
    /// editor cannot read or check.
    #[serde(rename = "extension_warnings")]
    #[command(ReplicaQuery)]
    ExtensionWarnings {
        session: SessionId,
    },

    /// Map a humanoid control onto a param, replacing its mapping. Without
    /// `stops`, a two-sided control spans the param's min, default and max,
    /// and a one-sided control rests at the default and reaches the end of
    /// the range farther from it. A control outside the standard list is
    /// [`ErrorCode::NonstandardControl`] unless `nonstandard`.
    #[serde(rename = "humanoid_map")]
    #[command(Edit)]
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
    #[serde(rename = "humanoid_unmap")]
    #[command(Edit)]
    HumanoidUnmap {
        session: SessionId,
        control: String,
    },
    /// The control mappings, and the standard controls.
    #[serde(rename = "humanoid_get")]
    #[command(ReplicaQuery)]
    HumanoidGet {
        session: SessionId,
    },
    /// Create or replace an expression or viseme from its settings, kept in
    /// the order given. A control value must lie in its control's range.
    #[serde(rename = "expression_set")]
    #[command(Edit)]
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
    #[serde(rename = "expression_delete")]
    #[command(Edit)]
    ExpressionDelete {
        session: SessionId,
        group: ExpressionGroup,
        name: String,
    },
    /// Both groups with their settings, and the standard names.
    #[serde(rename = "expression_list")]
    #[command(ReplicaQuery)]
    ExpressionList {
        session: SessionId,
    },
    /// Add the standard visemes with their default settings after the
    /// existing ones. An existing viseme of a standard name is skipped, or
    /// replaced in place when `replace`.
    #[serde(rename = "viseme_add_standard")]
    #[command(Edit)]
    VisemeAddStandard {
        session: SessionId,
        #[serde(default)]
        replace: bool,
    },
    /// The clips of `catchlight.animations`, with what each lane drives.
    #[serde(rename = "clip_list")]
    #[command(ReplicaQuery)]
    ClipList {
        session: SessionId,
    },

    /// The session's review: every thread, and the agents attending to it.
    #[serde(rename = "review_get")]
    #[command(ServerQuery)]
    ReviewGet {
        session: SessionId,
    },
    /// Open a thread on `target` with its first comment. `pose` is what the
    /// author was looking at.
    #[serde(rename = "review_thread_add")]
    #[command(Review)]
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
    #[serde(rename = "review_thread_set")]
    #[command(Review)]
    ReviewThreadSet {
        session: SessionId,
        thread: ThreadId,
        status: ThreadStatus,
    },
    #[serde(rename = "review_thread_delete")]
    #[command(Review)]
    ReviewThreadDelete {
        session: SessionId,
        thread: ThreadId,
    },
    /// Reply on a thread. An agent's reply releases its own claim on it.
    #[serde(rename = "review_comment_add")]
    #[command(Review)]
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
    #[serde(rename = "review_comment_delete")]
    #[command(Review)]
    ReviewCommentDelete {
        session: SessionId,
        thread: ThreadId,
        comment: u64,
    },
    /// Hold attachment `image` (PNG, JPEG or WebP) for a comment about to be
    /// written. One no comment claims is dropped as newer ones arrive.
    #[serde(rename = "review_image_add")]
    #[command(Review, attach(Fixed("image")))]
    ReviewImageAdd {
        session: SessionId,
    },
    /// An image's bytes, as the reply's payload.
    #[serde(rename = "review_image_get")]
    #[command(ServerQuery, payload)]
    ReviewImageGet {
        session: SessionId,
        image: u64,
    },
    /// Hand every unsent human comment on an open thread to the agent.
    #[serde(rename = "review_submit")]
    #[command(Review)]
    ReviewSubmit {
        session: SessionId,
    },
    /// An agent's heartbeat while it waits for comments: keeps it shown as
    /// listening and answers how many threads wait for it.
    #[serde(rename = "review_poll")]
    #[command(Review)]
    ReviewPoll {
        session: SessionId,
        agent: ReviewIdentity,
    },
    /// Read the threads waiting for `agent`: handed over and claimed by no
    /// other agent. Changes nothing but presence, so a second fetch before
    /// [`Command::ReviewAck`] returns the same comments.
    #[serde(rename = "review_fetch")]
    #[command(Review)]
    ReviewFetch {
        session: SessionId,
        agent: ReviewIdentity,
    },
    /// Mark exactly the comments of a fetched `batch` delivered once the agent
    /// has them, and claim their threads for it where nobody else holds them.
    /// Refused with `batch_stale`, changing nothing, when any of them was
    /// deleted, resolved or delivered since.
    #[serde(rename = "review_ack")]
    #[command(Review)]
    ReviewAck {
        session: SessionId,
        agent: ReviewIdentity,
        batch: String,
    },
    /// Take or renew a lease on each thread for `agent`; all or none. A thread
    /// another agent holds is refused with `thread_claimed`.
    #[serde(rename = "review_claim")]
    #[command(Review)]
    ReviewClaim {
        session: SessionId,
        agent: ReviewIdentity,
        threads: Vec<ThreadId>,
    },
    /// Give up `agent`'s own claims on the threads; another's are left alone.
    #[serde(rename = "review_release")]
    #[command(Review)]
    ReviewRelease {
        session: SessionId,
        agent: ReviewIdentity,
        threads: Vec<ThreadId>,
    },

    /// Keep the session's model as it is now as the next numbered version.
    /// Moves no revision, writes no file and leaves `dirty` alone.
    #[serde(rename = "version_save")]
    #[command(Version)]
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
    #[serde(rename = "version_list")]
    #[command(ServerQuery)]
    VersionList {
        session: SessionId,
    },
    /// One version's `.clm`, byte for byte as it was saved, as the reply's
    /// payload.
    #[serde(rename = "version_get")]
    #[command(ServerQuery, payload)]
    VersionGet {
        session: SessionId,
        version: u64,
    },
    /// Replace the session's model with a version's, as one undoable edit.
    #[serde(rename = "version_restore")]
    #[command(Edit)]
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
    #[serde(rename = "version_open")]
    #[command(Edit)]
    VersionOpen {
        session: SessionId,
        version: u64,
    },
}
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    Fixed(&'static str),
    /// `<family>:<suffix>`, any number; the command checks the suffixes.
    Family(&'static str),
    /// Exactly this name, needed or not as the command's own fields decide.
    Optional(&'static str),
}

impl Attachment {
    #[must_use]
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
pub struct CommandBytes {
    pub attachments: &'static [Attachment],
    pub payload: bool,
}

impl Command {
    /// What bytes *this* command carries. Differs from [`Command::bytes`] only
    /// for an [`Attachment::Optional`], such as an `extension_set` of JSON.
    #[must_use]
    pub fn carries_bytes(&self) -> Option<&'static CommandBytes> {
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
    #[must_use]
    pub const fn session(&self) -> Option<SessionId> {
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
            | Self::BindingInterpolationSet { session, .. }
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

const fn one() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> NodeId {
        NodeId::new(id).expect("valid id")
    }

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
}
