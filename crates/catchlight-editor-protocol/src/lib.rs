//! Wire types for the catchlight editor.
//!
//! A client sends a [`Request`] (one JSON object per line); the server answers
//! with a [`Reply`] — a correlated [`Reply::Ok`] / [`Reply::Err`], or an
//! unsolicited [`Reply::Event`]. The types are transport-agnostic: the same
//! messages travel over a Unix socket (native) or a WebSocket (web).
//!
//! Invariants this module carries:
//!
//! - **Ids are what the wire names things by.** A node, param, texture or
//!   slot travels as its [`NodeId`] / [`ParamId`] / [`TexId`] / [`SlotId`] —
//!   the same string the model file stores, not a per-session handle. A
//!   [`SlotId`] is unique within its part rather than within the model, so it
//!   always travels beside the node that carries it. So a reference survives
//!   the session that minted it, an addon can be written against a model by
//!   reading its tree, and two clients editing one session mean the same
//!   node by the same word. The Id types validate on the way in, so a string
//!   outside the charset
//!   (`[A-Za-z0-9_./-]`, no leading `.`, `/` or `-`) never reaches the server: it
//!   is refused as [`ErrorCode::BadRequest`] against the request's own `id`.
//!   [`SessionId`] is the exception — a session is not part of any model, so
//!   it stays an opaque `u64` the server allocates.
//!
//! - **An add may name the Id it makes**, under the same word the reply names
//!   it back with: `node` on [`Command::NodeAdd`] and [`Command::PhysicsAdd`],
//!   `param` on [`Command::ParamAdd`], `texture` on [`Command::TextureAdd`],
//!   `slot` on [`Command::SlotAdd`]. Absent, the editor draws a free
//!   one; present, it is refused as [`ErrorCode::DuplicateId`] if the model
//!   already carries it. Either way the reply says which Id was made. So a
//!   script that authors a rig writes the Ids it means once, rather than
//!   adding and then renaming — and [`Command::RenameId`] stays what it says
//!   it is, an author's deliberate break. The field cannot be called `id`: a
//!   [`Request`] flattens its command next to its own correlation `id`, and
//!   serde reads two of those as a malformed request.
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
//! - **A closed set travels as itself.** A mask mode, a blend mode, a
//!   physics kind or map mode, an interpolation, a node kind, a binding
//!   target: each is an enum here, so serde parses it and a client reads a
//!   union rather than a string. The spelling is exact — there is no case
//!   folding and there are no aliases — and a word outside the set is
//!   [`ErrorCode::BadRequest`] before any command runs, not a validation the
//!   server does afterwards.
//!
//! - **A field with three states is a merge patch.** Absent leaves the value
//!   alone, `null` sets it to nothing, and a value sets it to that. Today
//!   [`NodePatch::texture`] is the only one: a part keeps its texture, draws
//!   none, or draws the Id named. One field carries the whole change, so
//!   there is no second field to disagree with it and no rule about which of
//!   the two wins.
//!
//! - **A list of points is a list of points.** Mesh vertices, UVs and deform
//!   offsets travel as `[[x, y], …]` and triangles as `[[a, b, c], …]`, never
//!   as one flat run of numbers. A dangling coordinate or a triangle missing
//!   a corner is then [`ErrorCode::BadRequest`] from serde, before any
//!   command runs, rather than a length check the server has to remember to
//!   write.
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
//! - **A session's model is always complete.** A fragment enters one way and
//!   one way only: an import naming a `parent` ([`Command::ImportFile`] or
//!   [`Command::ImportJson`]), which installs it under that node in a single
//!   atomic edit and leaves a complete model behind. It never *becomes* the
//!   session's model, there is no extract command to match it, and no reply
//!   carries a multi-root tree — so every tree reply, the inspector and the
//!   commit gate keep one shape.
//!   `catchlight-cli` is where a fragment is cut, scanned and merged at the
//!   file level, without a session at all. [`Command::NodeTree`] on a
//!   fragment is [`ErrorCode::Fragment`], which nothing should see.
//!
//! - **A `path` field is an opaque storage key, and it is the server's own
//!   store.** [`Command::SessionOpen`], [`Command::Save`] and
//!   [`Command::ExportManifest`] each name a file with one string, and what
//!   that string addresses is the server's store: a filesystem path natively,
//!   an OPFS entry or a fetched URL in the browser, a blob key in the cloud.
//!   Only one thing reads a key's shape — `/` separates segments. Bytes a
//!   *client* holds never travel as a key: they are an attachment on the
//!   command that uses them. A client that builds keys should not assume
//!   more.
//!
//! - **An extension is carried, never interpreted.** A vendor files a value
//!   under a dotted key ([`ExtensionKey`], vendor first) and catchlight reads
//!   none of it: [`Command::ExtensionSet`] takes it, [`Command::Extensions`]
//!   and [`Command::ExtensionGet`] give it back, and a save round-trips it
//!   untouched. The two kinds differ in how they travel rather than in what
//!   they mean — a JSON value is inline everywhere, including in a structure
//!   feed, while bytes travel as a `{size, hash}` marker and are fetched by
//!   that hash, so a thumbnail does not ride along with every unrelated edit.
//!
//! - **Bytes travel beside a command, never in it.** A command that needs
//!   bytes declares the names they arrive under in [`COMMAND_BYTES`], and one
//!   whose reply is bytes declares that there too; [`Command::bytes`] reads
//!   the table by tag, so a command absent from it carries none. How the
//!   names and the blobs are framed is each transport's business — the wire
//!   types here are the same either way. A name the command did not declare,
//!   and a declared [`Attachment::Fixed`] that did not arrive, are both
//!   [`ErrorCode::BadRequest`] before the command runs; an
//!   [`Attachment::Family`]'s suffixes are the command's own to check against
//!   its fields.
//!
//! - **Every command says what it does, in one place.** [`COMMAND_KINDS`]
//!   gives each one a [`CommandKind`], and that is what a client routes by.
//!   `Edit` moves the session's revision, records undo and is saved.
//!   `Presence` publishes shared view state and moves nothing. `Scratch` shows
//!   a live edit on a puppet and never authors it. `ReplicaQuery` is a pure
//!   function of the [`Model`](catchlight_core::Model), so a client holding a
//!   replica answers it without asking the editor. `ServerQuery` needs the
//!   editor's own state, its store or its renderer, so only the editor can.
//!
//! - **A reply says which revision it reflects.** [`Reply::Ok`] carries the
//!   addressed session's `rev` *after* the command, so a client can tell a
//!   stale read from a fresh one without a second round trip.

use serde::{Deserialize, Serialize};

pub use catchlight_core::id::{ExtensionKey, NodeId, ParamId, SlotId, TexId};

/// One open editing session (one model and its undo history). Opaque: a
/// session is not part of any model, so it has no Id of its own.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SessionId(pub u64);

/// A client request. `id` correlates the reply; commands that target a session
/// carry it in their own fields.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Command {
    SessionNew {
        #[serde(default)]
        name: Option<String>,
    },
    /// Open the `.clm` the server's store holds at `path`.
    ///
    /// A file of the server's, and the session can save back over it. Bytes a
    /// client holds are not this command: those are [`Command::SessionNew`]
    /// followed by [`Command::ImportFile`], which leaves the session with no
    /// file to save to, because there is none.
    SessionOpen {
        path: String,
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
    /// Everything an inspector shows for one node: what [`NodePatch`] can set,
    /// under the same field names, plus the node's kind, its parent, its Id
    /// and the size of the mesh it holds. What [`Command::NodeTree`] carries
    /// is what a tree row draws; this is the rest.
    NodeInfo {
        session: SessionId,
        node: NodeId,
    },
    NodeAdd {
        session: SessionId,
        parent: NodeId,
        kind: NodeKindArg,
        #[serde(default)]
        name: Option<String>,
        /// The Id to create it under. Absent generates one; an Id the model
        /// already carries is [`ErrorCode::DuplicateId`].
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
    /// Append a mask source to a Part/Composite.
    MaskAdd {
        session: SessionId,
        node: NodeId,
        source: NodeId,
        mode: MaskMode,
    },
    /// Change the mode of the mask at `index`.
    MaskSet {
        session: SessionId,
        node: NodeId,
        index: u32,
        mode: MaskMode,
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
        #[serde(default)]
        kind: Option<PhysicsKind>,
        #[serde(default)]
        map_mode: Option<PhysicsMapMode>,
        #[serde(default)]
        local_only: Option<bool>,
        /// The params the driver writes ([`PhysicsTargets`]). Absent leaves
        /// both outputs as they are; present, both become exactly what it
        /// says, so `{}` detaches both and `{"length": "len"}` binds the
        /// second output and unbinds the first.
        #[serde(default)]
        target_params: Option<PhysicsTargets>,
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
    /// Give an image to `node`, which has to be a part: a texture is added
    /// and assigned in one edit. If that part was the last thing drawing
    /// whatever it drew before, that texture goes with this edit — see
    /// [`ResponseBody::Texture`].
    ///
    /// The bytes arrive as attachment `texture`, and `encoding` says how to
    /// read them.
    TextureAdd {
        session: SessionId,
        node: NodeId,
        /// How to read the attached bytes. The field, never a sniff.
        #[serde(default)]
        encoding: TextureEncoding,
        /// The Id to create it under. Absent generates one; an Id the model
        /// already carries is [`ErrorCode::DuplicateId`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        texture: Option<TexId>,
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
        /// The Id to create it under. Absent generates one; an Id the model
        /// already carries is [`ErrorCode::DuplicateId`].
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
        target: ScalarTarget,
    },
    /// Author one scalar keypoint (auto-creates the binding).
    BindingKey {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: ScalarTarget,
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
    /// Un-author a keypoint (back to derived).
    BindingUnset {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        cell: [u32; 2],
    },
    /// Author the identity value at a keypoint.
    BindingReset {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        cell: [u32; 2],
    },
    BindingDelete {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
    },
    BindingInterpolate {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        mode: Interpolate,
    },
    /// Negate every authored value.
    BindingInvert {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
    },
    /// Author the value evaluated at `from` into cell `to`.
    BindingCopyKey {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        target: BindingTarget,
        from: [u32; 2],
        to: [u32; 2],
    },
    /// Every binding on one node: what drives it, how it reads between its
    /// cells, and the grid the author keyed.
    ///
    /// [`ParamInfo::bindings`] counts what a param drives and names none of
    /// it, so this is the read a binding panel is drawn from.
    BindingList {
        session: SessionId,
        node: NodeId,
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
    /// Author per-vertex deform offsets, one `[dx, dy]` per mesh vertex and in
    /// the mesh's own order. This is what commits a live drag.
    DeformVertices {
        session: SessionId,
        #[serde(flatten)]
        params: BindingParams,
        node: NodeId,
        cell: [u32; 2],
        offsets: Vec<[f32; 2]>,
    },
    /// Replace a Part/MeshGroup mesh; every deform binding on the node is
    /// re-fitted onto the new topology in the same undoable step. Answers
    /// with the slots the new mesh emptied.
    MeshSet {
        session: SessionId,
        node: NodeId,
        /// One `[x, y]` per vertex.
        verts: Vec<[f32; 2]>,
        /// One `[u, v]` per vertex, as many as `verts`.
        uvs: Vec<[f32; 2]>,
        /// One `[a, b, c]` per triangle, each a `verts` index.
        indices: Vec<[u32; 3]>,
        origin: [f32; 2],
    },
    /// Derive a part's mesh from its own texture's alpha and apply it, with
    /// the same deform re-fit and the same emptied-slot reply as
    /// [`Command::MeshSet`].
    ///
    /// The editor does the tracing: it holds the texture bytes, and a client
    /// that traced them itself would have to decode an image, agree on the
    /// UV mapping, and send back a mesh — three chances to disagree with the
    /// editor about what the part looks like.
    MeshAuto {
        session: SessionId,
        node: NodeId,
        /// Absent is [`AutoMesh::Contour`] with every knob at its default.
        #[serde(default)]
        mode: AutoMesh,
    },
    /// Copy `from`'s mesh onto `to` (with the same deform re-fit and the same
    /// emptied-slot reply).
    MeshCopy {
        session: SessionId,
        from: NodeId,
        to: NodeId,
    },
    /// Add a slot to a part. The slot lands unfilled and pairs nothing: a
    /// weld is what pairs it with a slot on another part.
    SlotAdd {
        session: SessionId,
        node: NodeId,
        /// Absent generates one (`slot-<8 hex>`, re-drawn until it is free on
        /// the part). The reply names it either way.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<SlotId>,
    },
    /// Point a slot at one of the part's vertices.
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
    /// No other part's slots move.
    SlotDelete {
        session: SessionId,
        node: NodeId,
        slot: SlotId,
    },
    /// The slots a part carries, with what fills each.
    Slots {
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
    /// Move one pair's share of one weld's meeting point, leaving every other
    /// weight where it is — what a slider sends. [`Command::WeldSet`] can only
    /// rewrite a weld whole, so moving one weight through it means reading the
    /// rest back and sending them again unchanged.
    ///
    /// `slot` is a slot on `a`. `weight` is the share of the part named `a`,
    /// whichever way round the weld happens to be stored, and it has to be
    /// within `0..=1` — a share outside that has no meaning to flip.
    WeldWeight {
        session: SessionId,
        a: NodeId,
        b: NodeId,
        slot: SlotId,
        weight: f32,
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
    /// Unmake the weld pairing two parts, named in either order. Both parts
    /// and every slot on them stay; only the pairing goes.
    /// [`ErrorCode::UnknownWeld`] if nothing pairs them.
    WeldDelete {
        session: SessionId,
        a: NodeId,
        b: NodeId,
    },
    /// Add a SimplePhysics node.
    PhysicsAdd {
        session: SessionId,
        parent: NodeId,
        #[serde(default)]
        name: Option<String>,
        kind: PhysicsKind,
        /// The params the driver writes ([`PhysicsTargets`]). Absent binds
        /// neither output.
        #[serde(default)]
        target_params: PhysicsTargets,
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
        /// The Id to create it under. Absent generates one; an Id the model
        /// already carries is [`ErrorCode::DuplicateId`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<NodeId>,
    },
    Undo {
        session: SessionId,
    },
    Redo {
        session: SessionId,
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
    /// Show a deform on the session's puppet without authoring it: the live
    /// half of a vertex drag. A [`CommandKind::Scratch`], so a drag of any
    /// length produces no revision and no undo entry; committing it is
    /// [`Command::DeformVertices`], which produces exactly one.
    ///
    /// `offsets` is one `[dx, dy]` per mesh vertex, in the mesh's own order;
    /// an empty list clears the scratch deform. It is dropped the next time
    /// the model changes, because the puppet rebakes.
    ScratchDeform {
        session: SessionId,
        node: NodeId,
        offsets: Vec<[f32; 2]>,
    },
    /// Render one frame of a session and answer with the PNG as the reply's
    /// payload.
    ///
    /// The camera is explicit: absent it is the default height at the origin,
    /// and what a tab last looked at is never consulted, so a script's output
    /// does not depend on anyone else's view.
    Preview {
        session: SessionId,
        #[serde(default)]
        pose: Vec<ParamPose>,
        #[serde(default)]
        size: Option<[u32; 2]>,
        #[serde(default)]
        camera: Option<Camera>,
    },
    /// Import a `.clm` — complete or fragment, textures inside — from
    /// attachment `model` into an open session.
    ///
    /// `parent` absent replaces the session's whole model, and needs a
    /// pristine one (what [`Command::SessionNew`] makes) and a complete
    /// model: anything else is [`ErrorCode::NotEmpty`]. `parent` present
    /// installs the imported roots under that node, overriding whatever
    /// parent those roots name; Ids travel verbatim, and a
    /// collision or a missing requirement is refused whole.
    ImportFile {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<NodeId>,
    },
    /// Import a `.clm` **structure as JSON**, with its textures
    /// attached separately, into an open session.
    ///
    /// The same operation as [`Command::ImportFile`] and the same two paths —
    /// `parent` absent replaces a pristine session's model, `parent` present
    /// installs the imported roots under that node — differing only in how
    /// the model arrives. A client that is authoring a model rather than
    /// forwarding a file has the structure in hand and the images beside it,
    /// and this saves it building a container.
    ///
    /// Attachment `structure` is the structure as JSON, spelled exactly as
    /// the `.clm` format's serde spells it. Each entry of `textures` names an
    /// image arriving as `texture:<texture>`; an entry with no attachment,
    /// and a `texture:` attachment no entry names, are both
    /// [`ErrorCode::BadRequest`] naming the id. A texture the structure
    /// *references* but `textures` does not list is the reader's own refusal,
    /// the same one a `.clm` missing a payload gets.
    ///
    /// **A JSON import carries no extension bytes.** A byte extension is a
    /// `{size, hash}` marker in the structure and its payload lives in a
    /// section JSON has no room for, so a marker here is refused by key.
    /// Import the structure, then set the extension.
    ImportJson {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<NodeId>,
        /// Every texture the attachments carry, in any order.
        #[serde(default)]
        textures: Vec<ImportTexture>,
    },
    /// Set the extension filed under `key`, replacing whatever was there.
    ///
    /// `Edit`: an extension is part of the model, so this moves the
    /// revision and undo covers it. `catchlight.` is the format's own prefix
    /// and is refused ([`ErrorCode::ReservedExtension`]); a byte value over
    /// the format's cap is [`ErrorCode::Edit`].
    ExtensionSet {
        session: SessionId,
        key: ExtensionKey,
        value: ExtensionSet,
    },
    /// Drop the extension filed under `key`.
    ///
    /// `Edit`, like [`Command::ExtensionSet`]. A key the model does not
    /// carry is [`ErrorCode::NoExtension`] rather than a quiet no-op, so a
    /// typo in a key says so.
    ExtensionDelete {
        session: SessionId,
        key: ExtensionKey,
    },
    /// Every extension the model carries, in key order.
    ///
    /// `ReplicaQuery`: a pure read, so a tab holding a replica answers it
    /// without asking. A byte value is reported as its size and hash, never
    /// its bytes — [`Command::ExtensionGet`] is what fetches those.
    Extensions {
        session: SessionId,
    },
    /// One extension's value.
    ///
    /// `ServerQuery`, because a byte value comes back as the reply's payload
    /// and a replica has no payload channel. A JSON value is inline in the
    /// reply either way.
    ExtensionGet {
        session: SessionId,
        key: ExtensionKey,
    },
    /// Build a model from a manifest and its images and replace the session's
    /// model with it.
    ///
    /// The manifest arrives as attachment `manifest`; each texture it names
    /// arrives as `texture:<ref>`, where `<ref>` is the path string the
    /// manifest spells, verbatim. A reference with no attachment is
    /// [`ErrorCode::Manifest`] naming it. The session has to be pristine, on
    /// the same rule as [`Command::ImportFile`].
    ImportManifest {
        session: SessionId,
    },
}

/// What applying a [`Command`] does to the model it addresses.
///
/// This is the fact the whole notification story hangs on, and it is written
/// down exactly once — in [`COMMAND_KINDS`]. `cargo xtask generate` splits the
/// TypeScript `Command` union on it, so a client picks its send method by
/// type rather than by remembering which calls are "quiet"; the editor server
/// asserts against it in debug builds (see `Editor::handle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// Changes the session's model, or which sessions exist. The session's
    /// `rev` moves — or its saved/open state does, which a title bar reads the
    /// same way — so every view has to re-read. The only kind that moves `rev`.
    Edit,
    /// Publishes shared view state: pose, camera, selection. It goes to the
    /// editor because other clients read it back, and it changes no model.
    Presence,
    /// Shows a live edit on a puppet without authoring it — the drag path.
    /// No revision, no undo entry, and nothing to read back: whoever owns the
    /// puppet being drawn serves it. A client with a local replica serves its
    /// own; a client on the socket gets the editor's.
    Scratch,
    /// A read that is a pure function of the model. A client holding a replica
    /// of the model answers it without a round trip; the editor answers it the
    /// same way, from the same bytes.
    ReplicaQuery,
    /// A read that needs the editor itself: its session bookkeeping, its
    /// store, or its renderer. A replica cannot answer one.
    ///
    /// `export_manifest` is here despite writing bytes: what it writes lands
    /// in the store, not in the session, and no view of the model changes.
    ServerQuery,
}

/// Every command's wire tag paired with its [`CommandKind`].
///
/// Hand-maintained, and held to the enum by two tests: `xtask generate` checks
/// this list against the variants each emitter sees, so a new command cannot
/// reach TypeScript or Python unclassified, and [`Command::kind`] resolves
/// through here, so a tag missing from the list panics the first time it is
/// dispatched.
pub const COMMAND_KINDS: &[(&str, CommandKind)] = &[
    ("session_new", CommandKind::Edit),
    ("session_open", CommandKind::Edit),
    ("session_list", CommandKind::ServerQuery),
    ("session_close", CommandKind::Edit),
    ("save", CommandKind::Edit),
    ("export_manifest", CommandKind::ServerQuery),
    ("status", CommandKind::ServerQuery),
    ("check", CommandKind::ReplicaQuery),
    ("node_tree", CommandKind::ReplicaQuery),
    ("node_info", CommandKind::ReplicaQuery),
    ("node_add", CommandKind::Edit),
    ("node_set", CommandKind::Edit),
    ("node_reparent", CommandKind::Edit),
    ("node_reorder", CommandKind::Edit),
    ("node_move", CommandKind::Edit),
    ("node_duplicate", CommandKind::Edit),
    ("rename_id", CommandKind::Edit),
    ("mask_add", CommandKind::Edit),
    ("mask_set", CommandKind::Edit),
    ("mask_reorder", CommandKind::Edit),
    ("mask_delete", CommandKind::Edit),
    ("physics_set", CommandKind::Edit),
    ("physics_globals", CommandKind::Edit),
    ("node_delete", CommandKind::Edit),
    ("texture_add", CommandKind::Edit),
    ("texture_list", CommandKind::ReplicaQuery),
    ("param_add", CommandKind::Edit),
    ("param_list", CommandKind::ReplicaQuery),
    ("param_set", CommandKind::Edit),
    ("param_delete", CommandKind::Edit),
    ("param_key_insert", CommandKind::Edit),
    ("param_key_delete", CommandKind::Edit),
    ("param_key_move", CommandKind::Edit),
    ("param_flip", CommandKind::Edit),
    ("binding_add", CommandKind::Edit),
    ("binding_key", CommandKind::Edit),
    ("binding_keys", CommandKind::Edit),
    ("binding_unset", CommandKind::Edit),
    ("binding_reset", CommandKind::Edit),
    ("binding_delete", CommandKind::Edit),
    ("binding_interpolate", CommandKind::Edit),
    ("binding_invert", CommandKind::Edit),
    ("binding_copy_key", CommandKind::Edit),
    ("binding_list", CommandKind::ReplicaQuery),
    ("deform_set", CommandKind::Edit),
    ("deform_vertices", CommandKind::Edit),
    ("mesh_set", CommandKind::Edit),
    ("mesh_auto", CommandKind::Edit),
    ("mesh_copy", CommandKind::Edit),
    ("slot_add", CommandKind::Edit),
    ("slot_fill", CommandKind::Edit),
    ("slot_clear", CommandKind::Edit),
    ("slot_delete", CommandKind::Edit),
    ("slots", CommandKind::ReplicaQuery),
    ("welds", CommandKind::ReplicaQuery),
    ("unfilled_slots", CommandKind::ReplicaQuery),
    ("weld_set", CommandKind::Edit),
    ("weld_weight", CommandKind::Edit),
    ("weld_delete", CommandKind::Edit),
    ("physics_add", CommandKind::Edit),
    ("undo", CommandKind::Edit),
    ("redo", CommandKind::Edit),
    ("presence_set", CommandKind::Presence),
    ("presence_get", CommandKind::ServerQuery),
    ("scratch_deform", CommandKind::Scratch),
    ("preview", CommandKind::ServerQuery),
    ("import_file", CommandKind::Edit),
    ("import_json", CommandKind::Edit),
    ("import_manifest", CommandKind::Edit),
    ("extension_set", CommandKind::Edit),
    ("extension_delete", CommandKind::Edit),
    ("extensions", CommandKind::ReplicaQuery),
    ("extension_get", CommandKind::ServerQuery),
];

/// One name a command's bytes travel under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attachment {
    /// Exactly this name, and the command needs it.
    Fixed(&'static str),
    /// Any name of the form `<family>:<suffix>`, as many as the command's own
    /// fields call for. What the suffixes have to be is the command's to
    /// check: `Family("texture")` admits `texture:body.png` and says nothing
    /// about whether the manifest asked for one.
    Family(&'static str),
    /// Exactly this name, and the command may or may not need it — its own
    /// fields decide. [`Command::ExtensionSet`] is the case: a `json` value
    /// travels inline and a `bytes` one is attached, so neither "always" nor
    /// "never" is the truth about the same command.
    Optional(&'static str),
}

impl Attachment {
    /// Does `name` belong to this attachment?
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

/// What bytes one command carries in, and whether its reply carries bytes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bytes {
    pub attachments: &'static [Attachment],
    pub payload: bool,
}

/// Every command that carries bytes, and which ones.
///
/// The companion to [`COMMAND_KINDS`], written down once and checked from both
/// ends the same way: `xtask generate` fails on a tag here that is not a
/// command, and emits the table to TypeScript and Python so a client knows
/// what to attach without a table of its own. A command absent from this list
/// carries no bytes at all.
pub const COMMAND_BYTES: &[(&str, Bytes)] = &[
    (
        "texture_add",
        Bytes {
            attachments: &[Attachment::Fixed("texture")],
            payload: false,
        },
    ),
    (
        "import_file",
        Bytes {
            attachments: &[Attachment::Fixed("model")],
            payload: false,
        },
    ),
    (
        "import_json",
        Bytes {
            attachments: &[
                Attachment::Fixed("structure"),
                Attachment::Family("texture"),
            ],
            payload: false,
        },
    ),
    (
        "import_manifest",
        Bytes {
            attachments: &[Attachment::Fixed("manifest"), Attachment::Family("texture")],
            payload: false,
        },
    ),
    (
        "preview",
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
    /// The `cmd` tag this command carries on the wire.
    ///
    /// Exhaustive by construction: a new variant does not compile until it is
    /// named here, which is what makes [`COMMAND_KINDS`] checkable.
    pub fn tag(&self) -> &'static str {
        match self {
            Command::SessionNew { .. } => "session_new",
            Command::SessionOpen { .. } => "session_open",
            Command::SessionList => "session_list",
            Command::SessionClose { .. } => "session_close",
            Command::Save { .. } => "save",
            Command::ExportManifest { .. } => "export_manifest",
            Command::Status { .. } => "status",
            Command::Check { .. } => "check",
            Command::NodeTree { .. } => "node_tree",
            Command::NodeInfo { .. } => "node_info",
            Command::NodeAdd { .. } => "node_add",
            Command::NodeSet { .. } => "node_set",
            Command::NodeReparent { .. } => "node_reparent",
            Command::NodeReorder { .. } => "node_reorder",
            Command::NodeMove { .. } => "node_move",
            Command::NodeDuplicate { .. } => "node_duplicate",
            Command::RenameId { .. } => "rename_id",
            Command::MaskAdd { .. } => "mask_add",
            Command::MaskSet { .. } => "mask_set",
            Command::MaskReorder { .. } => "mask_reorder",
            Command::MaskDelete { .. } => "mask_delete",
            Command::PhysicsSet { .. } => "physics_set",
            Command::PhysicsGlobals { .. } => "physics_globals",
            Command::NodeDelete { .. } => "node_delete",
            Command::TextureAdd { .. } => "texture_add",
            Command::TextureList { .. } => "texture_list",
            Command::ParamAdd { .. } => "param_add",
            Command::ParamList { .. } => "param_list",
            Command::ParamSet { .. } => "param_set",
            Command::ParamDelete { .. } => "param_delete",
            Command::ParamKeyInsert { .. } => "param_key_insert",
            Command::ParamKeyDelete { .. } => "param_key_delete",
            Command::ParamKeyMove { .. } => "param_key_move",
            Command::ParamFlip { .. } => "param_flip",
            Command::BindingAdd { .. } => "binding_add",
            Command::BindingKey { .. } => "binding_key",
            Command::BindingKeys { .. } => "binding_keys",
            Command::BindingUnset { .. } => "binding_unset",
            Command::BindingReset { .. } => "binding_reset",
            Command::BindingDelete { .. } => "binding_delete",
            Command::BindingInterpolate { .. } => "binding_interpolate",
            Command::BindingInvert { .. } => "binding_invert",
            Command::BindingCopyKey { .. } => "binding_copy_key",
            Command::BindingList { .. } => "binding_list",
            Command::DeformSet { .. } => "deform_set",
            Command::DeformVertices { .. } => "deform_vertices",
            Command::MeshSet { .. } => "mesh_set",
            Command::MeshAuto { .. } => "mesh_auto",
            Command::MeshCopy { .. } => "mesh_copy",
            Command::SlotAdd { .. } => "slot_add",
            Command::SlotFill { .. } => "slot_fill",
            Command::SlotClear { .. } => "slot_clear",
            Command::SlotDelete { .. } => "slot_delete",
            Command::Slots { .. } => "slots",
            Command::Welds { .. } => "welds",
            Command::UnfilledSlots { .. } => "unfilled_slots",
            Command::WeldWeight { .. } => "weld_weight",
            Command::WeldSet { .. } => "weld_set",
            Command::WeldDelete { .. } => "weld_delete",
            Command::PhysicsAdd { .. } => "physics_add",
            Command::Undo { .. } => "undo",
            Command::Redo { .. } => "redo",
            Command::PresenceSet { .. } => "presence_set",
            Command::PresenceGet { .. } => "presence_get",
            Command::ScratchDeform { .. } => "scratch_deform",
            Command::Preview { .. } => "preview",
            Command::ImportFile { .. } => "import_file",
            Command::ImportJson { .. } => "import_json",
            Command::ImportManifest { .. } => "import_manifest",
            Command::ExtensionSet { .. } => "extension_set",
            Command::ExtensionDelete { .. } => "extension_delete",
            Command::Extensions { .. } => "extensions",
            Command::ExtensionGet { .. } => "extension_get",
        }
    }

    /// What applying this command does to the session's model.
    ///
    /// A tag missing from [`COMMAND_KINDS`] reads as [`CommandKind::Edit`].
    /// That case is unreachable — `xtask generate` fails the build on it — but the
    /// fallback still has to be the conservative one: calling an edit a
    /// `Edit` costs a redundant redraw, while calling it any of the other
    /// four loses the notification entirely and leaves a panel showing stale
    /// data. It also sends the command to whoever owns the model, which is
    /// the only place an unknown command can be safely applied.
    pub fn kind(&self) -> CommandKind {
        let tag = self.tag();
        COMMAND_KINDS
            .iter()
            .find(|(name, _)| *name == tag)
            .map(|(_, kind)| *kind)
            .unwrap_or(CommandKind::Edit)
    }

    /// What bytes this command carries, or `None` when it carries none.
    ///
    /// Resolved through [`COMMAND_BYTES`] by tag, the same way
    /// [`Command::kind`] resolves through [`COMMAND_KINDS`].
    pub fn bytes(&self) -> Option<&'static Bytes> {
        let tag = self.tag();
        COMMAND_BYTES
            .iter()
            .find(|(name, _)| *name == tag)
            .map(|(_, bytes)| bytes)
    }

    /// What bytes *this* command carries, or `None` when it carries none.
    ///
    /// [`Command::bytes`] answers for the tag — what a command of this kind
    /// *may* carry. This answers for the one in hand, which differs only
    /// where an [`Attachment::Optional`] is declared: an `extension_set` of a
    /// JSON value carries nothing at all, and a transport that cannot carry
    /// bytes should take it rather than refuse it for a shape it does not
    /// have. Every other command carries exactly what it declares.
    pub fn carries_bytes(&self) -> Option<&'static Bytes> {
        let carrying = match self {
            Command::ExtensionSet { value, .. } => matches!(value, ExtensionSet::Bytes),
            _ => true,
        };
        self.bytes().filter(|_| carrying)
    }

    /// The session this command addresses, if it addresses one.
    ///
    /// `None` for the editor-level commands: the ones that make a session
    /// ([`Command::SessionNew`] and friends, which name it in their *reply*),
    /// and the ones that read the editor rather than a model.
    ///
    /// Exhaustive by construction, like [`Command::tag`] — a new variant does
    /// not compile until it says whether it names a session.
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Command::SessionNew { .. } | Command::SessionOpen { .. } | Command::SessionList => None,
            Command::SessionClose { session }
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
            | Command::NodeMove { session, .. }
            | Command::NodeDuplicate { session, .. }
            | Command::RenameId { session, .. }
            | Command::MaskAdd { session, .. }
            | Command::MaskSet { session, .. }
            | Command::MaskReorder { session, .. }
            | Command::MaskDelete { session, .. }
            | Command::PhysicsSet { session, .. }
            | Command::PhysicsGlobals { session, .. }
            | Command::NodeDelete { session, .. }
            | Command::TextureAdd { session, .. }
            | Command::TextureList { session }
            | Command::ParamAdd { session, .. }
            | Command::ParamList { session }
            | Command::ParamSet { session, .. }
            | Command::ParamDelete { session, .. }
            | Command::ParamKeyInsert { session, .. }
            | Command::ParamKeyDelete { session, .. }
            | Command::ParamKeyMove { session, .. }
            | Command::ParamFlip { session, .. }
            | Command::BindingAdd { session, .. }
            | Command::BindingKey { session, .. }
            | Command::BindingKeys { session, .. }
            | Command::BindingUnset { session, .. }
            | Command::BindingReset { session, .. }
            | Command::BindingDelete { session, .. }
            | Command::BindingInterpolate { session, .. }
            | Command::BindingInvert { session, .. }
            | Command::BindingCopyKey { session, .. }
            | Command::BindingList { session, .. }
            | Command::DeformSet { session, .. }
            | Command::DeformVertices { session, .. }
            | Command::MeshSet { session, .. }
            | Command::MeshAuto { session, .. }
            | Command::MeshCopy { session, .. }
            | Command::SlotAdd { session, .. }
            | Command::SlotFill { session, .. }
            | Command::SlotClear { session, .. }
            | Command::SlotDelete { session, .. }
            | Command::Slots { session, .. }
            | Command::Welds { session }
            | Command::UnfilledSlots { session }
            | Command::WeldWeight { session, .. }
            | Command::WeldSet { session, .. }
            | Command::WeldDelete { session, .. }
            | Command::PhysicsAdd { session, .. }
            | Command::Undo { session }
            | Command::Redo { session }
            | Command::PresenceSet { session, .. }
            | Command::PresenceGet { session }
            | Command::ScratchDeform { session, .. }
            | Command::Preview { session, .. }
            | Command::ImportFile { session, .. }
            | Command::ImportJson { session, .. }
            | Command::ImportManifest { session }
            | Command::ExtensionSet { session, .. }
            | Command::ExtensionDelete { session, .. }
            | Command::Extensions { session }
            | Command::ExtensionGet { session, .. } => Some(*session),
        }
    }
}

/// Which Id [`Command::RenameId`] changes, and to what.
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

/// The param, or the pair of params, a binding is keyed by. With `param_y`
/// the binding's grid spans both params' key positions and `cell` indexes
/// both; without it the grid is one row and `cell[1]` is 0.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingParams {
    pub param: ParamId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_y: Option<ParamId>,
}

/// The params a physics driver writes, one field per output.
///
/// A driver has exactly two outputs and never a third, so this is a struct
/// rather than a list: there is no way to name an output that does not exist,
/// and no positional hole to spell with a `null`. An absent field is an output
/// bound to nothing, so `{"length": "len"}` is a driver whose second output
/// drives a param and whose first drives none, and `{}` is a driver that
/// writes nothing.
///
/// The names are the outputs under the `angle_length` map mode. Another map
/// mode changes what each output *means* — `length_angle` swaps them, `xy` and
/// `yx` make them a position — never how many there are.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PhysicsTargets {
    /// The param the driver's first output writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<ParamId>,
    /// The param the driver's second output writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<ParamId>,
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

/// How [`Command::MeshAuto`] derives a mesh from a part's alpha.
///
/// **Every knob is optional, and none of the defaults are written here.** They
/// live in `catchlight-editor-core` beside the code that reads them, so the
/// wire cannot drift from what the editor actually does; absent means "what
/// the editor would have used". `{"mode": "contour"}` is the default trace.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AutoMesh {
    /// Trace the alpha contours: one pinned boundary loop per connected
    /// component, plus the free interior vertices `rings` and `spacing` ask
    /// for.
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
        /// One ring of free vertices per factor, the traced outline scaled
        /// about its own centroid: 0 is the centroid itself, 1 the outline.
        /// A factor above 1 is clamped to 1 — `margin` already dilates the
        /// mask before tracing, and a vertex outside the pinned loop would
        /// only make triangles the alpha cull drops. Empty places no rings,
        /// which is the default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rings: Option<Vec<f32>>,
        /// Texels: a free vertex closer than this to one already placed is
        /// dropped, so rings and fill do not crowd the outline. The pinned
        /// outline itself is never thinned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_distance: Option<f32>,
        /// Texel x of a vertical mirror line: free vertices are generated
        /// from `x <= mirror_x` and reflected across it, so a symmetric part
        /// gets a symmetric interior. The outline is traced from the alpha
        /// and is not mirrored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mirror_x: Option<f32>,
    },
    /// A grid over the bounding box of the solid texels.
    Grid {
        /// Alpha strictly above this counts as solid, and is what the
        /// bounding box is measured from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u32>,
        /// Grid lines as fractions of the solid bounding box — 0 its left
        /// edge, 1 its right — so the grid need not be uniform. Values
        /// outside `0..=1` put a line outside the box. Present, it replaces
        /// both `cols` and `margin` on this axis. A grid is mirrored by
        /// asking for symmetric fractions, which is why there is no mirror
        /// knob here.
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
    /// Contour, every knob the editor's own: what a client that asks for "an
    /// automesh" and nothing more gets.
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

/// One slot of a model, named in full: a [`SlotId`] is unique within its part,
/// so it travels beside the node carrying it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SlotAddr {
    pub node: NodeId,
    pub slot: SlotId,
}

/// One pair a weld holds: a slot on its `a` part, a slot on its `b` part, and
/// `a`'s share of the point the two vertices are pulled toward.
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
    Group,
    Part,
    Composite,
    MeshGroup,
}

/// What a node is, as a reply reports it. [`NodeKindArg`] is the add side and
/// carries no `Physics`: [`Command::PhysicsAdd`] is what makes one.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum NodeKind {
    Group,
    Part,
    Composite,
    MeshGroup,
    Physics,
}

impl NodeKind {
    /// The word this travels under.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Part => "part",
            Self::Composite => "composite",
            Self::MeshGroup => "mesh_group",
            Self::Physics => "physics",
        }
    }

    /// The kind of a node the model holds.
    pub fn of(kind: &catchlight_core::ModelNodeKind) -> Self {
        use catchlight_core::ModelNodeKind as K;
        match kind {
            K::Group => Self::Group,
            K::Part(_) => Self::Part,
            K::Composite(_) => Self::Composite,
            K::MeshGroup(_) => Self::MeshGroup,
            K::SimplePhysics(_) => Self::Physics,
        }
    }
}

/// What a mask source does to the drawable it is attached to.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum MaskMode {
    Mask,
    Dodge,
}

impl MaskMode {
    /// The word this travels under.
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

/// The pendulum a physics driver swings.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum PhysicsKind {
    Rigid,
    Spring,
}

impl PhysicsKind {
    /// The word this travels under.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Rigid => "rigid",
            Self::Spring => "spring",
        }
    }
}

impl From<PhysicsKind> for catchlight_core::physics::PendulumKind {
    fn from(kind: PhysicsKind) -> Self {
        match kind {
            PhysicsKind::Rigid => Self::RigidPendulum,
            PhysicsKind::Spring => Self::SpringPendulum,
        }
    }
}

impl From<catchlight_core::physics::PendulumKind> for PhysicsKind {
    fn from(kind: catchlight_core::physics::PendulumKind) -> Self {
        match kind {
            catchlight_core::physics::PendulumKind::RigidPendulum => Self::Rigid,
            catchlight_core::physics::PendulumKind::SpringPendulum => Self::Spring,
        }
    }
}

/// What a physics driver's two outputs mean; see [`PhysicsTargets`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum PhysicsMapMode {
    Xy,
    Yx,
    AngleLength,
    LengthAngle,
}

impl PhysicsMapMode {
    /// The word this travels under.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Xy => "xy",
            Self::Yx => "yx",
            Self::AngleLength => "angle_length",
            Self::LengthAngle => "length_angle",
        }
    }
}

impl From<PhysicsMapMode> for catchlight_core::physics::PhysicsParamMapMode {
    fn from(mode: PhysicsMapMode) -> Self {
        match mode {
            PhysicsMapMode::Xy => Self::XY,
            PhysicsMapMode::Yx => Self::YX,
            PhysicsMapMode::AngleLength => Self::AngleLength,
            PhysicsMapMode::LengthAngle => Self::LengthAngle,
        }
    }
}

impl From<catchlight_core::physics::PhysicsParamMapMode> for PhysicsMapMode {
    fn from(mode: catchlight_core::physics::PhysicsParamMapMode) -> Self {
        use catchlight_core::physics::PhysicsParamMapMode as M;
        match mode {
            M::XY => Self::Xy,
            M::YX => Self::Yx,
            M::AngleLength => Self::AngleLength,
            M::LengthAngle => Self::LengthAngle,
        }
    }
}

/// How a binding reads between the cells its author keyed.
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
    /// The word this travels under.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Nearest => "nearest",
            Self::Stepped => "stepped",
            Self::Linear => "linear",
            Self::Cubic => "cubic",
        }
    }

    /// Every mode, for a picker that offers them all.
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

/// How to read a texture's bytes. The field a command carries beside its
/// image, so nothing has to guess from a file name.
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

/// What a texture's stored bytes mean by their alpha channel.
///
/// The bytes cannot say, so the command carrying them does. Straight is the
/// default because it is what an editor writes and what every PNG a person
/// exports holds; `.inx` textures are the premultiplied ones.
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

/// How a drawable composites onto what is already under it.
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
    /// The word this travels under. Snake_case, like everything else on this
    /// wire — the model file spells the same modes in PascalCase.
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

    /// Every mode, for a picker that offers them all.
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
///
/// The spellings are the model file's own, which is why the colour and
/// output-scale channels run their words together rather than reading as
/// snake_case.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ScalarTarget {
    Tx,
    Ty,
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
    #[serde(rename = "outputscalex")]
    OutputScaleX,
    #[serde(rename = "outputscaley")]
    OutputScaleY,
}

/// Any property a binding drives: one of [`ScalarTarget`]'s scalars, or the
/// per-vertex deform, which the deform commands author instead.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum BindingTarget {
    Tx,
    Ty,
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
    #[serde(rename = "outputscalex")]
    OutputScaleX,
    #[serde(rename = "outputscaley")]
    OutputScaleY,
    Deform,
}

impl ScalarTarget {
    /// The word this travels under — the model's own, which is why it is
    /// read out of the core table rather than spelled again here.
    pub fn wire_name(self) -> &'static str {
        catchlight_core::ScalarTarget::from(self).name()
    }
}

impl BindingTarget {
    /// The word this travels under, under the same rule.
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
            /// The scalar this names, or `None` for the deform — which is
            /// what the commands that take only a scalar are asking.
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
    OutputScaleX,
    OutputScaleY,
);

/// Every closed set prints as the word it travels under, so a tool shows a
/// value its user could type straight back.
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
    PhysicsKind,
    PhysicsMapMode,
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

/// Fields to change on a node; every field is optional (absent = unchanged).
/// Kind-specific fields are ignored on nodes of another kind: the colour fields
/// (`opacity`, `blend_mode`, `tint`, `screen_tint`) reach parts and composites
/// only, because a mesh group is never drawn and carries no colour.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// The texture the part draws, in three states: absent leaves it as it
    /// is, `null` draws none, an Id draws that one. Ignored on a node that is
    /// not a part. Dropping the last part drawing a texture takes the texture
    /// with it — see [`ResponseBody::Node::dropped`].
    #[serde(
        default,
        deserialize_with = "present_as_some",
        skip_serializing_if = "Option::is_none"
    )]
    // ts-rs would spell the double option `TexId | null | null`; the shape a
    // client reads is the same one a plain `Option` has, `?` for absent and
    // `null` for "draw none".
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

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingKeyEntry {
    pub target: ScalarTarget,
    pub value: f32,
}

/// One param at one value — a pose is a list of these. Params are scalar, so
/// there is one number.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ParamPose {
    pub param: ParamId,
    pub value: f32,
}

/// One texture an [`Command::ImportJson`] structure names, and how to read the
/// bytes that came with it.
///
/// The bytes arrive as attachment `texture:<texture>`. Encoding and alpha are
/// fields rather than a sniff or a file-name tail, because a JSON structure
/// names no files: the client that has the image is the only thing that knows
/// what it holds.
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
        /// The addressed session's revision *after* this command, so a client
        /// knows what its reply describes without a second round trip.
        ///
        /// Present whenever the command named a session — including one it
        /// only read, and including the create/open/import commands, which
        /// report the revision of the session they just made. Absent for the
        /// editor-level commands that name no session at all, and for a
        /// `session_close`, whose session is gone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rev: Option<u64>,
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
/// waiting on unfilled slots, a mesh editor offering to refill a slot —
/// branches on this rather than on the message text.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// A target that does not fit the thing it names: a physics field on a
    /// node that is not a driver, a `deform` where only a scalar can go. A
    /// *misspelled* one never gets this far — it is [`Self::BadRequest`].
    BadTarget,
    NothingToUndo,
    NothingToRedo,
    /// A save with no path, on a session that has no file of its own.
    NoSavePath,
    /// The part carries no such slot.
    UnknownSlot,
    /// An add named an Id the model already carries.
    DuplicateId,
    /// The part already carries a slot with that Id.
    DuplicateSlot,
    /// A weld pairs a slot the part on that side does not carry.
    WeldUnknownSlot,
    /// A weld pairs one slot twice.
    WeldSlotPairedTwice,
    /// A weld's two ends are the same node, or one of them is not a part.
    BadWeldEnd,
    /// Those two parts are already welded; one weld records a part pair.
    DuplicateWeld,
    /// A weight is a share of a meeting point, so it must be within `0..=1`.
    WeldWeightOutOfRange,
    /// No weld pairs the two parts named.
    UnknownWeld,
    /// The texture carries no pixel above the alpha threshold, so there is no
    /// shape to mesh. A client offers a lower threshold rather than an error.
    NothingToMesh,
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
    /// A command carrying bytes was sent over a transport that cannot carry
    /// them; send it over one that can.
    BulkOverHttp,
    /// An import that would replace the whole model was asked of a session
    /// that already holds one; import into a fresh session instead.
    NotEmpty,
    /// The model carries no extension under that key.
    NoExtension,
    /// `catchlight.` is the format's own prefix: a reader accepts a key under
    /// it, and nothing outside the format may author one.
    ReservedExtension,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "result", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
    /// One slot, named in full: what [`Command::SlotAdd`] answers with, so a
    /// client that let the editor draw the Id learns which one it drew.
    Slot {
        slot: SlotAddr,
    },
    Params {
        params: Vec<ParamInfo>,
    },
    /// Every binding on one node, in the order the model holds them.
    Bindings {
        bindings: Vec<BindingInfo>,
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
    /// One node in full, as an inspector shows it. Boxed because it is far
    /// the largest thing a reply carries, and every other reply would pay for
    /// it in [`Reply`]'s size.
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
    /// Slots nothing fills, across the whole model.
    UnfilledSlots {
        slots: Vec<SlotAddr>,
    },
    /// The slots a mesh edit emptied on `node`, in the part's slot order.
    Emptied {
        node: NodeId,
        slots: Vec<SlotId>,
    },
    /// Every extension the model carries, in key order.
    Extensions {
        extensions: Vec<ExtensionInfo>,
    },
    /// One extension's value. For a byte value the bytes are the reply's
    /// payload; this says how big they are and what they hash to.
    Extension {
        key: ExtensionKey,
        value: ExtensionValueInfo,
    },
}

/// Ephemeral shared view state. Rides its own path — decoupled from the model
/// so scrubbing/panning generate zero edit traffic and never persist.
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

/// What [`Command::ExtensionSet`] is setting: a JSON value inline, or bytes
/// that arrive as the `value` attachment.
///
/// Tagged by `kind` rather than sniffed, because a byte extension's own
/// marker is a JSON object and an untagged value could not tell the two
/// apart. `kind: "bytes"` carries no value here — the bytes are attached —
/// and an attachment on a `json` set is refused, so exactly one of the two
/// says what the value is.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExtensionSet {
    Json {
        /// Whatever JSON the vendor wrote. Nothing here reads it.
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        value: serde_json::Value,
    },
    Bytes,
}

/// What an extension holds, as a reply reports it.
///
/// A JSON value travels whole; bytes travel as the size and hash their marker
/// carries, which is what a client compares to decide whether to fetch them.
/// The bytes themselves come back as [`Command::ExtensionGet`]'s payload.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum ExtensionValueInfo {
    Json {
        /// Whatever JSON the vendor wrote. Nothing here reads it.
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        value: serde_json::Value,
    },
    Bytes {
        size: u32,
        /// Lowercase hex of the content hash the marker carries.
        hash: String,
    },
}

/// One extension, as [`Command::Extensions`] lists it.
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
    /// The set of open sessions changed: one was created, opened, imported or
    /// closed. Carries nothing — an observer that cares re-reads
    /// [`Command::SessionList`], which is the only answer that is not already
    /// stale by the time it arrives.
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TexInfo {
    pub id: TexId,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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

/// One binding, as a panel draws it: the params driving it, the property it
/// drives, how it reads between cells, and the grid the author keyed.
///
/// **The grid is `[y][x]`** — the transpose of the `cell: [x, y]` every
/// binding command takes, so `keys[cell[1]][cell[0]]` is the cell
/// [`Command::BindingKey`] would write. It is the full product of the params'
/// key positions, [`Self::width`] by [`Self::height`], with one row when
/// there is no `param_y`.
///
/// **A `null` in `keys` is a cell nobody authored.** The model stores only the
/// cells a rigger set and derives the rest at puppet build, and "unset" is a
/// state they act on — it is not a zero. The exception is a `deform` binding,
/// which authors per-vertex offsets rather than one number: every one of its
/// cells reads `null` here and [`Self::authored`] is the only thing that says
/// which are set. For every other target `authored[y][x]` is exactly
/// `keys[y][x] != null`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BindingInfo {
    /// The property driven — plus `deform`, which only the deform commands
    /// author.
    pub target: BindingTarget,
    /// The param along the grid's x axis.
    pub param: ParamId,
    /// The param along the grid's y axis. Absent when the grid is one row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_y: Option<ParamId>,
    /// How it reads between cells, as [`Command::BindingInterpolate`] takes
    /// it back.
    pub interpolate: Interpolate,
    /// How many key positions `param` has, so how wide the grid is.
    pub width: u32,
    /// How many key positions `param_y` has, or 1.
    pub height: u32,
    /// The authored value at each cell, `[y][x]`, `null` where nothing was
    /// authored.
    pub keys: Vec<Vec<Option<f32>>>,
    /// Whether each cell was authored at all, `[y][x]`.
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

/// One weld: the two parts it joins, and the slot pairs between them.
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
    /// What the node is addressed by, here and in the file.
    pub id: NodeId,
    /// What a person reads. Free to repeat.
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

/// One node in full: what [`NodePatch`] can set, plus what it cannot — the
/// node's Id, its kind, its parent, and the size of the mesh it holds.
///
/// **The settable fields carry [`NodePatch`]'s own names.** An inspector reads
/// a value here, edits it, and sends it straight back in a
/// [`Command::NodeSet`] under the same key; a field that spelled itself
/// differently on the way out would be a rename nothing checks.
///
/// A field a node's kind does not carry is absent, the way it is ignored on
/// the way in: the colour fields reach parts and composites only, `texture`
/// only a part, `mg_*` only a mesh group, and the two mesh counts only the
/// kinds that hold a mesh. `texture` is also absent on a part that draws
/// none, which `kind` tells apart from a node that could not have one — a
/// reply has nothing to undo, so it never carries the `null` a patch spells
/// "draw none" with.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NodeInfo {
    /// What the node is addressed by, here and in the file.
    pub id: NodeId,
    /// The same kind [`TreeNode::kind`] carries.
    pub kind: NodeKind,
    /// Absent on the root, which is the one node with no parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<NodeId>,
    /// What a person reads. Free to repeat; nothing is addressed by it.
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
    /// The part's albedo texture, absent when it draws none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<TexId>,
    /// How many vertices the node's mesh holds. Absent on a kind that carries
    /// no mesh — a part and a mesh group carry one, nothing else does — so
    /// `0` is a mesh with no vertices rather than a node that could not have
    /// had any. A client asking "has this part been meshed" reads this rather
    /// than [`Command::Check`], whose textured-but-untriangulated warning is a
    /// message for a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_count: Option<u32>,
    /// How many triangles that mesh holds, absent under the same rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triangle_count: Option<u32>,
    /// Composite: forward mesh-group deformation to children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagate_meshgroup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mg_translate_children: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
/// What a [`Command::Preview`] rendered. The pixels are the reply's payload;
/// this says how to read them.
pub struct PreviewInfo {
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

    /// The byte table is what a client reads to know what to attach, so its
    /// tags have to be tags and its names have to match the way the server
    /// matches them.
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

    /// A code a client branches on has to survive the wire under the word it
    /// is written down as.
    #[test]
    fn the_new_refusals_travel_as_their_own_words() {
        for (code, word) in [
            (ErrorCode::BulkOverHttp, "\"bulk_over_http\""),
            (ErrorCode::NotEmpty, "\"not_empty\""),
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
                target: ScalarTarget::Tx,
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
                target: ScalarTarget::Tx,
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

    /// A [`Request`] flattens its command next to its own correlation `id`,
    /// so an add that spelled its chosen Id `id` would send two fields of that
    /// name and serde would refuse the whole request. Each add names the thing
    /// it makes instead, under the word the reply names it back with.
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

        // And an add that names nothing leaves the field off entirely, so the
        // common case is the shape it always was.
        let line = serde_json::to_string(&Request {
            id: 8,
            command: Command::ParamAdd {
                session: SessionId(1),
                name: "Pull".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                key_positions: Vec::new(),
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
        // ...but the correlation id is still readable, so the server can
        // answer the request the client is actually waiting on.
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

    /// An editor-level command answers with no revision at all, rather than
    /// with a zero a client could mistake for a fresh session.
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

    /// The kind table is what a client routes by, so the two reads that a
    /// replica cannot answer must not drift into `ReplicaQuery`.
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
        assert_eq!(
            Command::ScratchDeform {
                session,
                node: node("hair"),
                offsets: Vec::new(),
            }
            .kind(),
            CommandKind::Scratch
        );
    }

    /// `Reply::rev` is filled from here, so a session-addressing command that
    /// forgot to report one would answer with a revision of `None`.
    #[test]
    fn a_command_names_the_session_it_addresses() {
        assert_eq!(
            Command::Undo {
                session: SessionId(5)
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

    /// The inspector's whole round trip: read a node, change one number, send
    /// it back as a `node_set`. That only works while [`NodeInfo`] spells its
    /// settable fields exactly as [`NodePatch`] reads them, so the reply is
    /// parsed *as* a patch here — a field renamed on one side and not the
    /// other stops arriving, and this test says which.
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
        // A reply naming a texture parses as the patch state that points at
        // it — a `Some(Some(..))`, not the `Some(None)` that would clear it.
        assert_eq!(
            patch
                .texture
                .as_ref()
                .map(|t| t.as_ref().map(TexId::as_str)),
            Some(Some("tex-1")),
        );
        // The mesh counts are a read, not a setting: they ride the reply and
        // a patch parsed from it simply has nowhere to put them.
        assert!(line.contains("\"vertex_count\":4"), "{line}");
        assert!(line.contains("\"triangle_count\":2"), "{line}");
        // A field this node's kind does not carry stays off the wire, so
        // sending the reply back sets nothing it should not.
        assert!(!line.contains("mg_translate_children"), "{line}");
        assert_eq!(patch.mg_translate_children, None);
        assert_eq!(patch.propagate_meshgroup, None);
    }

    /// A node's Id, kind and parent are not things a patch can set, so they
    /// have to be on the reply itself rather than left for a second read.
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
        // An unmapped part says so by carrying no texture at all.
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

    /// Every closed set has two tables — serde's, which the wire is parsed
    /// and written by, and `wire_name`, which a CLI or a panel prints. They
    /// have to be the same table, or a tool would show a word its user cannot
    /// type back.
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
            NodeKind::Physics,
        ]);
        same(&[MaskMode::Mask, MaskMode::Dodge]);
        same(&[PhysicsKind::Rigid, PhysicsKind::Spring]);
        same(&[
            PhysicsMapMode::Xy,
            PhysicsMapMode::Yx,
            PhysicsMapMode::AngleLength,
            PhysicsMapMode::LengthAngle,
        ]);
        same(&Interpolate::ALL);
        same(&BlendMode::ALL);
        same(&SCALAR_TARGETS);
        same(&BINDING_TARGETS);
    }

    /// The scalar spellings are the model file's own, and a rigger types them
    /// by hand, so they are pinned here rather than left to `rename_all`.
    #[test]
    fn a_binding_target_travels_under_the_models_own_spelling() {
        let words: Vec<String> = SCALAR_TARGETS.iter().map(|t| t.to_string()).collect();
        assert_eq!(
            words,
            [
                "tx",
                "ty",
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
                "outputscalex",
                "outputscaley",
            ]
        );
        // `deform` is the one target no scalar command takes.
        assert_eq!(BindingTarget::Deform.to_string(), "deform");
        assert_eq!(BindingTarget::from(ScalarTarget::Rz), BindingTarget::Rz);
    }

    const SCALAR_TARGETS: [ScalarTarget; 17] = [
        ScalarTarget::Tx,
        ScalarTarget::Ty,
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
        ScalarTarget::OutputScaleX,
        ScalarTarget::OutputScaleY,
    ];

    const BINDING_TARGETS: [BindingTarget; 18] = [
        BindingTarget::Tx,
        BindingTarget::Ty,
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
        BindingTarget::OutputScaleX,
        BindingTarget::OutputScaleY,
        BindingTarget::Deform,
    ];

    /// A misspelled word in a closed set is a request that does not parse, so
    /// it is answered before any command runs rather than validated after.
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
