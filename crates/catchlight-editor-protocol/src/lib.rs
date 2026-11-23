//! Wire types for the catchlight editor.
//!
//! Ids travel as the strings the model file stores, never per-session
//! handles, so [`Command::RenameId`] and texture drops break addons that
//! named the old Id. An add names its new Id under the reply's word (`node`,
//! `param`, …), never `id`: [`Request`] flattens its command beside its own
//! correlation `id`.

pub use catchlight_core::{MotionResponse, ResponseInput, ResponseSource};
use serde::{Deserialize, Serialize};

pub use catchlight_core::id::{ExtensionKey, NodeId, ParamId, SlotId, TexId};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionId(pub u64);

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
#[serde(tag = "cmd", rename_all = "snake_case")]
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
}

/// What applying a [`Command`] does to the model it addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// Changes the model or which sessions exist; the only kind that moves `rev`.
    Edit,
    /// Publishes shared view state (pose, camera, selection); changes no model.
    Presence,
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
];

impl Command {
    /// The `cmd` tag. Exhaustive so [`COMMAND_KINDS`] stays checkable.
    pub fn tag(&self) -> &'static str {
        match self {
            Command::EditApply { .. } => "edit_apply",
            Command::EditValidate { .. } => "edit_validate",
            Command::BindingCellsGet { .. } => "binding_cells_get",
            Command::BindingCellsSet { .. } => "binding_cells_set",
            Command::BindingCellsUnset { .. } => "binding_cells_unset",
            Command::MeshGet { .. } => "mesh_get",
            Command::ModelGet { .. } => "model_get",
            Command::GeometryGet { .. } => "geometry_get",
            Command::SessionFork { .. } => "session_fork",
            Command::ModelExport { .. } => "model_export",
            Command::SessionNew { .. } => "session_create",
            Command::SessionOpen { .. } => "session_open",
            Command::SessionList => "session_list",
            Command::SessionClose { .. } => "session_close",
            Command::Save { .. } => "session_save",
            Command::ExportManifest { .. } => "manifest_export",
            Command::Status { .. } => "session_get",
            Command::Check { .. } => "model_check",
            Command::NodeTree { .. } => "node_tree_get",
            Command::NodeInfo { .. } => "node_get",
            Command::NodeAdd { .. } => "node_add",
            Command::NodeSet { .. } => "node_set",
            Command::NodeReparent { .. } => "node_reparent",
            Command::NodeReorder { .. } => "node_reorder",
            Command::NodeDuplicate { .. } => "node_duplicate",
            Command::RenameId { .. } => "id_rename",
            Command::MaskAdd { .. } => "mask_add",
            Command::MaskDelete { .. } => "mask_delete",
            Command::MotionResponseSet { .. } => "motion_response_set",
            Command::NodeDelete { .. } => "node_delete",
            Command::TextureAdd { .. } => "texture_add",
            Command::TextureList { .. } => "texture_list",
            Command::ParamAdd { .. } => "param_add",
            Command::ParamList { .. } => "param_list",
            Command::ParamSet { .. } => "param_set",
            Command::ParamDelete { .. } => "param_delete",
            Command::BindingKeyInsert { .. } => "binding_key_insert",
            Command::BindingKeyDelete { .. } => "binding_key_delete",
            Command::BindingKeyMove { .. } => "binding_key_move",
            Command::BindingAdd { .. } => "binding_add",
            Command::BindingDelete { .. } => "binding_delete",
            Command::BindingInterpolate { .. } => "binding_interpolation_set",
            Command::BindingList { .. } => "binding_list",
            Command::MeshSet { .. } => "mesh_set",
            Command::MeshAuto { .. } => "mesh_generate",
            Command::SlotAdd { .. } => "slot_add",
            Command::SlotFill { .. } => "slot_fill",
            Command::SlotClear { .. } => "slot_clear",
            Command::SlotDelete { .. } => "slot_delete",
            Command::Slots { .. } => "slot_list",
            Command::Welds { .. } => "weld_list",
            Command::WeldSet { .. } => "weld_set",
            Command::WeldDelete { .. } => "weld_delete",
            Command::SpineAdd { .. } => "spine_add",
            Command::SpineSet { .. } => "spine_set",
            Command::SpineFit { .. } => "spine_fit",
            Command::Undo { .. } => "edit_undo",
            Command::Redo { .. } => "edit_redo",
            Command::EditGoto { .. } => "edit_goto",
            Command::EditHistoryGet { .. } => "edit_history_get",
            Command::PresenceSet { .. } => "presence_set",
            Command::PresenceGet { .. } => "presence_get",
            Command::Preview { .. } => "preview_render",
            Command::ImportJson { .. } => "structure_json_import",
            Command::ExtensionSet { .. } => "extension_set",
            Command::ExtensionDelete { .. } => "extension_delete",
            Command::Extensions { .. } => "extension_list",
            Command::ExtensionGet { .. } => "extension_get",
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
            .map(|(_, kind)| *kind)
            .unwrap_or(CommandKind::Edit)
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
            Command::SessionNew { source, .. } => source.is_some(),
            Command::ExtensionSet { value, .. } => matches!(value, ExtensionSet::Bytes),
            _ => true,
        };
        self.bytes().filter(|_| carrying)
    }

    /// The session this command addresses; `None` for editor-level commands.
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Command::SessionNew { .. } | Command::SessionOpen { .. } | Command::SessionList => None,
            Command::SessionClose { session }
            | Command::EditApply { session, .. }
            | Command::EditValidate { session, .. }
            | Command::BindingCellsGet { session, .. }
            | Command::BindingCellsSet { session, .. }
            | Command::BindingCellsUnset { session, .. }
            | Command::MeshGet { session, .. }
            | Command::ModelGet { session, .. }
            | Command::GeometryGet { session, .. }
            | Command::SessionFork { session, .. }
            | Command::ModelExport { session, .. }
            | Command::Save { session, .. }
            | Command::ExportManifest { session, .. }
            | Command::Status { session }
            | Command::Check { session }
            | Command::NodeTree { session }
            | Command::NodeInfo { session, .. }
            | Command::NodeAdd { session, .. }
            | Command::NodeSet { session, .. }
            | Command::NodeReparent { session, .. }
            | Command::NodeReorder { session, .. }
            | Command::NodeDuplicate { session, .. }
            | Command::RenameId { session, .. }
            | Command::MaskAdd { session, .. }
            | Command::MaskDelete { session, .. }
            | Command::MotionResponseSet { session, .. }
            | Command::NodeDelete { session, .. }
            | Command::TextureAdd { session, .. }
            | Command::TextureList { session }
            | Command::ParamAdd { session, .. }
            | Command::ParamList { session }
            | Command::ParamSet { session, .. }
            | Command::ParamDelete { session, .. }
            | Command::BindingKeyInsert { session, .. }
            | Command::BindingKeyDelete { session, .. }
            | Command::BindingKeyMove { session, .. }
            | Command::BindingAdd { session, .. }
            | Command::BindingDelete { session, .. }
            | Command::BindingInterpolate { session, .. }
            | Command::BindingList { session, .. }
            | Command::MeshSet { session, .. }
            | Command::MeshAuto { session, .. }
            | Command::SlotAdd { session, .. }
            | Command::SlotFill { session, .. }
            | Command::SlotClear { session, .. }
            | Command::SlotDelete { session, .. }
            | Command::Slots { session, .. }
            | Command::Welds { session }
            | Command::WeldSet { session, .. }
            | Command::WeldDelete { session, .. }
            | Command::SpineAdd { session, .. }
            | Command::SpineSet { session, .. }
            | Command::SpineFit { session, .. }
            | Command::Undo { session, .. }
            | Command::Redo { session, .. }
            | Command::EditGoto { session, .. }
            | Command::EditHistoryGet { session, .. }
            | Command::PresenceSet { session, .. }
            | Command::PresenceGet { session }
            | Command::Preview { session, .. }
            | Command::ImportJson { session, .. }
            | Command::ExtensionSet { session, .. }
            | Command::ExtensionDelete { session, .. }
            | Command::Extensions { session }
            | Command::ExtensionGet { session, .. } => Some(*session),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
#[serde(tag = "op", rename_all = "snake_case")]
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
#[serde(tag = "mode", rename_all = "snake_case")]
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
    /// Snake_case; the model file spells the same modes in PascalCase.
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ParamPose {
    pub param: ParamId,
    pub value: f32,
}

/// One texture an [`Command::ImportJson`] structure names, arriving as
/// attachment `texture:<texture>`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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
    /// `catchlight.` is the format's own prefix: a reader accepts a key under
    /// it, and nothing outside the format may author one.
    ReservedExtension,
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
        /// The current core ClmStructure JSON, including authored cells and extension markers.
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
    /// Stable budget name, such as request_bytes or derived_cell_vertices.
    pub resource: String,
    /// Maximum permitted count in the resource's units.
    pub limit: u64,
    /// Observed count; byte counting may stop at the first excess.
    pub requested: u64,
}

/// Ephemeral shared view state, kept off the model path: never an edit, never
/// saved.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Presence {
    #[serde(default)]
    pub pose: Vec<ParamPose>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<NodeId>,
}

/// What [`Command::ExtensionSet`] sets: inline JSON, or bytes attached as
/// `value`. Tagged by `kind` because a byte marker is itself a JSON object.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExtensionSet {
    Json {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        value: serde_json::Value,
    },
    Bytes,
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ExtensionInfo {
    pub key: ExtensionKey,
    pub value: ExtensionValueInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
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
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
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
            message: "part carries no such slot".into(),
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
        assert!(serde_json::from_str::<Command>(
            r#"{"cmd":"scratch_deform","session":1,"node":"hair","offsets":[]}"#
        )
        .is_err());
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

    /// [`NodeInfo`] must parse as a [`NodePatch`] for the inspector round trip.
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
        let patch: NodePatch = serde_json::from_str(&line).unwrap();

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
        let words: Vec<String> = SCALAR_TARGETS.iter().map(|t| t.to_string()).collect();
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
