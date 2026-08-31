//! Wire types for the catchlight editor.
//!
//! A client sends a [`Request`] (one JSON object per line); the server answers
//! with a [`Reply`] — a correlated [`Reply::Ok`] / [`Reply::Err`], or an
//! unsolicited [`Reply::Event`]. The types are transport-agnostic: the same
//! messages travel over a Unix socket (native) or a WebSocket (web).
//!
//! Invariants this module carries:
//!
//! - **Ids are what the wire names things by.** A node, param, texture, seam
//!   or slot travels as its [`NodeId`] / [`ParamId`] / [`TexId`] /
//!   [`SeamId`] / [`SlotId`] — the same string the model file stores, not a
//!   per-session handle. So a reference survives the session that minted it,
//!   an addon can be written against a model by reading its tree, and two
//!   clients editing one session mean the same node by the same word. The
//!   Id types validate on the way in, so a string outside the charset
//!   (`[A-Za-z0-9_./-]`, no leading `.`, `/` or `-`) never reaches the server: it
//!   is refused as [`ErrorCode::BadRequest`] against the request's own `id`.
//!   [`SessionId`] is the exception — a session is not part of any model, so
//!   it stays an opaque `u64` the server allocates.
//!
//! - **[`Command::RenameId`] is a breaking change for addons**, and the only
//!   command that is. An addon names what it needs in the base model by Id;
//!   renaming one rewrites every reference *inside* this model and none of
//!   the references outside it. There are no aliases. A client should treat
//!   it the way it treats deleting a param: an author's deliberate act, not
//!   an editing convenience.
//!
//! - **A texture belongs to a part.** [`Command::TextureAdd`] names the part
//!   the image is for and the model gains both in one edit; there is no
//!   command that adds a texture on its own. The rule holds on the way out,
//!   so an edit that leaves a texture with no part drawing it deletes it, and
//!   the reply says which ([`ResponseBody::Node::dropped`],
//!   [`ResponseBody::Texture::dropped`]). Like [`Command::RenameId`] it is a
//!   breaking change for an addon that named that Id — undo restores it in
//!   the session, nothing restores it downstream.
//!
//! - **Nothing is addressed by name.** [`Name`](catchlight_core::id::Name) is
//!   a label a person reads; two nodes may share one. Commands that carry a
//!   `name` are setting or reporting that label.
//!
//! - **Params are scalar.** A param has one range and one list of key
//!   positions. A binding is keyed by *one or two* params ([`BindingParams`])
//!   and its grid is the product of their key positions, so `cell` stays
//!   `[x, y]` — the y index is 0 for a one-param binding. An XY pad is a view
//!   over any two params, not a property of either.
//!
//! - **A session holds a complete model, never an addon fragment.** There is
//!   deliberately no install/extract command pair and no multi-root tree
//!   reply: `catchlight-clm` already installs, extracts and scans a fragment
//!   at the file level without a session, which is the whole workflow, and a
//!   session that could open one would need every tree reply, the inspector
//!   and the commit gate to grow a second shape for a case nothing asks for.
//!   [`Command::NodeTree`] on a fragment is [`ErrorCode::Fragment`].
//!
//! The document path ([`Command::NodeSet`] and friends) bumps the session's
//! revision, records undo and is saved. The presence path
//! ([`Command::PresenceSet`], [`Command::ScratchDeform`]) does none of those:
//! it is what a live drag and a shared camera ride on.

use serde::{Deserialize, Serialize};

pub use catchlight_core::id::{NodeId, ParamId, SeamId, SlotId, TexId};

/// One open editing session (one model and its undo history). Opaque: a
/// session is not part of any model, so it has no Id of its own.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(transparent)]
pub struct SessionId(pub u64);

/// A client request. `id` correlates the reply; commands that target a session
/// carry it in their own fields.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
}

/// Just the correlation id, for a request whose body did not parse — so a
/// malformed command is still answered against the id the client is waiting
/// on rather than against 0.
#[derive(Deserialize, Debug, Clone, Copy)]
pub struct RequestId {
    pub id: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    SessionNew {
        #[serde(default)]
        name: Option<String>,
    },
    SessionOpen {
        path: String,
    },
    SessionImport {
        manifest_path: String,
    },
    SessionList,
    SessionClose {
        session: SessionId,
    },
    Save {
        session: SessionId,
        #[serde(default)]
        path: Option<String>,
    },
    ExportManifest {
        session: SessionId,
        path: String,
    },
    Status {
        session: SessionId,
    },
    Check {
        session: SessionId,
    },
    NodeTree {
        session: SessionId,
    },
    NodeAdd {
        session: SessionId,
        parent: NodeId,
        kind: NodeKindArg,
        #[serde(default)]
        name: Option<String>,
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
    /// Reparent + position in one undoable step: `node` becomes `parent`'s
    /// child at `index` (clamped).
    NodeMove {
        session: SessionId,
        node: NodeId,
        parent: NodeId,
        index: u32,
    },
    /// Deep-copy a node's subtree as its next sibling (bindings and
    /// subtree-internal mask references come along). The copies get fresh
    /// generated Ids.
    NodeDuplicate {
        session: SessionId,
        node: NodeId,
    },
    /// Change a node's, a param's or a texture's Id, rewriting every
    /// reference this model holds to it.
    ///
    /// **Breaking for addons.** An addon reaches into a base model by Id, so
    /// renaming one is exactly as breaking as deleting it: nothing outside
    /// this model is rewritten and there is no alias left behind.
    RenameId {
        session: SessionId,
        rename: Rename,
    },
    /// Append a mask source to a Part/Composite. `mode` is mask|dodge.
    MaskAdd {
        session: SessionId,
        node: NodeId,
        source: NodeId,
        mode: String,
    },
    /// Change the mode of the mask at `index`.
    MaskSet {
        session: SessionId,
        node: NodeId,
        index: u32,
        mode: String,
    },
    /// Move the mask at `index` to position `to` (clamped).
    MaskReorder {
        session: SessionId,
        node: NodeId,
        index: u32,
        to: u32,
    },
    MaskDelete {
        session: SessionId,
        node: NodeId,
        index: u32,
    },
    /// Change fields on a SimplePhysics node; absent = unchanged.
    PhysicsSet {
        session: SessionId,
        node: NodeId,
        /// rigid | spring
        #[serde(default)]
        kind: Option<String>,
        /// xy | yx | angle_length | length_angle
        #[serde(default)]
        map_mode: Option<String>,
        #[serde(default)]
        local_only: Option<bool>,
        /// The one or two params the driver writes (angle, length). Absent =
        /// unchanged; see `clear_target_params` to detach.
        #[serde(default)]
        target_params: Option<Vec<ParamId>>,
        /// Detach the driven params (wins over `target_params`).
        #[serde(default)]
        clear_target_params: bool,
        #[serde(default)]
        gravity: Option<f32>,
        #[serde(default)]
        length: Option<f32>,
        #[serde(default)]
        frequency: Option<f32>,
        #[serde(default)]
        angle_damping: Option<f32>,
        #[serde(default)]
        length_damping: Option<f32>,
        #[serde(default)]
        output_scale: Option<[f32; 2]>,
    },
    /// Model-level physics constants.
    PhysicsGlobals {
        session: SessionId,
        #[serde(default)]
        gravity: Option<f32>,
        #[serde(default)]
        pixels_per_meter: Option<f32>,
    },
    NodeDelete {
        session: SessionId,
        node: NodeId,
    },
    /// Read an image from `path` and give it to `node`, which has to be a
    /// part: a texture is added and assigned in one edit. If that part was
    /// the last thing drawing whatever it drew before, that texture goes with
    /// this edit — see [`ResponseBody::Texture`].
    TextureAdd {
        session: SessionId,
        node: NodeId,
        path: String,
    },
    TextureList {
        session: SessionId,
    },
    /// Create a scalar param. `key_positions` are normalized 0..1 (empty =
    /// the two endpoints).
    ParamAdd {
        session: SessionId,
        name: String,
        #[serde(default)]
        min: f32,
        #[serde(default = "one")]
        max: f32,
        #[serde(default)]
        default: f32,
        #[serde(default)]
        key_positions: Vec<f32>,
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
    /// Insert a key position at normalized `value`, strictly inside (0, 1).
    /// Authored cells shift; the new row/column derives.
    ParamKeyInsert {
        session: SessionId,
        param: ParamId,
        value: f32,
    },
    /// Remove an interior key position; its authored cells are dropped.
    ParamKeyDelete {
        session: SessionId,
        param: ParamId,
        index: u32,
    },
    /// Move an interior key position to normalized `value` (must stay
    /// strictly between its neighbors).
    ParamKeyMove {
        session: SessionId,
        param: ParamId,
        index: u32,
        value: f32,
    },
    /// Mirror the param (key positions reflect, cells move to the mirrored
    /// index; values untouched — compose with BindingInvert).
    ParamFlip {
        session: SessionId,
        param: ParamId,
    },
    BindingAdd {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        /// tx|ty|sx|sy|rx|ry|rz|z_order|opacity|tint{r,g,b}|screentint{r,g,b}|outputscale{x,y}
        target: String,
    },
    /// Author one scalar keypoint (auto-creates the binding).
    BindingKey {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: String,
        /// `[x, y]` index into the binding's key grid; `y` is 0 for a
        /// one-param binding.
        cell: [u32; 2],
        value: f32,
    },
    /// Author several scalar keypoints at one cell in one undoable step (a
    /// gizmo drag commits tx+ty together).
    BindingKeys {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        cell: [u32; 2],
        entries: Vec<BindingKeyEntry>,
    },
    /// Un-author a keypoint (back to derived). `target` additionally accepts
    /// `deform`.
    BindingUnset {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: String,
        cell: [u32; 2],
    },
    /// Author the identity value at a keypoint.
    BindingReset {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: String,
        cell: [u32; 2],
    },
    BindingDelete {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: String,
    },
    /// nearest | stepped | linear | cubic
    BindingInterpolate {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: String,
        mode: String,
    },
    /// Negate every authored value.
    BindingInvert {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: String,
    },
    /// Author the value evaluated at `from` into cell `to`.
    BindingCopyKey {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: String,
        from: [u32; 2],
        to: [u32; 2],
    },
    /// Author a deform keypoint from an affine applied to the part's rest mesh.
    DeformSet {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        cell: [u32; 2],
        #[serde(default)]
        translate: Option<[f32; 2]>,
        #[serde(default)]
        rotate: Option<f32>,
        #[serde(default)]
        scale: Option<[f32; 2]>,
    },
    /// Author per-vertex deform offsets (`[dx, dy, …]`, matching the mesh).
    /// This is what commits a live drag.
    DeformVertices {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        cell: [u32; 2],
        offsets: Vec<f32>,
    },
    /// Replace a Part/MeshGroup mesh; every deform binding on the node is
    /// re-fitted onto the new topology in the same undoable step. Answers
    /// with the seam slots the new mesh emptied.
    MeshSet {
        session: SessionId,
        node: NodeId,
        verts: Vec<f32>,
        uvs: Vec<f32>,
        indices: Vec<u32>,
        origin: [f32; 2],
    },
    /// Copy `from`'s mesh onto `to` (with the same deform re-fit and the same
    /// emptied-slot reply).
    MeshCopy {
        session: SessionId,
        from: NodeId,
        to: NodeId,
    },
    /// Name a new seam on a part.
    SeamAdd {
        session: SessionId,
        node: NodeId,
        seam: SeamId,
    },
    /// Remove a seam, and every weld that named it.
    SeamDelete {
        session: SessionId,
        node: NodeId,
        seam: SeamId,
    },
    /// Add a slot to a seam. The slot lands unfilled, and reaches every seam
    /// welded to this one at [`catchlight_core::DEFAULT_SLOT_WEIGHT`] — a
    /// weld pairs two seams slot by slot, so their slot sets are one set.
    SlotAdd {
        session: SessionId,
        node: NodeId,
        seam: SeamId,
        slot: SlotId,
    },
    /// Point a slot at one of the part's vertices.
    SlotFill {
        session: SessionId,
        node: NodeId,
        seam: SeamId,
        slot: SlotId,
        vertex: u32,
    },
    /// Unfill a slot. Welds keep it and skip it until it is filled again.
    SlotClear {
        session: SessionId,
        node: NodeId,
        seam: SeamId,
        slot: SlotId,
    },
    /// Remove a slot — from this seam and from every seam welded to it.
    SlotDelete {
        session: SessionId,
        node: NodeId,
        seam: SeamId,
        slot: SlotId,
    },
    /// The seams a part carries, with what fills each slot.
    Seams {
        session: SessionId,
        node: NodeId,
    },
    /// Every weld in the model.
    Welds {
        session: SessionId,
    },
    /// Every slot in the model no vertex fills. A re-meshed part empties its
    /// slots, so this is what a commit gate reads.
    UnfilledSlots {
        session: SessionId,
    },
    /// Weld two seams together, replacing any weld already pairing them.
    /// Empty `weights` welds every slot at
    /// [`catchlight_core::DEFAULT_SLOT_WEIGHT`].
    WeldSet {
        session: SessionId,
        a: SeamAddr,
        b: SeamAddr,
        #[serde(default)]
        weights: Vec<SlotWeight>,
    },
    /// Add a SimplePhysics node. `kind` is rigid|spring.
    PhysicsAdd {
        session: SessionId,
        parent: NodeId,
        #[serde(default)]
        name: Option<String>,
        kind: String,
        /// The one or two params the driver writes (angle, length).
        #[serde(default)]
        target_params: Vec<ParamId>,
        #[serde(default)]
        length: Option<f32>,
        #[serde(default)]
        gravity: Option<f32>,
        #[serde(default)]
        frequency: Option<f32>,
        #[serde(default)]
        angle_damping: Option<f32>,
        #[serde(default)]
        length_damping: Option<f32>,
    },
    Undo {
        session: SessionId,
    },
    Redo {
        session: SessionId,
    },
    /// Publish ephemeral view state (pose / camera / selection) — a separate
    /// path from the document: never bumps rev, never undone, never saved.
    PresenceSet {
        session: SessionId,
        #[serde(flatten)]
        presence: Presence,
    },
    /// Read the session's current shared presence (latest publisher wins).
    PresenceGet {
        session: SessionId,
    },
    /// Show a deform on the session's puppet without authoring it: the live
    /// half of a vertex drag. On the presence path, so a drag of any length
    /// produces no revision and no undo entry; committing it is
    /// [`Command::DeformVertices`], which produces exactly one.
    ///
    /// `offsets` is `[dx, dy, …]` matching the node's mesh; an empty list
    /// clears the scratch deform. It is dropped the next time the model
    /// changes, because the puppet rebakes.
    ScratchDeform {
        session: SessionId,
        node: NodeId,
        offsets: Vec<f32>,
    },
    Preview {
        session: SessionId,
        #[serde(default)]
        pose: Vec<ParamPose>,
        #[serde(default)]
        size: Option<[u32; 2]>,
        #[serde(default)]
        out: Option<String>,
    },
}

/// Which Id [`Command::RenameId`] changes, and to what.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rename {
    Node { from: NodeId, to: NodeId },
    Param { from: ParamId, to: ParamId },
    Texture { from: TexId, to: TexId },
}

/// The param, or the pair of params, a binding is keyed by. With `param_y`
/// the binding's grid spans both params' key positions and `cell` indexes
/// both; without it the grid is one row and `cell[1]` is 0.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BindingParams {
    pub param: ParamId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_y: Option<ParamId>,
}

impl BindingParams {
    /// A binding keyed by one param.
    pub fn one(param: ParamId) -> Self {
        Self {
            param,
            param_y: None,
        }
    }

    /// A binding whose grid spans two params.
    pub fn two(x: ParamId, y: ParamId) -> Self {
        Self {
            param: x,
            param_y: Some(y),
        }
    }
}

/// One end of a weld: the seam, and the part carrying it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SeamAddr {
    pub node: NodeId,
    pub seam: SeamId,
}

/// One slot of a model, named in full.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SlotAddr {
    pub node: NodeId,
    pub seam: SeamId,
    pub slot: SlotId,
}

/// One slot of a part's seam — the part is whatever carries the reply.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SeamSlot {
    pub seam: SeamId,
    pub slot: SlotId,
}

/// A slot's share of the point its two welded vertices are pulled toward.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SlotWeight {
    pub slot: SlotId,
    pub weight: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKindArg {
    Group,
    Part,
    Composite,
    MeshGroup,
}

/// Fields to change on a node; every field is optional (absent = unchanged).
/// Kind-specific fields are ignored on nodes of another kind: the colour fields
/// (`opacity`, `blend_mode`, `tint`, `screen_tint`) reach parts and composites
/// only, because a mesh group is never drawn and carries no colour.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct NodePatch {
    /// The label a person reads. Free to repeat; nothing is addressed by it.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<TexId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_to_root: Option<bool>,
    /// Blend-mode name (Normal | Multiply | ColorDodge | …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<String>,
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
    pub mg_dynamic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mg_translate_children: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BindingKeyEntry {
    pub target: String,
    pub value: f32,
}

/// One param at one value — a pose is a list of these. Params are scalar, so
/// there is one number.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ParamPose {
    pub param: ParamId,
    pub value: f32,
}

/// The server's answer. `Ok`/`Err` carry the request's `id`; `Event` is
/// unsolicited (a document changed on a session this client observes).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Ok {
        id: u64,
        body: ResponseBody,
    },
    /// `code` is what a client branches on; `message` is for a person.
    Err {
        id: u64,
        code: ErrorCode,
        message: String,
    },
    Event(Event),
}

/// Why a command was refused. A client that has to react — a commit gate
/// waiting on unfilled slots, a mesh editor offering to refill a seam —
/// branches on this rather than on the message text.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No open session with that [`SessionId`].
    NoSession,
    /// The model carries no node with that [`NodeId`].
    NoNode,
    /// The model carries no param with that [`ParamId`].
    NoParam,
    /// The model carries no texture with that [`TexId`].
    NoTexture,
    /// The request did not parse: bad JSON, an unknown command, or a string
    /// that is not a valid Id.
    BadRequest,
    /// A binding target, blend mode or other enum name the server does not
    /// know, or one that does not fit the node it names.
    BadTarget,
    NothingToUndo,
    NothingToRedo,
    /// A save with no path, on a session that has no file of its own.
    NoSavePath,
    /// The part carries no such seam.
    UnknownSeam,
    /// The seam carries no such slot.
    UnknownSlot,
    /// The part already carries a seam with that Id.
    DuplicateSeam,
    /// The seam already holds a slot with that Id.
    DuplicateSlot,
    /// A weld's two seams must hold the same slots, each weighted once.
    WeldSlotMismatch,
    /// This needs a complete model and the session holds an addon fragment.
    /// A session never opens one, so nothing should see this today.
    Fragment,
    /// The model refused the edit for some other reason; read `message`.
    Edit,
    /// A manifest could not be read or written.
    Manifest,
    Io,
    /// An image could not be decoded.
    Image,
    /// The preview renderer failed.
    Preview,
    /// The command needs a filesystem or a GPU and this build has neither
    /// (wasm).
    NativeOnly,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ResponseBody {
    Empty,
    Session {
        session: SessionId,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    Node {
        node: NodeId,
        /// Textures the edit deleted, because no part draws them any more.
        /// Every texture a model carries is drawn by a part, so repointing or
        /// removing the last one takes the texture with it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<TexId>,
    },
    Param {
        param: ParamId,
    },
    Params {
        params: Vec<ParamInfo>,
    },
    Texture {
        texture: TexId,
        /// Textures the edit deleted, because the part that had been drawing
        /// them stopped and nothing else was. Every texture a model carries
        /// is drawn by a part, so an upload that displaces the last user of
        /// the old one takes it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dropped: Vec<TexId>,
    },
    Textures {
        textures: Vec<TexInfo>,
    },
    Tree {
        root: TreeNode,
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
    Seams {
        seams: Vec<SeamInfo>,
    },
    Welds {
        welds: Vec<WeldInfo>,
    },
    /// Slots nothing fills, across the whole model.
    UnfilledSlots {
        slots: Vec<SlotAddr>,
    },
    /// The slots a mesh edit emptied on `node`, in seam-then-slot order.
    Emptied {
        node: NodeId,
        slots: Vec<SeamSlot>,
    },
}

/// Ephemeral shared view state. Rides its own path — decoupled from the document
/// so scrubbing/panning generate zero document traffic and never persist.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Presence {
    #[serde(default)]
    pub pose: Vec<ParamPose>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<NodeId>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Camera {
    pub center: [f32; 2],
    pub height: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The document changed; `rev` is the session's new revision. Observers
    /// re-read when their last-seen rev is older.
    DocumentChanged { session: SessionId, rev: u64 },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
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
pub struct StatusInfo {
    pub title: String,
    pub node_count: u32,
    pub param_count: u32,
    pub texture_count: u32,
    pub dirty: bool,
    pub rev: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TexInfo {
    pub id: TexId,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ParamInfo {
    /// What the param is addressed by.
    pub id: ParamId,
    /// What a person reads. Free to repeat.
    pub name: String,
    pub min: f32,
    pub max: f32,
    #[serde(default)]
    pub default: f32,
    /// Key positions, normalized 0..1 across `[min, max]`. Always at least
    /// the two endpoints, so a binding's grid is `key_positions.len()` wide.
    #[serde(default)]
    pub key_positions: Vec<f32>,
    pub bindings: u32,
}

/// A seam and what currently fills it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SeamInfo {
    pub id: SeamId,
    pub slots: Vec<SlotInfo>,
}

/// One slot; `vertex` absent means unfilled, and welds skip it.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SlotInfo {
    pub id: SlotId,
    #[serde(default)]
    pub vertex: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WeldInfo {
    pub a: SeamAddr,
    pub b: SeamAddr,
    pub weights: Vec<SlotWeight>,
}

fn one() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TreeNode {
    /// What the node is addressed by, here and in the file.
    pub id: NodeId,
    /// What a person reads. Free to repeat.
    pub name: String,
    pub kind: String,
    pub z_order: f32,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub children: Vec<TreeNode>,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PreviewInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

/// `$XDG_RUNTIME_DIR/catchlight-editor/server.sock` (falling back to the temp
/// dir). Whoever owns this path is the server; clients connect here. Lives in
/// the protocol crate so server and clients cannot disagree about it.
#[cfg(unix)]
pub fn default_socket_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("catchlight-editor").join("server.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// An Id is the string the model file stores, not a per-session handle:
    /// that is what lets an addon name a base model's node, and what makes a
    /// recorded command replayable against a reopened session.
    #[test]
    fn ids_travel_as_the_plain_strings_a_model_file_stores() {
        let req = Request {
            id: 1,
            command: Command::BindingKey {
                session: SessionId(2),
                params: BindingParams::two(param("head.x"), param("head.y")),
                node: node("root/part-3f9a2c1e"),
                target: "tx".into(),
                cell: [1, 2],
                value: 0.5,
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains("\"param\":\"head.x\""), "{line}");
        assert!(line.contains("\"param_y\":\"head.y\""), "{line}");
        assert!(line.contains("\"node\":\"root/part-3f9a2c1e\""), "{line}");
        let back: Request = serde_json::from_str(&line).unwrap();
        match back.command {
            Command::BindingKey { params, cell, .. } => {
                assert_eq!(params.param.as_str(), "head.x");
                assert_eq!(params.param_y.unwrap().as_str(), "head.y");
                assert_eq!(cell, [1, 2]);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A one-param binding writes no `param_y` at all, so the common case
    /// stays the shape it was.
    #[test]
    fn a_one_param_binding_leaves_the_second_param_off_the_wire() {
        let line = serde_json::to_string(&Request {
            id: 1,
            command: Command::BindingAdd {
                session: SessionId(1),
                params: BindingParams::one(param("pull")),
                node: node("body"),
                target: "tx".into(),
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
    fn an_id_outside_the_charset_does_not_parse() {
        let line = r#"{"id":4,"cmd":"node_delete","session":1,"node":"a b"}"#;
        let err = serde_json::from_str::<Request>(line).unwrap_err();
        assert!(err.to_string().contains("invalid byte"), "{err}");
        // ...but the correlation id is still readable, so the server can
        // answer the request the client is actually waiting on.
        let RequestId { id } = serde_json::from_str(line).unwrap();
        assert_eq!(id, 4);
    }

    #[test]
    fn reply_variants_roundtrip() {
        let ok = Reply::Ok {
            id: 7,
            body: ResponseBody::Session {
                session: SessionId(3),
            },
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"reply\":\"ok\""));
        assert!(s.contains("\"result\":\"session\""));
        let evt = Reply::Event(Event::DocumentChanged {
            session: SessionId(3),
            rev: 9,
        });
        let s = serde_json::to_string(&evt).unwrap();
        let back: Reply = serde_json::from_str(&s).unwrap();
        assert!(matches!(
            back,
            Reply::Event(Event::DocumentChanged { rev: 9, .. })
        ));
    }

    #[test]
    fn an_error_reply_carries_a_code_a_client_can_match_on() {
        let s = serde_json::to_string(&Reply::Err {
            id: 3,
            code: ErrorCode::UnknownSlot,
            message: "seam carries no such slot".into(),
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
