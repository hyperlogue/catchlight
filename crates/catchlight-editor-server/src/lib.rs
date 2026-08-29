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
#[cfg(unix)]
mod transport;

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clp::{ClpIndices, ClpMesh, TextureAlpha, TextureEncoding};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
#[cfg(not(target_arch = "wasm32"))]
use catchlight_core::Vec2;
use catchlight_core::{from_clp_cached, Puppet, TexturePrepCache};
use catchlight_editor_core::{
    BindingTarget, EditComposite, EditError, EditMeshGroup, EditModel, EditNode, EditNodeKind,
    EditParam, EditPart, EditPhysics, EditTexture, ManifestError, NodeId, ParamId, ScalarTarget,
    TexId,
};
#[cfg(not(target_arch = "wasm32"))]
use catchlight_editor_core::{Manifest, TextureData};

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
    Edit(#[from] EditError),
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
    model: EditModel,
    title: String,
    file: Option<PathBuf>,
    rev: u64,
    saved_rev: u64,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    history_bytes: usize,
    /// Lazily (re)built from `model` for preview; invalidated on every edit.
    puppet: Option<Puppet>,
    puppet_dirty: bool,
    /// rev-gated document view for in-process observers (the GUI).
    snapshot: Option<Arc<DocSnapshot>>,
    /// Latest shared view state — its own path, never touches the model/rev.
    presence: Option<Presence>,
    /// Texture decode/crop memo across puppet rebuilds — the rebuild runs on
    /// every edit and must not re-decode unchanged textures.
    tex_cache: TexturePrepCache,
}

struct HistoryEntry {
    model: EditModel,
    bytes: usize,
}

impl HistoryEntry {
    fn new(model: EditModel) -> Self {
        let bytes = model.estimated_size_bytes();
        Self { model, bytes }
    }
}

impl Session {
    fn new(model: EditModel, title: String, file: Option<PathBuf>) -> Self {
        Self {
            model,
            title,
            file,
            rev: 0,
            saved_rev: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            history_bytes: 0,
            puppet: None,
            puppet_dirty: true,
            snapshot: None,
            presence: None,
            tex_cache: TexturePrepCache::default(),
        }
    }

    fn touch(&mut self) {
        self.rev += 1;
        self.puppet_dirty = true;
    }

    fn push_undo(&mut self, snapshot: EditModel) {
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

    fn puppet(&mut self) -> Result<&mut Puppet, EditorError> {
        if self.puppet.is_none() || self.puppet_dirty {
            let file = self.model.flatten()?;
            let mut built = from_clp_cached(&file, 0, &mut self.tex_cache)
                .map_err(|e| EditorError::Preview(e.to_string()))?;
            // Frozen physics keeps the authoring preview deterministic (the
            // dt=0 tick can't integrate anyway) and lets physics-driven
            // params be posed by hand.
            built.set_physics_enabled(false);
            self.puppet = Some(built);
            self.puppet_dirty = false;
        }
        self.puppet
            .as_mut()
            .ok_or_else(|| EditorError::Preview("puppet build failed".into()))
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

    /// Open a `.clp` from in-memory bytes — the browser file-picker path, and
    /// the only document-open the wasm build has (no filesystem there).
    pub fn open_bytes(
        &self,
        title: impl Into<String>,
        bytes: &[u8],
    ) -> Result<SessionId, EditorError> {
        let model = EditModel::from_clp_bytes(bytes)?;
        let id = self.alloc_id();
        self.insert_session(id, Session::new(model, title.into(), None));
        Ok(id)
    }

    /// Serialize a session to `.clp` bytes; the caller owns where the bytes
    /// land (blob download, OPFS, a file) and confirms with [`Self::mark_saved`]
    /// once they actually landed — a failed download must not clear the dirty
    /// flag.
    pub fn save_bytes(&self, id: SessionId) -> Result<Vec<u8>, EditorError> {
        self.with_session(id, |s| Ok(s.model.to_clp_bytes()?))
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
        f: impl FnOnce(&EditModel) -> R,
    ) -> Result<R, EditorError> {
        let session = self.session(id)?;
        let s = lock(&session);
        Ok(f(&s.model))
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
            let tex = s.model.add_texture(EditTexture {
                encoding,
                alpha: TextureAlpha::Straight,
                data: Arc::new(bytes),
            });
            s.touch();
            Ok(TexRef(tex.to_ffi()))
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
                session.puppet_dirty = true;
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
        let snap = Arc::new(DocSnapshot {
            rev: session.rev,
            root: build_tree(&session.model, session.model.root()),
            params: param_infos(&session.model),
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

    /// Run `f` with the session's built puppet, rebuilding it lazily if the
    /// document changed. The in-process GUI viewport renders it on eframe's own
    /// wgpu device (no readback); the puppet never leaves the session lock.
    pub fn with_puppet<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Puppet) -> R,
    ) -> Result<R, EditorError> {
        let session = self.session(id)?;
        let mut s = lock(&session);
        Ok(f(s.puppet()?))
    }

    fn dispatch(&self, cmd: Command) -> Result<ResponseBody, EditorError> {
        match cmd {
            Command::SessionNew { name } => {
                let id = self.alloc_id();
                let title = name.unwrap_or_else(|| format!("untitled-{}", id.0));
                self.insert_session(id, Session::new(EditModel::new(), title, None));
                Ok(ResponseBody::Session { session: id })
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::SessionOpen { path } => {
                let path = PathBuf::from(path);
                let model = EditModel::from_clp_bytes(&std::fs::read(&path)?)?;
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
                let model = EditModel::from_manifest(&manifest, &data)?;
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
                    (path, s.model.to_clp_bytes()?, s.rev)
                };
                // Temp-then-rename: an interrupted save must not truncate the
                // user's only copy.
                let tmp = path.with_extension("clp.tmp");
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
                        .filter_map(|(i, &tid)| {
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
                Ok(ResponseBody::Tree {
                    root: build_tree(&s.model, s.model.root()),
                })
            }),
            Command::NodeAdd {
                session,
                parent,
                kind,
                name,
            } => self.edit_session(session, |s| {
                let node =
                    EditNode::new(name.unwrap_or_else(|| default_name(kind)), make_kind(kind));
                let id = s.model.add_node(NodeId::from_ffi(parent.0), node)?;
                s.touch();
                Ok(ResponseBody::Node {
                    node: NodeRef(id.to_ffi()),
                })
            }),
            Command::NodeSet {
                session,
                node,
                patch,
            } => self.edit_session(session, |s| {
                if let Some(tref) = patch.texture {
                    if s.model.texture(TexId::from_ffi(tref.0)).is_none() {
                        return Err(EditorError::NoTexture(tref));
                    }
                }
                let n = s
                    .model
                    .node_mut(NodeId::from_ffi(node.0))
                    .ok_or(EditorError::NoNode(node))?;
                apply_patch(n, &patch)?;
                s.touch();
                Ok(ResponseBody::Node { node })
            }),
            Command::NodeReparent { session, node, to } => self.edit_session(session, |s| {
                s.model
                    .reparent(NodeId::from_ffi(node.0), NodeId::from_ffi(to.0))?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeReorder {
                session,
                node,
                index,
            } => self.edit_session(session, |s| {
                s.model.reorder(NodeId::from_ffi(node.0), index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeMove {
                session,
                node,
                parent,
                index,
            } => self.edit_session(session, |s| {
                let id = NodeId::from_ffi(node.0);
                s.model.reparent(id, NodeId::from_ffi(parent.0))?;
                // reorder can only fail on unknown/root, both excluded by the
                // successful reparent — the combined edit stays atomic.
                s.model.reorder(id, index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeDuplicate { session, node } => self.edit_session(session, |s| {
                let copy = s.model.duplicate_subtree(NodeId::from_ffi(node.0))?;
                s.touch();
                Ok(ResponseBody::Node {
                    node: NodeRef(copy.to_ffi()),
                })
            }),
            Command::MaskAdd {
                session,
                node,
                source,
                mode,
            } => self.edit_session(session, |s| {
                let mode = parse_mask_mode(&mode)?;
                s.model
                    .mask_add(NodeId::from_ffi(node.0), NodeId::from_ffi(source.0), mode)?;
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
                s.model
                    .mask_set_mode(NodeId::from_ffi(node.0), index as usize, mode)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MaskReorder {
                session,
                node,
                index,
                to,
            } => self.edit_session(session, |s| {
                s.model
                    .mask_reorder(NodeId::from_ffi(node.0), index as usize, to as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MaskDelete {
                session,
                node,
                index,
            } => self.edit_session(session, |s| {
                s.model
                    .mask_delete(NodeId::from_ffi(node.0), index as usize)?;
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
                        let pid = ParamId::from_ffi(p.0);
                        if s.model.param(pid).is_none() {
                            return Err(EditorError::BadTarget("target param".into()));
                        }
                        Some(Some(pid))
                    }
                    (false, None) => None,
                };
                let n = s
                    .model
                    .node_mut(NodeId::from_ffi(node.0))
                    .ok_or(EditorError::NoNode(node))?;
                let EditNodeKind::SimplePhysics(ph) = &mut n.kind else {
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
                if let Some(t) = target {
                    ph.target_param = t;
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
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::PhysicsGlobals {
                session,
                gravity,
                pixels_per_meter,
            } => self.edit_session(session, |s| {
                if let Some(g) = gravity {
                    s.model.physics.gravity = g;
                }
                if let Some(ppm) = pixels_per_meter {
                    s.model.physics.pixels_per_meter = ppm;
                }
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeDelete { session, node } => self.edit_session(session, |s| {
                s.model.delete_node(NodeId::from_ffi(node.0))?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            #[cfg(not(target_arch = "wasm32"))]
            Command::TextureAdd { session, path } => self.edit_session(session, |s| {
                let bytes = std::fs::read(&path)?;
                image_dims(&bytes)?; // validate it decodes
                let id = s.model.add_texture(EditTexture {
                    encoding: encoding_from_path(&path),
                    alpha: TextureAlpha::Straight,
                    data: Arc::new(bytes),
                });
                s.touch();
                Ok(ResponseBody::Texture {
                    texture: TexRef(id.to_ffi()),
                })
            }),
            Command::TextureList { session } => self.with_session(session, |s| {
                let mut textures = Vec::new();
                for &tid in s.model.texture_ids() {
                    if let Some(t) = s.model.texture(tid) {
                        let (width, height) = image_dims(&t.data).unwrap_or((0, 0));
                        textures.push(TexInfo {
                            texture: TexRef(tid.to_ffi()),
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
                if !catchlight_editor_core::param_range_is_valid(vec2, min, max) {
                    return Err(EditError::CellOutOfRange.into());
                }
                let axis_points_x = if axis_x.is_empty() {
                    vec![0.0, 1.0]
                } else {
                    axis_x
                };
                let axis_points_y = if vec2 {
                    if axis_y.is_empty() {
                        vec![0.0, 1.0]
                    } else {
                        axis_y
                    }
                } else {
                    vec![0.0]
                };
                let id = s.model.add_param(EditParam {
                    name,
                    is_vec2: vec2,
                    min,
                    max,
                    defaults,
                    axis_points_x,
                    axis_points_y,
                    bindings: Vec::new(),
                });
                s.touch();
                Ok(ResponseBody::Param {
                    param: ParamRef(id.to_ffi()),
                })
            }),
            Command::ParamList { session } => self.with_session(session, |s| {
                Ok(ResponseBody::Params {
                    params: param_infos(&s.model),
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
                let pid = ParamId::from_ffi(param.0);
                if let Some(n) = name {
                    s.model.set_param_name(pid, n)?;
                }
                if min.is_some() || max.is_some() {
                    let p = s.model.param(pid).ok_or(EditError::UnknownParam)?;
                    let new_min = min.unwrap_or(p.min);
                    let new_max = max.unwrap_or(p.max);
                    s.model.set_param_range(pid, new_min, new_max)?;
                }
                if let Some(d) = defaults {
                    s.model.set_param_defaults(pid, d)?;
                }
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamDelete { session, param } => self.edit_session(session, |s| {
                s.model.delete_param(ParamId::from_ffi(param.0))?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamAxisInsert {
                session,
                param,
                axis,
                value,
            } => self.edit_session(session, |s| {
                s.model
                    .axis_insert(ParamId::from_ffi(param.0), axis, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamAxisDelete {
                session,
                param,
                axis,
                index,
            } => self.edit_session(session, |s| {
                s.model
                    .axis_delete(ParamId::from_ffi(param.0), axis, index as usize)?;
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
                s.model
                    .axis_move(ParamId::from_ffi(param.0), axis, index as usize, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamFlip {
                session,
                param,
                axis,
            } => self.edit_session(session, |s| {
                s.model.param_flip(ParamId::from_ffi(param.0), axis)?;
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
                    s.model.set_binding_key(
                        ParamId::from_ffi(param.0),
                        NodeId::from_ffi(node.0),
                        t,
                        cell[0],
                        cell[1],
                        value,
                    )?;
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
                s.model.unset_binding_key(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
                    t,
                    cell[0],
                    cell[1],
                )?;
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
                s.model.reset_binding_key(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
                    t,
                    cell[0],
                    cell[1],
                )?;
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
                s.model
                    .delete_binding(ParamId::from_ffi(param.0), NodeId::from_ffi(node.0), t)?;
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
                s.model.set_binding_interpolate(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
                    t,
                    m,
                )?;
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
                s.model
                    .invert_binding(ParamId::from_ffi(param.0), NodeId::from_ffi(node.0), t)?;
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
                s.model.copy_binding_key(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
                    t,
                    from,
                    to,
                )?;
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
                s.model.set_deform_vertices(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
                    cell,
                    offsets,
                )?;
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
                    ClpIndices::U16(indices.iter().map(|&i| i as u16).collect())
                } else {
                    ClpIndices::U32(indices)
                };
                s.model.set_mesh_with_refit(
                    NodeId::from_ffi(node.0),
                    ClpMesh {
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
                let mesh = match s.model.node(NodeId::from_ffi(from.0)).map(|n| &n.kind) {
                    Some(EditNodeKind::Part(p)) => p.mesh.to_clp(),
                    Some(EditNodeKind::MeshGroup(mg)) => mg.mesh.to_clp(),
                    Some(_) => return Err(EditorError::BadTarget("not a meshed node".into())),
                    None => return Err(EditorError::NoNode(from)),
                };
                s.model.set_mesh_with_refit(NodeId::from_ffi(to.0), mesh)?;
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
                        let pid = ParamId::from_ffi(p.0);
                        if s.model.param(pid).is_none() {
                            return Err(EditorError::BadTarget("target param".into()));
                        }
                        Some(pid)
                    }
                    None => None,
                };
                let phys = EditPhysics {
                    kind: phys_kind,
                    map_mode: PhysicsParamMapMode::default(),
                    local_only: false,
                    target_param: target,
                    gravity: gravity.unwrap_or(9.8),
                    length: length.unwrap_or(100.0),
                    frequency: frequency.unwrap_or(1.0),
                    angle_damping: angle_damping.unwrap_or(0.5),
                    length_damping: length_damping.unwrap_or(0.5),
                    output_scale: [1.0, 1.0],
                };
                let node = EditNode::new(
                    name.unwrap_or_else(|| "Physics".into()),
                    EditNodeKind::SimplePhysics(phys),
                );
                let id = s.model.add_node(NodeId::from_ffi(parent.0), node)?;
                s.touch();
                Ok(ResponseBody::Node {
                    node: NodeRef(id.to_ffi()),
                })
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
                s.model.set_deform_from_transform(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
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
                s.model.add_scalar_binding(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
                    t,
                )?;
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
                s.model.set_binding_key(
                    ParamId::from_ffi(param.0),
                    NodeId::from_ffi(node.0),
                    t,
                    cell[0],
                    cell[1],
                    value,
                )?;
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
        let pose: Vec<(String, Vec2)> = params
            .into_iter()
            .map(|p| (p.name, Vec2::new(p.x, p.y)))
            .collect();

        let handle = self.session(session)?;
        let (mut puppet, rev) = {
            let mut s = lock(&handle);
            s.puppet()?;
            let puppet = s
                .puppet
                .take()
                .ok_or_else(|| EditorError::Preview("puppet build failed".into()))?;
            (puppet, s.rev)
        };

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
                s.puppet_dirty = false;
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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn build_tree(model: &EditModel, id: NodeId) -> TreeNode {
    let (name, kind, z_order, enabled, children) = match model.node(id) {
        Some(n) => (
            n.name.clone(),
            node_kind_str(&n.kind).to_string(),
            n.z_order,
            n.enabled,
            n.children().to_vec(),
        ),
        None => (String::new(), "group".to_string(), 0.0, true, Vec::new()),
    };
    TreeNode {
        node: NodeRef(id.to_ffi()),
        name,
        kind,
        z_order,
        enabled,
        children: children.iter().map(|&c| build_tree(model, c)).collect(),
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

fn param_infos(model: &EditModel) -> Vec<ParamInfo> {
    model
        .param_ids()
        .iter()
        .filter_map(|&pid| {
            let p = model.param(pid)?;
            let (w, h) = model.param_grid(pid).ok()?;
            Some(ParamInfo {
                param: ParamRef(pid.to_ffi()),
                name: p.name.clone(),
                vec2: p.is_vec2,
                min: p.min,
                max: p.max,
                defaults: p.defaults,
                axis: [w, h],
                axis_points_x: p.axis_points_x.clone(),
                axis_points_y: p.axis_points_y.clone(),
                bindings: p.bindings.len() as u32,
            })
        })
        .collect()
}

fn node_kind_str(kind: &EditNodeKind) -> &'static str {
    match kind {
        EditNodeKind::Group => "group",
        EditNodeKind::Part(_) => "part",
        EditNodeKind::Composite(_) => "composite",
        EditNodeKind::MeshGroup(_) => "mesh_group",
        EditNodeKind::SimplePhysics(_) => "physics",
    }
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

fn make_kind(kind: NodeKindArg) -> EditNodeKind {
    match kind {
        NodeKindArg::Group => EditNodeKind::Group,
        NodeKindArg::Part => EditNodeKind::Part(EditPart {
            mesh: ClpMesh::default().into(),
            albedo: None,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            masks: Vec::new(),
            mask_threshold: 0.5,
        }),
        NodeKindArg::Composite => EditNodeKind::Composite(EditComposite {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            masks: Vec::new(),
            mask_threshold: 0.5,
            propagate_meshgroup: false,
        }),
        NodeKindArg::MeshGroup => EditNodeKind::MeshGroup(EditMeshGroup {
            mesh: ClpMesh::default().into(),
            dynamic: false,
            translate_children: false,
        }),
    }
}

fn apply_patch(n: &mut EditNode, patch: &NodePatch) -> Result<(), EditorError> {
    // Parse before mutating so a bad enum string leaves the node untouched.
    let blend = patch
        .blend_mode
        .as_deref()
        .map(|s| BlendMode::from_name(s).ok_or_else(|| EditorError::BadTarget(s.to_string())))
        .transpose()?;
    if let Some(name) = &patch.name {
        n.name = name.clone();
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
    if let Some(tref) = patch.texture {
        if let EditNodeKind::Part(p) = &mut n.kind {
            p.albedo = Some(TexId::from_ffi(tref.0));
        }
    }
    if let Some(mode) = blend {
        match &mut n.kind {
            EditNodeKind::Part(p) => p.blend_mode = mode,
            EditNodeKind::Composite(c) => c.blend_mode = mode,
            _ => {}
        }
    }
    if let Some(t) = patch.tint {
        match &mut n.kind {
            EditNodeKind::Part(p) => p.tint = t,
            EditNodeKind::Composite(c) => c.tint = t,
            _ => {}
        }
    }
    if let Some(t) = patch.screen_tint {
        match &mut n.kind {
            EditNodeKind::Part(p) => p.screen_tint = t,
            EditNodeKind::Composite(c) => c.screen_tint = t,
            _ => {}
        }
    }
    if let Some(th) = patch.mask_threshold {
        match &mut n.kind {
            EditNodeKind::Part(p) => p.mask_threshold = th,
            EditNodeKind::Composite(c) => c.mask_threshold = th,
            _ => {}
        }
    }
    if let Some(v) = patch.propagate_meshgroup {
        if let EditNodeKind::Composite(c) = &mut n.kind {
            c.propagate_meshgroup = v;
        }
    }
    if let Some(v) = patch.mg_dynamic {
        if let EditNodeKind::MeshGroup(mg) = &mut n.kind {
            mg.dynamic = v;
        }
    }
    if let Some(v) = patch.mg_translate_children {
        if let EditNodeKind::MeshGroup(mg) = &mut n.kind {
            mg.translate_children = v;
        }
    }
    Ok(())
}

fn set_opacity(kind: &mut EditNodeKind, op: f32) {
    match kind {
        EditNodeKind::Part(p) => p.opacity = op,
        EditNodeKind::Composite(c) => c.opacity = op,
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
                .with_model(first, |_| {
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
        let mut session = Session::new(EditModel::new(), "history".into(), None);
        for name in ["first", "second", "third"] {
            let mut model = EditModel::new();
            model.node_mut(model.root()).unwrap().name = name.into();
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
                .node(session.undo[0].model.root())
                .unwrap()
                .name,
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

        let tmp = std::env::temp_dir().join(format!("clpedit-srv-{}.clp", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("clpedit-prev-{}", std::process::id()));
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
}
