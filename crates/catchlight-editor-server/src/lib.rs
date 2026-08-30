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
//!
//! Invariants this module enforces:
//!
//! - **The protocol's Ids are the model's Ids.** There is no handle table:
//!   a request names a node, param or texture by the string the file stores,
//!   and the server looks it up in the [`Model`]. So a reference outlives the
//!   session that produced it, and [`Command::RenameId`] is the one thing
//!   that invalidates one.
//!
//! - **A drag never snapshots the model; a commit does.**
//!   [`Command::ScratchDeform`] writes the session puppet's scratch deform and
//!   returns without touching `rev`, so a drag of any length costs no undo
//!   entries. [`Command::DeformVertices`] authors the same offsets into the
//!   model and costs exactly one. Everything that edits the document goes
//!   through [`Editor::edit_session`], which is the single place an undo
//!   snapshot is taken.
//!
//! - **The undo budget counts shared bytes once.** See [`History`]: 64
//!   snapshots of one rig hold its textures once, not 64 times.
//!
//! - **One render cache per previewed session.** See [`preview`]: a cache's
//!   slots name GPU state inside the one warm renderer, so switching the
//!   previewed session re-prepares it.

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
use catchlight_core::formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding};
use catchlight_core::id::{Name, SeededHex};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::Vec2;
use catchlight_core::{
    BindingKey, BindingTarget, Model, ModelComposite, ModelError, ModelMeshGroup, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelPhysics, ModelTexture, ModelWeld, Pose, Puppet,
    ScalarTarget, DEFAULT_SLOT_WEIGHT,
};
#[cfg(not(target_arch = "wasm32"))]
use catchlight_editor_core::{Manifest, ModelManifestExt as _, TextureData};
use catchlight_editor_core::{ManifestError, ModelMeshExt as _};

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
    #[error("no session {}", .0.0)]
    NoSession(SessionId),
    #[error("no node {0}")]
    NoNode(NodeId),
    #[error("no param {0}")]
    NoParam(ParamId),
    #[error("no texture {0}")]
    NoTexture(TexId),
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

impl EditorError {
    /// The wire code a client branches on. The message stays for a person;
    /// this is what a commit gate or a mesh editor reacts to.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NoSession(_) => ErrorCode::NoSession,
            Self::NoNode(_) => ErrorCode::NoNode,
            Self::NoParam(_) => ErrorCode::NoParam,
            Self::NoTexture(_) => ErrorCode::NoTexture,
            Self::BadTarget(_) => ErrorCode::BadTarget,
            Self::NothingToUndo => ErrorCode::NothingToUndo,
            Self::NothingToRedo => ErrorCode::NothingToRedo,
            Self::NoSavePath => ErrorCode::NoSavePath,
            Self::Manifest(_) => ErrorCode::Manifest,
            Self::Io(_) => ErrorCode::Io,
            Self::Image(_) => ErrorCode::Image,
            Self::Preview(_) => ErrorCode::Preview,
            Self::NativeOnly => ErrorCode::NativeOnly,
            // The model refuses an edit for many reasons; the ones a client
            // acts on differently get their own code, the rest are `Edit`.
            Self::Edit(e) => match e {
                ModelError::UnknownNode => ErrorCode::NoNode,
                ModelError::UnknownParam => ErrorCode::NoParam,
                ModelError::UnknownTexture => ErrorCode::NoTexture,
                ModelError::UnknownSeam => ErrorCode::UnknownSeam,
                ModelError::UnknownSlot => ErrorCode::UnknownSlot,
                ModelError::DuplicateSeam(_) => ErrorCode::DuplicateSeam,
                ModelError::DuplicateSlot(_) => ErrorCode::DuplicateSlot,
                ModelError::WeldSlotMismatch => ErrorCode::WeldSlotMismatch,
                ModelError::Fragment => ErrorCode::Fragment,
                _ => ErrorCode::Edit,
            },
        }
    }
}

struct Session {
    model: Model,
    /// Where generated Ids come from. Seeded per session so a session's Ids
    /// are reproducible; uniqueness is the model's job, not the seed's.
    hex: SeededHex,
    title: String,
    file: Option<PathBuf>,
    rev: u64,
    saved_rev: u64,
    history: History,
    /// Lazily baked from `model` for preview. Rebaked by its own generation
    /// gate on the next use after an edit, so nothing has to invalidate it.
    puppet: Option<Puppet>,
    /// rev-gated document view for in-process observers (the GUI).
    snapshot: Option<Arc<DocSnapshot>>,
    /// Latest shared view state — its own path, never touches the model/rev.
    presence: Option<Presence>,
}

/// The undo and redo stacks, and the budget that bounds them.
///
/// A snapshot is a shallow clone: texture payloads, meshes and binding grids
/// ride behind an `Arc` and are copied only when something edits them. So a
/// budget that charged every snapshot [`Model::estimated_size_bytes`] would
/// bill one rig's textures once per undo step and collapse the history of any
/// model bigger than a fraction of the cap.
///
/// **Each distinct texture payload is therefore counted once for the whole
/// history.** `Arc::as_ptr` identifies the allocation and the ledger below
/// counts how many snapshots hold it. Everything else is charged per
/// snapshot: a `Model` does not expose the sharing of its meshes and binding
/// grids, so those are over-counted. Over-counting is the safe side — the
/// history trims sooner than it strictly must, never later.
#[derive(Default)]
struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// Every texture payload held anywhere in the two stacks:
    /// allocation address -> (bytes, how many snapshots hold it).
    textures: HashMap<usize, (usize, usize)>,
    /// The sum of every snapshot's `own_bytes`.
    own_bytes: usize,
}

struct Snapshot {
    model: Model,
    /// What this snapshot holds that is not a texture payload.
    own_bytes: usize,
}

/// One entry per distinct texture payload: its allocation and its bytes.
fn texture_payloads(model: &Model) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for id in model.texture_ids() {
        if let Some(texture) = model.texture(id) {
            // The address of the `Vec` inside the `Arc`, not of its buffer:
            // two empty textures share a dangling buffer pointer but never an
            // allocation.
            let at = Arc::as_ptr(&texture.data) as usize;
            if !out.iter().any(|&(seen, _)| seen == at) {
                out.push((at, texture.data.capacity()));
            }
        }
    }
    out
}

impl History {
    fn snapshot(model: Model) -> Snapshot {
        let shared: usize = texture_payloads(&model)
            .iter()
            .fold(0, |bytes, &(_, len)| bytes.saturating_add(len));
        let own_bytes = model.estimated_size_bytes().saturating_sub(shared);
        Snapshot { model, own_bytes }
    }

    /// What the history holds, shared payloads counted once.
    fn bytes(&self) -> usize {
        self.textures
            .values()
            .fold(self.own_bytes, |bytes, &(len, _)| bytes.saturating_add(len))
    }

    fn hold(&mut self, snapshot: &Snapshot) {
        self.own_bytes = self.own_bytes.saturating_add(snapshot.own_bytes);
        for (at, len) in texture_payloads(&snapshot.model) {
            let entry = self.textures.entry(at).or_insert((len, 0));
            entry.1 += 1;
        }
    }

    fn release(&mut self, snapshot: &Snapshot) {
        self.own_bytes = self.own_bytes.saturating_sub(snapshot.own_bytes);
        for (at, _) in texture_payloads(&snapshot.model) {
            if let std::collections::hash_map::Entry::Occupied(mut held) = self.textures.entry(at) {
                held.get_mut().1 -= 1;
                if held.get().1 == 0 {
                    held.remove();
                }
            }
        }
    }

    fn push_undo(&mut self, model: Model) {
        while let Some(dropped) = self.redo.pop() {
            self.release(&dropped);
        }
        let snapshot = Self::snapshot(model);
        self.hold(&snapshot);
        self.undo.push(snapshot);
    }

    /// Swap `current` for the newest undo snapshot, pushing what it replaced
    /// onto the redo stack.
    fn undo(&mut self, current: &mut Model) -> Result<(), EditorError> {
        let previous = self.undo.pop().ok_or(EditorError::NothingToUndo)?;
        self.release(&previous);
        let replaced = Self::snapshot(std::mem::replace(current, previous.model));
        self.hold(&replaced);
        self.redo.push(replaced);
        Ok(())
    }

    fn redo(&mut self, current: &mut Model) -> Result<(), EditorError> {
        let next = self.redo.pop().ok_or(EditorError::NothingToRedo)?;
        self.release(&next);
        let replaced = Self::snapshot(std::mem::replace(current, next.model));
        self.hold(&replaced);
        self.undo.push(replaced);
        Ok(())
    }

    /// Drop the oldest snapshots until the history fits. The newest is always
    /// kept: an editor that cannot undo the last edit is worse than one that
    /// remembers nothing.
    fn trim(&mut self, max_depth: usize, max_bytes: usize) {
        while self.undo.len() > max_depth
            || (self.bytes() > max_bytes && self.undo.len() + self.redo.len() > 1)
        {
            let removed = if self.undo.is_empty() {
                self.redo.remove(0)
            } else {
                self.undo.remove(0)
            };
            self.release(&removed);
        }
    }
}

impl Session {
    fn new(model: Model, title: String, file: Option<PathBuf>) -> Self {
        Self {
            model,
            hex: SeededHex::new(0x1d5e_ed01),
            title,
            file,
            rev: 0,
            saved_rev: 0,
            history: History::default(),
            puppet: None,
            snapshot: None,
            presence: None,
        }
    }

    fn touch(&mut self) {
        self.rev += 1;
    }

    /// Creation splits the borrow between the model and its Id source.
    fn add_node(&mut self, parent: &NodeId, node: ModelNode) -> Result<NodeId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.add_node(parent, node, hex)?)
    }

    fn add_param(&mut self, param: ModelParam) -> Result<ParamId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.add_param(param, hex)?)
    }

    fn add_texture(&mut self, texture: ModelTexture) -> Result<TexId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.add_texture(texture, hex)?)
    }

    fn duplicate_subtree(&mut self, node: &NodeId) -> Result<NodeId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.duplicate_subtree(node, hex)?)
    }

    fn push_undo(&mut self, snapshot: Model) {
        self.history.push_undo(snapshot);
        self.history.trim(UNDO_DEPTH, UNDO_BYTES);
    }

    fn undo(&mut self) -> Result<(), EditorError> {
        self.history.undo(&mut self.model)?;
        self.history.trim(UNDO_DEPTH, UNDO_BYTES);
        self.touch();
        Ok(())
    }

    fn redo(&mut self) -> Result<(), EditorError> {
        self.history.redo(&mut self.model)?;
        self.history.trim(UNDO_DEPTH, UNDO_BYTES);
        self.touch();
        Ok(())
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
                code: e.code(),
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
        Ok((s.history.undo.len(), s.history.redo.len()))
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
        f: impl FnOnce(&Model) -> R,
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
    ) -> Result<TexId, EditorError> {
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
        let model = &session.model;
        // A session always holds a complete model: the editor's load path
        // reads one, and `Model::new` makes one.
        let root = model.root()?;
        let snap = Arc::new(DocSnapshot {
            rev,
            root: build_tree(model, root),
            params: param_infos(model),
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
                let tmp = path.with_extension("clm.tmp");
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
                // A session holds a complete model, so this is unreachable —
                // but `Fragment` says so on the wire rather than panicking.
                let root = s.model.root().ok_or(ModelError::Fragment)?;
                Ok(ResponseBody::Tree {
                    root: build_tree(&s.model, root),
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
                let node = s.add_node(&parent, node)?;
                s.touch();
                Ok(ResponseBody::Node { node })
            }),
            Command::NodeSet {
                session,
                node,
                patch,
            } => self.edit_session(session, |s| {
                if let Some(tex) = &patch.texture {
                    if s.model.texture(tex).is_none() {
                        return Err(EditorError::NoTexture(tex.clone()));
                    }
                    if matches!(
                        s.model.node(&node).map(|n| &n.kind),
                        Some(ModelNodeKind::Part(_))
                    ) {
                        s.model.set_part_albedo(&node, Some(tex.clone()))?;
                    }
                }
                s.model.update_node(&node, |n| apply_patch(n, &patch))??;
                s.touch();
                Ok(ResponseBody::Node { node })
            }),
            Command::NodeReparent { session, node, to } => self.edit_session(session, |s| {
                s.model.reparent(&node, &to)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeReorder {
                session,
                node,
                index,
            } => self.edit_session(session, |s| {
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
                s.model.reparent(&node, &parent)?;
                // reorder can only fail on unknown/root, both excluded by the
                // successful reparent — the combined edit stays atomic.
                s.model.reorder(&node, index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::NodeDuplicate { session, node } => self.edit_session(session, |s| {
                let copy = s.duplicate_subtree(&node)?;
                s.touch();
                Ok(ResponseBody::Node { node: copy })
            }),
            Command::RenameId { session, rename } => self.edit_session(session, |s| {
                match rename {
                    Rename::Node { from, to } => s.model.rename_node_id(&from, to)?,
                    Rename::Param { from, to } => s.model.rename_param_id(&from, to)?,
                    Rename::Texture { from, to } => s.model.rename_tex_id(&from, to)?,
                }
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MaskAdd {
                session,
                node,
                source,
                mode,
            } => self.edit_session(session, |s| {
                let mode = parse_mask_mode(&mode)?;
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
                s.model.mask_reorder(&node, index as usize, to as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MaskDelete {
                session,
                node,
                index,
            } => self.edit_session(session, |s| {
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
                target_params,
                clear_target_params,
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
                let targets = match (clear_target_params, target_params) {
                    (true, _) => Some([None, None]),
                    (false, Some(ids)) => Some(physics_targets(&s.model, ids)?),
                    (false, None) => None,
                };
                if let Some(t) = targets {
                    s.model.set_physics_targets(&node, t)?;
                }
                s.model.update_node(&node, |n| {
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
                })??;
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
                let mut textures = Vec::new();
                for tid in s.model.texture_ids() {
                    if let Some(t) = s.model.texture(tid) {
                        let (width, height) = image_dims(&t.data).unwrap_or((0, 0));
                        textures.push(TexInfo {
                            id: tid.clone(),
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
                min,
                max,
                default,
                key_positions,
            } => self.edit_session(session, |s| {
                if !catchlight_core::param_range_is_valid(min, max) {
                    return Err(ModelError::CellOutOfRange.into());
                }
                let param = s.add_param(ModelParam {
                    name: Name::truncated(name),
                    min,
                    max,
                    default,
                    key_positions: if key_positions.is_empty() {
                        vec![0.0, 1.0]
                    } else {
                        key_positions
                    },
                })?;
                s.touch();
                Ok(ResponseBody::Param { param })
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
                default,
            } => self.edit_session(session, |s| {
                if let Some(n) = name {
                    s.model.set_param_name(&param, Name::truncated(n))?;
                }
                if min.is_some() || max.is_some() {
                    let p = s.model.param(&param).ok_or(ModelError::UnknownParam)?;
                    let (new_min, new_max) = (min.unwrap_or(p.min), max.unwrap_or(p.max));
                    s.model.set_param_range(&param, new_min, new_max)?;
                }
                if let Some(d) = default {
                    s.model.set_param_default(&param, d)?;
                }
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamDelete { session, param } => self.edit_session(session, |s| {
                s.model.delete_param(&param)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamKeyInsert {
                session,
                param,
                value,
            } => self.edit_session(session, |s| {
                s.model.key_insert(&param, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamKeyDelete {
                session,
                param,
                index,
            } => self.edit_session(session, |s| {
                s.model.key_delete(&param, index as usize)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamKeyMove {
                session,
                param,
                index,
                value,
            } => self.edit_session(session, |s| {
                s.model.key_move(&param, index as usize, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::ParamFlip { session, param } => self.edit_session(session, |s| {
                s.model.param_flip(&param)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingAdd {
                session,
                params,
                node,
                target,
            } => self.edit_session(session, |s| {
                let t = ScalarTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                s.model
                    .add_binding(&binding_key(params, node, BindingTarget::Scalar(t))?)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingKey {
                session,
                params,
                node,
                target,
                cell,
                value,
            } => self.edit_session(session, |s| {
                let t = ScalarTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let key = binding_key(params, node, BindingTarget::Scalar(t))?;
                s.model.set_binding_key(&key, cell, value)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingKeys {
                session,
                params,
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
                    let key = binding_key(params.clone(), node.clone(), BindingTarget::Scalar(t))?;
                    s.model.set_binding_key(&key, cell, value)?;
                }
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingUnset {
                session,
                params,
                node,
                target,
                cell,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                s.model
                    .unset_binding_key(&binding_key(params, node, t)?, cell)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingReset {
                session,
                params,
                node,
                target,
                cell,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                s.model
                    .reset_binding_key(&binding_key(params, node, t)?, cell)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingDelete {
                session,
                params,
                node,
                target,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                s.model.delete_binding(&binding_key(params, node, t)?)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingInterpolate {
                session,
                params,
                node,
                target,
                mode,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                let m = parse_interpolate_mode(&mode)?;
                s.model
                    .set_binding_interpolate(&binding_key(params, node, t)?, m)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingInvert {
                session,
                params,
                node,
                target,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                s.model.invert_binding(&binding_key(params, node, t)?)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::BindingCopyKey {
                session,
                params,
                node,
                target,
                from,
                to,
            } => self.edit_session(session, |s| {
                let t = BindingTarget::parse(&target).ok_or(EditorError::BadTarget(target))?;
                s.model
                    .copy_binding_key(&binding_key(params, node, t)?, from, to)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::DeformSet {
                session,
                params,
                node,
                cell,
                translate,
                rotate,
                scale,
            } => self.edit_session(session, |s| {
                let key = binding_key(params, node, BindingTarget::Deform)?;
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
            Command::DeformVertices {
                session,
                params,
                node,
                cell,
                offsets,
            } => self.edit_session(session, |s| {
                let key = binding_key(params, node, BindingTarget::Deform)?;
                s.model.set_deform_vertices(&key, cell, offsets)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::MeshSet {
                session,
                node,
                verts,
                uvs,
                indices,
                origin,
            } => self.edit_session(session, |s| {
                let mesh = build_mesh(verts, uvs, indices, origin)?;
                let emptied = s.model.set_mesh_with_refit(&node, mesh)?;
                s.touch();
                Ok(emptied_reply(node, emptied))
            }),
            Command::MeshCopy { session, from, to } => self.edit_session(session, |s| {
                let mesh = match s.model.node_mesh(&from) {
                    Some(mesh) => mesh.clone(),
                    None if s.model.node(&from).is_some() => {
                        return Err(EditorError::BadTarget("not a meshed node".into()))
                    }
                    None => return Err(EditorError::NoNode(from)),
                };
                let emptied = s.model.set_mesh_with_refit(&to, mesh)?;
                s.touch();
                Ok(emptied_reply(to, emptied))
            }),
            Command::SeamAdd {
                session,
                node,
                seam,
            } => self.edit_session(session, |s| {
                s.model.seam_add(&node, seam)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::SeamDelete {
                session,
                node,
                seam,
            } => self.edit_session(session, |s| {
                s.model.seam_delete(&node, &seam)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::SlotAdd {
                session,
                node,
                seam,
                slot,
            } => self.edit_session(session, |s| {
                s.model.slot_add(&node, &seam, slot)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::SlotFill {
                session,
                node,
                seam,
                slot,
                vertex,
            } => self.edit_session(session, |s| {
                s.model.slot_fill(&node, &seam, &slot, vertex)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::SlotClear {
                session,
                node,
                seam,
                slot,
            } => self.edit_session(session, |s| {
                s.model.slot_clear(&node, &seam, &slot)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::SlotDelete {
                session,
                node,
                seam,
                slot,
            } => self.edit_session(session, |s| {
                s.model.slot_delete(&node, &seam, &slot)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::Seams { session, node } => self.with_session(session, |s| {
                let seams = s
                    .model
                    .seams(&node)
                    .ok_or_else(|| match s.model.node(&node) {
                        Some(_) => EditorError::Edit(ModelError::NotAPart),
                        None => EditorError::NoNode(node.clone()),
                    })?;
                Ok(ResponseBody::Seams {
                    seams: seams.iter().map(seam_info).collect(),
                })
            }),
            Command::Welds { session } => self.with_session(session, |s| {
                Ok(ResponseBody::Welds {
                    welds: s.model.welds().iter().map(weld_info).collect(),
                })
            }),
            Command::UnfilledSlots { session } => self.with_session(session, |s| {
                Ok(ResponseBody::UnfilledSlots {
                    slots: s
                        .model
                        .unfilled_slots()
                        .into_iter()
                        .map(|(node, seam, slot)| SlotAddr { node, seam, slot })
                        .collect(),
                })
            }),
            Command::WeldSet {
                session,
                a,
                b,
                weights,
            } => self.edit_session(session, |s| {
                let weld = build_weld(&s.model, a, b, weights)?;
                let mut welds = s.model.welds().to_vec();
                // One weld per pair of seams: setting a pair that is already
                // welded replaces it rather than stacking a second weld the
                // solver would fight over.
                welds.retain(|w| !pairs_the_same_seams(w, &weld));
                welds.push(weld);
                s.model.set_welds(welds)?;
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
                target_params,
                length,
                gravity,
                frequency,
                angle_damping,
                length_damping,
            } => self.edit_session(session, |s| {
                let phys_kind = parse_pendulum_kind(&kind)
                    .ok_or_else(|| EditorError::BadTarget(kind.clone()))?;
                let targets = physics_targets(&s.model, target_params)?;
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
                let node = s.add_node(&parent, node)?;
                s.model.set_physics_targets(&node, targets)?;
                s.touch();
                Ok(ResponseBody::Node { node })
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
            Command::ScratchDeform {
                session,
                node,
                offsets,
            } => {
                // The presence path: a drag shows on the puppet and leaves the
                // model, its revision and its undo history alone.
                let handle = self.session(session)?;
                let mut s = lock(&handle);
                let (model, puppet) = s.puppet();
                let idx = model
                    .node(&node)
                    .and_then(|_| puppet.node_idx(&node))
                    .ok_or_else(|| EditorError::NoNode(node.clone()))?;
                if offsets.is_empty() {
                    puppet.clear_scratch_deform(idx);
                } else {
                    if !offsets.len().is_multiple_of(2) || offsets.len() != model.deform_len(&node)
                    {
                        return Err(EditorError::BadTarget(
                            "deform offsets must be two per mesh vertex".into(),
                        ));
                    }
                    let deform: Vec<Vec2> = offsets
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&[x, y]| Vec2::new(x, y))
                        .collect();
                    puppet.set_scratch_deform(idx, &deform);
                }
                puppet.combine_deforms();
                Ok(ResponseBody::Empty)
            }
            #[cfg(not(target_arch = "wasm32"))]
            Command::Preview {
                session,
                pose,
                size,
                out,
            } => self.run_preview(session, pose, size, out),
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
        params: Vec<ParamPose>,
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
        // Params are scalar and the wire names them by Id, so a pose is
        // the [`Pose`] the model evaluates, with nothing to resolve.
        let pose: Pose = params.into_iter().map(|p| (p.param, p.value)).collect();

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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn build_tree(model: &Model, id: &NodeId) -> TreeNode {
    let (name, kind, z_order, enabled, children) = match model.node(id) {
        Some(n) => (
            n.name.to_string(),
            n.kind.name().to_string(),
            n.z_order,
            n.enabled,
            n.children(),
        ),
        None => (String::new(), "group".to_string(), 0.0, true, &[][..]),
    };
    TreeNode {
        id: id.clone(),
        name,
        kind,
        z_order,
        enabled,
        children: children.iter().map(|c| build_tree(model, c)).collect(),
    }
}

/// The binding one request names. A `param_y` makes it a two-param binding
/// whose grid spans both params' key positions.
fn binding_key(
    params: BindingParams,
    node: NodeId,
    target: BindingTarget,
) -> Result<BindingKey, EditorError> {
    Ok(match params.param_y {
        Some(y) => BindingKey::pair(params.param, y, node, target),
        None => BindingKey::new(params.param, node, target),
    })
}

/// A driver writes one or two params (angle, length). Every one it names has
/// to exist: `set_physics_targets` would refuse a dangling one anyway, but
/// this says which param was missing.
fn physics_targets(
    model: &Model,
    params: Vec<ParamId>,
) -> Result<[Option<ParamId>; 2], EditorError> {
    if params.len() > 2 {
        return Err(EditorError::BadTarget(
            "a driver writes at most two params".into(),
        ));
    }
    let mut targets = [None, None];
    for (slot, id) in targets.iter_mut().zip(params) {
        if model.param(&id).is_none() {
            return Err(EditorError::NoParam(id));
        }
        *slot = Some(id);
    }
    Ok(targets)
}

fn build_mesh(
    verts: Vec<f32>,
    uvs: Vec<f32>,
    indices: Vec<u32>,
    origin: [f32; 2],
) -> Result<ClmMesh, EditorError> {
    let vcount = verts.len() / 2;
    if !verts.len().is_multiple_of(2)
        || uvs.len() != verts.len()
        || !indices.len().is_multiple_of(3)
        || indices.iter().any(|&i| i as usize >= vcount)
    {
        return Err(EditorError::BadTarget("malformed mesh".into()));
    }
    let indices = if indices.iter().max().copied().unwrap_or(0) <= u16::MAX as u32 {
        ClmIndices::U16(indices.iter().map(|&i| i as u16).collect())
    } else {
        ClmIndices::U32(indices)
    };
    Ok(ClmMesh {
        verts,
        uvs,
        indices,
        origin,
    })
}

/// Re-authoring a mesh empties every slot on the part: which ones is what a
/// commit gate and a "refill these" prompt are built from, so it is the
/// reply rather than something the client has to go and ask for.
fn emptied_reply(node: NodeId, emptied: Vec<(SeamId, SlotId)>) -> ResponseBody {
    ResponseBody::Emptied {
        node,
        slots: emptied
            .into_iter()
            .map(|(seam, slot)| SeamSlot { seam, slot })
            .collect(),
    }
}

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

fn weld_info(weld: &ModelWeld) -> WeldInfo {
    let end = |(node, seam): &(NodeId, SeamId)| SeamAddr {
        node: node.clone(),
        seam: seam.clone(),
    };
    WeldInfo {
        a: end(weld.a()),
        b: end(weld.b()),
        weights: weld
            .weights()
            .iter()
            .map(|(slot, weight)| SlotWeight {
                slot: slot.clone(),
                weight: *weight,
            })
            .collect(),
    }
}

/// A weld pairs two seams slot by slot, so an empty `weights` means "every
/// slot, evenly". The two seams already hold the same slots — `slot_add`
/// propagates along a weld — so either end names them.
fn build_weld(
    model: &Model,
    a: SeamAddr,
    b: SeamAddr,
    weights: Vec<SlotWeight>,
) -> Result<ModelWeld, EditorError> {
    let weights = if weights.is_empty() {
        let seam = model
            .seam(&a.node, &a.seam)
            .ok_or(ModelError::UnknownSeam)?;
        seam.slots()
            .iter()
            .map(|slot| (slot.id().clone(), DEFAULT_SLOT_WEIGHT))
            .collect()
    } else {
        weights
            .into_iter()
            .map(|w| (w.slot, w.weight))
            .collect::<Vec<_>>()
    };
    Ok(ModelWeld::new((a.node, a.seam), (b.node, b.seam), weights))
}

/// Two welds join the same pair of seams, in either order.
fn pairs_the_same_seams(one: &ModelWeld, other: &ModelWeld) -> bool {
    (one.a() == other.a() && one.b() == other.b()) || (one.a() == other.b() && one.b() == other.a())
}

/// In-process document view handed to observers (the GUI); refs are stable for
/// the session's lifetime.
#[derive(Debug, Clone)]
pub struct DocSnapshot {
    pub rev: u64,
    pub root: TreeNode,
    pub params: Vec<ParamInfo>,
}

fn param_infos(model: &Model) -> Vec<ParamInfo> {
    let mut out = Vec::with_capacity(model.param_ids().len());
    for pid in model.param_ids() {
        let Some(p) = model.param(pid) else { continue };
        out.push(ParamInfo {
            id: pid.clone(),
            name: p.name.to_string(),
            min: p.min,
            max: p.max,
            default: p.default,
            key_positions: p.key_positions.clone(),
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
            Reply::Err { id: 0, code: ErrorCode::BadRequest, message }
                if message.contains("request exceeds")
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

    fn named_model(name: &str) -> Model {
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        model
            .update_node(&root, |n| n.name = Name::truncated(name))
            .unwrap();
        model
    }

    fn root_name(model: &Model) -> String {
        model
            .node(model.root().unwrap())
            .unwrap()
            .name
            .as_str()
            .to_string()
    }

    #[test]
    fn history_byte_budget_keeps_the_newest_snapshot() {
        let mut history = History::default();
        for name in ["first", "second", "third"] {
            history.push_undo(named_model(name));
        }
        let newest_bytes = history.undo.last().unwrap().own_bytes;

        history.trim(UNDO_DEPTH, newest_bytes);

        assert_eq!(history.undo.len(), 1);
        assert_eq!(root_name(&history.undo[0].model), "third");
        assert_eq!(history.bytes(), newest_bytes);
    }

    /// An undo snapshot is a shallow clone, so 64 of them hold one rig's
    /// textures once, not 64 times. Charging each snapshot the full
    /// `estimated_size_bytes` would bill them 64 times and collapse the
    /// history of any model whose textures approach the cap.
    #[test]
    fn a_texture_shared_by_every_snapshot_is_counted_once() {
        let mut model = Model::new();
        let mut hex = SeededHex::new(7);
        model
            .add_texture(
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: Arc::new(vec![0u8; 4 * 1024 * 1024]),
                },
                &mut hex,
            )
            .unwrap();
        let one = History::snapshot(model.clone());
        let payload = one
            .model
            .texture(&model.texture_ids()[0])
            .unwrap()
            .data
            .capacity();
        assert!(one.own_bytes < payload, "the payload is not charged twice");

        let mut history = History::default();
        for _ in 0..8 {
            history.push_undo(model.clone());
        }

        let bytes = history.bytes();
        assert!(
            bytes > payload,
            "the shared payload is counted: {bytes} <= {payload}"
        );
        assert!(
            bytes < payload + 8 * one.own_bytes + payload,
            "the shared payload is counted once, not eight times: {bytes}"
        );

        // One snapshot still holds the payload, so it is still counted...
        history.trim(1, usize::MAX);
        assert_eq!(history.undo.len(), 1);
        assert!(history.bytes() > payload);

        // ...and once none does, it stops being counted at all.
        history.trim(0, usize::MAX);
        assert!(history.undo.is_empty());
        assert_eq!(history.bytes(), 0);
    }

    /// Undo, redo and trimming all move snapshots between the stacks; the
    /// ledger has to follow them or it drifts.
    #[test]
    fn the_history_budget_tracks_undo_and_redo() {
        // Same-length names so the three snapshots weigh the same and the
        // totals below are exact rather than approximate.
        let mut history = History::default();
        let mut current = named_model("ccc");
        history.push_undo(named_model("aaa"));
        history.push_undo(named_model("bbb"));
        let full = history.bytes();

        history.undo(&mut current).unwrap();
        assert_eq!(root_name(&current), "bbb");
        assert_eq!(history.bytes(), full, "a snapshot moved, none was created");

        history.redo(&mut current).unwrap();
        assert_eq!(root_name(&current), "ccc");
        assert_eq!(history.bytes(), full);

        // A fresh edit discards the redo stack and stops counting it.
        history.undo(&mut current).unwrap();
        assert_eq!(history.redo.len(), 1);
        history.push_undo(named_model("ddd"));
        assert!(history.redo.is_empty());
        assert_eq!(history.bytes(), full);

        history.trim(1, usize::MAX);
        assert_eq!(history.undo.len(), 1);
        assert_eq!(history.bytes(), history.undo[0].own_bytes);
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
            ResponseBody::Tree { root } => root.id,
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
            ResponseBody::Tree { root } => root.id,
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
            ResponseBody::Tree { root } => root.id,
            other => panic!("{other:?}"),
        };
        for id in 3..=4 {
            ed.handle(req(
                id,
                Command::NodeAdd {
                    session: s,
                    parent: root.clone(),
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
                parent: a.root.id.clone(),
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
            pose: vec![ParamPose {
                param: ParamId::new("x").unwrap(),
                value: 1.0,
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
            ResponseBody::Presence { presence: Some(p) } => {
                assert_eq!(p.pose[0].param.as_str(), "x")
            }
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
                pose: vec![],
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

    /// A Preview request names params by Id and a param is a scalar, so the
    /// pose is the model's own [`Pose`] with nothing to resolve. Pixels are
    /// what says it landed: a preview that quietly rendered at rest would
    /// still write a valid PNG.
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
        let shot = |id: u64, name: &str, pose: Vec<ParamPose>| -> Vec<u8> {
            let out = dir.join(format!("{name}.png"));
            match ed.handle(req(
                id,
                Command::Preview {
                    session: s,
                    pose,
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
        let pull = ed
            .with_model(s, |model| {
                model
                    .param_ids()
                    .iter()
                    .find(|id| model.param(id).is_some_and(|p| p.name.as_str() == "pull"))
                    .cloned()
            })
            .unwrap()
            .expect("welded_seam has a `pull` param");
        let posed = shot(
            2,
            "posed",
            vec![ParamPose {
                param: pull,
                value: 1.0,
            }],
        );
        let back = shot(3, "back", Vec::new());

        assert_ne!(rest, posed, "posing pull must change the preview");
        assert_eq!(rest, back, "leaving pull out must render it at its default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
