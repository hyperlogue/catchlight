
//! Wire types for the catchlight editor.
//!
//! A client sends a [`Request`] (one JSON object per line); the server answers
//! with a [`Reply`] — a correlated [`Reply::Ok`] / [`Reply::Err`], or an
//! unsolicited [`Reply::Event`]. The types are pure and transport-agnostic: the
//! same messages travel over a Unix socket (native) or a WebSocket (web).
//!
//! Handles ([`NodeRef`], [`ParamRef`], [`TexRef`]) are opaque per-session `u64`s
//! the server maps to its internal ids; clients only echo them back.

use serde::{Deserialize, Serialize};

macro_rules! opaque_id {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
        #[serde(transparent)]
        pub struct $name(pub u64);
    )*};
}

opaque_id! {
    /// One open editing session (one puppet).
    SessionId,
    NodeRef,
    ParamRef,
    TexRef,
}

/// A client request. `id` correlates the reply; commands that target a session
/// carry it in their own fields.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub command: Command,
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
        parent: NodeRef,
        kind: NodeKindArg,
        #[serde(default)]
        name: Option<String>,
    },
    NodeSet {
        session: SessionId,
        node: NodeRef,
        #[serde(flatten)]
        patch: NodePatch,
    },
    NodeReparent {
        session: SessionId,
        node: NodeRef,
        to: NodeRef,
    },
    /// Move a node within its parent's children (clamped to the end).
    NodeReorder {
        session: SessionId,
        node: NodeRef,
        index: u32,
    },
    /// Reparent + position in one undoable step: `node` becomes `parent`'s
    /// child at `index` (clamped).
    NodeMove {
        session: SessionId,
        node: NodeRef,
        parent: NodeRef,
        index: u32,
    },
    /// Deep-copy a node's subtree as its next sibling (bindings and
    /// subtree-internal mask references come along).
    NodeDuplicate {
        session: SessionId,
        node: NodeRef,
    },
    /// Append a mask source to a Part/Composite. `mode` is mask|dodge.
    MaskAdd {
        session: SessionId,
        node: NodeRef,
        source: NodeRef,
        mode: String,
    },
    /// Change the mode of the mask at `index`.
    MaskSet {
        session: SessionId,
        node: NodeRef,
        index: u32,
        mode: String,
    },
    /// Move the mask at `index` to position `to` (clamped).
    MaskReorder {
        session: SessionId,
        node: NodeRef,
        index: u32,
        to: u32,
    },
    MaskDelete {
        session: SessionId,
        node: NodeRef,
        index: u32,
    },
    /// Change fields on a SimplePhysics node; absent = unchanged.
    PhysicsSet {
        session: SessionId,
        node: NodeRef,
        /// rigid | spring
        #[serde(default)]
        model: Option<String>,
        /// xy | yx | angle_length | length_angle
        #[serde(default)]
        map_mode: Option<String>,
        #[serde(default)]
        local_only: Option<bool>,
        #[serde(default)]
        target_param: Option<ParamRef>,
        /// Detach the driven param (wins over `target_param`).
        #[serde(default)]
        clear_target_param: bool,
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
    /// Puppet-level physics constants.
    PhysicsGlobals {
        session: SessionId,
        #[serde(default)]
        gravity: Option<f32>,
        #[serde(default)]
        pixels_per_meter: Option<f32>,
    },
    NodeDelete {
        session: SessionId,
        node: NodeRef,
    },
    TextureAdd {
        session: SessionId,
        path: String,
    },
    TextureList {
        session: SessionId,
    },
    /// `axis_x` / `axis_y` are normalized 0..1 keypoints (empty = endpoints
    /// only).
    ParamAdd {
        session: SessionId,
        name: String,
        #[serde(default)]
        vec2: bool,
        #[serde(default)]
        min: [f32; 2],
        #[serde(default = "unit2")]
        max: [f32; 2],
        #[serde(default)]
        defaults: [f32; 2],
        #[serde(default)]
        axis_x: Vec<f32>,
        #[serde(default)]
        axis_y: Vec<f32>,
    },
    ParamList {
        session: SessionId,
    },
    /// Change param metadata; absent = unchanged. Axis points are normalized,
    /// so a range change doesn't move them.
    ParamSet {
        session: SessionId,
        param: ParamRef,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        min: Option<[f32; 2]>,
        #[serde(default)]
        max: Option<[f32; 2]>,
        #[serde(default)]
        defaults: Option<[f32; 2]>,
    },
    ParamDelete {
        session: SessionId,
        param: ParamRef,
    },
    /// Insert an axis point at normalized `value`, strictly inside (0, 1).
    /// Authored cells shift; the new row/column derives.
    ParamAxisInsert {
        session: SessionId,
        param: ParamRef,
        axis: u8,
        value: f32,
    },
    /// Remove an interior axis point; its authored cells are dropped.
    ParamAxisDelete {
        session: SessionId,
        param: ParamRef,
        axis: u8,
        index: u32,
    },
    /// Move an interior axis point to normalized `value` (must stay strictly
    /// between neighbors).
    ParamAxisMove {
        session: SessionId,
        param: ParamRef,
        axis: u8,
        index: u32,
        value: f32,
    },
    /// Mirror the param along an axis (axis points reflect, cells move to the
    /// mirrored index; values untouched — compose with BindingInvert).
    ParamFlip {
        session: SessionId,
        param: ParamRef,
        axis: u8,
    },
    BindingAdd {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        /// tx|ty|sx|sy|rx|ry|rz|zsort|opacity|tint{r,g,b}|screentint{r,g,b}|outputscale{x,y}
        target: String,
    },
    BindingKey {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        target: String,
        /// `[x, y]` index into the param's axis grid.
        cell: [u32; 2],
        value: f32,
    },
    /// Author several scalar keypoints at one cell in one undoable step (a
    /// gizmo drag commits tx+ty together).
    BindingKeys {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        cell: [u32; 2],
        entries: Vec<BindingKeyEntry>,
    },
    /// Un-author a keypoint (back to derived). `target` additionally accepts
    /// `deform`.
    BindingUnset {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        target: String,
        cell: [u32; 2],
    },
    /// Author the identity value at a keypoint.
    BindingReset {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        target: String,
        cell: [u32; 2],
    },
    BindingDelete {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        target: String,
    },
    /// nearest | stepped | linear | cubic
    BindingInterpolate {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        target: String,
        mode: String,
    },
    /// Negate every authored value.
    BindingInvert {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        target: String,
    },
    /// Author the value evaluated at `from` into cell `to`.
    BindingCopyKey {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        target: String,
        from: [u32; 2],
        to: [u32; 2],
    },
    /// Author a deform keypoint from an affine applied to the part's rest mesh.
    DeformSet {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        cell: [u32; 2],
        #[serde(default)]
        translate: Option<[f32; 2]>,
        #[serde(default)]
        rotate: Option<f32>,
        #[serde(default)]
        scale: Option<[f32; 2]>,
    },
    /// Author per-vertex deform offsets (`[dx, dy, …]`, matching the mesh).
    DeformVertices {
        session: SessionId,
        param: ParamRef,
        node: NodeRef,
        cell: [u32; 2],
        offsets: Vec<f32>,
    },
    /// Replace a Part/MeshGroup mesh; every deform binding on the node is
    /// re-fitted onto the new topology in the same undoable step.
    MeshApply {
        session: SessionId,
        node: NodeRef,
        verts: Vec<f32>,
        uvs: Vec<f32>,
        indices: Vec<u32>,
        origin: [f32; 2],
    },
    /// Copy `from`'s mesh onto `to` (with the same deform re-fit).
    MeshCopy {
        session: SessionId,
        from: NodeRef,
        to: NodeRef,
    },
    /// Add a SimplePhysics node. `model` is rigid|spring.
    PhysicsAdd {
        session: SessionId,
        parent: NodeRef,
        #[serde(default)]
        name: Option<String>,
        model: String,
        #[serde(default)]
        target_param: Option<ParamRef>,
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
    Preview {
        session: SessionId,
        #[serde(default)]
        params: Vec<ParamValue>,
        #[serde(default)]
        size: Option<[u32; 2]>,
        #[serde(default)]
        out: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKindArg {
    Empty,
    Part,
    Composite,
    MeshGroup,
}

/// Fields to change on a node; every field is optional (absent = unchanged).
/// Kind-specific fields are ignored on nodes of another kind.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
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
    pub zsort: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<TexRef>,
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

/// A param pose for preview: `y` is ignored for 1D params.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ParamValue {
    pub name: String,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
}

/// The server's answer. `Ok`/`Err` carry the request's `id`; `Event` is
/// unsolicited (a document changed on a session this client observes).
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    Ok { id: u64, body: ResponseBody },
    Err { id: u64, message: String },
    Event(Event),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ResponseBody {
    Empty,
    Session { session: SessionId },
    Sessions { sessions: Vec<SessionInfo> },
    Node { node: NodeRef },
    Param { param: ParamRef },
    Params { params: Vec<ParamInfo> },
    Texture { texture: TexRef },
    Textures { textures: Vec<TexInfo> },
    Tree { root: TreeNode },
    Status { status: StatusInfo },
    Warnings { warnings: Vec<String> },
    Preview { preview: PreviewInfo },
    Saved { path: String },
    Presence { presence: Option<Presence> },
}

/// Ephemeral shared view state. Rides its own path — decoupled from the document
/// so scrubbing/panning generate zero document traffic and never persist.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Presence {
    #[serde(default)]
    pub pose: Vec<ParamValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<NodeRef>,
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
    pub texture: TexRef,
    pub width: u32,
    pub height: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ParamInfo {
    pub param: ParamRef,
    pub name: String,
    pub vec2: bool,
    pub min: [f32; 2],
    pub max: [f32; 2],
    #[serde(default)]
    pub defaults: [f32; 2],
    /// Axis grid dimensions `[width, height]`.
    pub axis: [u32; 2],
    /// Keypoint positions along each axis, normalized 0..1 across
    /// `[min, max]`.
    #[serde(default)]
    pub axis_points_x: Vec<f32>,
    #[serde(default)]
    pub axis_points_y: Vec<f32>,
    pub bindings: u32,
}

fn unit2() -> [f32; 2] {
    [1.0, 1.0]
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TreeNode {
    pub node: NodeRef,
    pub name: String,
    pub kind: String,
    pub zsort: f32,
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

    #[test]
    fn request_tagging_roundtrips() {
        let req = Request {
            id: 7,
            command: Command::NodeAdd {
                session: SessionId(1),
                parent: NodeRef(42),
                kind: NodeKindArg::Part,
                name: Some("Body".into()),
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains("\"cmd\":\"node_add\""));
        assert!(line.contains("\"id\":7"));
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
}
