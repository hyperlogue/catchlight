//! The editor app: panels + viewport over the in-process [`Editor`] API — the
//! same dispatch the socket exposes, so an agent co-drives what the GUI shows.
//!
//! Fixed three-region layout: tree (+textures) | viewport | inspector (+params).
//! The viewport renders on eframe's own wgpu device into an egui texture,
//! re-rendered when the document revision, pose, camera or preview state
//! changes — no readback. Continuous edits preview against the puppet's
//! working state and commit exactly one command on release.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use catchlight_core::formats::clp::TextureEncoding;
use catchlight_core::Vec2;
use catchlight_editor_core::{BindingTarget, EditModel, EditNodeKind, NodeId};
use catchlight_editor_protocol::{
    BindingKeyEntry, Command, NodePatch, NodeRef, ParamInfo, ParamRef, Reply, Request,
    ResponseBody, SessionId, TexRef, TreeNode,
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
use crate::mesh_edit::{MeshEditOutcome, MeshEditState};
use crate::params_panel::{ArmedInfo, BindingRow, ParamAction, ParamsPanel};
use crate::picking;
use crate::tree_panel::{TreeAction, TreePanel};
use crate::viewport::{NodePreview, ViewportRenderer};

mod selection;

use selection::NodeMapping;

pub struct App {
    editor: Arc<Editor>,
    session: Option<SessionId>,
    title: String,
    /// Local preview pose (param name -> [x, y]). Client-local view state.
    pose: HashMap<String, [f32; 2]>,
    status: String,
    io_queue: Arc<IoQueue>,
    /// Built lazily on the first frame, once eframe's wgpu device is in hand.
    viewport: Option<ViewportRenderer>,
    /// egui handle to the viewport's GPU texture (no CPU copy).
    texture_id: Option<egui::TextureId>,
    /// Signature the viewport texture was last rendered at.
    rendered: Option<RenderSig>,
    /// Document rev the viewport (and its transforms) last rendered. Pick,
    /// gizmo and vertex tools only run when this matches the current rev —
    /// after an edit, one render pass must refresh the transforms first.
    rendered_rev: u64,

    camera: EditorCamera,
    gizmo: Gizmo,
    /// The pan tool is active: primary drags pan the camera and clicks don't
    /// select. Mutually exclusive with the gizmo tools and `deform_mode`;
    /// every tool-switch site clears the others.
    pan_mode: bool,
    /// Ordered selection; the last entry is the primary (inspected) node.
    selection: Vec<NodeRef>,
    collapsed: HashSet<u64>,
    filter: String,
    isolated: Option<NodeRef>,
    /// Live gesture previews, applied to puppet working state each render.
    previews: Vec<NodePreview>,
    mapping: Option<NodeMapping>,
    thumbs: HashMap<usize, egui::TextureHandle>,
    /// Parts under the cursor at the last right-click (the picker menu).
    ctx_hits: Vec<(NodeRef, String)>,
    /// The viewport rect from the last frame — focus math needs its aspect.
    last_viewport_rect: Option<egui::Rect>,

    /// Recording state: the armed param — edits to TRS/z order/opacity write
    /// binding keys at the closest keypoint instead of node state.
    armed: Option<ParamRef>,
    /// Snap pose drags to keypoints (the controller's default).
    snap: bool,
    /// Keypoint clipboard: (param, node, target, cell) the value was taken at.
    copied_cell: Option<(ParamRef, NodeRef, String, [u32; 2])>,
    /// Per-vertex deform tool active (armed + Part selected).
    deform_mode: bool,
    deform_drag: Option<DeformDrag>,
    /// Live vertex-drag scratch deform: (core id, per-vertex node-local deltas).
    scratch_deform: Option<(u32, Vec<(usize, glam::Vec2)>)>,
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
    /// Rev-gated cache of the armed param's panel data.
    armed_cache: Option<(ArmedCacheKey, ArmedInfo)>,
}

/// (doc rev, armed param ffi id, armed cell) — the inputs ArmedInfo derives from.
type ArmedCacheKey = (u64, u64, [u32; 2]);

/// Everything the rendered viewport image depends on; a change re-renders.
#[derive(Clone, PartialEq)]
struct RenderSig {
    rev: u64,
    pose: Vec<(String, [f32; 2])>,
    camera: EditorCamera,
    size: (u32, u32),
    isolated: Option<NodeRef>,
    previews: Vec<NodePreview>,
    scratch_deform: Option<(u32, Vec<(usize, glam::Vec2)>)>,
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
            status: "no session — Import a manifest or Open a .clp".into(),
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
            mapping: None,
            thumbs: HashMap::new(),
            ctx_hits: Vec::new(),
            last_viewport_rect: None,
            armed: None,
            snap: true,
            copied_cell: None,
            deform_mode: false,
            deform_drag: None,
            scratch_deform: None,
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
        self.session = Some(session);
        self.title = title;
        self.pose.clear();
        self.armed = None;
        self.copied_cell = None;
        self.deform_mode = false;
        self.deform_drag = None;
        self.scratch_deform = None;
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
        self.mapping = None;
        self.thumbs.clear();
        self.camera = EditorCamera::default();
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
                    if let Some(session) = self.session {
                        let encoding = if is_tga {
                            TextureEncoding::Tga
                        } else {
                            TextureEncoding::Png
                        };
                        match self.editor.add_texture_bytes(session, encoding, bytes) {
                            Ok(_) => self.status = "texture added".into(),
                            Err(e) => self.status = format!("texture: {e}"),
                        }
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

    fn pose_of(&self, snap: &catchlight_editor_server::DocSnapshot, param: ParamRef) -> [f32; 2] {
        let Some(info) = snap.params.iter().find(|p| p.param == param) else {
            return [0.0, 0.0];
        };
        self.pose.get(&info.name).copied().unwrap_or(info.defaults)
    }

    /// The keypoint cell recording writes to: nearest axis point per axis at
    /// the current pose.
    fn armed_cell(
        &self,
        snap: &catchlight_editor_server::DocSnapshot,
    ) -> Option<(ParamRef, [u32; 2])> {
        let param = self.armed?;
        let info = snap.params.iter().find(|p| p.param == param)?;
        let pose = self.pose_of(snap, param);
        // Axis points are normalized 0..1; map the pose into that space.
        let normed = |v: f32, min: f32, max: f32| {
            if (max - min).abs() > f32::EPSILON {
                ((v - min) / (max - min)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let cx = nearest_index(
            &info.axis_points_x,
            normed(pose[0], info.min[0], info.max[0]),
        );
        let cy = nearest_index(
            &info.axis_points_y,
            normed(pose[1], info.min[1], info.max[1]),
        );
        Some((param, [cx, cy]))
    }

    /// Rebuild the armed-param panel data only when (rev, param, cell) moved —
    /// it walks every binding of the param, too heavy for every frame.
    fn refresh_armed_cache(&mut self, snap: &Arc<catchlight_editor_server::DocSnapshot>) {
        let Some(info) = self
            .armed_cell(snap)
            .map(|(param, cell)| (snap.rev, param.0, cell))
        else {
            self.armed_cache = None;
            return;
        };
        if self.armed_cache.as_ref().map(|(key, _)| *key) == Some(info) {
            return;
        }
        self.armed_cache = self.armed_info(snap).map(|v| (info, v));
    }

    fn armed_info(&self, snap: &Arc<catchlight_editor_server::DocSnapshot>) -> Option<ArmedInfo> {
        let session = self.session?;
        let (param, cell) = self.armed_cell(snap)?;
        let info = snap.params.iter().find(|p| p.param == param)?;
        let (w, h) = (info.axis[0] as usize, info.axis[1] as usize);
        let editor = self.editor.clone();
        let pid = catchlight_editor_core::ParamId::from_ffi(param.0);
        let data = editor
            .with_model(session, |m| {
                let p = m.param(pid)?;
                let mut authored_count = vec![0u32; w * h];
                let mut rows = Vec::new();
                for b in &p.bindings {
                    let target = catchlight_editor_core::target_of(&b.values);
                    let mut mark = |x: u32, y: u32| {
                        // A stray out-of-grid cell must not wrap into another
                        // row's slot.
                        if (x as usize) < w && (y as usize) < h {
                            if let Some(slot) = authored_count.get_mut(y as usize * w + x as usize)
                            {
                                *slot += 1;
                            }
                        }
                    };
                    let mut authored_at = false;
                    if let Some(cells) = catchlight_editor_core::scalar_cells(&b.values) {
                        for c in cells {
                            mark(c.x, c.y);
                            authored_at |= [c.x, c.y] == cell;
                        }
                    }
                    if let Some(cells) = catchlight_editor_core::deform_cells(&b.values) {
                        for c in cells {
                            mark(c.x, c.y);
                            authored_at |= [c.x, c.y] == cell;
                        }
                    }
                    rows.push(BindingRow {
                        node: NodeRef(b.node.to_ffi()),
                        node_name: m
                            .node(b.node)
                            .map(|n| n.name.clone())
                            .unwrap_or_else(|| "?".into()),
                        target: target.name().to_string(),
                        interpolate: interp_name(b.interpolate_mode).to_string(),
                        authored_at_cell: authored_at,
                    });
                }
                let total = p.bindings.len() as u32;
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
            param,
            cell,
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
        param: ParamRef,
        cell: [u32; 2],
        node: NodeRef,
        patch: &NodePatch,
    ) -> Vec<BindingKeyEntry> {
        let Some(session) = self.session else {
            return Vec::new();
        };
        let Some(core) = self.core_of_ref(node) else {
            return Vec::new();
        };
        let editor = self.editor.clone();
        // Puppet working state = the pose *without* the gesture (previews are
        // app-side overrides, never baked into the puppet between renders).
        let Ok(Some((pt, pr, ps, pz, pop))) = editor.with_puppet(session, |p| {
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
        use catchlight_editor_core::ScalarTarget as T;
        let pid = catchlight_editor_core::ParamId::from_ffi(param.0);
        let nid = NodeId::from_ffi(node.0);
        let key_at = |t: T| {
            editor
                .with_model(session, |m| {
                    m.scalar_value_at(pid, nid, t, cell).unwrap_or(t.identity())
                })
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
    fn commit_patch(&mut self, node: NodeRef, patch: NodePatch) {
        let Some(session) = self.session else { return };
        if self.armed.is_some() {
            let armed = self
                .editor
                .doc_snapshot(session)
                .and_then(|snap| self.armed_cell(&snap));
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
                Some((param, cell)) if has_recordable => {
                    let entries = self.record_entries(param, cell, node, &patch);
                    if !entries.is_empty() {
                        self.send(Command::BindingKeys {
                            session,
                            param,
                            node,
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

    // ---- mesh edit mode ----

    fn enter_mesh_edit(&mut self) {
        let Some(session) = self.session else { return };
        let Some(node) = self.primary() else { return };
        let Some(core) = self.core_of_ref(node) else {
            return;
        };
        let id = NodeId::from_ffi(node.0);
        let editor = self.editor.clone();
        let Ok(Some((mesh, tex_bytes))) = editor.with_model(session, |m| {
            let albedo = match m.node(id).map(|n| &n.kind) {
                Some(EditNodeKind::Part(p)) => p.albedo,
                Some(EditNodeKind::MeshGroup(_)) => None,
                _ => return None,
            };
            let mesh = m.node_mesh(id)?.clone();
            let bytes = albedo
                .and_then(|t| m.texture(t))
                .map(|t| t.data.as_ref().clone());
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
        let node_world = self
            .viewport
            .as_ref()
            .map(|v| v.transforms.get(catchlight_core::NodeIdx(core)))
            .unwrap_or(glam::Mat4::IDENTITY);
        let working = catchlight_editor_core::WorkingMesh::from_mesh(&mesh);
        self.deform_mode = false;
        self.mesh_edit = Some(MeshEditState::new(
            node, core, working, uv_map, alpha, node_world,
        ));
    }

    fn apply_mesh_edit(&mut self) {
        let Some(session) = self.session else { return };
        let Some(mesh) = self.mesh_edit.take() else {
            return;
        };
        match mesh.working.to_mesh(&mesh.uv_map, mesh.alpha.as_ref()) {
            Ok(new_mesh) => {
                let indices = match &new_mesh.indices {
                    catchlight_core::formats::clp::ClpIndices::U16(v) => {
                        v.iter().map(|&i| i as u32).collect()
                    }
                    catchlight_core::formats::clp::ClpIndices::U32(v) => v.clone(),
                };
                self.send(Command::MeshApply {
                    session,
                    node: mesh.node,
                    verts: new_mesh.verts,
                    uvs: new_mesh.uvs,
                    indices,
                    origin: new_mesh.origin,
                });
            }
            Err(e) => self.status = format!("mesh apply: {e}"),
        }
    }

    /// Replace the *working* mesh with another node's topology (the document
    /// is untouched until Apply).
    fn mesh_copy_into_working(&mut self, src: NodeRef) {
        let Some(session) = self.session else { return };
        let id = NodeId::from_ffi(src.0);
        let editor = self.editor.clone();
        let Ok(Some(src_mesh)) = editor.with_model(session, |m| m.node_mesh(id).cloned()) else {
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
        let armed_cell = snapshot.as_ref().and_then(|s| self.armed_cell(s));
        match action {
            ParamAction::Pose { name, value } => {
                self.pose.insert(name, value);
            }
            ParamAction::Arm(param) => {
                self.armed = param;
                if param.is_none() {
                    self.deform_mode = false;
                }
            }
            ParamAction::AddParam { name, vec2 } => {
                self.send(Command::ParamAdd {
                    session,
                    name,
                    vec2,
                    min: if vec2 { [-1.0, -1.0] } else { [0.0, 0.0] },
                    max: [1.0, 1.0],
                    defaults: [0.0, 0.0],
                    axis_x: Vec::new(),
                    axis_y: Vec::new(),
                });
            }
            ParamAction::Rename { param, name } => {
                self.send(Command::ParamSet {
                    session,
                    param,
                    name: Some(name),
                    min: None,
                    max: None,
                    defaults: None,
                });
            }
            ParamAction::Delete(param) => {
                if self.armed == Some(param) {
                    self.armed = None;
                }
                self.send(Command::ParamDelete { session, param });
            }
            ParamAction::AxisInsert { param, axis, value } => {
                self.send(Command::ParamAxisInsert {
                    session,
                    param,
                    axis,
                    value,
                });
            }
            ParamAction::AxisDelete { param, axis, index } => {
                self.send(Command::ParamAxisDelete {
                    session,
                    param,
                    axis,
                    index,
                });
            }
            ParamAction::Flip { param, axis } => {
                self.send(Command::ParamFlip {
                    session,
                    param,
                    axis,
                });
            }
            ParamAction::BindingUnset { node, target } => {
                if let Some((param, cell)) = armed_cell {
                    self.send(Command::BindingUnset {
                        session,
                        param,
                        node,
                        target,
                        cell,
                    });
                }
            }
            ParamAction::BindingReset { node, target } => {
                if let Some((param, cell)) = armed_cell {
                    self.send(Command::BindingReset {
                        session,
                        param,
                        node,
                        target,
                        cell,
                    });
                }
            }
            ParamAction::BindingDelete { node, target } => {
                if let Some((param, _)) = armed_cell {
                    self.send(Command::BindingDelete {
                        session,
                        param,
                        node,
                        target,
                    });
                }
            }
            ParamAction::BindingInterpolate { node, target, mode } => {
                if let Some((param, _)) = armed_cell {
                    self.send(Command::BindingInterpolate {
                        session,
                        param,
                        node,
                        target,
                        mode,
                    });
                }
            }
            ParamAction::BindingInvert { node, target } => {
                if let Some((param, _)) = armed_cell {
                    self.send(Command::BindingInvert {
                        session,
                        param,
                        node,
                        target,
                    });
                }
            }
            ParamAction::CopyCell { node, target } => {
                if let Some((param, cell)) = armed_cell {
                    self.copied_cell = Some((param, node, target, cell));
                    self.status = "keypoint copied".into();
                }
            }
            ParamAction::PasteCell { node, target } => {
                let Some((param, cell)) = armed_cell else {
                    return;
                };
                let Some((src_param, src_node, src_target, src_cell)) = self.copied_cell.clone()
                else {
                    return;
                };
                if src_param != param || src_target != target {
                    self.status = "paste needs the same param and target".into();
                    return;
                }
                if src_node == node {
                    self.send(Command::BindingCopyKey {
                        session,
                        param,
                        node,
                        target,
                        from: src_cell,
                        to: cell,
                    });
                    return;
                }
                // Cross-node paste: scalars carry over directly; deforms are
                // re-fitted from the source mesh onto the target's topology.
                let pid = catchlight_editor_core::ParamId::from_ffi(param.0);
                let src_id = NodeId::from_ffi(src_node.0);
                let dst_id = NodeId::from_ffi(node.0);
                let editor = self.editor.clone();
                if target == "deform" {
                    let refit = editor
                        .with_model(session, |m| {
                            let src_mesh = m.node_mesh(src_id)?.clone();
                            let dst_verts = m.node_mesh(dst_id)?.verts.clone();
                            let src_offsets = m.deform_value_at(pid, src_id, src_cell).ok()?;
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
                                param,
                                node,
                                cell,
                                offsets,
                            });
                        }
                        None => self.status = "paste: source or target has no mesh".into(),
                    }
                } else {
                    let value = editor
                        .with_model(session, |m| {
                            let BindingTarget::Scalar(t) = BindingTarget::parse(&target)? else {
                                return None;
                            };
                            m.scalar_value_at(pid, src_id, t, src_cell).ok()
                        })
                        .ok()
                        .flatten();
                    match value {
                        Some(value) => {
                            self.send(Command::BindingKey {
                                session,
                                param,
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
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.drain_io();

        let snapshot = self.session.and_then(|s| self.editor.doc_snapshot(s));
        let rev = snapshot.as_ref().map(|s| s.rev).unwrap_or(0);
        if self.session.is_some() {
            self.ensure_mapping(rev);
        }
        // The armed param can vanish under us (undo of its ParamAdd, a
        // co-driving agent's delete) — recording must stop, not fall back to
        // document edits of posed values.
        if let (Some(armed), Some(snap)) = (self.armed, &snapshot) {
            if !snap.params.iter().any(|p| p.param == armed) {
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
        let mut mesh_outcome = MeshEditOutcome::Continue;
        let mut mesh_copy: Option<NodeRef> = None;
        let mesh_title = self.mesh_edit.as_ref().and_then(|m| {
            snapshot
                .as_ref()
                .and_then(|s| find_subtree(&s.root, m.node))
                .map(|n| n.name.clone())
        });
        let copy_sources: Vec<(NodeRef, String)> = match (&self.mesh_edit, &snapshot) {
            (Some(mesh), Some(s)) => {
                let mut out = Vec::new();
                collect_parts(&s.root, &mut out);
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
                                let (outcome, copy) = mesh.panel_ui(ui, &copy_sources);
                                mesh_outcome = outcome;
                                mesh_copy = copy;
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
                    });
                egui::ScrollArea::vertical()
                    .id_salt("tree-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(snap) = &snapshot {
                            let sel: HashSet<u64> = self.selection.iter().map(|r| r.0).collect();
                            let mut panel = TreePanel {
                                selection: &sel,
                                isolated: self.isolated,
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
            self.viewport_ui(ui, frame, rev, &snapshot);
        });

        if let Some(src) = mesh_copy {
            self.mesh_copy_into_working(src);
        }
        match mesh_outcome {
            MeshEditOutcome::Continue => {}
            MeshEditOutcome::Cancel => self.mesh_edit = None,
            MeshEditOutcome::Apply => self.apply_mesh_edit(),
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
                    .add_filter("catchlight puppet", &["clp"])
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
            if ui.button("Save As…").clicked() {
                if let Some(session) = self.session {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("catchlight puppet", &["clp"])
                        .set_file_name(format!("{}.clp", self.title))
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
                crate::io::pick_clp(self.io_queue.clone());
            }
            if ui.button("Download .clp").clicked() {
                if let Some(session) = self.session {
                    let name = format!("{}.clp", self.title);
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
            && self.primary().and_then(|r| self.core_of_ref(r)).is_some();
        if !deform_active && (self.deform_drag.is_some() || self.scratch_deform.is_some()) {
            // The tool was switched off out from under a drag — drop it
            // rather than letting a stale gesture commit later.
            self.deform_drag = None;
            self.scratch_deform = None;
        }
        // The transforms picking/gizmo/vertex tools read come from the last
        // render; after an edit they describe the old puppet until one render
        // pass runs, so interactive tools sit out that frame.
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
            // The captured core id must follow the rev-gated mapping —
            // structural edits renumber the puppet arena; a deleted node
            // cancels the mode.
            let resolved = self
                .mesh_edit
                .as_ref()
                .map(|m| m.node)
                .and_then(|n| self.core_of_ref(n));
            match resolved {
                Some(core) => {
                    let camera = self.camera;
                    if let Some(mesh) = &mut self.mesh_edit {
                        mesh.core = core;
                        if let Some(viewport) = self.viewport.as_ref() {
                            mesh.node_world =
                                viewport.transforms.get(catchlight_core::NodeIdx(core));
                        }
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
                                .and_then(|s| find_subtree(&s.root, r))
                                .map(|n| (r, n.name.clone()))
                        })
                        .take(12)
                        .collect();
                }
            }
            let mut picked: Option<NodeRef> = None;
            resp.context_menu(|ui| {
                if self.ctx_hits.is_empty() {
                    ui.label("nothing under cursor");
                }
                for (r, name) in &self.ctx_hits {
                    if ui.button(name).clicked() {
                        picked = Some(*r);
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
        let mut pose: Vec<(String, [f32; 2])> =
            self.pose.iter().map(|(k, v)| (k.clone(), *v)).collect();
        pose.sort_by(|a, b| a.0.cmp(&b.0));
        let sig = RenderSig {
            rev,
            pose,
            camera: self.camera,
            size: (w, h),
            isolated: self.isolated,
            previews: self.previews.clone(),
            scratch_deform: self.scratch_deform.clone(),
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
            let pose: Vec<(String, Vec2)> = sig
                .pose
                .iter()
                .map(|(name, v)| (name.clone(), Vec2::new(v[0], v[1])))
                .collect();
            let editor = self.editor.clone();
            let camera = self.camera;
            let previews = self.previews.clone();
            let scratch_deform = self.scratch_deform.clone();
            let upload_key = (session.0, rev);
            if let Some(viewport) = self.viewport.as_mut() {
                match editor.with_puppet(session, |puppet| {
                    viewport.render(
                        &render_state,
                        puppet,
                        upload_key,
                        &pose,
                        &previews,
                        scratch_deform.as_ref(),
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
            mesh.draw(ui, rect, &self.camera);
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
        let Some(core) = self.core_of_ref(primary) else {
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
        if let Ok(Ok(bytes)) = editor.with_model(session, |m| m.to_clp_bytes()) {
            crate::io::autosave_write(self.io_queue.clone(), bytes);
            self.autosave_rev = rev;
        }
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
        let Some(viewport) = self.viewport.as_ref() else {
            return Vec::new();
        };
        let m = viewport.transforms.get(catchlight_core::NodeIdx(core));
        self.editor
            .with_puppet(session, |p| {
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
        let Some(core) = self.primary().and_then(|r| self.core_of_ref(r)) else {
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
            let Some(viewport) = self.viewport.as_ref() else {
                return false;
            };
            let Some(node_inv) = catchlight_core::checked_affine_inverse(
                viewport.transforms.get(catchlight_core::NodeIdx(core)),
            ) else {
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
        let scratch: Vec<(usize, glam::Vec2)> =
            drag.pending.iter().map(|(&v, &d)| (v, d)).collect();

        if resp.drag_stopped_by(egui::PointerButton::Primary) {
            let core = drag.core;
            let deltas = std::mem::take(&mut drag.pending);
            self.deform_drag = None;
            self.scratch_deform = None;
            self.commit_deform_deltas(session, core, &deltas, snapshot);
        } else {
            self.scratch_deform = Some((drag.core, scratch));
        }
        true
    }

    /// Write `current cell offsets + deltas` back as the authored cell.
    fn commit_deform_deltas(
        &mut self,
        session: SessionId,
        core: u32,
        deltas: &HashMap<usize, glam::Vec2>,
        snapshot: &Option<Arc<catchlight_editor_server::DocSnapshot>>,
    ) {
        let Some((param, cell)) = snapshot.as_ref().and_then(|s| self.armed_cell(s)) else {
            return;
        };
        let Some(node) = self.ref_of_core(core) else {
            return;
        };
        let pid = catchlight_editor_core::ParamId::from_ffi(param.0);
        let nid = NodeId::from_ffi(node.0);
        let editor = self.editor.clone();
        let base = editor
            .with_model(session, |m| m.deform_value_at(pid, nid, cell).ok())
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
            param,
            node,
            cell,
            offsets,
        });
    }

    fn draw_deform_handles(&mut self, ui: &egui::Ui, rect: egui::Rect, session: SessionId) {
        let Some(core) = self.primary().and_then(|r| self.core_of_ref(r)) else {
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
            m.texture_ids()
                .iter()
                .filter_map(|&t| {
                    m.texture(t)
                        .map(|tex| (TexRef(t.to_ffi()), tex.data.clone()))
                })
                .collect::<Vec<_>>()
        }) else {
            return;
        };

        ui.horizontal(|ui| {
            #[cfg(not(target_arch = "wasm32"))]
            if ui.button("＋ add…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("image", &["png", "tga"])
                    .pick_file()
                {
                    self.send(Command::TextureAdd {
                        session,
                        path: path.display().to_string(),
                    });
                }
            }
            #[cfg(target_arch = "wasm32")]
            if ui.button("＋ add…").clicked() {
                crate::io::pick_texture(self.io_queue.clone());
            }
        });

        let selected_part = self.primary();
        egui::ScrollArea::vertical()
            .id_salt("tex-scroll")
            .max_height(180.0)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, (tref, data)) in texs.iter().enumerate() {
                        // Keyed by the byte buffer's address, not the TexRef:
                        // undo rewinds slotmap versions, so a TexRef can be
                        // re-minted for a different image.
                        let key = Arc::as_ptr(data) as usize;
                        let handle = self
                            .thumbs
                            .entry(key)
                            .or_insert_with(|| thumb_texture(ui.ctx(), &format!("tex{i}"), data));
                        let _ = tref;
                        let resp = ui
                            .add(egui::Button::image(egui::load::SizedTexture::new(
                                handle.id(),
                                egui::vec2(56.0, 56.0),
                            )))
                            .on_hover_text(format!("tex{i} — click to assign to selected part"));
                        if resp.clicked() {
                            if let Some(node) = selected_part {
                                self.send(Command::NodeSet {
                                    session,
                                    node,
                                    patch: NodePatch {
                                        texture: Some(*tref),
                                        ..Default::default()
                                    },
                                });
                            }
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
        let Ok(Some(mut data)) = editor.with_model(session, |m| build_inspector_data(m, primary))
        else {
            return Vec::new();
        };
        // While armed, TRS/z order/opacity edit the *posed* values (they record
        // to the keypoint), so display the puppet's working state.
        if self.armed.is_some() {
            if let Some(core) = self.core_of_ref(primary) {
                if let Ok(Some((t, r, s, z, op))) = editor.with_puppet(session, |p| {
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
        let parts: Vec<(NodeRef, String)> = snapshot
            .as_ref()
            .map(|s| {
                let mut out = Vec::new();
                collect_parts(&s.root, &mut out);
                out
            })
            .unwrap_or_default();
        let params: Vec<(ParamRef, String)> = snapshot
            .as_ref()
            .map(|s| s.params.iter().map(|p| (p.param, p.name.clone())).collect())
            .unwrap_or_default();
        let textures: Vec<(TexRef, String)> = editor
            .with_model(session, |m| {
                m.texture_ids()
                    .iter()
                    .enumerate()
                    .map(|(i, &t)| (TexRef(t.to_ffi()), format!("tex{i}")))
                    .collect()
            })
            .unwrap_or_default();
        let ctx = InspectorContext {
            parts: &parts,
            params: &params,
        };
        inspector_ui(ui, &data, &ctx, &textures)
    }

    fn puppet_globals(&mut self, ui: &mut egui::Ui, session: SessionId) {
        ui.label("(no selection)");
        ui.separator();
        ui.label("Puppet physics");
        let editor = self.editor.clone();
        let Ok((gravity, ppm)) =
            editor.with_model(session, |m| (m.physics.gravity, m.physics.pixels_per_meter))
        else {
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
        visible: &[NodeRef],
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
                    .and_then(|s| find_subtree(&s.root, parent))
                    .and_then(|p| p.children.iter().position(|c| c.node == node))
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
                    target_param: None,
                    length: None,
                    gravity: None,
                    frequency: None,
                    angle_damping: None,
                    length_damping: None,
                });
            }
            TreeAction::Duplicate(node) => {
                if let Reply::Ok {
                    body: ResponseBody::Node { node: copy },
                    ..
                } = self.send(Command::NodeDuplicate { session, node })
                {
                    self.selection = vec![copy];
                }
            }
            TreeAction::Delete(node) => {
                self.selection.retain(|&r| r != node);
                if self.isolated == Some(node) {
                    self.isolated = None;
                }
                self.send(Command::NodeDelete { session, node });
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
                if let Some(core) = self.core_of_ref(primary) {
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
                self.commit_patch(primary, patch);
            }
            InspectorAction::PhysicsCommit(p) => {
                self.send(Command::PhysicsSet {
                    session,
                    node: primary,
                    kind: p.kind,
                    map_mode: p.map_mode,
                    local_only: p.local_only,
                    target_param: p.target_param,
                    clear_target_param: p.clear_target_param,
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
                    mode: catchlight_editor_core::mask_mode_name(mode).into(),
                });
            }
            InspectorAction::MaskSetMode { index, mode } => {
                self.send(Command::MaskSet {
                    session,
                    node: primary,
                    index,
                    mode: catchlight_editor_core::mask_mode_name(mode).into(),
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
            InspectorAction::EditMesh => self.enter_mesh_edit(),
        }
    }
}

fn build_inspector_data(model: &EditModel, node: NodeRef) -> Option<InspectorData> {
    let id = NodeId::from_ffi(node.0);
    let n = model.node(id)?;
    let kind = match &n.kind {
        EditNodeKind::Group => InspectorKind::Group,
        EditNodeKind::Part(p) => InspectorKind::Part {
            props: DrawableProps {
                opacity: p.opacity,
                blend_mode: p.blend_mode,
                tint: p.tint,
                screen_tint: p.screen_tint,
                mask_threshold: p.mask_threshold,
                masks: mask_rows(model, &p.masks),
            },
            albedo: p.albedo.map(|t| TexRef(t.to_ffi())),
            vert_count: p.mesh.verts.len() / 2,
            tri_count: match &p.mesh.indices {
                catchlight_core::formats::clp::ClpIndices::U16(v) => v.len() / 3,
                catchlight_core::formats::clp::ClpIndices::U32(v) => v.len() / 3,
            },
        },
        EditNodeKind::Composite(c) => InspectorKind::Composite {
            props: DrawableProps {
                opacity: c.opacity,
                blend_mode: c.blend_mode,
                tint: c.tint,
                screen_tint: c.screen_tint,
                mask_threshold: c.mask_threshold,
                masks: mask_rows(model, &c.masks),
            },
            propagate_meshgroup: c.propagate_meshgroup,
        },
        EditNodeKind::MeshGroup(mg) => InspectorKind::MeshGroup {
            dynamic: mg.dynamic,
            translate_children: mg.translate_children,
            vert_count: mg.mesh.verts.len() / 2,
        },
        EditNodeKind::SimplePhysics(ph) => InspectorKind::Physics {
            kind: ph.kind,
            map_mode: ph.map_mode,
            local_only: ph.local_only,
            target_param: ph.target_param.map(|p| ParamRef(p.to_ffi())),
            gravity: ph.gravity,
            length: ph.length,
            frequency: ph.frequency,
            angle_damping: ph.angle_damping,
            length_damping: ph.length_damping,
            output_scale: ph.output_scale,
        },
    };
    Some(InspectorData {
        name: n.name.clone(),
        enabled: n.enabled,
        lock_to_root: n.lock_to_root,
        z_order: n.z_order,
        translation: n.transform.translation,
        rotation: n.transform.rotation,
        scale: n.transform.scale,
        kind,
    })
}

fn mask_rows(model: &EditModel, masks: &[catchlight_editor_core::EditMask]) -> Vec<MaskRow> {
    masks
        .iter()
        .map(|m| MaskRow {
            source_name: model
                .node(m.source)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "?".into()),
            mode: m.mode,
        })
        .collect()
}

fn collect_parts(node: &TreeNode, out: &mut Vec<(NodeRef, String)>) {
    if node.kind == "part" {
        out.push((node.node, node.name.clone()));
    }
    for c in &node.children {
        collect_parts(c, out);
    }
}

fn find_subtree(root: &TreeNode, target: NodeRef) -> Option<&TreeNode> {
    if root.node == target {
        return Some(root);
    }
    root.children.iter().find_map(|c| find_subtree(c, target))
}

/// Posed value lookup for `ParamsPanel`: live pose overrides, else defaults.
fn pose_reader<'a>(
    pose: &'a HashMap<String, [f32; 2]>,
    params: &'a [ParamInfo],
) -> impl Fn(&str) -> [f32; 2] + 'a {
    move |name: &str| {
        pose.get(name).copied().unwrap_or_else(|| {
            params
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.defaults)
                .unwrap_or([0.0, 0.0])
        })
    }
}

fn collect_refs(node: &TreeNode, f: &mut impl FnMut(NodeRef)) {
    f(node.node);
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

fn interp_name(m: catchlight_core::params::InterpolateMode) -> &'static str {
    use catchlight_core::params::InterpolateMode as I;
    match m {
        I::Nearest => "nearest",
        I::Stepped => "stepped",
        I::Linear => "linear",
        I::Cubic => "cubic",
    }
}

fn patch_is_empty(p: &NodePatch) -> bool {
    *p == NodePatch::default()
}
