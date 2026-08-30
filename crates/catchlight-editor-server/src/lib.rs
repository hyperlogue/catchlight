//! The catchlight editor server.
//!
//! [`Editor`] holds many [`Session`]s (one puppet each), sharing one warm
//! headless renderer. [`Editor::handle`] is the synchronous dispatch every
//! frontend funnels into: the future GUI calls it in-process, [`serve_unix`]
//! exposes it to the CLI / an agent over a Unix socket. Both speak the same
//! [`catchlight_editor_protocol`] messages.
//!
//! Each session serializes its own commands. Live observers / events arrive
//! with the snapshot phase.

#[cfg(not(target_arch = "wasm32"))]
mod preview;
mod refs;
#[cfg(unix)]
mod transport;

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding};
use catchlight_core::id::{Name, NodeId, ParamId, SeededHex, TexId};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::Vec2;
use catchlight_core::{
    BindingKey, BindingTarget, Model, ModelComposite, ModelError, ModelMeshGroup, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelPhysics, ModelTexture, Pose, Puppet, ScalarTarget,
};
#[cfg(not(target_arch = "wasm32"))]
use catchlight_editor_core::{Manifest, ModelManifestExt as _, TextureData};
use catchlight_editor_core::{ManifestError, ModelMeshExt as _};
pub use refs::RefMap;

/// Per-session undo history depth.
const UNDO_DEPTH: usize = 64;
const UNDO_BYTES: usize = 256 * 1024 * 1024;
use catchlight_editor_protocol::*;

#[cfg(not(target_arch = "wasm32"))]
use preview::PreviewRenderer;

/// Orthographic half-height for previews until the protocol carries a camera.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_CAMERA_HEIGHT: f32 = 2000.0;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("no session {0:?}")]
    NoSession(SessionId),
    #[error("no node {0:?}")]
    NoNode(NodeRef),
    #[error("no param {0:?}")]
    NoParam(ParamRef),
    #[error("no texture {0:?}")]
    NoTexture(TexRef),
    #[error("unknown binding target {0:?}")]
    BadTarget(String),
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("session has no file; pass a path to save")]
    NoSavePath,
    #[error("edit: {0}")]
    Edit(#[from] ModelError),
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("image: {0}")]
    Image(String),
    #[error("preview: {0}")]
    Preview(String),
    #[error("command is native-only (path IO / headless preview)")]
    NativeOnly,
}

struct Session {
    model: Model,
    /// Handle <-> Id for this session's clients; grows, never forgets.
    refs: RefMap,
    /// Where generated Ids come from. Seeded per session so a session's Ids
    /// are reproducible; uniqueness is the model's job, not the seed's.
    hex: SeededHex,
    title: String,
    file: Option<PathBuf>,
    rev: u64,
    saved_rev: u64,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    history_bytes: usize,
    /// Lazily baked from `model` for preview. Rebaked by its own generation
    /// gate on the next use after an edit, so nothing has to invalidate it.
    puppet: Option<Puppet>,
    /// rev-gated document view for in-process observers (the GUI).
    snapshot: Option<Arc<DocSnapshot>>,
    /// Latest shared view state — its own path, never touches the model/rev.
    presence: Option<Presence>,
}

struct HistoryEntry {
    model: Model,
    bytes: usize,
}

impl HistoryEntry {
    fn new(model: Model) -> Self {
        let bytes = model.estimated_size_bytes();
        Self { model, bytes }
    }
}

impl Session {
    fn new(model: Model, title: String, file: Option<PathBuf>) -> Self {
        Self {
            model,
            refs: RefMap::default(),
            hex: SeededHex::new(0x1d5e_ed01),
            title,
            file,
            rev: 0,
            saved_rev: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            history_bytes: 0,
            puppet: None,
            snapshot: None,
            presence: None,
        }
    }

    fn touch(&mut self) {
        self.rev += 1;
    }

    fn node_id(&self, handle: NodeRef) -> Result<NodeId, EditorError> {
        self.refs
            .node_id(handle)
            .cloned()
            .ok_or(EditorError::NoNode(handle))
    }

    fn param_id(&self, handle: ParamRef) -> Result<ParamId, EditorError> {
        self.refs
            .param_id(handle)
            .cloned()
            .ok_or(EditorError::NoParam(handle))
    }

    fn tex_id(&self, handle: TexRef) -> Result<TexId, EditorError> {
        self.refs
            .tex_id(handle)
            .cloned()
            .ok_or(EditorError::NoTexture(handle))
    }

    /// The binding one protocol request names.
    fn binding_key(
        &self,
        param: ParamRef,
        node: NodeRef,
        target: BindingTarget,
    ) -> Result<BindingKey, EditorError> {
        Ok(BindingKey::new(
            self.param_id(param)?,
            self.node_id(node)?,
            target,
        ))
    }

    /// Creation splits the borrow between the model and its Id source.
    fn add_node(&mut self, parent: &NodeId, node: ModelNode) -> Result<NodeRef, EditorError> {
        let Self {
            model, hex, refs, ..
        } = self;
        let id = model.add_node(parent, node, hex)?;
        Ok(refs.node(&id))
    }

    fn add_param(&mut self, param: ModelParam) -> Result<ParamRef, EditorError> {
        let Self {
            model, hex, refs, ..
        } = self;
        let id = model.add_param(param, hex)?;
        Ok(refs.param(&id))
    }

    fn add_texture(&mut self, texture: ModelTexture) -> Result<TexRef, EditorError> {
        let Self {
            model, hex, refs, ..
        } = self;
        let id = model.add_texture(texture, hex)?;
        Ok(refs.texture(&id))
    }

    fn duplicate_subtree(&mut self, node: &NodeId) -> Result<NodeRef, EditorError> {
        let Self {
            model, hex, refs, ..
        } = self;
        let id = model.duplicate_subtree(node, hex)?;
        Ok(refs.node(&id))
    }

    fn push_undo(&mut self, snapshot: Model) {
        self.history_bytes = self.history_bytes.saturating_sub(
            self.redo
                .iter()
                .fold(0usize, |bytes, entry| bytes.saturating_add(entry.bytes)),
        );
        self.redo.clear();
        let entry = HistoryEntry::new(snapshot);
        self.history_bytes = self.history_bytes.saturating_add(entry.bytes);
        self.undo.push(entry);
        self.trim_history();
    }

    fn undo(&mut self) -> Result<(), EditorError> {
        let prev = self.undo.pop().ok_or(EditorError::NothingToUndo)?;
        self.history_bytes = self.history_bytes.saturating_sub(prev.bytes);
        let current = HistoryEntry::new(std::mem::replace(&mut self.model, prev.model));
        self.history_bytes = self.history_bytes.saturating_add(current.bytes);
        self.redo.push(current);
        self.trim_history();
        self.touch();
        Ok(())
    }

    fn redo(&mut self) -> Result<(), EditorError> {
        let next = self.redo.pop().ok_or(EditorError::NothingToRedo)?;
        self.history_bytes = self.history_bytes.saturating_sub(next.bytes);
        let current = HistoryEntry::new(std::mem::replace(&mut self.model, next.model));
        self.history_bytes = self.history_bytes.saturating_add(current.bytes);
        self.undo.push(current);
        self.trim_history();
        self.touch();
        Ok(())
    }

    fn trim_history(&mut self) {
        self.trim_history_to(UNDO_DEPTH, UNDO_BYTES);
    }

    fn trim_history_to(&mut self, max_depth: usize, max_bytes: usize) {
        while self.undo.len() > max_depth
            || (self.history_bytes > max_bytes && self.undo.len() + self.redo.len() > 1)
        {
            let removed = if self.undo.is_empty() {
                self.redo.remove(0)
            } else {
                self.undo.remove(0)
            };
            self.history_bytes = self.history_bytes.saturating_sub(removed.bytes);
        }
    }

    fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    /// The session's puppet and the model it animates, baked on first use and
    /// rebaked by [`Puppet::sync`] when the model has moved since.
    fn puppet(&mut self) -> (&Model, &mut Puppet) {
        // Destructured so the model and the puppet are two borrows of two
        // fields rather than one of the session.
        let Self { model, puppet, .. } = self;
        let puppet = puppet.get_or_insert_with(|| {
            let mut built = Puppet::new(model);
            // Frozen physics keeps the authoring preview deterministic (the
            // dt=0 tick can't integrate anyway) and lets physics-driven
            // params be posed by hand.
            built.set_physics_enabled(false);
            built
        });
        puppet.sync(model);
        (model, puppet)
    }
}

pub struct Editor {
    sessions: Mutex<HashMap<SessionId, Arc<Mutex<Session>>>>,
    next_id: AtomicU64,
    #[cfg(not(target_arch = "wasm32"))]
    preview_seq: AtomicU64,
    #[cfg(not(target_arch = "wasm32"))]
    preview: Mutex<Option<PreviewRenderer>>,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            #[cfg(not(target_arch = "wasm32"))]
            preview_seq: AtomicU64::new(0),
            #[cfg(not(target_arch = "wasm32"))]
            preview: Mutex::new(None),
        }
    }

    /// Apply one request and produce its reply. Synchronous and the single
    /// funnel for every client (in-process or socket).
    pub fn handle(&self, req: Request) -> Reply {
        match self.dispatch(req.command) {
            Ok(body) => Reply::Ok { id: req.id, body },
            Err(e) => Reply::Err {
                id: req.id,
                message: e.to_string(),
            },
        }
    }

    fn alloc_id(&self) -> SessionId {
        SessionId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn insert_session(&self, id: SessionId, session: Session) {
        lock(&self.sessions).insert(id, Arc::new(Mutex::new(session)));
    }

    fn session(&self, id: SessionId) -> Result<Arc<Mutex<Session>>, EditorError> {
        lock(&self.sessions)
            .get(&id)
            .cloned()
            .ok_or(EditorError::NoSession(id))
    }

    /// Open a `.clm` from in-memory bytes — the browser file-picker path, and
    /// the only document-open the wasm build has (no filesystem there).
    pub fn open_bytes(
        &self,
        title: impl Into<String>,
        bytes: &[u8],
    ) -> Result<SessionId, EditorError> {
        let model = Model::from_clm_bytes(bytes)?;
        let id = self.alloc_id();
        self.insert_session(id, Session::new(model, title.into(), None));
        Ok(id)
    }

    /// Serialize a session to `.clm` bytes; the caller owns where the bytes
    /// land (blob download, OPFS, a file) and confirms with [`Self::mark_saved`]
    /// once they actually landed — a failed download must not clear the dirty
    /// flag.
    pub fn save_bytes(&self, id: SessionId) -> Result<Vec<u8>, EditorError> {
        self.with_session(id, |s| Ok(s.model.to_clm_bytes()?))
    }

    /// Record that the caller durably persisted the bytes from `save_bytes`.
    pub fn mark_saved(&self, id: SessionId) -> Result<(), EditorError> {
        self.with_session(id, |s| {
            s.saved_rev = s.rev;
            Ok(())
        })
    }

    /// Undo/redo stack depths — the history panel's scrub range.
    pub fn history(&self, id: SessionId) -> Result<(usize, usize), EditorError> {
        let session = self.session(id)?;
        let s = lock(&session);
        Ok((s.undo.len(), s.redo.len()))
    }

    /// Is the session dirty (unsaved edits since the last save/save_bytes)?
    pub fn is_dirty(&self, id: SessionId) -> Result<bool, EditorError> {
        let session = self.session(id)?;
        let s = lock(&session);
        Ok(s.dirty())
    }

    /// Read the session's document directly — the in-process observer path for
    /// panels that need more than the tree snapshot (inspector, textures).
    /// Deep-read protocol commands wait until a remote client exists.
    pub fn with_model<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model, &mut RefMap) -> R,
    ) -> Result<R, EditorError> {
        let session = self.session(id)?;
        let mut s = lock(&session);
        let Session { model, refs, .. } = &mut *s;
        Ok(f(model, refs))
    }

    /// Register an already-encoded PNG/TGA texture from bytes (the browser
    /// picker path; native flows can keep handing paths to `TextureAdd`).
    pub fn add_texture_bytes(
        &self,
        id: SessionId,
        encoding: TextureEncoding,
        bytes: Vec<u8>,
    ) -> Result<TexRef, EditorError> {
        image_dims(&bytes)?;
        self.edit_session(id, |s| {
            let tex = s.add_texture(ModelTexture {
                encoding,
                alpha: TextureAlpha::Straight,
                data: Arc::new(bytes),
            })?;
            s.touch();
            Ok(tex)
        })
    }

    /// Run `f` against a session. No undo snapshot is taken, so `f` must not
    /// edit the document (session metadata like `file`/`saved_rev` is fine);
    /// document edits go through `edit_session`. The signature can't express
    /// this — both hand out `&mut Session` because metadata mutation is allowed
    /// — so a debug assert catches a stray document edit (any edit bumps `rev`,
    /// which is exactly what `edit_session` snapshots on).
    fn with_session<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<R, EditorError> {
        let session = self.session(id)?;
        let mut session = lock(&session);
        let rev_before = session.rev;
        let result = f(&mut session);
        debug_assert_eq!(
            session.rev, rev_before,
            "with_session edited the document (rev bumped) without an undo snapshot; use edit_session",
        );
        result
    }

    /// Run a document edit `f` against a session, auto-capturing a pre-edit undo
    /// snapshot that is pushed only when the edit succeeds and actually changed
    /// the document (rev bumped). The snapshot deep-copies every mesh and
    /// binding grid (only texture bytes are Arc-shared), which is why read-only
    /// commands stay on `with_session`.
    fn edit_session<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<R, EditorError> {
        let session = self.session(id)?;
        let mut session = lock(&session);
        let before = session.rev;
        let snapshot = session.model.clone();
        let result = f(&mut session);
        match &result {
            Ok(_) if session.rev != before => session.push_undo(snapshot),
            Ok(_) => {}
            Err(_) => {
                // A failed command must leave no partial edit behind —
                // multi-step commands can fail midway through mutating.
                session.model = snapshot;
                session.rev = before;
                session.puppet = None;
            }
        }
        result
    }

    /// Current document view for an in-process observer (the GUI), rebuilt only
    /// when the session's revision changed.
    pub fn doc_snapshot(&self, id: SessionId) -> Option<Arc<DocSnapshot>> {
        let session = self.session(id).ok()?;
        let mut session = lock(&session);
        if let Some(cached) = &session.snapshot {
            if cached.rev == session.rev {
                return Some(cached.clone());
            }
        }
        let rev = session.rev;
        let Session { model, refs, .. } = &mut *session;
        // A session always holds a complete model: the editor's load path
        // reads one, and `Model::new` makes one.
        let root = model.root().cloned()?;
        let snap = Arc::new(DocSnapshot {
            rev,
            root: build_tree(model, refs, &root),
            params: param_infos(model, refs),
        });
        session.snapshot = Some(snap.clone());
        Some(snap)
    }

    /// In-process shared-view read/write (the presence path) for the GUI.
    pub fn set_presence(&self, id: SessionId, presence: Presence) -> bool {
        let Ok(session) = self.session(id) else {
            return false;
        };
        lock(&session).presence = Some(presence);
        true
    }

    pub fn presence(&self, id: SessionId) -> Option<Presence> {
        let session = self.session(id).ok()?;
        let presence = lock(&session).presence.clone();
        presence
    }

    /// [`fold_pose`] against this session's model.
    pub fn fold_pose(
        &self,
        id: SessionId,
        pose: &[(String, Vec2)],
    ) -> Result<Vec<(String, Vec2)>, EditorError> {
        self.with_model(id, |model, _| fold_pose(model, pose))
    }

    /// Run `f` with the session's model and the puppet animating it, baking
    /// the puppet lazily and rebaking it if the model changed. The in-process
    /// GUI viewport renders them on eframe's own wgpu device (no readback);
    /// neither leaves the session lock.
    pub fn with_puppet<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model, &mut Puppet) -> R,
    ) -> Result<R, EditorError> {
        let session = self.session(id)?;
        let mut s = lock(&session);
        let (model, puppet) = s.puppet();
        Ok(f(model, puppet))
    }

    fn dispatch(&self, cmd: Command) -> Result<ResponseBody, EditorError> {
        match cmd {
            Command::SessionNew { name } => {
                let id = self.alloc_id();
                let title = name.unwrap_or_else(|| format!("untitled-{}", id.0));
                self.insert_session(id, Session::new(Model::new(), title, None));
                Ok(ResponseBody::Session { session: id })
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::SessionOpen { path } => {
                let path = PathBuf::from(path);
                let model = Model::from_clm_bytes(&std::fs::read(&path)?)?;
                let id = self.alloc_id();
                let title = file_stem(&path);
                self.insert_session(id, Session::new(model, title, Some(path)));
                Ok(ResponseBody::Session { session: id })
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::SessionImport { manifest_path } => {
                let mpath = PathBuf::from(manifest_path);
                let manifest = Manifest::from_json(&std::fs::read_to_string(&mpath)?)?;
                let base = mpath
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                let mut data = HashMap::new();
                for t in &manifest.textures {
                    let bytes = std::fs::read(base.join(&t.path))?;
                    data.insert(
                        t.id.clone(),
                        TextureData {
                            encoding: encoding_from_path(&t.path),
                            bytes: Arc::new(bytes),
                        },
                    );
                }
                let model = Model::from_manifest(&manifest, &data)?;
                let id = self.alloc_id();
                let title = if manifest.name.is_empty() {
                    file_stem(&mpath)
                } else {
                    manifest.name.clone()
                };
                self.insert_session(id, Session::new(model, title, None));
                Ok(ResponseBody::Session { session: id })
            }
            Command::SessionList => {
                let handles: Vec<_> = lock(&self.sessions)
                    .iter()
                    .map(|(&id, session)| (id, session.clone()))
                    .collect();
                let mut sessions = Vec::with_capacity(handles.len());
                for (id, handle) in handles {
                    let s = lock(&handle);
                    sessions.push(SessionInfo {
                        session: id,
                        title: s.title.clone(),
                        file: s.file.as_ref().map(|p| p.display().to_string()),
                        dirty: s.dirty(),
                        rev: s.rev,
                        node_count: s.model.node_count() as u32,
                    });
                }
                sessions.sort_by_key(|s| s.session.0);
                Ok(ResponseBody::Sessions { sessions })
            }
            Command::SessionClose { session } => {
                lock(&self.sessions)
                    .remove(&session)
                    .ok_or(EditorError::NoSession(session))?;
                Ok(ResponseBody::Empty)
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::Save { session, path } => {
                let handle = self.session(session)?;
                let (path, bytes, rev) = {
                    let s = lock(&handle);
                    let path = match path {
                        Some(p) => PathBuf::from(p),
                        None => s.file.clone().ok_or(EditorError::NoSavePath)?,
                    };
                    (path, s.model.to_clm_bytes()?, s.rev)
                };
                // Temp-then-rename: an interrupted save must not truncate the
                // user's only copy.
                let tmp = path.with_extension("legacy.tmp");
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, &path)?;
                let mut s = lock(&handle);
                s.file = Some(path.clone());
                s.saved_rev = rev;
                Ok(ResponseBody::Saved {
                    path: path.display().to_string(),
                })
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::ExportManifest { session, path } => {
                let handle = self.session(session)?;
                let path = PathBuf::from(path);
                let (manifest, textures) = {
                    let s = lock(&handle);
                    let manifest = s.model.to_manifest().to_json()?;
                    let textures = s
                        .model
                        .texture_ids()
                        .iter()
                        .enumerate()
                        .filter_map(|(i, tid)| {
                            let texture = s.model.texture(tid)?;
                            let ext = match texture.encoding {
                                TextureEncoding::Tga => "tga",
                                TextureEncoding::Png => "png",
                            };
                            Some((format!("tex{i}.{ext}"), texture.data.clone()))
                        })
                        .collect::<Vec<_>>();
                    (manifest, textures)
                };
                std::fs::write(&path, manifest)?;
                let base = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."));
                for (name, data) in textures {
                    std::fs::write(base.join(name), &*data)?;
                }
                Ok(ResponseBody::Saved {
                    path: path.display().to_string(),
                })
            }
            Command::Status { session } => self.with_session(session, |s| {
                Ok(ResponseBody::Status {
                    status: StatusInfo {
                        title: s.title.clone(),
                        node_count: s.model.node_count() as u32,
                        param_count: s.model.param_ids().len() as u32,
                        texture_count: s.model.texture_ids().len() as u32,
                        dirty: s.dirty(),
                        rev: s.rev,
                    },
                })
            }),
            Command::Check { session } => self.with_session(session, |s| {
                Ok(ResponseBody::Warnings {
                    warnings: s.model.check().into_iter().map(|w| w.message).collect(),
                })
            }),
            Command::NodeTree { session } => self.with_session(session, |s| {
                let root = s.model.root().cloned().ok_or(ModelError::Fragment)?;
                let Session { model, refs, .. } = s;
                Ok(ResponseBody::Tree {
                    root: build_tree(model, refs, &root),
                })
            }),
            Command::NodeAdd {
                session,
                parent,
                kind,
                name,
            } => self.edit_session(session, |s| {
                let node =
                    ModelNode::new(name.unwrap_or_else(|| default_name(kind)), make_kind(kind));
                let parent = s.node_id(parent)?;
                let node = s.add_node(&parent, node)?;
                s.touch();
                Ok(ResponseBody::Node { node })
            }),
            Command::NodeSet {
                session,
                node,
                patch,
            } => self.edit_session(session, |s| {
                let id = s.node_id(node)?;
                if let Some(tref) = patch.texture {
                    let tex = s.tex_id(tref)?;
                    if s.model.texture(&tex).is_none() {
                        return Err(EditorError::NoTexture(tref));
                    }
                    if matches!(
                        s.model.node(&id).map(|n| &n.kind),
                        Some(ModelNodeKind::Part(_))
                    ) {
                        s.model.set_part_albedo(&id, Some(tex))?;
                    }
                }
                s.model
                    .update_node(&id, |n| apply_patch(n, &patch))
                    .map_err(|_| EditorError::NoNode(node))??;
                s.touch();
                Ok(ResponseBody::Node { node })
            }),
            Command::NodeReparent { session, node, to } => self.edit_session(session, |s| {
                let (node, to) = (s.node_id(node)?, s.node_id(to)?);
                s.model.reparent(&node, &to)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeReorder {
                session,
                node,
                index,
            } => self.edit_session(session, |s| {
                let node = s.node_id(node)?;
                s.model.reorder(&node, index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeMove {
                session,
                node,
                parent,
                index,
            } => self.edit_session(session, |s| {
                let (id, parent) = (s.node_id(node)?, s.node_id(parent)?);
                s.model.reparent(&id, &parent)?;
                // reorder can only fail on unknown/root, both excluded by the
                // successful reparent — the combined edit stays atomic.
                s.model.reorder(&id, index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeDuplicate { session, node } => self.edit_session(session, |s| {
                let node = s.node_id(node)?;
                let copy = s.duplicate_subtree(&node)?;
                s.touch();
                Ok(ResponseBody::Node { node: copy })
            }),
            Command::MaskAdd {
                session,
                node,
                source,
                mode,
            } => self.edit_session(session, |s| {
                let mode = parse_mask_mode(&mode)?;
                let (node, source) = (s.node_id(node)?, s.node_id(source)?);
                s.model.mask_add(&node, &source, mode)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MaskSet {
                session,
                node,
                index,
                mode,
            } => self.edit_session(session, |s| {
                let mode = parse_mask_mode(&mode)?;
                let node = s.node_id(node)?;
                s.model.mask_set_mode(&node, index as usize, mode)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MaskReorder {
                session,
                node,
                index,
                to,
            } => self.edit_session(session, |s| {
                let node = s.node_id(node)?;
                s.model.mask_reorder(&node, index as usize, to as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MaskDelete {
                session,
                node,
                index,
            } => self.edit_session(session, |s| {
                let node = s.node_id(node)?;
                s.model.mask_delete(&node, index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::PhysicsSet {
                session,
                node,
                kind,
                map_mode,
                local_only,
                target_param,
                clear_target_param,
                gravity,
                length,
                frequency,
                angle_damping,
                length_damping,
                output_scale,
            } => self.edit_session(session, |s| {
                let parsed_kind = kind
                    .as_deref()
                    .map(|m| parse_pendulum_kind(m).ok_or(EditorError::BadTarget(m.to_string())))
                    .transpose()?;
                let parsed_map = map_mode
                    .as_deref()
                    .map(|m| parse_map_mode(m).ok_or(EditorError::BadTarget(m.to_string())))
                    .transpose()?;
                let target = match (clear_target_param, target_param) {
                    (true, _) => Some(None),
                    (false, Some(p)) => {
                        let pid = s.param_id(p)?;
                        if s.model.param(&pid).is_none() {
                            return Err(EditorError::BadTarget("target param".into()));
                        }
                        Some(Some(pid))
                    }
                    (false, None) => None,
                };
                let id = s.node_id(node)?;
                if let Some(t) = target {
                    s.model.set_physics_targets(&id, [t, None])?;
                }
                s.model
                    .update_node(&id, |n| {
                        let ModelNodeKind::SimplePhysics(ph) = &mut n.kind else {
                            return Err(EditorError::BadTarget("not a physics node".into()));
                        };
                        if let Some(m) = parsed_kind {
                            ph.kind = m;
                        }
                        if let Some(m) = parsed_map {
                            ph.map_mode = m;
                        }
                        if let Some(v) = local_only {
                            ph.local_only = v;
                        }
                        if let Some(v) = gravity {
                            ph.gravity = v;
                        }
                        if let Some(v) = length {
                            ph.length = v;
                        }
                        if let Some(v) = frequency {
                            ph.frequency = v;
                        }
                        if let Some(v) = angle_damping {
                            ph.angle_damping = v;
                        }
                        if let Some(v) = length_damping {
                            ph.length_damping = v;
                        }
                        if let Some(v) = output_scale {
                            ph.output_scale = v;
                        }
                        Ok(())
                    })
                    .map_err(|_| EditorError::NoNode(node))??;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::PhysicsGlobals {
                session,
                gravity,
                pixels_per_meter,
            } => self.edit_session(session, |s| {
                let mut physics = *s.model.physics();
                if let Some(g) = gravity {
                    physics.gravity = g;
                }
                if let Some(ppm) = pixels_per_meter {
                    physics.pixels_per_meter = ppm;
                }
                s.model.set_physics(physics);
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeDelete { session, node } => self.edit_session(session, |s| {
                let node = s.node_id(node)?;
                s.model.delete_node(&node)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            #[cfg(not(target_arch = "wasm32"))]
            Command::TextureAdd { session, path } => self.edit_session(session, |s| {
                let bytes = std::fs::read(&path)?;
                image_dims(&bytes)?; // validate it decodes
                let texture = s.add_texture(ModelTexture {
                    encoding: encoding_from_path(&path),
                    alpha: TextureAlpha::Straight,
                    data: Arc::new(bytes),
                })?;
                s.touch();
                Ok(ResponseBody::Texture { texture })
            }),
            Command::TextureList { session } => self.with_session(session, |s| {
                let Session { model, refs, .. } = s;
                let mut textures = Vec::new();
                for tid in model.texture_ids() {
                    if let Some(t) = model.texture(tid) {
                        let (width, height) = image_dims(&t.data).unwrap_or((0, 0));
                        textures.push(TexInfo {
                            texture: refs.texture(tid),
                            width,
                            height,
                        });
                    }
                }
                Ok(ResponseBody::Textures { textures })
            }),
            Command::ParamAdd {
                session,
                name,
                vec2,
                min,
                max,
                defaults,
                axis_x,
                axis_y,
            } => self.edit_session(session, |s| {
                if !catchlight_core::param_range_is_valid(min[0], max[0])
                    || (vec2 && !catchlight_core::param_range_is_valid(min[1], max[1]))
                {
                    return Err(ModelError::CellOutOfRange.into());
                }
                let positions = |given: Vec<f32>| {
                    if given.is_empty() {
                        vec![0.0, 1.0]
                    } else {
                        given
                    }
                };
                // A param is a scalar; the protocol's `vec2` asks for the two
                // halves a two-param binding would span (cl-32i.13 replaces it).
                let (name_x, name_y) = if vec2 {
                    (format!("{name}.x"), format!("{name}.y"))
                } else {
                    (name, String::new())
                };
                let param = s.add_param(ModelParam {
                    name: Name::truncated(name_x),
                    min: min[0],
                    max: max[0],
                    default: defaults[0],
                    key_positions: positions(axis_x),
                })?;
                if vec2 {
                    s.add_param(ModelParam {
                        name: Name::truncated(name_y),
                        min: min[1],
                        max: max[1],
                        default: defaults[1],
                        key_positions: positions(axis_y),
                    })?;
                }
                s.touch();
                Ok(ResponseBody::Param { param })
            }),
            Command::ParamList { session } => self.with_session(session, |s| {
                let Session { model, refs, .. } = s;
                Ok(ResponseBody::Params {
                    params: param_infos(model, refs),
                })
            }),
            Command::ParamSet {
                session,
                param,
                name,
                min,
                max,
                defaults,
            } => self.edit_session(session, |s| {
                let pid = s.param_id(param)?;
                if let Some(n) = name {
                    s.model.set_param_name(&pid, Name::truncated(n))?;
                }
                if min.is_some() || max.is_some() {
                    let p = s.model.param(&pid).ok_or(ModelError::UnknownParam)?;
                    let new_min = min.map_or(p.min, |m| m[0]);
                    let new_max = max.map_or(p.max, |m| m[0]);
                    s.model.set_param_range(&pid, new_min, new_max)?;
                }
                if let Some(d) = defaults {
                    s.model.set_param_default(&pid, d[0])?;
                }
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamDelete { session, param } => self.edit_session(session, |s| {
                let param = s.param_id(param)?;
                s.model.delete_param(&param)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamAxisInsert {
                session,
                param,
                axis,
                value,
            } => self.edit_session(session, |s| {
                let param = s.param_id(param)?;
                only_x_axis(axis)?;
                s.model.key_insert(&param, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamAxisDelete {
                session,
                param,
                axis,
                index,
            } => self.edit_session(session, |s| {
                let param = s.param_id(param)?;
                only_x_axis(axis)?;
                s.model.key_delete(&param, index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamAxisMove {
                session,
                param,
                axis,
                index,
                value,
            } => self.edit_session(session, |s| {
                let param = s.param_id(param)?;
                only_x_axis(axis)?;
                s.model.key_move(&param, index as usize, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamFlip {
                session,
                param,
                axis,
            } => self.edit_session(session, |s| {
                let param = s.param_id(param)?;
                only_x_axis(axis)?;
                s.model.param_flip(&param)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingKeys {
                session,
                param,
                node,
                cell,
                entries,
            } => self.edit_session(session, |s| {
                let parsed: Vec<(ScalarTarget, f32)> = entries
                    .iter()
                    .map(|e| {
                        ScalarTarget::parse(&e.target)
                            .map(|t| (t, e.value))
                            .ok_or_else(|| EditorError::BadTarget(e.target.clone()))
                    })
                    .collect::<Result<_, _>>()?;
                for (t, value) in parsed {
                    let key = s.binding_key(param, node, BindingTarget::Scalar(t))?;
                    s.model.set_binding_key(&key, cell, value)?;
                }
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingUnset {
                session,
                param,
                node,
                target,
                cell,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = s.binding_key(param, node, t)?;
                s.model.unset_binding_key(&key, cell)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingReset {
                session,
                param,
                node,
                target,
                cell,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = s.binding_key(param, node, t)?;
                s.model.reset_binding_key(&key, cell)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingDelete {
                session,
                param,
                node,
                target,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = s.binding_key(param, node, t)?;
                s.model.delete_binding(&key)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingInterpolate {
                session,
                param,
                node,
                target,
                mode,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let m = parse_interpolate_mode(&mode)?;
                let key = s.binding_key(param, node, t)?;
                s.model.set_binding_interpolate(&key, m)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingInvert {
                session,
                param,
                node,
                target,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = s.binding_key(param, node, t)?;
                s.model.invert_binding(&key)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingCopyKey {
                session,
                param,
                node,
                target,
                from,
                to,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = s.binding_key(param, node, t)?;
                s.model.copy_binding_key(&key, from, to)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::DeformVertices {
                session,
                param,
                node,
                cell,
                offsets,
            } => self.edit_session(session, |s| {
                let key = s.binding_key(param, node, BindingTarget::Deform)?;
                s.model.set_deform_vertices(&key, cell, offsets)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MeshApply {
                session,
                node,
                verts,
                uvs,
                indices,
                origin,
            } => self.edit_session(session, |s| {
                let vcount = verts.len() / 2;
                if verts.len() % 2 != 0
                    || uvs.len() != verts.len()
                    || indices.len() % 3 != 0
                    || indices.iter().any(|&i| i as usize >= vcount)
                {
                    return Err(EditorError::BadTarget("malformed mesh".into()));
                }
                let indices = if indices.iter().max().copied().unwrap_or(0) <= u16::MAX as u32 {
                    ClmIndices::U16(indices.iter().map(|&i| i as u16).collect())
                } else {
                    ClmIndices::U32(indices)
                };
                let node = s.node_id(node)?;
                s.model.set_mesh_with_refit(
                    &node,
                    ClmMesh {
                        verts,
                        uvs,
                        indices,
                        origin,
                    },
                )?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MeshCopy { session, from, to } => self.edit_session(session, |s| {
                let from_id = s.node_id(from)?;
                let mesh = match s.model.node_mesh(&from_id) {
                    Some(mesh) => mesh.clone(),
                    None if s.model.node(&from_id).is_some() => {
                        return Err(EditorError::BadTarget("not a meshed node".into()))
                    }
                    None => return Err(EditorError::NoNode(from)),
                };
                let to = s.node_id(to)?;
                s.model.set_mesh_with_refit(&to, mesh)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::Undo { session } => {
                let handle = self.session(session)?;
                lock(&handle).undo()?;
                Ok(ResponseBody::Empty)
            }
            Command::Redo { session } => {
                let handle = self.session(session)?;
                lock(&handle).redo()?;
                Ok(ResponseBody::Empty)
            }
            Command::PhysicsAdd {
                session,
                parent,
                name,
                kind,
                target_param,
                length,
                gravity,
                frequency,
                angle_damping,
                length_damping,
            } => self.edit_session(session, |s| {
                let phys_kind = parse_pendulum_kind(&kind)
                    .ok_or_else(|| EditorError::BadTarget(kind.clone()))?;
                let target = match target_param {
                    Some(p) => {
                        let pid = s.param_id(p)?;
                        if s.model.param(&pid).is_none() {
                            return Err(EditorError::BadTarget("target param".into()));
                        }
                        Some(pid)
                    }
                    None => None,
                };
                let mut phys = ModelPhysics::new(phys_kind);
                if let Some(v) = gravity {
                    phys.gravity = v;
                }
                if let Some(v) = length {
                    phys.length = v;
                }
                if let Some(v) = frequency {
                    phys.frequency = v;
                }
                if let Some(v) = angle_damping {
                    phys.angle_damping = v;
                }
                if let Some(v) = length_damping {
                    phys.length_damping = v;
                }
                let node = ModelNode::new(
                    name.unwrap_or_else(|| "Physics".into()),
                    ModelNodeKind::SimplePhysics(phys),
                );
                let parent = s.node_id(parent)?;
                let handle = s.add_node(&parent, node)?;
                let id = s.node_id(handle)?;
                s.model.set_physics_targets(&id, [target, None])?;
                s.touch();
                Ok(ResponseBody::Node { node: handle })
            }),
            Command::DeformSet {
                session,
                param,
                node,
                cell,
                translate,
                rotate,
                scale,
            } => self.edit_session(session, |s| {
                let key = s.binding_key(param, node, BindingTarget::Deform)?;
                s.model.set_deform_from_transform(
                    &key,
                    cell,
                    translate.unwrap_or([0.0, 0.0]),
                    rotate.unwrap_or(0.0),
                    scale.unwrap_or([1.0, 1.0]),
                )?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingAdd {
                session,
                param,
                node,
                target,
            } => self.edit_session(session, |s| {
                let t = ScalarTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = s.binding_key(param, node, BindingTarget::Scalar(t))?;
                s.model.add_binding(&key)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingKey {
                session,
                param,
                node,
                target,
                cell,
                value,
            } => self.edit_session(session, |s| {
                let t = ScalarTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = s.binding_key(param, node, BindingTarget::Scalar(t))?;
                s.model.set_binding_key(&key, cell, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::PresenceSet { session, presence } => {
                // Deliberately not via with_session: presence must not bump rev,
                // snapshot, or record undo — it is not the document.
                let handle = self.session(session)?;
                lock(&handle).presence = Some(presence);
                Ok(ResponseBody::Empty)
            }
            Command::PresenceGet { session } => {
                let handle = self.session(session)?;
                let presence = lock(&handle).presence.clone();
                Ok(ResponseBody::Presence { presence })
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::Preview {
                session,
                params,
                size,
                out,
            } => self.run_preview(session, params, size, out),
            #[cfg(target_arch = "wasm32")]
            Command::SessionOpen { .. }
            | Command::SessionImport { .. }
            | Command::Save { .. }
            | Command::ExportManifest { .. }
            | Command::TextureAdd { .. }
            | Command::Preview { .. } => Err(EditorError::NativeOnly),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn run_preview(
        &self,
        session: SessionId,
        params: Vec<ParamValue>,
        size: Option<[u32; 2]>,
        out: Option<String>,
    ) -> Result<ResponseBody, EditorError> {
        let [width, height] = size.unwrap_or([512, 512]);
        let seq = self.preview_seq.fetch_add(1, Ordering::Relaxed);
        let out_path = match out {
            Some(p) => PathBuf::from(p),
            None => std::env::temp_dir()
                .join("catchlight-editor")
                .join(format!("preview-{}-{seq}.png", session.0)),
        };
        let named: Vec<(String, Vec2)> = params
            .into_iter()
            .map(|p| (p.name, Vec2::new(p.x, p.y)))
            .collect();

        let handle = self.session(session)?;
        // The model is cloned rather than borrowed so the session lock is not
        // held across the GPU render; a clone is a shallow copy sharing every
        // heavy leaf, and it carries the same identity and generation, so the
        // puppet and the cache accept it as the model they were built from.
        let (model, mut puppet, rev) = {
            let mut s = lock(&handle);
            s.puppet();
            let puppet = s
                .puppet
                .take()
                .ok_or_else(|| EditorError::Preview("puppet build failed".into()))?;
            (s.model.clone(), puppet, s.rev)
        };
        let pose = pose_by_name(&model, &named);

        let render_result = (|| {
            let mut prev = lock(&self.preview);
            if prev.is_none() {
                *prev =
                    Some(PreviewRenderer::new().map_err(|e| EditorError::Preview(e.to_string()))?);
            }
            let pr = prev
                .as_mut()
                .ok_or_else(|| EditorError::Preview("renderer unavailable".into()))?;
            pr.render_png(
                session,
                &model,
                &mut puppet,
                &pose,
                width,
                height,
                DEFAULT_CAMERA_HEIGHT,
                &out_path,
            )
            .map_err(|e| EditorError::Preview(e.to_string()))
        })();

        {
            let mut s = lock(&handle);
            if s.rev == rev && s.puppet.is_none() {
                s.puppet = Some(puppet);
            }
        }
        render_result?;
        Ok(ResponseBody::Preview {
            preview: PreviewInfo {
                path: out_path.display().to_string(),
                width,
                height,
            },
        })
    }
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
pub use catchlight_editor_protocol::default_socket_path;
#[cfg(unix)]
pub use transport::serve_unix;
#[cfg(all(unix, test))]
use transport::{
    bind_unix_listener, serve_connection, ConnectionLimiter, MAX_REQUEST_BYTES,
    MAX_SOCKET_CONNECTIONS,
};

/// The pose `named` describes, keyed by Id.
///
/// The protocol still names params — its handles are Ids only for nodes,
/// params and textures, and a pose is not one (cl-32i.12 changes that). A
/// model's params are scalar, so a name that was one 2-D param upstream is
/// looked up as the split pair `<n>.x` / `<n>.y` and takes both components;
/// a name that resolves directly takes `x` and ignores `y`. A name that
/// resolves to nothing is dropped, exactly as posing an unknown param by name
/// always was.
pub fn pose_by_name(model: &Model, named: &[(String, Vec2)]) -> Pose {
    let id_of = |name: &str| {
        model
            .param_ids()
            .iter()
            .find(|id| model.param(id).is_some_and(|p| p.name.as_str() == name))
            .cloned()
    };
    let mut pose = Pose::new();
    for (name, value) in named {
        if let Some(id) = id_of(name) {
            pose.set(id, value.x);
            continue;
        }
        if let Some(id) = id_of(&format!("{name}.x")) {
            pose.set(id, value.x);
        }
        if let Some(id) = id_of(&format!("{name}.y")) {
            pose.set(id, value.y);
        }
    }
    pose
}

/// Legacy 2-D re-pairing: fold a pose keyed by a model's scalar param names
/// into the `<n>` names the *legacy* runtime's 2-D params carry, pairing
/// `<n>.x` with `<n>.y` and passing everything else through.
///
/// Nothing in the tree calls this any more — the preview poses the model's
/// own scalar params through [`pose_by_name`] — and it is kept, unchanged,
/// only for a client still speaking the old shape. cl-32i.12 decides its
/// fate with the rest of the protocol.
pub fn fold_pose(model: &Model, pose: &[(String, Vec2)]) -> Vec<(String, Vec2)> {
    let value_of = |name: &str| pose.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
    let ids = model.param_ids();
    let mut out = Vec::with_capacity(pose.len());
    let mut i = 0;
    while i < ids.len() {
        let Some(p) = model.param(&ids[i]) else {
            i += 1;
            continue;
        };
        let partner = ids.get(i + 1).and_then(|id| model.param(id)).filter(|q| {
            p.name
                .as_str()
                .strip_suffix(".x")
                .is_some_and(|base| !base.is_empty() && q.name.as_str() == format!("{base}.y"))
        });
        match partner {
            Some(q) => {
                let (x, y) = (value_of(p.name.as_str()), value_of(q.name.as_str()));
                if x.is_some() || y.is_some() {
                    let base = p.name.as_str().trim_end_matches(".x").to_string();
                    out.push((
                        base,
                        Vec2::new(x.map_or(p.default, |v| v.x), y.map_or(q.default, |v| v.x)),
                    ));
                }
                i += 2;
            }
            None => {
                if let Some(v) = value_of(p.name.as_str()) {
                    out.push((p.name.to_string(), v));
                }
                i += 1;
            }
        }
    }
    out
}

/// A param has one axis; the protocol still names two (cl-32i.13 drops it).
fn only_x_axis(axis: u8) -> Result<(), EditorError> {
    if axis == 0 {
        Ok(())
    } else {
        Err(EditorError::BadTarget(
            "params are scalar: only axis 0 exists".into(),
        ))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn build_tree(model: &Model, refs: &mut RefMap, id: &NodeId) -> TreeNode {
    let (name, kind, z_order, enabled, children) = match model.node(id) {
        Some(n) => (
            n.name.to_string(),
            n.kind.name().to_string(),
            n.z_order,
            n.enabled,
            n.children().to_vec(),
        ),
        None => (String::new(), "group".to_string(), 0.0, true, Vec::new()),
    };
    TreeNode {
        node: refs.node(id),
        name,
        kind,
        z_order,
        enabled,
        children: children
            .iter()
            .map(|c| build_tree(model, refs, c))
            .collect(),
    }
}

/// In-process document view handed to observers (the GUI); refs are stable for
/// the session's lifetime.
#[derive(Debug, Clone)]
pub struct DocSnapshot {
    pub rev: u64,
    pub root: TreeNode,
    pub params: Vec<ParamInfo>,
}

fn param_infos(model: &Model, refs: &mut RefMap) -> Vec<ParamInfo> {
    let mut out = Vec::with_capacity(model.param_ids().len());
    for pid in model.param_ids() {
        let (Some(p), Ok(keys)) = (model.param(pid), model.key_count(pid)) else {
            continue;
        };
        // Params are scalars now, so the wire's second axis is always the one
        // degenerate position; cl-32i.13 drops it from the protocol.
        out.push(ParamInfo {
            param: refs.param(pid),
            name: p.name.to_string(),
            vec2: false,
            min: [p.min, 0.0],
            max: [p.max, 0.0],
            defaults: [p.default, 0.0],
            axis: [keys, 1],
            axis_points_x: p.key_positions.clone(),
            axis_points_y: vec![0.0],
            bindings: model.bindings_of_param(pid).count() as u32,
        });
    }
    out
}

fn default_name(kind: NodeKindArg) -> String {
    match kind {
        NodeKindArg::Group => "Group",
        NodeKindArg::Part => "Part",
        NodeKindArg::Composite => "Composite",
        NodeKindArg::MeshGroup => "MeshGroup",
    }
    .to_string()
}

fn make_kind(kind: NodeKindArg) -> ModelNodeKind {
    match kind {
        NodeKindArg::Group => ModelNodeKind::Group,
        NodeKindArg::Part => ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
        NodeKindArg::Composite => ModelNodeKind::Composite(ModelComposite::new()),
        NodeKindArg::MeshGroup => ModelNodeKind::MeshGroup(ModelMeshGroup::new(ClmMesh::default())),
    }
}

fn apply_patch(n: &mut ModelNode, patch: &NodePatch) -> Result<(), EditorError> {
    // Parse before mutating so a bad enum string leaves the node untouched.
    let blend = patch
        .blend_mode
        .as_deref()
        .map(|s| BlendMode::from_name(s).ok_or_else(|| EditorError::BadTarget(s.to_string())))
        .transpose()?;
    if let Some(name) = &patch.name {
        n.name = Name::truncated(name);
    }
    if let Some(t) = patch.translate {
        n.transform.translation = t;
    }
    if let Some(r) = patch.rotate {
        n.transform.rotation = r;
    }
    if let Some(sc) = patch.scale {
        n.transform.scale = sc;
    }
    if let Some(z) = patch.z_order {
        n.z_order = z;
    }
    if let Some(en) = patch.enabled {
        n.enabled = en;
    }
    if let Some(v) = patch.lock_to_root {
        n.lock_to_root = v;
    }
    if let Some(op) = patch.opacity {
        set_opacity(&mut n.kind, op);
    }
    if let Some(mode) = blend {
        match &mut n.kind {
            ModelNodeKind::Part(p) => p.blend_mode = mode,
            ModelNodeKind::Composite(c) => c.blend_mode = mode,
            _ => {}
        }
    }
    if let Some(t) = patch.tint {
        match &mut n.kind {
            ModelNodeKind::Part(p) => p.tint = t,
            ModelNodeKind::Composite(c) => c.tint = t,
            _ => {}
        }
    }
    if let Some(t) = patch.screen_tint {
        match &mut n.kind {
            ModelNodeKind::Part(p) => p.screen_tint = t,
            ModelNodeKind::Composite(c) => c.screen_tint = t,
            _ => {}
        }
    }
    if let Some(th) = patch.mask_threshold {
        match &mut n.kind {
            ModelNodeKind::Part(p) => p.mask_threshold = th,
            ModelNodeKind::Composite(c) => c.mask_threshold = th,
            _ => {}
        }
    }
    if let Some(v) = patch.propagate_meshgroup {
        if let ModelNodeKind::Composite(c) = &mut n.kind {
            c.propagate_meshgroup = v;
        }
    }
    if let Some(v) = patch.mg_dynamic {
        if let ModelNodeKind::MeshGroup(mg) = &mut n.kind {
            mg.dynamic = v;
        }
    }
    if let Some(v) = patch.mg_translate_children {
        if let ModelNodeKind::MeshGroup(mg) = &mut n.kind {
            mg.translate_children = v;
        }
    }
    Ok(())
}

fn set_opacity(kind: &mut ModelNodeKind, op: f32) {
    match kind {
        ModelNodeKind::Part(p) => p.opacity = op,
        ModelNodeKind::Composite(c) => c.opacity = op,
        _ => {}
    }
}

/// Accepts both the protocol's lowercase names (rigid | spring) and the
/// serde/CamelCase forms, so PhysicsAdd and PhysicsSet agree.
fn parse_pendulum_kind(s: &str) -> Option<PendulumKind> {
    match s.to_ascii_lowercase().as_str() {
        "rigid" | "rigidpendulum" | "pendulum" => Some(PendulumKind::RigidPendulum),
        "spring" | "springpendulum" => Some(PendulumKind::SpringPendulum),
        _ => None,
    }
}

/// xy | yx | angle_length | length_angle (case-insensitive, CamelCase too).
fn parse_map_mode(s: &str) -> Option<PhysicsParamMapMode> {
    match s.to_ascii_lowercase().replace('_', "").as_str() {
        "xy" => Some(PhysicsParamMapMode::XY),
        "yx" => Some(PhysicsParamMapMode::YX),
        "anglelength" => Some(PhysicsParamMapMode::AngleLength),
        "lengthangle" => Some(PhysicsParamMapMode::LengthAngle),
        _ => None,
    }
}

fn parse_interpolate_mode(
    s: &str,
) -> Result<catchlight_core::params::InterpolateMode, EditorError> {
    use catchlight_core::params::InterpolateMode as I;
    match s.to_ascii_lowercase().as_str() {
        "nearest" => Ok(I::Nearest),
        "stepped" => Ok(I::Stepped),
        "linear" => Ok(I::Linear),
        "cubic" => Ok(I::Cubic),
        other => Err(EditorError::BadTarget(other.to_string())),
    }
}

fn parse_mask_mode(s: &str) -> Result<MaskMode, EditorError> {
    match s.to_ascii_lowercase().as_str() {
        "mask" => Ok(MaskMode::Mask),
        "dodge" | "dodge_mask" | "dodgemask" => Ok(MaskMode::DodgeMask),
        other => Err(EditorError::BadTarget(other.to_string())),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn encoding_from_path(path: &str) -> TextureEncoding {
    if path.to_ascii_lowercase().ends_with(".tga") {
        TextureEncoding::Tga
    } else {
        TextureEncoding::Png
    }
}

fn image_dims(bytes: &[u8]) -> Result<(u32, u32), EditorError> {
    catchlight_editor_core::image_dims(bytes).map_err(|e| EditorError::Image(e.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::{BufRead, BufReader, Write};
    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};

    #[cfg(unix)]
    static SOCKET_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[cfg(unix)]
    fn socket_test_path(label: &str) -> PathBuf {
        let sequence = SOCKET_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "catchlight-{label}-{}-{sequence}.sock",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    #[test]
    fn socket_startup_does_not_replace_a_regular_file() {
        let path = socket_test_path("regular");
        std::fs::write(&path, b"keep").unwrap();

        let err = bind_unix_listener(&path).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn socket_startup_preserves_a_live_listener() {
        let path = socket_test_path("live");
        let listener = bind_unix_listener(&path).unwrap();

        let err = bind_unix_listener(&path).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        UnixStream::connect(&path).unwrap();
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn socket_startup_replaces_a_stale_socket() {
        let path = socket_test_path("stale");
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);

        let listener = bind_unix_listener(&path).unwrap();

        UnixStream::connect(&path).unwrap();
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn socket_connection_limit_releases_capacity_on_drop() {
        let limiter = Arc::new(ConnectionLimiter::default());
        let mut permits = (0..MAX_SOCKET_CONNECTIONS)
            .map(|_| limiter.try_acquire().unwrap())
            .collect::<Vec<_>>();
        assert!(limiter.try_acquire().is_none());

        permits.pop();

        assert!(limiter.try_acquire().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn socket_connection_rejects_an_oversized_request() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let editor = Editor::new();
            serve_connection(&editor, server);
        });
        client
            .write_all(&vec![b'x'; MAX_REQUEST_BYTES + 1])
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();

        assert!(matches!(
            reply,
            Reply::Err { id: 0, message } if message.contains("request exceeds")
        ));
        server_thread.join().unwrap();
    }

    fn body(reply: Reply) -> ResponseBody {
        match reply {
            Reply::Ok { body, .. } => body,
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    fn req(id: u64, command: Command) -> Request {
        Request { id, command }
    }

    fn session_of(b: ResponseBody) -> SessionId {
        match b {
            ResponseBody::Session { session } => session,
            other => panic!("expected session, got {other:?}"),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn independent_sessions_do_not_share_a_command_lock() {
        let editor = Arc::new(Editor::new());
        let first = session_of(body(
            editor.handle(req(1, Command::SessionNew { name: None })),
        ));
        let second = session_of(body(
            editor.handle(req(2, Command::SessionNew { name: None })),
        ));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocking_editor = editor.clone();
        let blocking = std::thread::spawn(move || {
            blocking_editor
                .with_model(first, |_, _| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .unwrap();
        });
        entered_rx.recv().unwrap();

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let querying_editor = editor.clone();
        let query = std::thread::spawn(move || {
            done_tx.send(querying_editor.is_dirty(second)).unwrap();
        });
        let result = done_rx.recv_timeout(std::time::Duration::from_secs(1));
        release_tx.send(()).unwrap();
        blocking.join().unwrap();
        query.join().unwrap();

        assert!(!result.unwrap().unwrap());
    }

    #[test]
    fn history_byte_budget_keeps_the_newest_snapshot() {
        let mut session = Session::new(Model::new(), "history".into(), None);
        for name in ["first", "second", "third"] {
            let mut model = Model::new();
            let root = model.root().unwrap().clone();
            model
                .update_node(&root, |n| n.name = Name::truncated(name))
                .unwrap();
            let entry = HistoryEntry::new(model);
            session.history_bytes = session.history_bytes.saturating_add(entry.bytes);
            session.undo.push(entry);
        }
        let newest_bytes = session.undo.last().unwrap().bytes;

        session.trim_history_to(UNDO_DEPTH, newest_bytes);

        assert_eq!(session.undo.len(), 1);
        assert_eq!(
            session.undo[0]
                .model
                .node(session.undo[0].model.root().unwrap())
                .unwrap()
                .name
                .as_str(),
            "third"
        );
        assert_eq!(session.history_bytes, newest_bytes);
    }

    #[test]
    fn session_node_save_reopen_lifecycle() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: Some("t".into()),
            },
        ))));

        let root = match body(ed.handle(req(2, Command::NodeTree { session: s }))) {
            ResponseBody::Tree { root } => root.node,
            other => panic!("{other:?}"),
        };
        let node = match body(ed.handle(req(
            3,
            Command::NodeAdd {
                session: s,
                parent: root,
                kind: NodeKindArg::Group,
                name: Some("A".into()),
            },
        ))) {
            ResponseBody::Node { node } => node,
            other => panic!("{other:?}"),
        };

        ed.handle(req(
            4,
            Command::NodeSet {
                session: s,
                node,
                patch: NodePatch {
                    translate: Some([1.0, 2.0, 3.0]),
                    ..Default::default()
                },
            },
        ));

        let status = match body(ed.handle(req(5, Command::Status { session: s }))) {
            ResponseBody::Status { status } => status,
            other => panic!("{other:?}"),
        };
        assert_eq!(status.node_count, 2);
        assert!(status.dirty);

        let tmp = std::env::temp_dir().join(format!("catchlight-srv-{}.clm", std::process::id()));
        assert!(matches!(
            body(ed.handle(req(
                6,
                Command::Save {
                    session: s,
                    path: Some(tmp.display().to_string()),
                },
            ))),
            ResponseBody::Saved { .. }
        ));

        // a fresh session opened from the saved file has the same node count.
        let s2 = session_of(body(ed.handle(req(
            7,
            Command::SessionOpen {
                path: tmp.display().to_string(),
            },
        ))));
        let st2 = match body(ed.handle(req(8, Command::Status { session: s2 }))) {
            ResponseBody::Status { status } => status,
            other => panic!("{other:?}"),
        };
        assert_eq!(st2.node_count, 2);
        assert!(!st2.dirty, "freshly opened session is clean");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn delete_then_save_drops_the_node() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        let root = match body(ed.handle(req(2, Command::NodeTree { session: s }))) {
            ResponseBody::Tree { root } => root.node,
            other => panic!("{other:?}"),
        };
        let node = match body(ed.handle(req(
            3,
            Command::NodeAdd {
                session: s,
                parent: root,
                kind: NodeKindArg::Part,
                name: None,
            },
        ))) {
            ResponseBody::Node { node } => node,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            body(ed.handle(req(4, Command::NodeDelete { session: s, node }))),
            ResponseBody::Empty
        ));
        let status = match body(ed.handle(req(5, Command::Status { session: s }))) {
            ResponseBody::Status { status } => status,
            other => panic!("{other:?}"),
        };
        assert_eq!(status.node_count, 1);
    }

    #[test]
    fn unknown_session_is_an_error_reply() {
        let ed = Editor::new();
        assert!(matches!(
            ed.handle(req(
                1,
                Command::Status {
                    session: SessionId(999)
                }
            )),
            Reply::Err { .. }
        ));
    }

    fn node_count(ed: &Editor, s: SessionId, id: u64) -> u32 {
        match body(ed.handle(req(id, Command::Status { session: s }))) {
            ResponseBody::Status { status } => status.node_count,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn undo_redo_round_trips() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        let root = match body(ed.handle(req(2, Command::NodeTree { session: s }))) {
            ResponseBody::Tree { root } => root.node,
            other => panic!("{other:?}"),
        };
        for id in 3..=4 {
            ed.handle(req(
                id,
                Command::NodeAdd {
                    session: s,
                    parent: root,
                    kind: NodeKindArg::Group,
                    name: None,
                },
            ));
        }
        assert_eq!(node_count(&ed, s, 5), 3);
        assert!(matches!(
            ed.handle(req(6, Command::Undo { session: s })),
            Reply::Ok { .. }
        ));
        assert!(matches!(
            ed.handle(req(7, Command::Undo { session: s })),
            Reply::Ok { .. }
        ));
        assert_eq!(node_count(&ed, s, 8), 1);
        assert!(matches!(
            ed.handle(req(9, Command::Undo { session: s })),
            Reply::Err { .. }
        ));
        assert!(matches!(
            ed.handle(req(10, Command::Redo { session: s })),
            Reply::Ok { .. }
        ));
        assert_eq!(node_count(&ed, s, 11), 2);
    }

    #[test]
    fn doc_snapshot_is_rev_gated() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        let a = ed.doc_snapshot(s).unwrap();
        let b = ed.doc_snapshot(s).unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "unchanged session returns the cached snapshot"
        );
        ed.handle(req(
            2,
            Command::NodeAdd {
                session: s,
                parent: a.root.node,
                kind: NodeKindArg::Group,
                name: None,
            },
        ));
        let c = ed.doc_snapshot(s).unwrap();
        assert!(c.rev > a.rev);
        assert_eq!(c.root.children.len(), 1);
    }

    #[test]
    fn presence_is_off_the_document_path() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        let snap0 = ed.doc_snapshot(s).unwrap();
        let presence = Presence {
            pose: vec![ParamValue {
                name: "x".into(),
                x: 1.0,
                y: 0.0,
            }],
            camera: None,
            selection: None,
        };
        assert!(matches!(
            ed.handle(req(
                2,
                Command::PresenceSet {
                    session: s,
                    presence
                }
            )),
            Reply::Ok { .. }
        ));
        // presence must not change the document: same cached snapshot Arc.
        let snap1 = ed.doc_snapshot(s).unwrap();
        assert!(Arc::ptr_eq(&snap0, &snap1));
        match body(ed.handle(req(3, Command::PresenceGet { session: s }))) {
            ResponseBody::Presence { presence: Some(p) } => assert_eq!(p.pose[0].name, "x"),
            other => panic!("{other:?}"),
        }
        match body(ed.handle(req(4, Command::Status { session: s }))) {
            ResponseBody::Status { status } => {
                assert_eq!(status.rev, 0, "presence does not bump rev");
                assert!(!status.dirty, "presence does not dirty the document");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn preview_renders_png_smoke() {
        let dir = std::env::temp_dir().join(format!("catchlight-prev-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(32, 32);
        for px in img.pixels_mut() {
            *px = image::Rgba([220, 40, 40, 255]);
        }
        img.save(dir.join("face.png")).unwrap();
        std::fs::write(
            dir.join("m.json"),
            r#"{"textures":[{"id":"face","path":"face.png"}],
               "nodes":[{"id":"face","kind":"part","texture":"face","mesh":{"auto":"quad"}}]}"#,
        )
        .unwrap();

        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionImport {
                manifest_path: dir.join("m.json").display().to_string(),
            },
        ))));
        let out = dir.join("preview.png");
        match ed.handle(req(
            2,
            Command::Preview {
                session: s,
                params: vec![],
                size: Some([128, 128]),
                out: Some(out.display().to_string()),
            },
        )) {
            Reply::Ok {
                body: ResponseBody::Preview { preview },
                ..
            } => {
                assert_eq!(preview.width, 128);
                assert!(
                    std::fs::metadata(&preview.path).unwrap().len() > 0,
                    "preview png is empty"
                );
            }
            other => panic!("preview failed: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Preview request names params the way a person does, and the model's
    /// params are scalar, so the name has to resolve against the model rather
    /// than against a re-paired 2-D bridge. Pixels are what says it landed: a
    /// preview that quietly rendered at rest would still write a valid PNG.
    #[test]
    fn a_preview_honours_the_pose_it_is_given() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/models/welded_seam.clm"
        ))
        .unwrap();
        let dir = std::env::temp_dir().join(format!("catchlight-posed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ed = Editor::new();
        let s = ed.open_bytes("welded_seam", &bytes).unwrap();
        let shot = |id: u64, name: &str, params: Vec<ParamValue>| -> Vec<u8> {
            let out = dir.join(format!("{name}.png"));
            match ed.handle(req(
                id,
                Command::Preview {
                    session: s,
                    params,
                    // welded_seam spans 300x240 world units, well inside the
                    // default 2000-unit camera.
                    size: Some([256, 256]),
                    out: Some(out.display().to_string()),
                },
            )) {
                Reply::Ok {
                    body: ResponseBody::Preview { .. },
                    ..
                } => std::fs::read(&out).unwrap(),
                other => panic!("preview failed: {other:?}"),
            }
        };

        let rest = shot(1, "rest", Vec::new());
        let posed = shot(
            2,
            "posed",
            vec![ParamValue {
                name: "pull".into(),
                x: 1.0,
                y: 0.0,
            }],
        );
        let back = shot(3, "back", Vec::new());

        assert_ne!(rest, posed, "posing pull must change the preview");
        assert_eq!(rest, back, "leaving pull out must render it at its default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
