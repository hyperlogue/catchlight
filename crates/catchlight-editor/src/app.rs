//! The editor app: panels + viewport over the in-process [`Editor`] API — the
//! same dispatch the socket exposes, so an agent co-drives what the GUI shows.
//!
//! Fixed three-region layout: tree (+textures) | viewport | inspector (+params).
//! The viewport renders on eframe's own wgpu device into an egui texture,
//! re-rendered when the document revision, pose, camera or preview state
//! changes — no readback.
//!
//! Invariants this module carries:
//!
//! - **A drag rides the presence path; only the release edits the document.**
//!   A vertex drag sends [`Command::ScratchDeform`] per pointer move — the
//!   same command an out-of-process client sends, so there is one live-edit
//!   path and not a GUI-only copy of it — which shows the drag on the
//!   session's puppet without touching the model, its revision or its undo
//!   history. Releasing sends one [`Command::DeformVertices`], which is the
//!   only undo entry a gesture of any length produces. The gizmo's transform
//!   preview is the same split by other means: `NodePreview`s are re-applied
//!   through `Puppet::refold_with_node_edits` after every fold and commit as
//!   one `NodeSet`.
//!
//! - **Recording never authors a one-param binding beside a two-param one.**
//!   See [`App::record_target`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use catchlight_core::formats::clm::TextureEncoding;
use catchlight_core::{BindingKey, BindingTarget, Model, ModelNodeKind};
use catchlight_editor_protocol::{
    BindingKeyEntry, BindingParams, Command, NodeId, NodePatch, ParamId, ParamInfo, Rename, Reply,
    Request, ResponseBody, SeamAddr, SeamId, SeamInfo, SessionId, SlotAddr, SlotId, SlotInfo,
    SlotWeight, TexId, TreeNode, WeldInfo,
};
use catchlight_editor_server::Editor;
use eframe::egui;

use crate::camera::EditorCamera;
use crate::gizmo::{Gizmo, GizmoEvent, GizmoMode, GizmoTarget};
use crate::inspector::{
    inspector_ui, DrawableProps, InspectorAction, InspectorContext, InspectorData, InspectorKind,
    MaskRow,
};
use crate::io::{IoEvent, IoQueue};
use crate::mesh_edit::{MeshEditAction, MeshEditState, SeamAction, SeamView};
use crate::params_panel::{
    Armed, ArmedInfo, BindingAddr, BindingOp, BindingRow, ParamAction, ParamsPanel,
};
use crate::picking;
use crate::tree_panel::{TreeAction, TreePanel};
use crate::viewport::{NodePreview, ViewportRenderer};

mod selection;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

pub struct App {
    editor: Arc<Editor>,
    session: Option<SessionId>,
    title: String,
    /// Local preview pose, by param Id. Client-local view state.
    pose: HashMap<ParamId, f32>,
    status: String,
    io_queue: Arc<IoQueue>,
    /// Built lazily on the first frame, once eframe's wgpu device is in hand.
    viewport: Option<ViewportRenderer>,
    /// egui handle to the viewport's GPU texture (no CPU copy).
    texture_id: Option<egui::TextureId>,
    /// Signature the viewport texture was last rendered at.
    rendered: Option<RenderSig>,
    /// Document rev the last render evaluated. Pick, gizmo and vertex tools
    /// read the puppet's frame and only run when this matches the current rev:
    /// right after an edit the puppet has rebaked but nothing has ticked it,
    /// so its frame describes a pose nobody has recomputed yet.
    rendered_rev: u64,

    camera: EditorCamera,
    gizmo: Gizmo,
    /// The pan tool is active: primary drags pan the camera and clicks don't
    /// select. Mutually exclusive with the gizmo tools and `deform_mode`;
    /// every tool-switch site clears the others.
    pan_mode: bool,
    /// Ordered selection; the last entry is the primary (inspected) node.
    selection: Vec<NodeId>,
    collapsed: HashSet<NodeId>,
    filter: String,
    isolated: Option<NodeId>,
    /// Live gesture previews, applied to puppet working state each render.
    previews: Vec<NodePreview>,
    thumbs: HashMap<usize, egui::TextureHandle>,
    /// Parts under the cursor at the last right-click (the picker menu).
    ctx_hits: Vec<(NodeId, String)>,
    /// The viewport rect from the last frame — focus math needs its aspect.
    last_viewport_rect: Option<egui::Rect>,

    /// Recording state: the armed param, or the pair on the pad — edits to
    /// TRS/z order/opacity write binding keys at the closest keypoint instead
    /// of node state.
    armed: Option<Armed>,
    /// Snap pose drags to keypoints (the controller's default).
    snap: bool,
    /// Keypoint clipboard: the binding and cell the value was taken at.
    copied_cell: Option<(BindingParams, NodeId, String, [u32; 2])>,
    /// Per-vertex deform tool active (armed + Part selected).
    deform_mode: bool,
    deform_drag: Option<DeformDrag>,
    /// The node a live vertex drag is showing on the session's puppet. The
    /// offsets themselves live there, written by `Command::ScratchDeform`;
    /// this is only what has to be cleared when the gesture ends.
    scratch: Option<NodeId>,
    /// Moves whenever the scratch deform is rewritten or cleared. The puppet
    /// holds the drag, so nothing else in the render signature would notice.
    scratch_rev: u64,
    /// Deform sub-tool: single vertex, weighted brush, or lasso selection.
    deform_kind: DeformKind,
    /// The core node the lasso selection belongs to; a different node or a
    /// rebuilt mesh invalidates the indices.
    deform_sel_core: Option<u32>,
    brush_radius: f32,
    brush_flow: bool,
    deform_selection: HashSet<usize>,
    lasso_points: Vec<egui::Pos2>,
    /// Mesh edit mode — tool-local working mesh; the document changes on Apply.
    mesh_edit: Option<MeshEditState>,

    /// A PNG snapshot readback runs after the next render.
    snapshot_requested: bool,
    /// Autosave debounce: the rev last written, the last rev seen, and when it
    /// changed (egui time).
    autosave_rev: u64,
    last_rev_seen: u64,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    rev_changed_at: f64,
    /// A previous session's autosave waiting for the user's restore decision.
    pending_restore: Option<Vec<u8>>,
    /// An Id rename waiting to be confirmed. See [`IdRename`].
    id_rename: Option<IdRename>,
    /// An edit that would delete a texture, waiting to be confirmed. See
    /// [`TextureDrop`].
    texture_drop: Option<TextureDrop>,
    /// Slots a mesh edit in this session emptied. See [`App::commit_block`].
    emptied: Vec<SlotAddr>,
    /// `Model::check()` warnings, re-read when the revision moves.
    warnings: Option<(u64, Vec<String>)>,
    /// Rev-gated cache of the armed param's panel data.
    armed_cache: Option<(ArmedCacheKey, ArmedInfo)>,
}

/// (doc rev, armed params, armed cell) — the inputs ArmedInfo derives from.
type ArmedCacheKey = (u64, Armed, [u32; 2]);

/// An Id rename the author has asked for and not yet confirmed.
///
/// An Id is what an addon names this model's nodes and params by, and there
/// are no aliases: changing one breaks every addon that referenced it, exactly
/// as deleting it would. So it is never a side effect of relabelling — a Name
/// is edited in place, an Id only through this prompt.
struct IdRename {
    subject: RenameSubject,
    /// What the author is typing. Parsed against the Id charset on confirm.
    to: String,
    /// Why the last confirm was refused, if it was.
    error: Option<String>,
}

/// An edit the author has asked for that would delete a texture, held until
/// they confirm it.
///
/// Every texture a model carries is drawn by a part, so an edit that takes a
/// texture's last user takes the texture. Undo puts it back in this session;
/// nothing puts it back for an addon that named it by Id — the same cost
/// [`IdRename`] exists to state, so it is stated the same way.
struct TextureDrop {
    /// What the edit would delete, in the model's texture order.
    dropped: Vec<TexId>,
    edit: DroppingEdit,
}

/// An edit that can take a texture with it. `Send` covers everything the
/// server has a command for; an upload does not, because its bytes come from
/// the file picker rather than off the wire.
enum DroppingEdit {
    Send(Box<Command>),
    Upload {
        part: NodeId,
        encoding: TextureEncoding,
        bytes: Vec<u8>,
    },
}

/// What an [`IdRename`] renames, typed so the old Id is never re-parsed.
enum RenameSubject {
    Node(NodeId),
    Param(ParamId),
    Texture(TexId),
}

impl RenameSubject {
    fn kind(&self) -> &'static str {
        match self {
            Self::Node(_) => "node",
            Self::Param(_) => "param",
            Self::Texture(_) => "texture",
        }
    }

    fn from(&self) -> &str {
        match self {
            Self::Node(id) => id.as_str(),
            Self::Param(id) => id.as_str(),
            Self::Texture(id) => id.as_str(),
        }
    }
}

/// Everything the rendered viewport image depends on; a change re-renders.
#[derive(Clone, PartialEq)]
struct RenderSig {
    rev: u64,
    pose: Vec<(ParamId, f32)>,
    camera: EditorCamera,
    size: (u32, u32),
    isolated: Option<NodeId>,
    previews: Vec<NodePreview>,
    /// The puppet holds the live drag, so the signature tracks *that it
    /// moved*, not what it says.
    scratch_rev: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeformKind {
    Single,
    Brush,
    Lasso,
}

struct DeformDrag {
    core: u32,
    /// Captured vertices with their falloff weights.
    verts: Vec<(usize, f32)>,
    start_world: glam::Vec2,
    last_world: glam::Vec2,
    node_inv: glam::Mat4,
    flow: bool,
    /// Accumulated node-local deltas (flow) or the weighted total (drag).
    pending: HashMap<usize, glam::Vec2>,
}

impl App {
    pub fn new(editor: Arc<Editor>, ctx: egui::Context) -> Self {
        crate::theme::install(&ctx);
        Self {
            editor,
            session: None,
            title: "untitled".into(),
            pose: HashMap::new(),
            status: "no session — Import a manifest or Open a .clm".into(),
            io_queue: IoQueue::new(ctx),
            viewport: None,
            texture_id: None,
            rendered: None,
            rendered_rev: u64::MAX,
            camera: EditorCamera::default(),
            gizmo: Gizmo::default(),
            pan_mode: false,
            selection: Vec::new(),
            collapsed: HashSet::new(),
            filter: String::new(),
            isolated: None,
            previews: Vec::new(),
            thumbs: HashMap::new(),
            ctx_hits: Vec::new(),
            last_viewport_rect: None,
            armed: None,
            snap: true,
            copied_cell: None,
            deform_mode: false,
            deform_drag: None,
            scratch: None,
            scratch_rev: 0,
            deform_kind: DeformKind::Single,
            deform_sel_core: None,
            brush_radius: 60.0,
            brush_flow: false,
            deform_selection: HashSet::new(),
            lasso_points: Vec::new(),
            mesh_edit: None,
            snapshot_requested: false,
            autosave_rev: 0,
            last_rev_seen: 0,
            rev_changed_at: 0.0,
            pending_restore: None,
            texture_drop: None,
            id_rename: None,
            emptied: Vec::new(),
            warnings: None,
            armed_cache: None,
        }
    }

    /// Start with an already-open session (the native binary's CLI argument).
    pub fn with_session(
        editor: Arc<Editor>,
        ctx: egui::Context,
        session: SessionId,
        title: String,
    ) -> Self {
        let mut app = Self::new(editor, ctx);
        app.adopt_session(session, title);
        app
    }

    pub fn io_queue(&self) -> Arc<IoQueue> {
        self.io_queue.clone()
    }

    fn send(&mut self, command: Command) -> Reply {
        let reply = self.editor.handle(Request { id: 0, command });
        if let Reply::Err { message, .. } = &reply {
            self.status = format!("error: {message}");
        }
        reply
    }

    fn adopt_session(&mut self, session: SessionId, title: String) {
        // Before the session changes: the drag lives on the *old* session's
        // puppet, and nothing else would ever take it off.
        self.clear_scratch_deform();
        self.session = Some(session);
        self.title = title;
        self.pose.clear();
        self.armed = None;
        self.copied_cell = None;
        self.deform_mode = false;
        self.deform_drag = None;
        self.scratch = None;
        self.scratch_rev = 0;
        self.deform_selection.clear();
        self.lasso_points.clear();
        self.mesh_edit = None;
        self.snapshot_requested = false;
        self.autosave_rev = 0;
        self.last_rev_seen = 0;
        self.rendered = None;
        self.rendered_rev = u64::MAX;
        self.deform_sel_core = None;
        self.armed_cache = None;
        self.selection.clear();
        self.collapsed.clear();
        self.isolated = None;
        self.previews.clear();
        self.thumbs.clear();
        self.camera = EditorCamera::default();
        self.id_rename = None;
        // A held texture-drop confirm names the old session's model; letting
        // it survive would apply that edit to whatever loads next.
        self.texture_drop = None;
        self.emptied.clear();
        self.warnings = None;
        self.status = format!("session {}", session.0);
    }

    /// Send a session-opening command and adopt the new session.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_session(&mut self, command: Command, title: String) {
        if let Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } = self.send(command)
        {
            self.adopt_session(session, title);
        }
    }

    fn drain_io(&mut self) {
        for event in self.io_queue.drain() {
            match event {
                IoEvent::Opened { title, bytes } => match self.editor.open_bytes(&title, &bytes) {
                    Ok(session) => self.adopt_session(session, title),
                    Err(e) => self.status = format!("open: {e}"),
                },
                IoEvent::DemoLoaded { title, bytes } => {
                    // The demo fetch races user opens and the autosave
                    // restore; whatever the user opened wins.
                    if self.session.is_none() && self.pending_restore.is_none() {
                        match self.editor.open_bytes(&title, &bytes) {
                            Ok(session) => self.adopt_session(session, title),
                            Err(e) => self.status = format!("open: {e}"),
                        }
                    }
                }
                IoEvent::PickedTexture { bytes, is_tga } => {
                    // A texture goes to the part that draws it, so the picker
                    // is only reachable with a part selected — but the pick is
                    // async, and the selection can move while it is open.
                    match self.selected_part() {
                        Some(part) => {
                            let encoding = if is_tga {
                                TextureEncoding::Tga
                            } else {
                                TextureEncoding::Png
                            };
                            self.guard_texture_drop(DroppingEdit::Upload {
                                part,
                                encoding,
                                bytes,
                            });
                        }
                        None => self.status = "select a part to give the texture to".into(),
                    }
                }
                IoEvent::AutosaveFound { bytes } => {
                    // Only offer when nothing meaningful is open yet.
                    if self.session.is_none() || self.last_rev_seen == 0 {
                        self.pending_restore = Some(bytes);
                    }
                }
                IoEvent::Status(s) => self.status = s,
                IoEvent::Error(e) => self.status = format!("io: {e}"),
            }
        }
    }

    // ---- recording (armed param) ----

    /// Which of a param's key positions the current pose sits nearest — the
    /// index recording writes at along that param's axis.
    fn key_index(
        &self,
        snap: &catchlight_editor_server::DocSnapshot,
        param: &ParamId,
    ) -> Option<u32> {
        let info = snap.params.iter().find(|p| &p.id == param)?;
        let pose = self.pose.get(&info.id).copied().unwrap_or(info.default);
        // Key positions are normalized 0..1; map the pose into that space.
        let normed = if (info.max - info.min).abs() > f32::EPSILON {
            ((pose - info.min) / (info.max - info.min)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some(nearest_index(&info.key_positions, normed))
    }

    /// The binding the armed state names and the cell the pose lands on.
    /// One param is one row (`y` is 0); a pad is the product of both.
    fn armed_cell(
        &self,
        snap: &catchlight_editor_server::DocSnapshot,
    ) -> Option<(BindingParams, [u32; 2])> {
        let armed = self.armed.clone()?;
        let cx = self.key_index(snap, armed.x())?;
        match armed.y() {
            Some(y) => {
                let cy = self.key_index(snap, y)?;
                Some((BindingParams::two(armed.x().clone(), y.clone()), [cx, cy]))
            }
            None => Some((BindingParams::one(armed.x().clone()), [cx, 0])),
        }
    }

    /// The binding a recording on `node` writes into, and its cell.
    ///
    /// Arming one param normally authors a one-param binding. But an
    /// inochi2d 2-D param imports as two params driving *two-param* bindings,
    /// and a model may not hold a `One(p)` binding beside a `Two(p, q)` one —
    /// the v0 flatten refuses the pair as unpairable. So when this param is
    /// already half of a pair, recording joins that binding and fills the
    /// partner's axis from the current pose, which is the only place the pose
    /// exists: the server holds none.
    fn record_target(
        &self,
        snap: &catchlight_editor_server::DocSnapshot,
        node: &NodeId,
    ) -> Option<(BindingParams, [u32; 2])> {
        let (params, cell) = self.armed_cell(snap)?;
        if params.param_y.is_some() {
            return Some((params, cell));
        }
        let session = self.session?;
        let armed = params.param.clone();
        let node = node.clone();
        let pair = self
            .editor
            .with_model(session, |m| {
                let pair_of = |b: &catchlight_core::ModelBinding| match b.params() {
                    catchlight_core::BindingParams::Two(x, y) => Some((x.clone(), y.clone())),
                    catchlight_core::BindingParams::One(_) => None,
                };
                // This node's own pair first — a second pair elsewhere in the
                // model would be a worse guess for this binding.
                m.bindings_of_node(&node)
                    .filter(|b| b.params().contains(&armed))
                    .find_map(pair_of)
                    .or_else(|| m.bindings_of_param(&armed).find_map(pair_of))
            })
            .ok()
            .flatten();
        match pair {
            Some((x, y)) => {
                let cx = self.key_index(snap, &x)?;
                let cy = self.key_index(snap, &y)?;
                Some((BindingParams::two(x, y), [cx, cy]))
            }
            None => Some((params, cell)),
        }
    }

    /// Rebuild the armed panel data only when (rev, params, cell) moved — it
    /// walks every binding of the armed params, too heavy for every frame.
    fn refresh_armed_cache(&mut self, snap: &Arc<catchlight_editor_server::DocSnapshot>) {
        let Some(key) = self
            .armed
            .clone()
            .zip(self.armed_cell(snap))
            .map(|(armed, (_, cell))| (snap.rev, armed, cell))
        else {
            self.armed_cache = None;
            return;
        };
        if self.armed_cache.as_ref().map(|(k, _)| k) == Some(&key) {
            return;
        }
        self.armed_cache = self.armed_info(snap).map(|v| (key, v));
    }

    fn armed_info(&self, snap: &Arc<catchlight_editor_server::DocSnapshot>) -> Option<ArmedInfo> {
        let session = self.session?;
        let armed = self.armed.clone()?;
        let (_, cell) = self.armed_cell(snap)?;
        // The armed grid: this param's key positions, by the pad partner's if
        // there is one. A binding may span more than this — see below.
        let axis_len = |param: &ParamId| {
            snap.params
                .iter()
                .find(|p| &p.id == param)
                .map_or(0, |p| p.key_positions.len())
        };
        let w = axis_len(armed.x());
        let h = armed.y().map_or(1, axis_len);
        if w == 0 || h == 0 {
            return None;
        }
        // Every binding's cell has to be read in the armed grid, and the two
        // grids need not agree: a `One(x)` binding is constant along y, so it
        // authors every row of its column; a `Two(x, y)` binding armed on x
        // alone collapses onto the x axis.
        let cells_of = |axis: Option<u8>, c: (u32, u32), len: usize| -> Vec<u32> {
            match axis {
                Some(0) => vec![c.0],
                Some(_) => vec![c.1],
                None => (0..len as u32).collect(),
            }
        };
        let editor = self.editor.clone();
        let armed_for_model = armed.clone();
        let data = editor
            .with_model(session, |m| {
                for param in armed_for_model.iter() {
                    m.param(param)?;
                }
                let mut authored_count = vec![0u32; w * h];
                let mut rows = Vec::new();
                for b in m
                    .bindings()
                    .filter(|b| armed_for_model.iter().any(|p| b.params().contains(p)))
                {
                    let params = wire_params(b.params());
                    let ax = b.params().axis_of(armed_for_model.x());
                    let ay = armed_for_model.y().and_then(|y| b.params().axis_of(y));
                    // Where this binding's own grid the pose lands on.
                    let row_cell = [
                        self.key_index(snap, &params.param).unwrap_or(0),
                        params
                            .param_y
                            .as_ref()
                            .and_then(|y| self.key_index(snap, y))
                            .unwrap_or(0),
                    ];
                    let mut mark = |c: (u32, u32)| {
                        for x in cells_of(ax, c, w) {
                            for y in cells_of(ay, c, h) {
                                if (x as usize) < w && (y as usize) < h {
                                    if let Some(slot) =
                                        authored_count.get_mut(y as usize * w + x as usize)
                                    {
                                        *slot += 1;
                                    }
                                }
                            }
                        }
                    };
                    let mut authored_at = false;
                    if let Some(cells) = catchlight_core::scalar_cells(b.values()) {
                        for c in cells {
                            mark((c.x, c.y));
                            authored_at |= [c.x, c.y] == row_cell;
                        }
                    }
                    if let Some(cells) = catchlight_core::deform_cells(b.values()) {
                        for c in cells {
                            mark((c.x, c.y));
                            authored_at |= [c.x, c.y] == row_cell;
                        }
                    }
                    rows.push(BindingRow {
                        node: b.node().clone(),
                        node_name: m
                            .node(b.node())
                            .map(|n| n.name.to_string())
                            .unwrap_or_else(|| "?".into()),
                        target: b.target().name().to_string(),
                        interpolate: interp_name(b.interpolate_mode()).to_string(),
                        authored_at_cell: authored_at,
                        params,
                        cell: row_cell,
                    });
                }
                let total = rows.len() as u32;
                let states = authored_count
                    .into_iter()
                    .map(|n| {
                        if n == 0 {
                            0
                        } else if n < total {
                            1
                        } else {
                            2
                        }
                    })
                    .collect();
                Some((states, rows))
            })
            .ok()
            .flatten()?;
        Some(ArmedInfo {
            armed,
            cell,
            grid: (w, h),
            cell_states: data.0,
            bindings: data.1,
        })
    }

    /// Turn a node patch into binding-key entries at the armed cell: additive
    /// targets record `value - base`, multiplicative ones `value / base`.
    /// Turn a committed patch into binding-key entries at the armed cell.
    /// The gesture's delta — committed value minus the *posed* value the drag
    /// started from — lands on top of the cell's current key, so contributions
    /// from other params bound to the same target never leak into this one
    /// (the posed value is a sum/product over every binding).
    fn record_entries(
        &self,
        params: &BindingParams,
        cell: [u32; 2],
        node: &NodeId,
        patch: &NodePatch,
    ) -> Vec<BindingKeyEntry> {
        let Some(session) = self.session else {
            return Vec::new();
        };
        let Some(core) = self.core_of_ref(node) else {
            return Vec::new();
        };
        let editor = self.editor.clone();
        // The puppet's working state = the pose *without* the gesture
        // (previews are app-side overrides, never folded into the puppet
        // between renders).
        let Ok(Some((pt, pr, ps, pz, pop))) = editor.with_puppet(session, |_model, p| {
            p.get(catchlight_core::NodeIdx(core)).map(|n| {
                let op = match &n.kind {
                    catchlight_core::NodeKind::Part(part) => part.opacity,
                    catchlight_core::NodeKind::Composite(c) => c.opacity,
                    _ => 1.0,
                };
                (
                    n.transform.translation.to_array(),
                    n.transform.rotation.to_array(),
                    n.transform.scale.to_array(),
                    n.z_order,
                    op,
                )
            })
        }) else {
            return Vec::new();
        };
        use catchlight_core::ScalarTarget as T;
        let key_at = |t: T| {
            editor
                .with_model(session, |m| {
                    m.scalar_value_at(&binding_key(params, node, BindingTarget::Scalar(t)), cell)
                        .ok()
                })
                .ok()
                .flatten()
                .unwrap_or(t.identity())
        };
        let mut out = Vec::new();
        {
            let mut additive = |t: T, committed: f32, posed: f32| {
                out.push(BindingKeyEntry {
                    target: t.name().into(),
                    value: key_at(t) + (committed - posed),
                });
            };
            if let Some(tr) = patch.translate {
                additive(T::Tx, tr[0], pt[0]);
                additive(T::Ty, tr[1], pt[1]);
            }
            if let Some(r) = patch.rotate {
                additive(T::Rx, r[0], pr[0]);
                additive(T::Ry, r[1], pr[1]);
                additive(T::Rz, r[2], pr[2]);
            }
            if let Some(z) = patch.z_order {
                additive(T::ZOrder, z, pz);
            }
        }

        let mut multiplicative = |t: T, committed: f32, posed: f32| {
            let ratio = if posed.abs() < 1e-6 {
                1.0
            } else {
                committed / posed
            };
            out.push(BindingKeyEntry {
                target: t.name().into(),
                value: key_at(t) * ratio,
            });
        };
        if let Some(sc) = patch.scale {
            multiplicative(T::Sx, sc[0], ps[0]);
            multiplicative(T::Sy, sc[1], ps[1]);
        }
        if let Some(op) = patch.opacity {
            multiplicative(T::Opacity, op, pop);
        }
        out
    }

    /// Route a committed patch: recordable fields go to binding keys when
    /// armed; the rest stays a document NodeSet. When armed but the keypoint
    /// can't be resolved (the param vanished), the recordable fields are
    /// dropped rather than baked into the document as posed values.
    fn commit_patch(&mut self, node: NodeId, patch: NodePatch) {
        let Some(session) = self.session else { return };
        if self.armed.is_some() {
            let armed = self
                .editor
                .doc_snapshot(session)
                .and_then(|snap| self.record_target(&snap, &node));
            let rest = NodePatch {
                translate: None,
                rotate: None,
                scale: None,
                z_order: None,
                opacity: None,
                ..patch.clone()
            };
            let has_recordable = patch != rest;
            match armed {
                Some((params, cell)) if has_recordable => {
                    let entries = self.record_entries(&params, cell, &node, &patch);
                    if !entries.is_empty() {
                        self.send(Command::BindingKeys {
                            session,
                            params,
                            node: node.clone(),
                            cell,
                            entries,
                        });
                    }
                    if !patch_is_empty(&rest) {
                        self.send(Command::NodeSet {
                            session,
                            node,
                            patch: rest,
                        });
                    }
                    return;
                }
                None if has_recordable => {
                    self.armed = None;
                    self.deform_mode = false;
                    self.status =
                        "armed param has no keypoint — recording dropped (disarmed)".into();
                    if !patch_is_empty(&rest) {
                        self.send(Command::NodeSet {
                            session,
                            node,
                            patch: rest,
                        });
                    }
                    return;
                }
                _ => {}
            }
        }
        self.send(Command::NodeSet {
            session,
            node,
            patch,
        });
    }

    // ---- ids ----

    /// Open the Id-rename prompt. Nothing is sent until it is confirmed.
    fn begin_id_rename(&mut self, subject: RenameSubject) {
        self.id_rename = Some(IdRename {
            to: subject.from().to_string(),
            subject,
            error: None,
        });
    }

    /// Send the confirmed rename, and follow the thing that was renamed: the
    /// selection, the isolate filter and the armed param all name it by the
    /// Id that just stopped existing.
    fn confirm_id_rename(&mut self) {
        let Some(session) = self.session else { return };
        let Some(mut pending) = self.id_rename.take() else {
            return;
        };
        let rename = match &pending.subject {
            RenameSubject::Node(from) => NodeId::new(&pending.to).map(|to| Rename::Node {
                from: from.clone(),
                to,
            }),
            RenameSubject::Param(from) => ParamId::new(&pending.to).map(|to| Rename::Param {
                from: from.clone(),
                to,
            }),
            RenameSubject::Texture(from) => TexId::new(&pending.to).map(|to| Rename::Texture {
                from: from.clone(),
                to,
            }),
        };
        let rename = match rename {
            Ok(rename) => rename,
            Err(e) => {
                pending.error = Some(e.to_string());
                self.id_rename = Some(pending);
                return;
            }
        };
        if let Reply::Err { message, .. } = self.send(Command::RenameId {
            session,
            rename: rename.clone(),
        }) {
            pending.error = Some(message);
            self.id_rename = Some(pending);
            return;
        }
        self.follow_rename(&rename);
    }

    /// Re-point the client-side state that names what was just renamed.
    fn follow_rename(&mut self, rename: &Rename) {
        match rename {
            Rename::Node { from, to } => {
                for node in &mut self.selection {
                    if node == from {
                        *node = to.clone();
                    }
                }
                if self.isolated.as_ref() == Some(from) {
                    self.isolated = Some(to.clone());
                }
                if self.collapsed.remove(from) {
                    self.collapsed.insert(to.clone());
                }
                if let Some(mesh) = &mut self.mesh_edit {
                    if mesh.node == *from {
                        mesh.node = to.clone();
                    }
                }
                if let Some((_, node, _, _)) = &mut self.copied_cell {
                    if node == from {
                        *node = to.clone();
                    }
                }
            }
            Rename::Param { from, to } => {
                self.armed = self.armed.take().map(|armed| match armed {
                    Armed::One(p) => Armed::One(swap_param(p, from, to)),
                    Armed::Two(x, y) => {
                        Armed::Two(swap_param(x, from, to), swap_param(y, from, to))
                    }
                });
                if let Some(value) = self.pose.remove(from) {
                    self.pose.insert(to.clone(), value);
                }
                if let Some((params, _, _, _)) = &mut self.copied_cell {
                    params.param = swap_param(params.param.clone(), from, to);
                    params.param_y = params.param_y.take().map(|y| swap_param(y, from, to));
                }
                self.armed_cache = None;
            }
            Rename::Texture { .. } => {}
        }
    }

    /// The confirmation itself. Says what a rename costs before it happens,
    /// because nothing undoes it for an addon author downstream.
    fn id_rename_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.id_rename else {
            return;
        };
        let kind = pending.subject.kind();
        let from = pending.subject.from().to_string();
        let mut confirm = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("id-rename")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(format!("Rename {kind} Id"));
            ui.label(
                egui::RichText::new(
                    "An addon reaches into this model by Id, and there are no \
                     aliases — renaming one breaks every addon that named it, \
                     exactly as deleting it would. Names are the free ones.",
                )
                .weak(),
            );
            ui.add_space(6.0);
            ui.label(format!("from: {from}"));
            ui.horizontal(|ui| {
                ui.label("to:");
                let edit = ui
                    .add(egui::TextEdit::singleline(&mut pending.to).desired_width(f32::INFINITY));
                if edit.changed() {
                    pending.error = None;
                }
                confirm |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
            if let Some(error) = &pending.error {
                ui.colored_label(egui::Color32::from_rgb(240, 120, 120), error);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let unchanged = pending.to == from || pending.to.is_empty();
                confirm |= ui
                    .add_enabled(!unchanged, egui::Button::new("Rename — break addons"))
                    .clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if cancel {
            self.id_rename = None;
        } else if confirm {
            self.confirm_id_rename();
        }
    }

    // ---- textures ----

    /// The selected node, if it is a part. A texture goes to the part that
    /// draws it, so every texture command needs one.
    fn selected_part(&self) -> Option<NodeId> {
        let session = self.session?;
        let node = self.primary()?;
        self.editor
            .with_model(session, |m| {
                matches!(m.node(&node).map(|n| &n.kind), Some(ModelNodeKind::Part(_)))
            })
            .ok()?
            .then_some(node)
    }

    /// What `edit` would leave with no part drawing it. The model answers;
    /// this only asks the question the edit is about.
    fn textures_dropped_by(&self, edit: &DroppingEdit) -> Vec<TexId> {
        let Some(session) = self.session else {
            return Vec::new();
        };
        self.editor
            .with_model(session, |m| match edit {
                DroppingEdit::Send(command) => match command.as_ref() {
                    Command::NodeDelete { node, .. } => m.textures_dropped_by_deleting(node),
                    Command::NodeSet { node, patch, .. } => m
                        .texture_dropped_by_repointing(node, patch.texture.as_ref())
                        .into_iter()
                        .collect(),
                    // An upload displaces whatever the part drew before.
                    Command::TextureAdd { node, .. } => m
                        .texture_dropped_by_repointing(node, None)
                        .into_iter()
                        .collect(),
                    _ => Vec::new(),
                },
                DroppingEdit::Upload { part, .. } => m
                    .texture_dropped_by_repointing(part, None)
                    .into_iter()
                    .collect(),
            })
            .unwrap_or_default()
    }

    /// Run `edit`, or hold it until the author has seen what it deletes.
    fn guard_texture_drop(&mut self, edit: DroppingEdit) {
        let dropped = self.textures_dropped_by(&edit);
        if dropped.is_empty() {
            self.run_dropping_edit(edit);
        } else {
            self.texture_drop = Some(TextureDrop { dropped, edit });
        }
    }

    fn run_dropping_edit(&mut self, edit: DroppingEdit) {
        let Some(session) = self.session else { return };
        match edit {
            DroppingEdit::Send(command) => {
                self.send(*command);
            }
            DroppingEdit::Upload {
                part,
                encoding,
                bytes,
            } => match self
                .editor
                .add_texture_bytes(session, &part, encoding, bytes)
            {
                Ok(_) => self.status = "texture added".into(),
                Err(e) => self.status = format!("texture: {e}"),
            },
        }
    }

    /// The confirmation itself. Says what the edit deletes before it happens,
    /// because nothing undoes it for an addon author downstream.
    fn texture_drop_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.texture_drop else {
            return;
        };
        let dropped: Vec<String> = pending.dropped.iter().map(TexId::to_string).collect();
        let mut confirm = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::new("texture-drop")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading(if dropped.len() == 1 {
                "Delete a texture?".to_string()
            } else {
                format!("Delete {} textures?", dropped.len())
            });
            ui.label(
                egui::RichText::new(
                    "Every texture a model carries is drawn by a part. This edit \
                     leaves nothing drawing these, so they go with it — undo brings \
                     them back here, but an addon that named one by Id is broken for \
                     good.",
                )
                .weak(),
            );
            ui.add_space(6.0);
            for id in &dropped {
                ui.label(id.as_str());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                confirm |= ui.button("Delete and continue").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if cancel {
            self.texture_drop = None;
        } else if confirm {
            if let Some(pending) = self.texture_drop.take() {
                self.run_dropping_edit(pending.edit);
            }
        }
    }

    // ---- mesh edit mode ----

    fn enter_mesh_edit(&mut self) {
        let Some(session) = self.session else { return };
        let Some(node) = self.primary() else { return };
        let Some(core) = self.core_of_ref(&node) else {
            return;
        };
        let editor = self.editor.clone();
        let id = node.clone();
        let Ok(Some((mesh, tex_bytes))) = editor.with_model(session, |m| {
            let albedo = match m.node(&id).map(|n| &n.kind) {
                Some(ModelNodeKind::Part(p)) => p.albedo(),
                Some(ModelNodeKind::MeshGroup(_)) => None,
                _ => return None,
            };
            let mesh = m.node_mesh(&id)?.clone();
            let bytes = albedo.and_then(|t| m.texture(t)).map(|t| t.data.to_vec());
            Some((mesh, bytes))
        }) else {
            self.status = "mesh edit: node has no mesh".into();
            return;
        };
        let alpha = tex_bytes.and_then(|b| catchlight_editor_core::AlphaMask::decode(&b));
        let uv_map = catchlight_editor_core::UvMap::fit(&mesh.verts, &mesh.uvs)
            .or_else(|| {
                alpha.as_ref().map(|a| {
                    catchlight_editor_core::UvMap::from_texture_size(
                        a.width as f32,
                        a.height as f32,
                    )
                })
            })
            .unwrap_or_else(|| catchlight_editor_core::UvMap::from_texture_size(1024.0, 1024.0));
        let node_world = self.node_world(core);
        let working = catchlight_editor_core::WorkingMesh::from_mesh(&mesh);
        self.deform_mode = false;
        self.mesh_edit = Some(MeshEditState::new(
            node, core, working, uv_map, alpha, node_world,
        ));
    }

    /// Apply the working mesh to the document.
    ///
    /// The reply says which seam slots the new mesh emptied. Those are the
    /// author's to refill — a slot names a vertex of a mesh that no longer
    /// exists, and guessing a new one is exactly what naming vertices by slot
    /// exists to avoid — so the mode stays open on the seam tool instead of
    /// closing over the problem, and [`Self::commit_block`] keeps the model
    /// from being saved until they are refilled or their seam is deleted.
    fn apply_mesh_edit(&mut self) {
        let Some(session) = self.session else { return };
        let Some(mesh) = self.mesh_edit.take() else {
            return;
        };
        // Nothing edited since the working mesh last was the document's: there
        // is nothing to apply, and applying anyway would empty every seam slot
        // on the part a second time — including the ones just refilled.
        if mesh.matches_document() {
            return;
        }
        let new_mesh = match mesh.working.to_mesh(&mesh.uv_map, mesh.alpha.as_ref()) {
            Ok(new_mesh) => new_mesh,
            Err(e) => {
                self.status = format!("mesh apply: {e}");
                self.mesh_edit = Some(mesh);
                return;
            }
        };
        let indices = match &new_mesh.indices {
            catchlight_core::formats::clm::ClmIndices::U16(v) => {
                v.iter().map(|&i| i as u32).collect()
            }
            catchlight_core::formats::clm::ClmIndices::U32(v) => v.clone(),
        };
        let node = mesh.node.clone();
        let reply = self.send(Command::MeshSet {
            session,
            node: node.clone(),
            verts: new_mesh.verts,
            uvs: new_mesh.uvs,
            indices,
            origin: new_mesh.origin,
        });
        let emptied = match reply {
            Reply::Ok {
                body: ResponseBody::Emptied { node, slots },
                ..
            } => {
                for slot in &slots {
                    let addr = SlotAddr {
                        node: node.clone(),
                        seam: slot.seam.clone(),
                        slot: slot.slot.clone(),
                    };
                    if !self.emptied.contains(&addr) {
                        self.emptied.push(addr);
                    }
                }
                slots
                    .into_iter()
                    .map(|s| (s.seam, s.slot))
                    .collect::<Vec<_>>()
            }
            Reply::Err { .. } => {
                self.mesh_edit = Some(mesh);
                return;
            }
            _ => Vec::new(),
        };
        if emptied.is_empty() {
            return;
        }
        // Re-seat on what the document now holds: the slot the author is about
        // to fill has to name a vertex of *that* mesh.
        let editor = self.editor.clone();
        let Ok(Some(doc_mesh)) = editor.with_model(session, |m| m.node_mesh(&node).cloned()) else {
            return;
        };
        let mut mesh = mesh;
        mesh.reseat(
            catchlight_editor_core::WorkingMesh::from_mesh(&doc_mesh),
            emptied,
        );
        self.status = "the new mesh emptied this part's seam slots — refill them".into();
        self.mesh_edit = Some(mesh);
    }

    /// What the seam panel reads: this part's seams, the welds naming them,
    /// and every seam elsewhere a weld could reach.
    fn seam_view(&self) -> SeamView {
        let (Some(session), Some(node)) = (self.session, self.mesh_edit.as_ref().map(|m| &m.node))
        else {
            return SeamView::default();
        };
        let node = node.clone();
        self.editor
            .with_model(session, |m| {
                let seams = m
                    .seams(&node)
                    .map(|seams| seams.iter().map(seam_info).collect())
                    .unwrap_or_default();
                let welds = m
                    .welds()
                    .iter()
                    .map(|w| WeldInfo {
                        a: seam_addr(w.a()),
                        b: seam_addr(w.b()),
                        weights: w
                            .weights()
                            .iter()
                            .map(|(slot, weight)| SlotWeight {
                                slot: slot.clone(),
                                weight: *weight,
                            })
                            .collect(),
                    })
                    .collect();
                let mut others = Vec::new();
                for other in m.nodes_in_order() {
                    if other == node {
                        continue;
                    }
                    let name = m
                        .node(&other)
                        .map(|n| n.name.to_string())
                        .unwrap_or_default();
                    for seam in m.seams(&other).unwrap_or(&[]) {
                        others.push((other.clone(), name.clone(), seam.id().clone()));
                    }
                }
                SeamView {
                    seams,
                    welds,
                    others,
                }
            })
            .unwrap_or_default()
    }

    fn apply_mesh_action(&mut self, action: MeshEditAction) {
        match action {
            MeshEditAction::Apply => self.apply_mesh_edit(),
            MeshEditAction::Cancel => self.mesh_edit = None,
            MeshEditAction::CopyFrom(src) => self.mesh_copy_into_working(src),
            MeshEditAction::Seam(action) => self.apply_seam_action(action),
        }
    }

    /// A seam or weld edit, on the node the mesh editor is open on.
    fn apply_seam_action(&mut self, action: SeamAction) {
        let Some(session) = self.session else { return };
        let Some(node) = self.mesh_edit.as_ref().map(|m| m.node.clone()) else {
            return;
        };
        match action {
            SeamAction::AddSeam(seam) => {
                self.send(Command::SeamAdd {
                    session,
                    node,
                    seam,
                });
            }
            SeamAction::DeleteSeam(seam) => {
                self.send(Command::SeamDelete {
                    session,
                    node,
                    seam,
                });
            }
            SeamAction::AddSlot { seam, slot } => {
                self.send(Command::SlotAdd {
                    session,
                    node,
                    seam,
                    slot,
                });
            }
            SeamAction::DeleteSlot { seam, slot } => {
                self.send(Command::SlotDelete {
                    session,
                    node,
                    seam,
                    slot,
                });
            }
            SeamAction::ClearSlot { seam, slot } => {
                self.send(Command::SlotClear {
                    session,
                    node,
                    seam,
                    slot,
                });
            }
            SeamAction::FillSlot { seam, slot, vertex } => {
                self.send(Command::SlotFill {
                    session,
                    node,
                    seam,
                    slot,
                    vertex,
                });
            }
            SeamAction::Weld { seam, other } => {
                // No weights: every slot welds at DEFAULT_SLOT_WEIGHT, and
                // slot_add has already made the two slot sets one.
                self.send(Command::WeldSet {
                    session,
                    a: SeamAddr { node, seam },
                    b: other,
                    weights: Vec::new(),
                });
            }
            SeamAction::SetWeight {
                seam,
                other,
                slot,
                weight,
            } => self.set_slot_weight(session, node, seam, other, slot, weight),
            SeamAction::Undo => {
                self.send(Command::Undo { session });
            }
        }
    }

    /// `WeldSet` replaces the whole weld, so one slider carries every other
    /// slot's weight along unchanged.
    fn set_slot_weight(
        &mut self,
        session: SessionId,
        node: NodeId,
        seam: SeamId,
        other: SeamAddr,
        slot: SlotId,
        weight: f32,
    ) {
        let a = SeamAddr {
            node: node.clone(),
            seam: seam.clone(),
        };
        let editor = self.editor.clone();
        let (pa, pb) = (a.clone(), other.clone());
        let weights = editor
            .with_model(session, |m| {
                m.welds()
                    .iter()
                    .find(|w| {
                        let ends = [seam_addr(w.a()), seam_addr(w.b())];
                        ends.contains(&pa) && ends.contains(&pb)
                    })
                    .map(|w| {
                        w.weights()
                            .iter()
                            .map(|(s, v)| SlotWeight {
                                slot: s.clone(),
                                weight: if *s == slot { weight } else { *v },
                            })
                            .collect::<Vec<_>>()
                    })
            })
            .ok()
            .flatten()
            .unwrap_or_default();
        self.send(Command::WeldSet {
            session,
            a,
            b: other,
            weights,
        });
    }

    /// The slots this session's mesh edits emptied that nobody has refilled.
    ///
    /// A weld skips an unfilled slot, so a model saved in this state holds a
    /// weld that quietly no longer closes its seam. Refilling the slot or
    /// deleting the seam clears the entry; nothing else does, and the save
    /// paths refuse while the list is non-empty.
    fn commit_block(&self) -> Vec<SlotAddr> {
        let Some(session) = self.session else {
            return Vec::new();
        };
        if self.emptied.is_empty() {
            return Vec::new();
        }
        let pending = self.emptied.clone();
        self.editor
            .with_model(session, |m| {
                pending
                    .into_iter()
                    .filter(|addr| {
                        m.seam(&addr.node, &addr.seam)
                            .and_then(|s| s.slot(&addr.slot))
                            .is_some_and(|slot| slot.vertex().is_none())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Refuse a save while a re-meshed part still has an unfilled slot, and
    /// say what to do about it.
    fn blocked_from_saving(&mut self) -> bool {
        let blocked = self.commit_block();
        self.emptied = blocked.clone();
        if blocked.is_empty() {
            return false;
        }
        let first = blocked
            .first()
            .map(|a| format!("{} · {} · {}", a.node, a.seam, a.slot))
            .unwrap_or_default();
        self.status = format!(
            "cannot save: {} seam slot(s) emptied by a mesh edit are still \
             unfilled ({first}…) — refill them, or delete the seam",
            blocked.len()
        );
        true
    }

    /// Replace the *working* mesh with another node's topology (the document
    /// is untouched until Apply).
    fn mesh_copy_into_working(&mut self, src: NodeId) {
        let Some(session) = self.session else { return };
        let editor = self.editor.clone();
        let Ok(Some(src_mesh)) = editor.with_model(session, |m| m.node_mesh(&src).cloned()) else {
            return;
        };
        if let Some(mesh) = &mut self.mesh_edit {
            mesh.replace_working(catchlight_editor_core::WorkingMesh::from_mesh(&src_mesh));
        }
    }

    fn apply_param_action(
        &mut self,
        action: ParamAction,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
    ) {
        let Some(session) = self.session else { return };
        match action {
            ParamAction::Pose { param, value } => {
                self.pose.insert(param, value);
            }
            ParamAction::Arm(armed) => {
                if armed.is_none() {
                    self.deform_mode = false;
                }
                self.armed = armed;
            }
            ParamAction::AddParam { name } => {
                self.send(Command::ParamAdd {
                    session,
                    name,
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    key_positions: Vec::new(),
                });
            }
            ParamAction::AddParamPair { name } => {
                // Two scalars, opened on the pad. The pair is the pad's, and
                // through it the binding's: neither param knows about the
                // other, and either can be posed on its own track.
                let base = if name.is_empty() { "param" } else { &name };
                let pair: Vec<ParamId> = ["x", "y"]
                    .iter()
                    .filter_map(|axis| {
                        match self.send(Command::ParamAdd {
                            session,
                            name: format!("{base}.{axis}"),
                            min: -1.0,
                            max: 1.0,
                            default: 0.0,
                            key_positions: Vec::new(),
                        }) {
                            Reply::Ok {
                                body: ResponseBody::Param { param },
                                ..
                            } => Some(param),
                            _ => None,
                        }
                    })
                    .collect();
                if let [x, y] = &pair[..] {
                    self.armed = Some(Armed::Two(x.clone(), y.clone()));
                }
            }
            ParamAction::RenameId(param) => {
                self.begin_id_rename(RenameSubject::Param(param));
            }
            ParamAction::Rename { param, name } => {
                self.send(Command::ParamSet {
                    session,
                    param,
                    name: Some(name),
                    min: None,
                    max: None,
                    default: None,
                });
            }
            ParamAction::Delete(param) => {
                if self.armed.as_ref().is_some_and(|a| a.contains(&param)) {
                    self.armed = None;
                    self.deform_mode = false;
                }
                self.send(Command::ParamDelete { session, param });
            }
            ParamAction::KeyInsert { param, value } => {
                self.send(Command::ParamKeyInsert {
                    session,
                    param,
                    value,
                });
            }
            ParamAction::KeyDelete { param, index } => {
                self.send(Command::ParamKeyDelete {
                    session,
                    param,
                    index,
                });
            }
            ParamAction::Flip { param } => {
                self.send(Command::ParamFlip { session, param });
            }
            ParamAction::Binding { row, op } => self.apply_binding_op(session, row, op, snapshot),
        }
    }

    /// A binding-row action. The row names its own binding — one param or
    /// two — so nothing here reconstructs a key from the armed param.
    fn apply_binding_op(
        &mut self,
        session: SessionId,
        row: BindingAddr,
        op: BindingOp,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
    ) {
        let BindingAddr {
            params,
            node,
            target,
            cell,
        } = row;
        match op {
            BindingOp::Unset => {
                self.send(Command::BindingUnset {
                    session,
                    params,
                    node,
                    target,
                    cell,
                });
            }
            BindingOp::Reset => {
                self.send(Command::BindingReset {
                    session,
                    params,
                    node,
                    target,
                    cell,
                });
            }
            BindingOp::Delete => {
                self.send(Command::BindingDelete {
                    session,
                    params,
                    node,
                    target,
                });
            }
            BindingOp::Interpolate(mode) => {
                self.send(Command::BindingInterpolate {
                    session,
                    params,
                    node,
                    target,
                    mode,
                });
            }
            BindingOp::Invert => {
                self.send(Command::BindingInvert {
                    session,
                    params,
                    node,
                    target,
                });
            }
            BindingOp::Copy => {
                self.copied_cell = Some((params, node, target, cell));
                self.status = "keypoint copied".into();
            }
            BindingOp::Paste => {
                self.paste_cell(session, params, node, target, cell, snapshot);
            }
        }
    }

    /// Paste the clipboard keypoint into `cell`. Within one node it is a
    /// server-side copy; across nodes a deform has to be re-fitted onto the
    /// target's topology first.
    fn paste_cell(
        &mut self,
        session: SessionId,
        params: BindingParams,
        node: NodeId,
        target: String,
        cell: [u32; 2],
        _snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
    ) {
        let Some((src_params, src_node, src_target, src_cell)) = self.copied_cell.clone() else {
            return;
        };
        if src_params != params || src_target != target {
            self.status = "paste needs the same params and target".into();
            return;
        }
        if src_node == node {
            self.send(Command::BindingCopyKey {
                session,
                params,
                node,
                target,
                from: src_cell,
                to: cell,
            });
            return;
        }
        let editor = self.editor.clone();
        if target == "deform" {
            let (src_id, dst_id) = (src_node.clone(), node.clone());
            let src_key = binding_key(&params, &src_id, BindingTarget::Deform);
            let refit = editor
                .with_model(session, |m| {
                    let src_mesh = m.node_mesh(&src_id)?.clone();
                    let dst_verts = m.node_mesh(&dst_id)?.verts.clone();
                    let src_offsets = m.deform_value_at(&src_key, src_cell).ok()?;
                    Some(catchlight_editor_core::refit_deform_offsets(
                        &src_mesh,
                        &dst_verts,
                        &src_offsets,
                    ))
                })
                .ok()
                .flatten();
            match refit {
                Some(offsets) => {
                    self.send(Command::DeformVertices {
                        session,
                        params,
                        node,
                        cell,
                        offsets,
                    });
                }
                None => self.status = "paste: source or target has no mesh".into(),
            }
            return;
        }
        let target_name = target.clone();
        let src_key = params.clone();
        let value = editor
            .with_model(session, |m| {
                let t = BindingTarget::parse(&target_name)?;
                if !matches!(t, BindingTarget::Scalar(_)) {
                    return None;
                }
                m.scalar_value_at(&binding_key(&src_key, &src_node, t), src_cell)
                    .ok()
            })
            .ok()
            .flatten();
        match value {
            Some(value) => {
                self.send(Command::BindingKey {
                    session,
                    params,
                    node,
                    target,
                    cell,
                    value,
                });
            }
            None => self.status = "paste: source binding not found".into(),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.drain_io();

        let snapshot = self.session.and_then(|s| self.editor.doc_snapshot(s));
        let rev = snapshot.as_ref().map(|s| s.rev).unwrap_or(0);
        // The armed param can vanish under us (undo of its ParamAdd, a
        // co-driving agent's delete) — recording must stop, not fall back to
        // document edits of posed values.
        if let (Some(armed), Some(snap)) = (self.armed.clone(), &snapshot) {
            if !armed
                .iter()
                .all(|param| snap.params.iter().any(|p| p.id == *param))
            {
                self.armed = None;
                self.deform_mode = false;
            }
        }

        egui::Panel::top("menu").show_inside(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| self.menu_bar(ui));
            ui.add(egui::Label::new(egui::RichText::new(&self.status).weak().small()).truncate());
            ui.add_space(2.0);
        });

        if self.pending_restore.is_some() {
            egui::Panel::top("restore-bar").show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("An autosave from a previous session exists.");
                    if ui.button("Restore").clicked() {
                        if let Some(bytes) = self.pending_restore.take() {
                            match self.editor.open_bytes("autosave", &bytes) {
                                Ok(session) => self.adopt_session(session, "autosave".into()),
                                Err(e) => self.status = format!("restore: {e}"),
                            }
                        }
                    }
                    if ui.button("Dismiss").clicked() {
                        self.pending_restore = None;
                    }
                });
            });
        }

        #[cfg(target_arch = "wasm32")]
        self.autosave_tick(ui.ctx(), rev);

        if let Some(snap) = &snapshot {
            self.refresh_armed_cache(snap);
        } else {
            self.armed_cache = None;
        }
        // Modal layout: mesh edit and recording replace the node tree — both
        // modes work on one already-chosen target, so the tree would only
        // offer wrong turns. Mesh edit hides the inspector side too.
        let mesh_editing = self.mesh_edit.is_some();
        let recording = !mesh_editing && self.armed_cache.is_some();

        let mut tree_actions = Vec::new();
        let mut visible_order = Vec::new();
        let mut param_actions = Vec::new();
        let mesh_title = self.mesh_edit.as_ref().and_then(|m| {
            snapshot
                .as_ref()
                .and_then(|s| find_subtree(&s.root, &m.node))
                .map(|n| n.name.clone())
        });
        let seam_view = self.seam_view();
        let copy_sources: Vec<(NodeId, String)> = match (&self.mesh_edit, &snapshot) {
            (Some(mesh), Some(s)) => {
                let mut out = Vec::new();
                // A mesh is copied from another *part*: a composite has none.
                collect_by_kind(&s.root, |k| k == "part", &mut out);
                out.retain(|(r, _)| *r != mesh.node);
                out
            }
            _ => Vec::new(),
        };
        egui::Panel::left("tree")
            .default_size(250.0)
            .size_range(180.0..=520.0)
            .show_inside(ui, |ui| {
                ui.add_space(4.0);
                if mesh_editing {
                    ui.heading("Mesh editor");
                    if let Some(name) = &mesh_title {
                        ui.label(egui::RichText::new(name).weak());
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("mesh-edit-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(mesh) = &mut self.mesh_edit {
                                mesh.panel_ui(ui, &copy_sources, &seam_view);
                            }
                        });
                    return;
                }
                if recording {
                    self.recording_panel(ui, &snapshot, &mut param_actions);
                    return;
                }
                ui.horizontal(|ui| {
                    ui.heading("Nodes");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("filter")
                            .desired_width(f32::INFINITY),
                    );
                });
                egui::Panel::bottom("tree-extras")
                    .resizable(false)
                    .show_inside(ui, |ui| {
                        egui::CollapsingHeader::new("Textures")
                            .default_open(false)
                            .show(ui, |ui| self.textures_panel(ui));
                        egui::CollapsingHeader::new("History")
                            .default_open(false)
                            .show(ui, |ui| self.history_panel(ui));
                        self.warnings_panel(ui, rev);
                    });
                egui::ScrollArea::vertical()
                    .id_salt("tree-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(snap) = &snapshot {
                            let sel: HashSet<NodeId> = self.selection.iter().cloned().collect();
                            let mut panel = TreePanel {
                                selection: &sel,
                                isolated: self.isolated.clone(),
                                filter: &self.filter.clone(),
                                collapsed: &mut self.collapsed,
                                actions: Vec::new(),
                                visible: Vec::new(),
                            };
                            panel.show(ui, &snap.root);
                            tree_actions = panel.actions;
                            visible_order = panel.visible;
                        } else {
                            ui.label("(none)");
                        }
                    });
            });

        let mut inspector_actions = Vec::new();
        if !mesh_editing {
            egui::Panel::right("inspector")
                .default_size(320.0)
                .size_range(240.0..=440.0)
                .show_inside(ui, |ui| {
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .id_salt("inspector-scroll")
                        .auto_shrink([false, true])
                        .max_height(ui.available_height() * 0.5)
                        .show(ui, |ui| {
                            ui.heading("Inspector");
                            inspector_actions = self.inspector_section(ui, &snapshot);
                        });
                    ui.separator();
                    ui.heading("Params");
                    egui::ScrollArea::vertical()
                        .id_salt("params-scroll")
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            if let Some(snap) = &snapshot {
                                let read_pose = pose_reader(&self.pose, &snap.params);
                                let mut panel = ParamsPanel {
                                    params: &snap.params,
                                    pose: &read_pose,
                                    armed: self.armed_cache.as_ref().map(|(_, info)| info),
                                    snap: &mut self.snap,
                                    can_paste: self.copied_cell.is_some(),
                                    actions: Vec::new(),
                                };
                                panel.show(ui);
                                param_actions.extend(panel.actions);
                            }
                        });
                });
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.viewport_ui(ui, frame, rev, &snapshot, &seam_view);
        });
        self.id_rename_modal(&ui.ctx().clone());
        self.texture_drop_modal(&ui.ctx().clone());

        let mesh_actions = self
            .mesh_edit
            .as_mut()
            .map(|m| std::mem::take(&mut m.actions))
            .unwrap_or_default();
        for action in mesh_actions {
            self.apply_mesh_action(action);
        }

        for action in tree_actions {
            self.apply_tree_action(action, &snapshot, &visible_order);
        }
        for action in inspector_actions {
            self.apply_inspector_action(action);
        }
        for action in param_actions {
            self.apply_param_action(action, &snapshot);
        }
    }
}

impl App {
    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if ui.button("Import…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("manifest", &["json"])
                    .pick_file()
                {
                    let title = file_title(&path);
                    self.open_session(
                        Command::SessionImport {
                            manifest_path: path.display().to_string(),
                        },
                        title,
                    );
                }
            }
            if ui.button("Open…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("catchlight puppet", &["clm"])
                    .pick_file()
                {
                    let title = file_title(&path);
                    self.open_session(
                        Command::SessionOpen {
                            path: path.display().to_string(),
                        },
                        title,
                    );
                }
            }
            if ui.button("Save As…").clicked() && !self.blocked_from_saving() {
                if let Some(session) = self.session {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("catchlight puppet", &["clm"])
                        .set_file_name(format!("{}.clm", self.title))
                        .save_file()
                    {
                        if let Reply::Ok {
                            body: ResponseBody::Saved { path },
                            ..
                        } = self.send(Command::Save {
                            session,
                            path: Some(path.display().to_string()),
                        }) {
                            self.status = format!("saved -> {path}");
                        }
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            if ui.button("Open…").clicked() {
                crate::io::pick_clm(self.io_queue.clone());
            }
            if ui.button("Download .clm").clicked() && !self.blocked_from_saving() {
                if let Some(session) = self.session {
                    let name = format!("{}.clm", self.title);
                    match self.editor.save_bytes(session) {
                        Ok(bytes) => match crate::io::download_bytes(&name, &bytes) {
                            Ok(()) => {
                                let _ = self.editor.mark_saved(session);
                                self.status = format!("downloaded {name}");
                            }
                            Err(e) => self.status = e,
                        },
                        Err(e) => self.status = format!("save: {e}"),
                    }
                }
            }
        }
        ui.separator();
        // Mesh edit mode has its own nested undo scope; the document stack
        // only sees the final Apply.
        if ui.button("Undo").clicked() {
            if let Some(mesh) = &mut self.mesh_edit {
                mesh.undo();
            } else if let Some(session) = self.session {
                self.send(Command::Undo { session });
            }
        }
        if ui.button("Redo").clicked() {
            if let Some(mesh) = &mut self.mesh_edit {
                mesh.redo();
            } else if let Some(session) = self.session {
                self.send(Command::Redo { session });
            }
        }
        if ui
            .button("📷 PNG")
            .on_hover_text("snapshot the viewport")
            .clicked()
        {
            self.snapshot_requested = true;
        }
        ui.separator();
        // The cursor-tool strip: exactly one of pan / gizmo / deform active.
        // Hotkeys are ignored while a drag is in flight — switching tools
        // mid-gesture would reinterpret or orphan it.
        let hotkey = |ui: &egui::Ui, key: egui::Key| {
            !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(key))
        };
        if ui
            .selectable_label(self.pan_mode, "✋ Pan (H)")
            .on_hover_text("drag pans the view")
            .clicked()
            || (!self.any_drag_active() && hotkey(ui, egui::Key::H))
        {
            self.pan_mode = true;
            self.deform_mode = false;
        }
        for (label, mode, key) in [
            ("Move (W)", GizmoMode::Translate, egui::Key::W),
            ("Rotate (E)", GizmoMode::Rotate, egui::Key::E),
            ("Scale (R)", GizmoMode::Scale, egui::Key::R),
        ] {
            let on = self.gizmo.mode == mode && !self.deform_mode && !self.pan_mode;
            if ui.selectable_label(on, label).clicked()
                || (!self.any_drag_active() && hotkey(ui, key))
            {
                self.gizmo.mode = mode;
                self.deform_mode = false;
                self.pan_mode = false;
            }
        }
        if self.armed.is_some()
            && ui
                .selectable_label(self.deform_mode, "Deform (D)")
                .on_hover_text("drag part vertices; writes the armed keypoint's deform")
                .clicked()
        {
            self.deform_mode = !self.deform_mode;
            self.pan_mode = false;
        }
        if self.isolated.is_some() {
            ui.separator();
            if ui.button("🔍 Show all").clicked() {
                self.isolated = None;
            }
        }
    }

    // ---- viewport ----

    #[allow(clippy::too_many_lines)]
    fn viewport_ui(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        rev: u64,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
        seam_view: &SeamView,
    ) {
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
        self.last_viewport_rect = Some(rect);
        let Some(session) = self.session else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "no session",
                egui::FontId::default(),
                ui.visuals().text_color(),
            );
            return;
        };

        // Camera input.
        if resp.hovered() {
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = zoom * (1.0 + scroll * 0.0015);
            if (factor - 1.0).abs() > 1e-4 {
                if let Some(pos) = ui.ctx().pointer_hover_pos() {
                    self.camera.zoom_around(rect, pos, factor);
                }
            }
        }
        if resp.dragged_by(egui::PointerButton::Middle) {
            self.camera.pan(rect, resp.drag_delta());
        }
        // The pan tool owns the primary button outright (in every mode —
        // navigating while mesh editing or recording is the point).
        if self.pan_mode {
            if resp.hovered() || resp.dragged() {
                ui.ctx()
                    .set_cursor_icon(if resp.dragged_by(egui::PointerButton::Primary) {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    });
            }
            if resp.dragged_by(egui::PointerButton::Primary) {
                self.camera.pan(rect, resp.drag_delta());
            }
        }
        if !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::F)) {
            self.focus_selected();
        }

        if !ui.ctx().egui_wants_keyboard_input()
            && !self.any_drag_active()
            && ui.input(|i| i.key_pressed(egui::Key::D))
            && self.armed.is_some()
        {
            self.deform_mode = !self.deform_mode;
            self.pan_mode = false;
        }
        let deform_active = self.deform_mode
            && self.armed.is_some()
            && self.primary().and_then(|r| self.core_of_ref(&r)).is_some();
        if !deform_active && (self.deform_drag.is_some() || self.scratch.is_some()) {
            // The tool was switched off out from under a drag — drop it
            // rather than letting a stale gesture commit later.
            self.deform_drag = None;
            self.clear_scratch_deform();
        }
        // The frame picking/gizmo/vertex tools read is the puppet's, and one
        // render pass has to tick it after an edit before it describes the
        // current document — so interactive tools sit out that frame.
        let transforms_fresh = self.rendered_rev == rev && self.viewport.is_some();
        let isolate = snapshot.as_ref().and_then(|s| self.isolate_set(&s.root));

        // Tool interaction (before click-select so handles win). Mesh edit
        // mode owns the pointer entirely; the deform vertex tool replaces the
        // gizmo while active. Everything here reads last-render transforms,
        // so it waits out the one frame after an edit.
        let mut gizmo_target = None;
        let mut gizmo_consumed = false;
        if self.pan_mode || !transforms_fresh {
            // Pan owns the pointer / transforms are one frame stale — the
            // tools sit this frame out.
        } else if self.mesh_edit.is_some() {
            // The captured core id is re-resolved from the node's Id every
            // frame — structural edits renumber the puppet arena; a deleted
            // node cancels the mode.
            let resolved = self
                .mesh_edit
                .as_ref()
                .map(|m| m.node.clone())
                .and_then(|n| self.core_of_ref(&n));
            match resolved {
                Some(core) => {
                    let camera = self.camera;
                    let node_world = self.node_world(core);
                    if let Some(mesh) = &mut self.mesh_edit {
                        mesh.core = core;
                        mesh.node_world = node_world;
                        gizmo_consumed = mesh.interact(ui, rect, &resp, &camera);
                    }
                }
                None => {
                    self.mesh_edit = None;
                    self.status = "mesh edit cancelled: the node is gone".into();
                }
            }
        } else if deform_active {
            gizmo_consumed = self.deform_tool(&resp, rect, session, snapshot);
        } else {
            gizmo_target = self.gizmo_target();
            if let Some(target) = &gizmo_target {
                let snap = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
                if let Some(event) = self.gizmo.update(rect, &self.camera, target, &resp, snap) {
                    gizmo_consumed = true;
                    self.apply_gizmo_event(event);
                    // Re-resolve after a commit so the overlay tracks the new pose.
                    gizmo_target = self.gizmo_target();
                }
            }
        }

        // Click-select. Off in pan mode (clicks are navigation misses there)
        // and in mesh edit (the mode is pinned to its one node).
        let selectable = !self.pan_mode && self.mesh_edit.is_none();
        if selectable && transforms_fresh && resp.clicked() && !gizmo_consumed {
            let grabbing_gizmo = gizmo_target
                .as_ref()
                .zip(resp.interact_pointer_pos())
                .is_some_and(|(t, pos)| self.gizmo.hit_test(rect, &self.camera, t, pos));
            if !grabbing_gizmo {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let world = self.camera.screen_to_world(rect, pos);
                    let mut hits = self.pick(session, world);
                    if let Some(allowed) = isolate.as_ref() {
                        hits.retain(|c| allowed.contains(c));
                    }
                    let additive = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
                    match hits.first().and_then(|&c| self.ref_of_core(c)) {
                        Some(r) => self.select(r, additive, false, &[]),
                        None if !additive => self.selection.clear(),
                        None => {}
                    }
                }
            }
        }

        // Under-cursor part picker (right-click menu). Selection is the
        // menu's only outcome, so mesh edit suppresses it with the rest.
        if self.mesh_edit.is_none() {
            if transforms_fresh && resp.secondary_clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let world = self.camera.screen_to_world(rect, pos);
                    let mut hits = self.pick(session, world);
                    if let Some(allowed) = isolate.as_ref() {
                        hits.retain(|c| allowed.contains(c));
                    }
                    self.ctx_hits = hits
                        .into_iter()
                        .filter_map(|c| self.ref_of_core(c))
                        .filter_map(|r| {
                            snapshot
                                .as_ref()
                                .and_then(|s| find_subtree(&s.root, &r))
                                .map(|n| (r.clone(), n.name.clone()))
                        })
                        .take(12)
                        .collect();
                }
            }
            let mut picked: Option<NodeId> = None;
            resp.context_menu(|ui| {
                if self.ctx_hits.is_empty() {
                    ui.label("nothing under cursor");
                }
                for (r, name) in &self.ctx_hits {
                    if ui.button(name).clicked() {
                        picked = Some(r.clone());
                        ui.close();
                    }
                }
            });
            if let Some(r) = picked {
                self.select(r, false, false, &[]);
            }
        }

        // Render when the signature moved.
        let w = rect.width().round().max(1.0) as u32;
        let h = rect.height().round().max(1.0) as u32;
        let mut pose: Vec<(ParamId, f32)> =
            self.pose.iter().map(|(k, v)| (k.clone(), *v)).collect();
        pose.sort_by(|a, b| a.0.cmp(&b.0));
        let sig = RenderSig {
            rev,
            pose,
            camera: self.camera,
            size: (w, h),
            isolated: self.isolated.clone(),
            previews: self.previews.clone(),
            scratch_rev: self.scratch_rev,
        };
        if self.rendered.as_ref() != Some(&sig) || self.texture_id.is_none() {
            let Some(render_state) = frame.wgpu_render_state() else {
                self.status = "viewport unavailable: editor is not on the wgpu backend".into();
                return;
            };
            let render_state = render_state.clone();
            if self.viewport.is_none() {
                self.viewport = Some(ViewportRenderer::new(&render_state, w, h));
            }
            let pose = sig.pose.clone();
            let editor = self.editor.clone();
            let camera = self.camera;
            let previews = self.previews.clone();
            if let Some(viewport) = self.viewport.as_mut() {
                match editor.with_puppet(session, |model, puppet| {
                    viewport.render(
                        &render_state,
                        session.0,
                        model,
                        puppet,
                        &pose,
                        &previews,
                        &camera,
                        w,
                        h,
                        isolate.as_ref(),
                    )
                }) {
                    Ok(Ok(id)) => {
                        self.texture_id = Some(id);
                        self.rendered = Some(sig);
                        self.rendered_rev = rev;
                    }
                    Ok(Err(e)) => self.status = format!("render: {e}"),
                    Err(e) => self.status = format!("render: {e}"),
                }
            }
        }
        if let Some(id) = self.texture_id {
            ui.painter().image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        if self.snapshot_requested && self.texture_id.is_some() {
            self.snapshot_requested = false;
            self.take_snapshot();
        }

        // Selection overlay + tools (transforms-derived: same freshness rule).
        if self.rendered_rev != rev {
            return;
        }
        if let Some(mesh) = &self.mesh_edit {
            mesh.draw(ui, rect, &self.camera, seam_view);
        } else {
            self.draw_selection_bounds(ui, rect, session);
            if deform_active {
                self.draw_deform_handles(ui, rect, session);
            } else if let Some(target) = &gizmo_target {
                self.gizmo.draw(ui.painter(), rect, &self.camera, target);
            }
        }
    }

    fn any_drag_active(&self) -> bool {
        self.deform_drag.is_some()
            || self.gizmo.is_dragging()
            || self.mesh_edit.as_ref().is_some_and(|m| m.is_dragging())
    }

    fn apply_gizmo_event(&mut self, event: GizmoEvent) {
        let Some(primary) = self.primary() else {
            return;
        };
        let Some(core) = self.core_of_ref(&primary) else {
            return;
        };
        match event {
            GizmoEvent::Preview {
                translation,
                rotation,
                scale,
            } => {
                self.previews = vec![NodePreview {
                    core_id: core,
                    translation,
                    rotation,
                    scale,
                    ..Default::default()
                }];
            }
            GizmoEvent::Commit {
                translation,
                rotation,
                scale,
            } => {
                self.previews.clear();
                self.commit_patch(
                    primary,
                    NodePatch {
                        translate: translation,
                        rotate: rotation,
                        scale,
                        ..Default::default()
                    },
                );
            }
        }
    }

    /// Debounced single-slot OPFS autosave: write when the document sat
    /// unchanged for a moment past the last autosaved rev. Serialization goes
    /// through the model directly so the dirty flag stays honest.
    #[cfg(target_arch = "wasm32")]
    fn autosave_tick(&mut self, ctx: &egui::Context, rev: u64) {
        const SETTLE_SECONDS: f64 = 3.0;
        let Some(session) = self.session else { return };
        let now = ctx.input(|i| i.time);
        if rev != self.last_rev_seen {
            self.last_rev_seen = rev;
            self.rev_changed_at = now;
        }
        if rev == 0 || rev == self.autosave_rev {
            return;
        }
        if now - self.rev_changed_at < SETTLE_SECONDS {
            // Wake up again to fire the debounce without user input.
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return;
        }
        let editor = self.editor.clone();
        if let Ok(Ok(bytes)) = editor.with_model(session, |m| m.to_clm_bytes()) {
            crate::io::autosave_write(self.io_queue.clone(), bytes);
            self.autosave_rev = rev;
        }
    }

    /// `Model::check()`'s findings. Most are cosmetic; the three about seams
    /// are not — an unfilled slot is a weld that no longer closes, and the two
    /// about a weld's seams mean the model will not save at all.
    fn warnings_panel(&mut self, ui: &mut egui::Ui, rev: u64) {
        let Some(session) = self.session else { return };
        if self.warnings.as_ref().map(|(at, _)| *at) != Some(rev) {
            let warnings = match self.send(Command::Check { session }) {
                Reply::Ok {
                    body: ResponseBody::Warnings { warnings },
                    ..
                } => warnings,
                _ => Vec::new(),
            };
            self.warnings = Some((rev, warnings));
        }
        let Some((_, warnings)) = &self.warnings else {
            return;
        };
        let label = if warnings.is_empty() {
            "Warnings".to_string()
        } else {
            format!("⚠ Warnings ({})", warnings.len())
        };
        let warnings = warnings.clone();
        egui::CollapsingHeader::new(label)
            .default_open(false)
            .show(ui, |ui| {
                if warnings.is_empty() {
                    ui.label("(none)");
                }
                egui::ScrollArea::vertical()
                    .id_salt("warnings-scroll")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for w in &warnings {
                            ui.label(egui::RichText::new(w).small());
                        }
                    });
            });
    }

    /// Undo-stack scrubber: the slider position is the undo depth; moving it
    /// replays Undo/Redo through the same command path as the buttons.
    fn history_panel(&mut self, ui: &mut egui::Ui) {
        let Some(session) = self.session else { return };
        ui.heading("History");
        let Ok((undo_n, redo_n)) = self.editor.history(session) else {
            return;
        };
        let total = undo_n + redo_n;
        if total == 0 {
            ui.label("(no edits yet)");
            return;
        }
        let mut pos = undo_n as f32;
        let resp = ui.add(
            egui::Slider::new(&mut pos, 0.0..=total as f32)
                .integer()
                .text(format!("{undo_n} / {total}")),
        );
        if resp.changed() {
            let target = pos.round() as i64;
            let diff = target - undo_n as i64;
            for _ in 0..diff.abs() {
                if diff < 0 {
                    self.send(Command::Undo { session });
                } else {
                    self.send(Command::Redo { session });
                }
            }
        }
    }

    /// Read the rendered target back and hand it out as a PNG (save dialog on
    /// native, download on web).
    fn take_snapshot(&mut self) {
        let Some(viewport) = self.viewport.as_ref() else {
            return;
        };
        let (device, queue, texture, w, h) = viewport.snapshot_source();
        let name = format!("{}.png", self.title);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let png = pollster::block_on(catchlight_wgpu::read_texture_to_rgba(
                &device, &queue, &texture, w, h,
            ))
            .ok()
            .and_then(|px| crate::snapshot::encode_png(px, w, h));
            match png {
                Some(png) => {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("png", &["png"])
                        .set_file_name(&name)
                        .save_file()
                    {
                        match std::fs::write(&path, png) {
                            Ok(()) => self.status = format!("snapshot -> {}", path.display()),
                            Err(e) => self.status = format!("snapshot: {e}"),
                        }
                    }
                }
                None => self.status = "snapshot readback failed".into(),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let io_queue = self.io_queue.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let png = catchlight_wgpu::read_texture_to_rgba(&device, &queue, &texture, w, h)
                    .await
                    .ok()
                    .and_then(|px| crate::snapshot::encode_png(px, w, h));
                match png {
                    Some(png) => match crate::io::download_bytes(&name, &png) {
                        Ok(()) => {
                            io_queue.push(crate::io::IoEvent::Status(format!("downloaded {name}")))
                        }
                        Err(e) => io_queue.push(crate::io::IoEvent::Error(e)),
                    },
                    None => {
                        io_queue.push(crate::io::IoEvent::Error("snapshot readback failed".into()))
                    }
                }
            });
        }
    }

    // ---- deform vertex tool ----

    /// Deformed world-space vertex positions of the primary Part.
    fn part_world_verts(&self, session: SessionId, core: u32) -> Vec<glam::Vec2> {
        self.editor
            .with_puppet(session, |_model, p| {
                let m = p.transforms().get(catchlight_core::NodeIdx(core));
                let Some(node) = p.get(catchlight_core::NodeIdx(core)) else {
                    return Vec::new();
                };
                let catchlight_core::NodeKind::Part(part) = &node.kind else {
                    return Vec::new();
                };
                (0..part.mesh.vertices.len())
                    .map(|i| picking::part_world_vertex(part, &m, i))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Vertex-drag interaction (single / brush / lasso); returns true when the
    /// pointer was consumed.
    fn deform_tool(
        &mut self,
        resp: &egui::Response,
        rect: egui::Rect,
        session: SessionId,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
    ) -> bool {
        let Some(core) = self.primary().and_then(|r| self.core_of_ref(&r)) else {
            return false;
        };
        // Lasso indices belong to one node's mesh — a different primary (or a
        // rebuilt document, which renumbers core ids) invalidates them.
        if self.deform_sel_core != Some(core) {
            self.deform_selection.clear();
            self.lasso_points.clear();
            self.deform_sel_core = Some(core);
        }

        // Lasso stroke: collect points while dragging; select on release.
        if self.deform_kind == DeformKind::Lasso && self.deform_drag.is_none() {
            if resp.drag_started_by(egui::PointerButton::Primary) {
                self.lasso_points.clear();
            }
            if resp.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if self
                        .lasso_points
                        .last()
                        .map(|p| (*p - pos).length() > 4.0)
                        .unwrap_or(true)
                    {
                        self.lasso_points.push(pos);
                    }
                }
                return true;
            }
            if resp.drag_stopped_by(egui::PointerButton::Primary) && self.lasso_points.len() >= 3 {
                let verts = self.part_world_verts(session, core);
                let lasso = std::mem::take(&mut self.lasso_points);
                let additive = resp.ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
                if !additive {
                    self.deform_selection.clear();
                }
                for (i, v) in verts.iter().enumerate() {
                    let p = self.camera.world_to_screen(rect, *v);
                    if point_in_polygon(p, &lasso) {
                        self.deform_selection.insert(i);
                    }
                }
                return true;
            }
        }

        if resp.drag_started_by(egui::PointerButton::Primary) && self.deform_drag.is_none() {
            let Some(pos) = resp.interact_pointer_pos() else {
                return false;
            };
            let verts = self.part_world_verts(session, core);
            let captured: Vec<(usize, f32)> = match self.deform_kind {
                DeformKind::Brush => {
                    let r = self.brush_radius.max(2.0);
                    verts
                        .iter()
                        .enumerate()
                        .filter_map(|(i, v)| {
                            let d = (self.camera.world_to_screen(rect, *v) - pos).length();
                            (d < r).then(|| {
                                let t = 1.0 - (d / r) * (d / r);
                                (i, t * t)
                            })
                        })
                        .collect()
                }
                DeformKind::Single | DeformKind::Lasso => {
                    // A mesh edit can shrink the vertex count under a live
                    // selection; stale indices must not reach the commit.
                    self.deform_selection.retain(|&i| i < verts.len());
                    let mut best: Option<(usize, f32)> = None;
                    for (i, v) in verts.iter().enumerate() {
                        let d = (self.camera.world_to_screen(rect, *v) - pos).length();
                        if d < 10.0 && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                            best = Some((i, d));
                        }
                    }
                    match best {
                        Some((v, _))
                            if !self.deform_selection.is_empty()
                                && self.deform_selection.contains(&v) =>
                        {
                            self.deform_selection.iter().map(|&i| (i, 1.0)).collect()
                        }
                        Some((v, _)) => vec![(v, 1.0)],
                        None => Vec::new(),
                    }
                }
            };
            if captured.is_empty() {
                return false;
            }
            let Some(node_inv) = catchlight_core::checked_affine_inverse(self.node_world(core))
            else {
                self.status = "cannot deform a node under a singular transform".into();
                return true;
            };
            let world = self.camera.screen_to_world(rect, pos);
            self.deform_drag = Some(DeformDrag {
                core,
                verts: captured,
                start_world: world,
                last_world: world,
                node_inv,
                flow: self.deform_kind == DeformKind::Brush && self.brush_flow,
                pending: HashMap::new(),
            });
            return true;
        }

        let Some(drag) = &mut self.deform_drag else {
            return false;
        };
        let Some(pos) = resp
            .interact_pointer_pos()
            .or_else(|| resp.ctx.pointer_latest_pos())
        else {
            return true;
        };
        let world = self.camera.screen_to_world(rect, pos);
        // World delta -> node-local delta (deform offsets live in node space).
        let to_local = |m: &glam::Mat4, d: glam::Vec2| -> glam::Vec2 {
            let a = m.transform_point3(glam::Vec3::ZERO);
            let b = m.transform_point3(glam::vec3(d.x, d.y, 0.0));
            (b - a).truncate()
        };
        if drag.flow {
            // Flow: each frame's motion accrues into the pending offsets.
            let frame_local = to_local(&drag.node_inv, world - drag.last_world);
            drag.last_world = world;
            for &(v, w) in &drag.verts {
                *drag.pending.entry(v).or_insert(glam::Vec2::ZERO) += frame_local * w;
            }
        } else {
            // Drag: the total delta, weighted per captured vertex.
            let local = to_local(&drag.node_inv, world - drag.start_world);
            drag.pending = drag.verts.iter().map(|&(v, w)| (v, local * w)).collect();
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) {
            let core = drag.core;
            let deltas = std::mem::take(&mut drag.pending);
            self.deform_drag = None;
            self.clear_scratch_deform();
            self.commit_deform_deltas(session, core, &deltas, snapshot);
        } else {
            let deltas = drag.pending.clone();
            if let Some(node) = self.ref_of_core(core) {
                self.set_scratch_deform(session, &node, &deltas);
            }
        }
        true
    }

    /// Show a vertex drag on the session's puppet without authoring it. This
    /// is the presence path: no revision, no undo entry, however long the
    /// gesture runs. `deltas` are node-local, keyed by vertex.
    fn set_scratch_deform(
        &mut self,
        session: SessionId,
        node: &NodeId,
        deltas: &HashMap<usize, glam::Vec2>,
    ) {
        let editor = self.editor.clone();
        let Ok(len) = editor.with_model(session, |m| m.deform_len(node)) else {
            return;
        };
        if len == 0 {
            return;
        }
        // The command wants the whole mesh, and a stale vertex index (a mesh
        // edit under a live selection) must not stretch it past that.
        let mut offsets = vec![0.0f32; len];
        for (&vertex, delta) in deltas {
            if let Some(slot) = offsets.get_mut(vertex * 2..vertex * 2 + 2) {
                slot[0] = delta.x;
                slot[1] = delta.y;
            }
        }
        self.send(Command::ScratchDeform {
            session,
            node: node.clone(),
            offsets,
        });
        self.scratch = Some(node.clone());
        self.scratch_rev = self.scratch_rev.wrapping_add(1);
    }

    /// End the live drag: the puppet drops the scratch deform and the next
    /// render shows the document again.
    fn clear_scratch_deform(&mut self) {
        let (Some(session), Some(node)) = (self.session, self.scratch.take()) else {
            return;
        };
        self.scratch_rev = self.scratch_rev.wrapping_add(1);
        // A node that is gone took its puppet slot (and the drag on it) with
        // it; asking the server to clear it would only be an error to report.
        let editor = self.editor.clone();
        if !editor
            .with_model(session, |m| m.node(&node).is_some())
            .unwrap_or(false)
        {
            return;
        }
        self.send(Command::ScratchDeform {
            session,
            node,
            offsets: Vec::new(),
        });
    }

    /// Write `current cell offsets + deltas` back as the authored cell.
    fn commit_deform_deltas(
        &mut self,
        session: SessionId,
        core: u32,
        deltas: &HashMap<usize, glam::Vec2>,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
    ) {
        let Some(node) = self.ref_of_core(core) else {
            return;
        };
        let Some((params, cell)) = snapshot.as_ref().and_then(|s| self.record_target(s, &node))
        else {
            return;
        };
        let editor = self.editor.clone();
        let key = binding_key(&params, &node, BindingTarget::Deform);
        let base = editor
            .with_model(session, |m| m.deform_value_at(&key, cell).ok())
            .ok()
            .flatten();
        let Some(mut offsets) = base else { return };
        for (&vertex, &local) in deltas {
            if offsets.len() <= vertex * 2 + 1 {
                offsets.resize(vertex * 2 + 2, 0.0);
            }
            offsets[vertex * 2] += local.x;
            offsets[vertex * 2 + 1] += local.y;
        }
        self.send(Command::DeformVertices {
            session,
            params,
            node,
            cell,
            offsets,
        });
    }

    fn draw_deform_handles(&mut self, ui: &egui::Ui, rect: egui::Rect, session: SessionId) {
        let Some(core) = self.primary().and_then(|r| self.core_of_ref(&r)) else {
            return;
        };
        let verts = self.part_world_verts(session, core);
        let paint = ui.painter_at(rect);
        let dragged: HashSet<usize> = self
            .deform_drag
            .as_ref()
            .map(|d| d.verts.iter().map(|&(v, _)| v).collect())
            .unwrap_or_default();
        for (i, v) in verts.iter().enumerate() {
            let pos = self.camera.world_to_screen(rect, *v);
            let color = if dragged.contains(&i) {
                egui::Color32::from_rgb(240, 170, 60)
            } else if self.deform_selection.contains(&i) {
                egui::Color32::from_rgb(255, 220, 90)
            } else {
                egui::Color32::from_rgb(90, 150, 240)
            };
            paint.rect_filled(
                egui::Rect::from_center_size(pos, egui::vec2(5.0, 5.0)),
                0.0,
                color,
            );
        }
        if self.deform_kind == DeformKind::Brush {
            if let Some(pos) = ui.ctx().pointer_latest_pos() {
                if rect.contains(pos) {
                    paint.circle_stroke(
                        pos,
                        self.brush_radius,
                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(240, 170, 60)),
                    );
                }
            }
        }
        if self.lasso_points.len() >= 2 {
            for pair in self.lasso_points.windows(2) {
                paint.line_segment(
                    [pair[0], pair[1]],
                    egui::Stroke::new(1.0_f32, ui.visuals().selection.bg_fill),
                );
            }
        }
    }

    /// The recording-mode left panel: the armed param's controller, bindings,
    /// and (while the deform tool is on) the deform sub-tools.
    fn recording_panel(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
        actions: &mut Vec<ParamAction>,
    ) {
        ui.horizontal(|ui| {
            ui.heading("Recording");
            if ui
                .button("⏹ Stop")
                .on_hover_text("disarm the param")
                .clicked()
            {
                actions.push(ParamAction::Arm(None));
            }
        });
        let Some(snap) = snapshot else { return };
        // Vertical-only: the controller sizes itself to the panel width, and
        // the wide binding rows scroll sideways inside show_recording.
        egui::ScrollArea::vertical()
            .id_salt("recording-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                {
                    let read_pose = pose_reader(&self.pose, &snap.params);
                    let mut panel = ParamsPanel {
                        params: &snap.params,
                        pose: &read_pose,
                        armed: self.armed_cache.as_ref().map(|(_, info)| info),
                        snap: &mut self.snap,
                        can_paste: self.copied_cell.is_some(),
                        actions: Vec::new(),
                    };
                    panel.show_recording(ui);
                    actions.extend(panel.actions);
                }
                if self.deform_mode {
                    ui.separator();
                    ui.label("Deform tool");
                    self.deform_tools_ui(ui);
                }
            });
    }

    fn deform_tools_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (label, kind) in [
                ("Single", DeformKind::Single),
                ("Brush", DeformKind::Brush),
                ("Lasso", DeformKind::Lasso),
            ] {
                if ui
                    .selectable_label(self.deform_kind == kind, label)
                    .clicked()
                {
                    self.deform_kind = kind;
                    self.lasso_points.clear();
                }
            }
        });
        if self.deform_kind == DeformKind::Brush {
            ui.add(egui::Slider::new(&mut self.brush_radius, 8.0..=240.0).text("radius (px)"));
            ui.checkbox(&mut self.brush_flow, "flow (accumulate along stroke)");
        }
        if !self.deform_selection.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!("{} selected", self.deform_selection.len()));
                if ui.small_button("clear").clicked() {
                    self.deform_selection.clear();
                }
            });
        }
    }

    // ---- panels ----

    fn textures_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Textures");
        let Some(session) = self.session else { return };
        let editor = self.editor.clone();
        let Ok(texs) = editor.with_model(session, |m| {
            let ids = m.texture_ids().to_vec();
            ids.iter()
                .filter_map(|t| m.texture(t).map(|tex| (t.clone(), tex.data.clone())))
                .collect::<Vec<_>>()
        }) else {
            return;
        };

        // An upload goes *to* a part: the model has no texture that nothing
        // draws, so there is no adding one and finding it a part later.
        let selected_part = self.selected_part();
        ui.horizontal(|ui| {
            let enabled = selected_part.is_some();
            let add = ui
                .add_enabled(enabled, egui::Button::new("＋ add…"))
                .on_disabled_hover_text("select a part to give the texture to");
            if add.clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                if let (Some(node), Some(path)) = (
                    selected_part.clone(),
                    rfd::FileDialog::new()
                        .add_filter("image", &["png", "tga"])
                        .pick_file(),
                ) {
                    self.guard_texture_drop(DroppingEdit::Send(Box::new(Command::TextureAdd {
                        session,
                        node,
                        path: path.display().to_string(),
                    })));
                }
                #[cfg(target_arch = "wasm32")]
                crate::io::pick_texture(self.io_queue.clone());
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("tex-scroll")
            .max_height(180.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, (tid, data)) in texs.iter().enumerate() {
                        // Keyed by the payload's allocation: two parts drawing
                        // one image share a thumbnail, and replacing an image
                        // gets a new one.
                        let key = Arc::as_ptr(data) as usize;
                        let handle = self
                            .thumbs
                            .entry(key)
                            .or_insert_with(|| thumb_texture(ui.ctx(), &format!("tex{i}"), data));
                        // A texture has no Name — an Id is all it is called,
                        // so the Id is what the tile shows.
                        let resp = ui
                            .vertical(|ui| {
                                let resp =
                                    ui.add(egui::Button::image(egui::load::SizedTexture::new(
                                        handle.id(),
                                        egui::vec2(56.0, 56.0),
                                    )));
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(tid.as_str()).small().weak(),
                                    )
                                    .truncate(),
                                );
                                resp
                            })
                            .inner
                            .on_hover_text(format!("{tid} — click to assign to selected part"));
                        if resp.clicked() {
                            if let Some(node) = selected_part.clone() {
                                self.guard_texture_drop(DroppingEdit::Send(Box::new(
                                    Command::NodeSet {
                                        session,
                                        node,
                                        patch: NodePatch {
                                            texture: Some(tid.clone()),
                                            ..Default::default()
                                        },
                                    },
                                )));
                            }
                        }
                        let mut rename = None;
                        resp.context_menu(|ui| {
                            if ui
                                .button("Rename Id…")
                                .on_hover_text("what parts and addons name this texture by")
                                .clicked()
                            {
                                rename = Some(tid.clone());
                                ui.close();
                            }
                        });
                        if let Some(tid) = rename {
                            self.begin_id_rename(RenameSubject::Texture(tid));
                        }
                    }
                });
            });
    }

    fn inspector_section(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
    ) -> Vec<InspectorAction> {
        let Some(session) = self.session else {
            return Vec::new();
        };
        let Some(primary) = self.primary() else {
            // Nothing selected: puppet-level settings live here.
            self.puppet_globals(ui, session);
            return Vec::new();
        };
        let editor = self.editor.clone();
        let Ok(Some(mut data)) = editor.with_model(session, |m| build_inspector_data(m, &primary))
        else {
            return Vec::new();
        };
        // While armed, TRS/z order/opacity edit the *posed* values (they record
        // to the keypoint), so display the puppet's working state.
        if self.armed.is_some() {
            if let Some(core) = self.core_of_ref(&primary) {
                if let Ok(Some((t, r, s, z, op))) = editor.with_puppet(session, |_model, p| {
                    p.get(catchlight_core::NodeIdx(core)).map(|n| {
                        let op = match &n.kind {
                            catchlight_core::NodeKind::Part(part) => Some(part.opacity),
                            catchlight_core::NodeKind::Composite(c) => Some(c.opacity),
                            _ => None,
                        };
                        (
                            n.transform.translation.to_array(),
                            n.transform.rotation.to_array(),
                            n.transform.scale.to_array(),
                            n.z_order,
                            op,
                        )
                    })
                }) {
                    data.translation = t;
                    data.rotation = r;
                    data.scale = s;
                    data.z_order = z;
                    if let Some(op) = op {
                        match &mut data.kind {
                            InspectorKind::Part { props, .. }
                            | InspectorKind::Composite { props, .. } => props.opacity = op,
                            _ => {}
                        }
                    }
                }
            }
            ui.colored_label(
                egui::Color32::from_rgb(220, 100, 100),
                "⏺ recording — TRS / z order / opacity write to the armed keypoint",
            );
        }
        let mask_sources: Vec<(NodeId, String)> = snapshot
            .as_ref()
            .map(|s| {
                let mut out = Vec::new();
                collect_by_kind(&s.root, is_mask_source_kind, &mut out);
                out
            })
            .unwrap_or_default();
        let params: Vec<(ParamId, String)> = snapshot
            .as_ref()
            .map(|s| {
                s.params
                    .iter()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let textures: Vec<(TexId, String)> = editor
            .with_model(session, |m| {
                m.texture_ids()
                    .iter()
                    .map(|t| (t.clone(), t.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let ctx = InspectorContext {
            mask_sources: &mask_sources,
            params: &params,
        };
        inspector_ui(ui, &data, &ctx, &textures)
    }

    fn puppet_globals(&mut self, ui: &mut egui::Ui, session: SessionId) {
        ui.label("(no selection)");
        ui.separator();
        ui.label("Puppet physics");
        let editor = self.editor.clone();
        let Ok((gravity, ppm)) = editor.with_model(session, |m| {
            (m.physics().gravity, m.physics().pixels_per_meter)
        }) else {
            return;
        };
        let mut g = gravity;
        let rg = ui.add(egui::DragValue::new(&mut g).speed(0.05).prefix("gravity: "));
        if rg.drag_stopped() || (rg.changed() && !rg.dragged()) {
            self.send(Command::PhysicsGlobals {
                session,
                gravity: Some(g),
                pixels_per_meter: None,
            });
        }
        let mut p = ppm;
        let rp = ui.add(
            egui::DragValue::new(&mut p)
                .speed(1.0)
                .prefix("pixels/meter: "),
        );
        if rp.drag_stopped() || (rp.changed() && !rp.dragged()) {
            self.send(Command::PhysicsGlobals {
                session,
                gravity: None,
                pixels_per_meter: Some(p),
            });
        }
    }

    // ---- action application ----

    fn apply_tree_action(
        &mut self,
        action: TreeAction,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
        visible: &[NodeId],
    ) {
        let Some(session) = self.session else { return };
        match action {
            TreeAction::Select {
                node,
                additive,
                range,
            } => self.select(node, additive, range, visible),
            TreeAction::DropInto { node, parent } => {
                self.send(Command::NodeReparent {
                    session,
                    node,
                    to: parent,
                });
            }
            TreeAction::DropBefore {
                node,
                parent,
                index,
            } => {
                // If the node already sits before the target slot under the
                // same parent, removal shifts the slot left by one. Resolved
                // against a fresh snapshot — a mid-frame undo could have
                // shifted siblings since the panel rendered.
                let fresh = self.editor.doc_snapshot(session);
                let adjusted = fresh
                    .as_ref()
                    .or(snapshot.as_ref())
                    .and_then(|s| find_subtree(&s.root, &parent))
                    .and_then(|p| p.children.iter().position(|c| c.id == node))
                    .map(|old| {
                        if (old as u32) < index {
                            index - 1
                        } else {
                            index
                        }
                    })
                    .unwrap_or(index);
                self.send(Command::NodeMove {
                    session,
                    node,
                    parent,
                    index: adjusted,
                });
            }
            TreeAction::AddChild { parent, kind } => {
                self.send(Command::NodeAdd {
                    session,
                    parent,
                    kind,
                    name: None,
                });
            }
            TreeAction::AddPhysics { parent } => {
                self.send(Command::PhysicsAdd {
                    session,
                    parent,
                    name: None,
                    kind: "rigid".into(),
                    target_params: Vec::new(),
                    length: None,
                    gravity: None,
                    frequency: None,
                    angle_damping: None,
                    length_damping: None,
                });
            }
            TreeAction::Duplicate(node) => {
                if let Reply::Ok {
                    body: ResponseBody::Node { node: copy, .. },
                    ..
                } = self.send(Command::NodeDuplicate { session, node })
                {
                    self.selection = vec![copy];
                }
            }
            TreeAction::Delete(node) => {
                self.selection.retain(|r| *r != node);
                if self.isolated.as_ref() == Some(&node) {
                    self.isolated = None;
                }
                self.guard_texture_drop(DroppingEdit::Send(Box::new(Command::NodeDelete {
                    session,
                    node,
                })));
            }
            TreeAction::SetEnabled { node, enabled } => {
                self.send(Command::NodeSet {
                    session,
                    node,
                    patch: NodePatch {
                        enabled: Some(enabled),
                        ..Default::default()
                    },
                });
            }
            TreeAction::RenameId(node) => self.begin_id_rename(RenameSubject::Node(node)),
            TreeAction::Isolate(node) => self.isolated = node,
            TreeAction::Focus(node) => {
                self.selection = vec![node];
                self.focus_selected();
            }
        }
    }

    fn apply_inspector_action(&mut self, action: InspectorAction) {
        let Some(session) = self.session else { return };
        let Some(primary) = self.primary() else {
            return;
        };
        match action {
            InspectorAction::Preview(patch) => {
                if let Some(core) = self.core_of_ref(&primary) {
                    self.previews = vec![NodePreview {
                        core_id: core,
                        translation: patch.translate,
                        rotation: patch.rotate,
                        scale: patch.scale,
                        z_order: patch.z_order,
                        opacity: patch.opacity,
                    }];
                }
            }
            InspectorAction::Commit(patch) => {
                self.previews.clear();
                // An albedo change is the one patch field that can delete
                // something; a texture is never recordable, so routing it
                // around `commit_patch` sends the same NodeSet either way.
                if patch.texture.is_some() {
                    self.guard_texture_drop(DroppingEdit::Send(Box::new(Command::NodeSet {
                        session,
                        node: primary,
                        patch,
                    })));
                } else {
                    self.commit_patch(primary, patch);
                }
            }
            InspectorAction::PhysicsCommit(p) => {
                self.send(Command::PhysicsSet {
                    session,
                    node: primary,
                    kind: p.kind,
                    map_mode: p.map_mode,
                    local_only: p.local_only,
                    target_params: p.target_params,
                    clear_target_params: p.clear_target_params,
                    gravity: p.gravity,
                    length: p.length,
                    frequency: p.frequency,
                    angle_damping: p.angle_damping,
                    length_damping: p.length_damping,
                    output_scale: p.output_scale,
                });
            }
            InspectorAction::MaskAdd { source, mode } => {
                self.send(Command::MaskAdd {
                    session,
                    node: primary,
                    source,
                    mode: catchlight_core::mask_mode_name(mode).into(),
                });
            }
            InspectorAction::MaskSetMode { index, mode } => {
                self.send(Command::MaskSet {
                    session,
                    node: primary,
                    index,
                    mode: catchlight_core::mask_mode_name(mode).into(),
                });
            }
            InspectorAction::MaskReorder { index, to } => {
                self.send(Command::MaskReorder {
                    session,
                    node: primary,
                    index,
                    to,
                });
            }
            InspectorAction::MaskDelete { index } => {
                self.send(Command::MaskDelete {
                    session,
                    node: primary,
                    index,
                });
            }
            InspectorAction::ModelMesh => self.enter_mesh_edit(),
            InspectorAction::RenameId => self.begin_id_rename(RenameSubject::Node(primary)),
        }
    }
}

/// The binding one or two params drive `target` on `node` through — the wire
/// spelling of a key, turned back into the model's.
fn binding_key(params: &BindingParams, node: &NodeId, target: BindingTarget) -> BindingKey {
    match &params.param_y {
        Some(y) => BindingKey::pair(params.param.clone(), y.clone(), node.clone(), target),
        None => BindingKey::new(params.param.clone(), node.clone(), target),
    }
}

/// The wire spelling of a binding's params.
fn wire_params(params: &catchlight_core::BindingParams) -> BindingParams {
    match params {
        catchlight_core::BindingParams::One(x) => BindingParams::one(x.clone()),
        catchlight_core::BindingParams::Two(x, y) => BindingParams::two(x.clone(), y.clone()),
    }
}

fn build_inspector_data(model: &Model, node: &NodeId) -> Option<InspectorData> {
    let n = model.node(node)?;
    let id = node.clone();
    let kind = match &n.kind {
        ModelNodeKind::Group => InspectorKind::Group,
        ModelNodeKind::Part(p) => InspectorKind::Part {
            props: DrawableProps {
                opacity: p.opacity,
                blend_mode: p.blend_mode,
                tint: p.tint,
                screen_tint: p.screen_tint,
                mask_threshold: p.mask_threshold,
                masks: mask_rows(model, p.masks()),
            },
            albedo: p.albedo().cloned(),
            vert_count: p.mesh().verts.len() / 2,
            tri_count: match &p.mesh().indices {
                catchlight_core::formats::clm::ClmIndices::U16(v) => v.len() / 3,
                catchlight_core::formats::clm::ClmIndices::U32(v) => v.len() / 3,
            },
        },
        ModelNodeKind::Composite(c) => InspectorKind::Composite {
            props: DrawableProps {
                opacity: c.opacity,
                blend_mode: c.blend_mode,
                tint: c.tint,
                screen_tint: c.screen_tint,
                mask_threshold: c.mask_threshold,
                masks: mask_rows(model, c.masks()),
            },
            propagate_meshgroup: c.propagate_meshgroup,
        },
        ModelNodeKind::MeshGroup(mg) => InspectorKind::MeshGroup {
            dynamic: mg.dynamic,
            translate_children: mg.translate_children,
            vert_count: mg.mesh().verts.len() / 2,
        },
        ModelNodeKind::SimplePhysics(ph) => InspectorKind::Physics {
            kind: ph.kind,
            map_mode: ph.map_mode,
            local_only: ph.local_only,
            target_params: ph.target_params().clone(),
            gravity: ph.gravity,
            length: ph.length,
            frequency: ph.frequency,
            angle_damping: ph.angle_damping,
            length_damping: ph.length_damping,
            output_scale: ph.output_scale,
        },
    };
    Some(InspectorData {
        id,
        name: n.name.to_string(),
        enabled: n.enabled,
        lock_to_root: n.lock_to_root,
        z_order: n.z_order,
        translation: n.transform.translation,
        rotation: n.transform.rotation,
        scale: n.transform.scale,
        kind,
    })
}

fn mask_rows(model: &Model, masks: &[catchlight_core::ModelMask]) -> Vec<MaskRow> {
    masks
        .iter()
        .map(|m| MaskRow {
            source_name: model
                .node(m.source())
                .map(|n| n.name.to_string())
                .unwrap_or_else(|| "?".into()),
            mode: m.mode(),
        })
        .collect()
}

/// Every node in the tree whose kind `want` accepts, in tree order.
fn collect_by_kind(node: &TreeNode, want: fn(&str) -> bool, out: &mut Vec<(NodeId, String)>) {
    if want(&node.kind) {
        out.push((node.id.clone(), node.name.clone()));
    }
    for c in &node.children {
        collect_by_kind(c, want, out);
    }
}

/// The nodes a mask may name as its source: the two kinds the renderer draws.
/// `Model::mask_add` refuses the rest, so offering one would only author a
/// refused edit.
fn is_mask_source_kind(kind: &str) -> bool {
    kind == "part" || kind == "composite"
}

fn find_subtree<'a>(root: &'a TreeNode, target: &NodeId) -> Option<&'a TreeNode> {
    if root.id == *target {
        return Some(root);
    }
    root.children.iter().find_map(|c| find_subtree(c, target))
}

/// Posed value lookup for `ParamsPanel`: live pose overrides, else defaults.
fn pose_reader<'a>(
    pose: &'a HashMap<ParamId, f32>,
    params: &'a [ParamInfo],
) -> impl Fn(&ParamId) -> f32 + 'a {
    move |id: &ParamId| {
        pose.get(id).copied().unwrap_or_else(|| {
            params
                .iter()
                .find(|p| &p.id == id)
                .map(|p| p.default)
                .unwrap_or(0.0)
        })
    }
}

fn collect_refs<'a>(node: &'a TreeNode, f: &mut impl FnMut(&'a NodeId)) {
    f(&node.id);
    for c in &node.children {
        collect_refs(c, f);
    }
}

fn thumb_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> egui::TextureHandle {
    let img = image::load_from_memory(bytes)
        .map(|i| i.thumbnail(64, 64).to_rgba8())
        .unwrap_or_else(|_| image::RgbaImage::new(8, 8));
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    ctx.load_texture(name, color, egui::TextureOptions::LINEAR)
}

#[cfg(not(target_arch = "wasm32"))]
fn file_title(path: &std::path::Path) -> String {
    catchlight_editor_server::file_stem(path)
}

fn point_in_polygon(p: egui::Pos2, poly: &[egui::Pos2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn nearest_index(points: &[f32], v: f32) -> u32 {
    let mut best = 0usize;
    let mut best_d = f32::INFINITY;
    for (i, &p) in points.iter().enumerate() {
        let d = (p - v).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best as u32
}

fn interp_name(m: catchlight_core::interpolate::InterpolateMode) -> &'static str {
    use catchlight_core::interpolate::InterpolateMode as I;
    match m {
        I::Nearest => "nearest",
        I::Stepped => "stepped",
        I::Linear => "linear",
        I::Cubic => "cubic",
    }
}

/// The wire spelling of a seam, for the seam panel.
fn seam_info(seam: &catchlight_core::Seam) -> SeamInfo {
    SeamInfo {
        id: seam.id().clone(),
        slots: seam
            .slots()
            .iter()
            .map(|slot| SlotInfo {
                id: slot.id().clone(),
                vertex: slot.vertex(),
            })
            .collect(),
    }
}

fn seam_addr(end: &(NodeId, SeamId)) -> SeamAddr {
    SeamAddr {
        node: end.0.clone(),
        seam: end.1.clone(),
    }
}

/// `id`, with `from` replaced by `to`.
fn swap_param(id: ParamId, from: &ParamId, to: &ParamId) -> ParamId {
    if id == *from {
        to.clone()
    } else {
        id
    }
}

fn patch_is_empty(p: &NodePatch) -> bool {
    *p == NodePatch::default()
}
