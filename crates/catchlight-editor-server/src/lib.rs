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
//!   entries. It is the only command a client with its own puppet may serve
//!   itself instead of sending here. [`Command::DeformVertices`] authors the same offsets into the
//!   model and costs exactly one. Everything that edits the document goes
//!   through [`Editor::edit_session`], which is the single place an undo
//!   snapshot is taken.
//!
//! - **An observer never runs under a lock.** [`Editor::subscribe`] registers
//!   a callback for every [`Event`], and a callback's whole reason to exist is
//!   to read the session it was just told about. So every emission point
//!   collects what to say, drops the sessions map and the session guard, and
//!   only then calls out. Re-entering [`Editor`] from an observer is expected.
//!
//! - **The undo budget counts shared bytes once.** See [`History`]: 64
//!   snapshots of one model hold its textures once, not 64 times.
//!
//! - **Each session draws its own Ids.** See [`session_hex`]: the seed comes
//!   from the [`SessionId`], so two documents open at once and edited the same
//!   way do not name their new nodes identically — while replaying a script
//!   against a fresh editor still rebuilds the same model, Ids included.
//!
//! - **One render cache per previewed session.** See [`preview`]: a cache's
//!   slots name GPU state inside the one warm renderer, so switching the
//!   previewed session re-prepares it.
//!
//! - **A `path` is a storage key, not a filesystem path.** See [`storage`]:
//!   every command that names bytes resolves its key through a [`Storage`],
//!   so the same command set serves the filesystem, the browser and a blob
//!   store, and a command that read a key *into* a document releases it — so a
//!   session opened from a transient upload holds no `file` to save back to.
//!   [`Command::Preview`] is the one command that is still native — it
//!   needs the headless renderer, not just bytes.
//!
//! - **A model-only read has one implementation.** See [`query`]: the reads
//!   [`CommandKind::ReplicaQuery`] names are pure functions of the [`Model`],
//!   so a browser tab holding a replica answers them itself. `dispatch` routes
//!   those arms into the very same code rather than keeping a second copy.
//!
//! - **A browser gets the same protocol, plus bytes.** See [`http`]: `/ws`
//!   carries one JSON [`Request`] per text frame and answers each with its
//!   [`Reply`], exactly as [`serve_unix`] does, and additionally pushes every
//!   [`Event`] to the connections that are open; the structure and texture
//!   payloads a replica needs go over HTTP rather than through a frame. Loopback
//!   is not a permission — any page on any origin can reach that port — so a
//!   random per-launch token gates every door, and `GET /token` is readable only
//!   from an allowlisted origin.

#[cfg(not(target_arch = "wasm32"))]
mod http;
#[cfg(not(target_arch = "wasm32"))]
mod preview;
mod query;
mod storage;
#[cfg(unix)]
mod transport;

#[cfg(not(target_arch = "wasm32"))]
pub use http::{bind_http, serve_http, HttpOptions, HttpServer};
pub use query::{replica_query, replica_reply, seam_info};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::FileStorage;
pub use storage::{join_key, key_stem, parent_key, NoStorage, StagingStorage, Storage};

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding};
use catchlight_core::id::{HexSource as _, Name, SeededHex};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::Vec2;
use catchlight_core::{
    BindingKey, BindingTarget, Model, ModelComposite, ModelError, ModelMeshGroup, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelPhysics, ModelTexture, ModelWeld, Puppet,
    ScalarTarget, DEFAULT_SLOT_WEIGHT,
};
// Only the headless preview builds one; the browser GUI poses its own puppet.
#[cfg(not(target_arch = "wasm32"))]
use catchlight_core::Pose;
use catchlight_editor_core::{
    contour_automesh, grid_automesh, AlphaMask, ContourKnobs, Manifest, ManifestError, MeshError,
    ModelManifestExt as _, ModelMeshExt as _, TextureData, UvMap,
};

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
    #[error("{0}")]
    BadTarget(String),
    /// The command does not belong on the path that was asked to run it —
    /// a replica handed a command only the editor can apply.
    #[error("{0}")]
    BadRequest(String),
    /// The two seams named are not welded to each other.
    #[error("no weld pairs those two seams")]
    UnknownWeld,
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("session has no file; pass a path to save")]
    NoSavePath,
    /// The command needs a part and was given something else.
    #[error("node {0} is not a part")]
    NotAPart(NodeId),
    /// The part draws no texture, so there is no alpha to trace.
    #[error("part {0} draws no texture")]
    NoAlbedo(NodeId),
    /// Building the mesh itself failed — an empty alpha mask, a triangulation
    /// the solver refused.
    #[error("mesh: {0}")]
    Mesh(#[from] MeshError),
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
    /// An enum name the server does not know: a binding target, a blend
    /// mode, an interpolation mode.
    fn unknown(kind: &str, name: &str) -> Self {
        Self::BadTarget(format!("unknown {kind} {name:?}"))
    }

    /// The wire code a client branches on. The message stays for a person;
    /// this is what a commit gate or a mesh editor reacts to.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NoSession(_) => ErrorCode::NoSession,
            Self::NoNode(_) => ErrorCode::NoNode,
            Self::NoParam(_) => ErrorCode::NoParam,
            Self::NoTexture(_) => ErrorCode::NoTexture,
            Self::BadTarget(_) => ErrorCode::BadTarget,
            Self::BadRequest(_) => ErrorCode::BadRequest,
            Self::UnknownWeld => ErrorCode::UnknownWeld,
            // A node of the wrong kind is the same answer a bad binding
            // target gets: the command parsed and does not fit what it names.
            Self::NotAPart(_) => ErrorCode::BadTarget,
            Self::NoAlbedo(_) => ErrorCode::NoTexture,
            // Only the empty mask is worth branching on — a client answers it
            // by offering a lower threshold. The rest is an ordinary refusal.
            Self::Mesh(MeshError::NothingToMesh) => ErrorCode::NothingToMesh,
            Self::Mesh(_) => ErrorCode::Edit,
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
                ModelError::DuplicateId(_) => ErrorCode::DuplicateId,
                ModelError::DuplicateSeam(_) => ErrorCode::DuplicateSeam,
                ModelError::DuplicateSlot(_) => ErrorCode::DuplicateSlot,
                ModelError::WeldSlotMismatch => ErrorCode::WeldSlotMismatch,
                ModelError::UnknownWeld => ErrorCode::UnknownWeld,
                ModelError::Fragment => ErrorCode::Fragment,
                _ => ErrorCode::Edit,
            },
        }
    }
}

/// What every session's Id seed is derived from. Fixed, so a given
/// [`SessionId`] always draws the same sequence.
const ID_SEED: u32 = 0x1d5e_ed01;

/// The Id source for one session, seeded from its [`SessionId`].
///
/// **Deterministic and per-session, both on purpose.** Deterministic because a
/// test that pins a generated Id has to keep passing, and because a model
/// built by replaying a recorded script should come out byte-identical.
/// Per-session because uniqueness is checked *within* a model: two sessions
/// sharing one seed mint the same Ids for the same edits, so a part copied
/// between them collides on arrival and an addon extracted from one names
/// something in the other by accident.
///
/// The seed is drawn *by* a `SeededHex` rather than mixed here, so consecutive
/// session ids land far apart in the sequence instead of one Weyl step apart —
/// which is what adding the id to the seed would have done, making session 2's
/// first Id session 1's second.
fn session_hex(id: SessionId) -> SeededHex {
    SeededHex::new(SeededHex::new(ID_SEED ^ id.0 as u32).next_bits())
}

struct Session {
    model: Model,
    /// Where generated Ids come from. See [`session_hex`]: seeded from the
    /// session's own Id, so two sessions never mint the same one for the same
    /// edits. Uniqueness within a model is still the model's job.
    hex: SeededHex,
    title: String,
    /// The storage key this session was opened from / last saved to, if any.
    /// Opaque — see [`storage`].
    file: Option<String>,
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
/// bill one model's textures once per undo step and collapse the history of any
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
            // The payload's address inside its own `Arc` allocation, so
            // one address per payload: an empty payload still owns an
            // allocation, unlike an empty `Vec`'s dangling buffer.
            let at = Arc::as_ptr(&texture.data) as *const u8 as usize;
            if !out.iter().any(|&(seen, _)| seen == at) {
                out.push((at, texture.data.len()));
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
    fn new(id: SessionId, model: Model, title: String, file: Option<String>) -> Self {
        Self {
            model,
            hex: session_hex(id),
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
    ///
    /// `id` is the Id the request asked for; `None` draws a free one. The
    /// model refuses a chosen Id it already carries, which is the whole check
    /// — the charset was validated where the request was decoded.
    fn add_node(
        &mut self,
        parent: &NodeId,
        id: Option<NodeId>,
        node: ModelNode,
    ) -> Result<NodeId, EditorError> {
        let Self { model, hex, .. } = self;
        match id {
            Some(id) => {
                model.add_node_with_id(id.clone(), parent, node)?;
                Ok(id)
            }
            None => Ok(model.add_node(parent, node, hex)?),
        }
    }

    fn add_param(
        &mut self,
        id: Option<ParamId>,
        param: ModelParam,
    ) -> Result<ParamId, EditorError> {
        let Self { model, hex, .. } = self;
        match id {
            Some(id) => {
                model.add_param_with_id(id.clone(), param)?;
                Ok(id)
            }
            None => Ok(model.add_param(param, hex)?),
        }
    }

    /// Add `texture` and point `part` at it, in one edit. Hands back the new
    /// Id and whatever the upload displaced: a texture the part had been the
    /// last to draw goes with the same edit.
    fn add_texture(
        &mut self,
        part: &NodeId,
        id: Option<TexId>,
        texture: ModelTexture,
    ) -> Result<(TexId, Vec<TexId>), EditorError> {
        let dropped = self
            .model
            .texture_dropped_by_repointing(part, None)
            .into_iter()
            .collect();
        let Self { model, hex, .. } = self;
        let id = match id {
            Some(id) => {
                model.add_texture_with_id(id.clone(), part, texture)?;
                id
            }
            None => model.add_texture(part, texture, hex)?,
        };
        Ok((id, dropped))
    }

    fn seam_add_generated(&mut self, node: &NodeId) -> Result<SeamId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.seam_add_generated(node, hex)?)
    }

    fn slot_add_generated(&mut self, node: &NodeId, seam: &SeamId) -> Result<SlotId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.slot_add_generated(node, seam, hex)?)
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

/// A callback the editor hands every [`Event`] to.
///
/// It is called with no editor lock held, from whichever thread ran the
/// command, so it may call straight back into [`Editor`] — a transport
/// observer does exactly that, reading the session it was told changed.
pub type Observer = Box<dyn Fn(&Event) + Send + Sync>;

/// One registered [`Observer`], shared so the list can be copied out from
/// under the lock before any of them is called.
type SharedObserver = Arc<dyn Fn(&Event) + Send + Sync>;

pub struct Editor {
    sessions: Mutex<HashMap<SessionId, Arc<Mutex<Session>>>>,
    next_id: AtomicU64,
    /// Everyone listening for [`Event`]s, by the handle [`Editor::subscribe`]
    /// gave out.
    observers: Mutex<Vec<(u64, SharedObserver)>>,
    next_observer: AtomicU64,
    /// Resolves the keys the protocol calls `path`. See [`storage`].
    storage: Arc<dyn Storage>,
    #[cfg(not(target_arch = "wasm32"))]
    preview_seq: AtomicU64,
    #[cfg(not(target_arch = "wasm32"))]
    preview: Mutex<Option<PreviewRenderer>>,
}

impl Editor {
    /// An editor over the ambient default store: the filesystem natively,
    /// rooted at the current directory, [`NoStorage`] on wasm — where the host
    /// must supply one with [`Editor::with_storage`].
    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let storage = Arc::new(FileStorage::default());
        #[cfg(target_arch = "wasm32")]
        let storage = Arc::new(NoStorage);
        Self::with_storage(storage)
    }

    /// An editor whose `path` keys resolve through `storage`.
    pub fn with_storage(storage: Arc<dyn Storage>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            observers: Mutex::new(Vec::new()),
            next_observer: AtomicU64::new(1),
            storage,
            #[cfg(not(target_arch = "wasm32"))]
            preview_seq: AtomicU64::new(0),
            #[cfg(not(target_arch = "wasm32"))]
            preview: Mutex::new(None),
        }
    }

    /// Apply one request and produce its reply. Synchronous and the single
    /// funnel for every client (in-process or socket).
    pub fn handle(&self, req: Request) -> Reply {
        // `Document` is the one kind that may move a session's revision.
        // That classification is what a client picks its send method by — a
        // presence or scratch command that quietly bumped `rev` would
        // re-render every panel on every pointer move, and a query that did
        // would skip the re-render entirely. Checked here rather than per arm
        // so it holds for commands nobody thought to test.
        #[cfg(debug_assertions)]
        let kind = req.command.kind();
        #[cfg(debug_assertions)]
        let revs_before = self.revs();

        // Read before dispatch consumes the command. A create/open/import
        // names its session only in the reply, so the body has the last word.
        let addressed = req.command.session();

        let reply = match self.dispatch(req.command) {
            Ok(body) => {
                let session = match &body {
                    ResponseBody::Session { session } => Some(*session),
                    _ => addressed,
                };
                Reply::Ok {
                    id: req.id,
                    rev: session.and_then(|id| self.rev(id)),
                    body,
                }
            }
            Err(e) => Reply::Err {
                id: req.id,
                code: e.code(),
                message: e.to_string(),
            },
        };

        #[cfg(debug_assertions)]
        if kind != CommandKind::Document {
            debug_assert_eq!(
                self.revs(),
                revs_before,
                "a {kind:?} command moved a session revision; either it belongs \
                 in CommandKind::Document or it should not be editing",
            );
        }
        reply
    }

    /// One session's revision, or `None` if it is not open — which is what a
    /// `session_close` reply reports, its session having just gone.
    fn rev(&self, id: SessionId) -> Option<u64> {
        let session = self.session(id).ok()?;
        let rev = lock(&session).rev;
        Some(rev)
    }

    /// [`Self::rev`] for a caller outside this crate: what an in-process
    /// replica stamps on the model it just took through [`Self::with_model`].
    pub fn revision(&self, id: SessionId) -> Option<u64> {
        self.rev(id)
    }

    /// Register `observer` for every [`Event`] this editor emits, and hand
    /// back the handle [`Editor::unsubscribe`] takes.
    ///
    /// The callback runs on whichever thread ran the command, with no editor
    /// lock held.
    pub fn subscribe(&self, observer: Observer) -> u64 {
        let handle = self.next_observer.fetch_add(1, Ordering::Relaxed);
        lock(&self.observers).push((handle, Arc::from(observer)));
        handle
    }

    /// Drop the observer `handle` names. Unknown handles are ignored, so
    /// unsubscribing twice is safe.
    pub fn unsubscribe(&self, handle: u64) {
        lock(&self.observers).retain(|(h, _)| *h != handle);
    }

    /// Hand `event` to every observer.
    ///
    /// The list is copied out and the guard dropped before the first call: an
    /// observer reads the editor, and one that subscribed or unsubscribed
    /// from inside a callback would otherwise deadlock on this very lock.
    fn notify(&self, event: Event) {
        let observers: Vec<SharedObserver> = {
            let guard = lock(&self.observers);
            guard.iter().map(|(_, o)| o.clone()).collect()
        };
        for observer in observers {
            observer(&event);
        }
    }

    /// Say that `session`'s document now reads as `rev`. Every revision move
    /// routes through here, and so does a save — a title bar reads `dirty` the
    /// same way it reads the tree.
    fn notify_document(&self, session: SessionId, rev: u64) {
        self.notify(Event::DocumentChanged { session, rev });
    }

    /// Say that the set of open sessions changed. Carries nothing: an
    /// observer that cares re-reads the list.
    fn notify_sessions(&self) {
        self.notify(Event::SessionsChanged);
    }

    /// Every open session's revision, for the debug check in [`Self::handle`].
    /// Sessions are few and this only exists in a debug build, so the walk is
    /// cheaper than threading the addressed session out of every command.
    #[cfg(debug_assertions)]
    fn revs(&self) -> Vec<(SessionId, u64)> {
        let handles: Vec<_> = lock(&self.sessions)
            .iter()
            .map(|(&id, session)| (id, session.clone()))
            .collect();
        let mut revs: Vec<_> = handles
            .into_iter()
            .map(|(id, handle)| (id, lock(&handle).rev))
            .collect();
        revs.sort_by_key(|(id, _)| id.0);
        revs
    }

    fn alloc_id(&self) -> SessionId {
        SessionId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// The one place a session joins the editor — and so the one place
    /// `SessionsChanged` is emitted. The map guard is dropped first.
    fn insert_session(&self, id: SessionId, session: Session) {
        lock(&self.sessions).insert(id, Arc::new(Mutex::new(session)));
        self.notify_sessions();
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
        self.insert_session(id, Session::new(id, model, title.into(), None));
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
        let rev = self.with_session(id, |s| {
            s.saved_rev = s.rev;
            Ok(s.rev)
        })?;
        self.notify_document(id, rev);
        Ok(())
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
        part: &NodeId,
        encoding: TextureEncoding,
        bytes: Vec<u8>,
    ) -> Result<(TexId, Vec<TexId>), EditorError> {
        image_dims(&bytes)?;
        self.edit_session(id, |s| {
            let added = s.add_texture(
                part,
                None,
                ModelTexture {
                    encoding,
                    alpha: TextureAlpha::Straight,
                    data: bytes.into(),
                },
            )?;
            s.touch();
            Ok(added)
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
    /// the document (rev bumped). The snapshot is a shallow clone — meshes,
    /// binding grids and texture payloads all ride behind an `Arc` and are
    /// copied only when something edits them (see [`History`]) — but it still
    /// walks and copies the whole tree, which is why read-only commands stay
    /// on `with_session`.
    fn edit_session<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<R, EditorError> {
        let handle = self.session(id)?;
        let (result, moved) = {
            let mut session = lock(&handle);
            let before = session.rev;
            let snapshot = session.model.clone();
            let result = f(&mut session);
            let mut moved = None;
            match &result {
                Ok(_) if session.rev != before => {
                    session.push_undo(snapshot);
                    moved = Some(session.rev);
                }
                Ok(_) => {}
                Err(_) => {
                    // A failed command must leave no partial edit behind —
                    // multi-step commands can fail midway through mutating.
                    session.model = snapshot;
                    session.rev = before;
                    session.puppet = None;
                }
            }
            (result, moved)
        };
        // Outside the guard: an observer reads the session it was told about.
        if let Some(rev) = moved {
            self.notify_document(id, rev);
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
            root: query::build_tree(model, root),
            params: query::param_infos(model),
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

    /// Read a manifest and the storage keys its textures live at.
    ///
    /// The one place a manifest's texture references are resolved:
    /// [`Command::SessionImport`] reads those keys, and
    /// [`Command::ManifestRequirements`] reports them, so a client that stages
    /// bytes itself stages exactly what the import will ask for.
    fn read_manifest(&self, manifest_path: &str) -> Result<(Manifest, Vec<String>), EditorError> {
        let json = String::from_utf8(self.storage.read(manifest_path)?).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{manifest_path}: manifest is not UTF-8: {e}"),
            )
        })?;
        let manifest = Manifest::from_json(&json)?;
        // Texture references are relative to the manifest's own key.
        let base = parent_key(manifest_path);
        let keys = manifest
            .textures
            .iter()
            .map(|t| join_key(base, &t.path))
            .collect();
        Ok((manifest, keys))
    }

    fn dispatch(&self, cmd: Command) -> Result<ResponseBody, EditorError> {
        match cmd {
            Command::SessionNew { name } => {
                let id = self.alloc_id();
                let title = name.unwrap_or_else(|| format!("untitled-{}", id.0));
                self.insert_session(id, Session::new(id, Model::new(), title, None));
                Ok(ResponseBody::Session { session: id })
            }
            Command::SessionOpen { path } => {
                let model = Model::from_clm_bytes(&self.storage.read(&path)?)?;
                let id = self.alloc_id();
                let title = key_stem(&path);
                // The model owns its own copy now, so the store may drop a
                // staged upload — and a key that was one is not a file this
                // session can save back to. See [`storage`].
                let file = (!self.storage.release(&path)).then_some(path);
                self.insert_session(id, Session::new(id, model, title, file));
                Ok(ResponseBody::Session { session: id })
            }
            Command::SessionImport { manifest_path } => {
                let (manifest, keys) = self.read_manifest(&manifest_path)?;
                let mut data = HashMap::new();
                for (t, key) in manifest.textures.iter().zip(&keys) {
                    let bytes = self.storage.read(key)?;
                    data.insert(
                        t.id.clone(),
                        TextureData {
                            encoding: encoding_from_path(&t.path),
                            bytes: bytes.into(),
                        },
                    );
                }
                let model = Model::from_manifest(&manifest, &data)?;
                // Everything the import read is in the model; release what was
                // staged for it. An import already has no `file`.
                self.storage.release(&manifest_path);
                for key in &keys {
                    self.storage.release(key);
                }
                let id = self.alloc_id();
                let title = if manifest.name.is_empty() {
                    key_stem(&manifest_path)
                } else {
                    manifest.name.clone()
                };
                self.insert_session(id, Session::new(id, model, title, None));
                Ok(ResponseBody::Session { session: id })
            }
            Command::ManifestRequirements { manifest_path } => {
                let (_, textures) = self.read_manifest(&manifest_path)?;
                Ok(ResponseBody::ManifestRequirements { textures })
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
                        file: s.file.clone(),
                        dirty: s.dirty(),
                        rev: s.rev,
                        node_count: s.model.node_count() as u32,
                    });
                }
                sessions.sort_by_key(|s| s.session.0);
                Ok(ResponseBody::Sessions { sessions })
            }
            Command::SessionClose { session } => {
                let removed = lock(&self.sessions).remove(&session);
                removed.ok_or(EditorError::NoSession(session))?;
                self.notify_sessions();
                Ok(ResponseBody::Empty)
            }
            Command::Save { session, path } => {
                let handle = self.session(session)?;
                let (key, bytes, rev) = {
                    let s = lock(&handle);
                    let key = match path {
                        Some(p) => p,
                        None => s.file.clone().ok_or(EditorError::NoSavePath)?,
                    };
                    (key, s.model.to_clm_bytes()?, s.rev)
                };
                // The store owns write atomicity; see `storage`.
                self.storage.write(&key, &bytes)?;
                {
                    let mut s = lock(&handle);
                    s.file = Some(key.clone());
                    s.saved_rev = rev;
                }
                // A save moves no revision but does flip `dirty`, which a
                // title bar reads the same way it reads the tree.
                self.notify_document(session, rev);
                Ok(ResponseBody::Saved { path: key })
            }
            Command::ExportManifest { session, path } => {
                let handle = self.session(session)?;
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
                self.storage.write(&path, manifest.as_bytes())?;
                // Textures land beside the manifest, as its own references
                // expect them.
                let base = parent_key(&path).to_string();
                for (name, data) in textures {
                    self.storage.write(&join_key(&base, &name), &data)?;
                }
                Ok(ResponseBody::Saved { path })
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
            // The model-only reads. A browser tab answers these against its
            // own replica, so they live in `query` and the editor runs the
            // very same code against the session's model.
            Command::Check { session }
            | Command::NodeTree { session }
            | Command::NodeInfo { session, .. }
            | Command::TextureList { session }
            | Command::ParamList { session }
            | Command::BindingList { session, .. }
            | Command::Seams { session, .. }
            | Command::Welds { session }
            | Command::UnfilledSlots { session } => {
                self.with_model(session, |model| query::replica_query(model, &cmd))?
            }
            Command::NodeAdd {
                session,
                parent,
                kind,
                name,
                node: id,
            } => self.edit_session(session, |s| {
                let node =
                    ModelNode::new(name.unwrap_or_else(|| default_name(kind)), make_kind(kind));
                let node = s.add_node(&parent, id, node)?;
                s.touch();
                Ok(ResponseBody::Node {
                    node,
                    dropped: Vec::new(),
                })
            }),
            Command::NodeSet {
                session,
                node,
                patch,
            } => self.edit_session(session, |s| {
                let mut dropped = Vec::new();
                // `clear_texture` wins over `texture`: one says "draw none",
                // the other "draw this", and a patch carrying both means the
                // former, the way `clear_target_params` beats `target_params`.
                let albedo = match (patch.clear_texture, &patch.texture) {
                    (true, _) => Some(None),
                    (false, Some(tex)) => {
                        if s.model.texture(tex).is_none() {
                            return Err(EditorError::NoTexture(tex.clone()));
                        }
                        Some(Some(tex.clone()))
                    }
                    (false, None) => None,
                };
                if let Some(albedo) = albedo {
                    if matches!(
                        s.model.node(&node).map(|n| &n.kind),
                        Some(ModelNodeKind::Part(_))
                    ) {
                        // Repointing the last part drawing a texture deletes
                        // it; the reply says so, because nothing downstream
                        // of this session gets it back.
                        dropped.extend(
                            s.model
                                .texture_dropped_by_repointing(&node, albedo.as_ref()),
                        );
                        s.model.set_part_albedo(&node, albedo)?;
                    }
                }
                s.model.update_node(&node, |n| apply_patch(n, &patch))??;
                s.touch();
                Ok(ResponseBody::Node { node, dropped })
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
                Ok(ResponseBody::Node {
                    node: copy,
                    dropped: Vec::new(),
                })
            }),
            Command::RenameId { session, rename } => self.edit_session(session, |s| {
                match rename {
                    Rename::Node { from, to } => s.model.rename_node_id(&from, to)?,
                    Rename::Param { from, to } => s.model.rename_param_id(&from, to)?,
                    Rename::Texture { from, to } => s.model.rename_tex_id(&from, to)?,
                    Rename::Seam { node, from, to } => s.model.rename_seam(&node, &from, to)?,
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
                    .map(|m| {
                        parse_pendulum_kind(m)
                            .ok_or_else(|| EditorError::unknown("pendulum kind", m))
                    })
                    .transpose()?;
                let parsed_map = map_mode
                    .as_deref()
                    .map(|m| parse_map_mode(m).ok_or_else(|| EditorError::unknown("map mode", m)))
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
                let dropped = s.model.textures_dropped_by_deleting(&node);
                s.model.delete_node(&node)?;
                s.touch();
                Ok(ResponseBody::Node { node, dropped })
            }),
            Command::TextureAdd {
                session,
                node,
                path,
                texture: id,
            } => {
                // Read outside `edit_session`: the store is not the session's
                // to borrow, and a failed read must not open an edit.
                let bytes = self.storage.read(&path)?;
                let encoding = encoding_from_path(&path);
                let added = self.edit_session(session, move |s| {
                    let bytes = bytes;
                    image_dims(&bytes)?; // validate it decodes
                    let (texture, dropped) = s.add_texture(
                        &node,
                        id,
                        ModelTexture {
                            encoding,
                            alpha: TextureAlpha::Straight,
                            data: bytes.into(),
                        },
                    )?;
                    s.touch();
                    Ok(ResponseBody::Texture { texture, dropped })
                })?;
                // Only now: a command that failed keeps its bytes staged so
                // the caller may retry without uploading them again.
                self.storage.release(&path);
                Ok(added)
            }
            Command::ParamAdd {
                session,
                name,
                min,
                max,
                default,
                key_positions,
                param: id,
            } => self.edit_session(session, |s| {
                if !catchlight_core::param_range_is_valid(min, max) {
                    return Err(ModelError::CellOutOfRange.into());
                }
                let param = s.add_param(
                    id,
                    ModelParam {
                        name: Name::truncated(name),
                        min,
                        max,
                        default,
                        key_positions: if key_positions.is_empty() {
                            vec![0.0, 1.0]
                        } else {
                            key_positions
                        },
                    },
                )?;
                s.touch();
                Ok(ResponseBody::Param { param })
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
                let t = ScalarTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
                let t = ScalarTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
                            .ok_or_else(|| EditorError::unknown("binding target", &e.target))
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
                let t = BindingTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
                let t = BindingTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
                let t = BindingTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
                let t = BindingTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
                let t = BindingTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
                let t = BindingTarget::parse(&target)
                    .ok_or_else(|| EditorError::unknown("binding target", &target))?;
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
            Command::MeshAuto {
                session,
                node,
                mode,
            } => self.edit_session(session, |s| {
                let mesh = automesh(&s.model, &node, mode)?;
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
                let seam = match seam {
                    Some(seam) => {
                        s.model.seam_add(&node, seam.clone())?;
                        seam
                    }
                    None => s.seam_add_generated(&node)?,
                };
                s.touch();
                Ok(ResponseBody::Seam {
                    seam: SeamAddr { node, seam },
                })
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
                let slot = match slot {
                    Some(slot) => {
                        s.model.slot_add(&node, &seam, slot.clone())?;
                        slot
                    }
                    None => s.slot_add_generated(&node, &seam)?,
                };
                s.touch();
                Ok(ResponseBody::Slot {
                    slot: SlotAddr { node, seam, slot },
                })
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
            Command::WeldWeight {
                session,
                a,
                b,
                slot,
                weight,
            } => self.edit_session(session, |s| {
                s.model.set_weld_slot_weight(
                    (&a.node, &a.seam),
                    (&b.node, &b.seam),
                    &slot,
                    weight,
                )?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::WeldDelete { session, a, b } => self.edit_session(session, |s| {
                // A seam delete already unmakes a weld, by taking one of its
                // ends with it. This is the edit that leaves both seams and
                // their slots exactly where they are.
                let mut welds = s.model.welds().to_vec();
                let before = welds.len();
                welds.retain(|w| !joins(w, &a, &b));
                if welds.len() == before {
                    return Err(EditorError::UnknownWeld);
                }
                s.model.set_welds(welds)?;
                s.touch();
                Ok(ResponseBody::Empty)
            }),
            Command::Undo { session } => {
                let handle = self.session(session)?;
                let rev = {
                    let mut s = lock(&handle);
                    s.undo()?;
                    s.rev
                };
                self.notify_document(session, rev);
                Ok(ResponseBody::Empty)
            }
            Command::Redo { session } => {
                let handle = self.session(session)?;
                let rev = {
                    let mut s = lock(&handle);
                    s.redo()?;
                    s.rev
                };
                self.notify_document(session, rev);
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
                node: id,
            } => self.edit_session(session, |s| {
                let phys_kind = parse_pendulum_kind(&kind)
                    .ok_or_else(|| EditorError::unknown("pendulum kind", &kind))?;
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
                let node = s.add_node(&parent, id, node)?;
                s.model.set_physics_targets(&node, targets)?;
                s.touch();
                Ok(ResponseBody::Node {
                    node,
                    dropped: Vec::new(),
                })
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
                // The scratch path: a drag shows on the puppet and leaves the
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
            // Preview is the one command that stays native: it needs the
            // headless renderer, not just bytes. Everything else that used to
            // be native-only now resolves its key through `storage`.
            #[cfg(target_arch = "wasm32")]
            Command::Preview { .. } => Err(EditorError::NativeOnly),
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
/// The two outputs a driver writes, positionally.
///
/// A `None` entry is an output nothing is bound to, which is why the list is
/// positional rather than a set: a driver whose length drives a param and
/// whose angle drives none is `[None, Some(len)]`, and there is no other way
/// to say it. A list shorter than two leaves the rest unbound.
fn physics_targets(
    model: &Model,
    params: Vec<Option<ParamId>>,
) -> Result<[Option<ParamId>; 2], EditorError> {
    if params.len() > 2 {
        return Err(EditorError::BadTarget(
            "a driver writes at most two params".into(),
        ));
    }
    let mut targets = [None, None];
    for (slot, id) in targets.iter_mut().zip(params) {
        if let Some(id) = id {
            if model.param(&id).is_none() {
                return Err(EditorError::NoParam(id));
            }
            *slot = Some(id);
        }
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

/// The grid `mesh_auto` lays down when the request names no size — the same
/// one the desktop mesh editor opens on.
const AUTOMESH_GRID: (u32, u32) = (6, 6);

/// Derive `node`'s mesh from the alpha of the texture it draws.
///
/// **The editor traces, because the editor holds the bytes.** A client that
/// did this itself would decode the image, guess the UV mapping, and send back
/// a mesh — and any of the three could disagree with what the editor thinks
/// the part looks like.
///
/// **The UV mapping comes from the mesh being replaced, when there is one to
/// read.** `UvMap::fit` recovers where the texture actually sits relative to
/// the node's origin, which is not the centered convention for a model
/// imported with cropped textures; falling back to the texture's own size is
/// right only for a part that has nothing to fit. Getting this wrong moves the
/// art rather than failing, so the fit comes first — which is also what the
/// desktop mesh editor does, so both produce the same mesh.
fn automesh(model: &Model, node: &NodeId, mode: AutoMesh) -> Result<ClmMesh, EditorError> {
    let Some(ModelNodeKind::Part(part)) = model.node(node).map(|n| &n.kind) else {
        return match model.node(node) {
            Some(_) => Err(EditorError::NotAPart(node.clone())),
            None => Err(EditorError::NoNode(node.clone())),
        };
    };
    let albedo = part
        .albedo()
        .ok_or_else(|| EditorError::NoAlbedo(node.clone()))?;
    let texture = model
        .texture(albedo)
        .ok_or_else(|| EditorError::NoTexture(albedo.clone()))?;
    let alpha = AlphaMask::decode(&texture.data)
        .ok_or_else(|| EditorError::Image(format!("texture {albedo} does not decode")))?;

    let mesh = part.mesh();
    let uv_map = UvMap::fit(&mesh.verts, &mesh.uvs)
        .unwrap_or_else(|| UvMap::from_texture_size(alpha.width as f32, alpha.height as f32));
    let origin = mesh.origin;

    let working = match mode {
        AutoMesh::Contour {
            threshold,
            simplify,
            margin,
            spacing,
        } => {
            let d = ContourKnobs::default();
            let knobs = ContourKnobs {
                threshold: threshold.unwrap_or(d.threshold),
                simplify: simplify.unwrap_or(d.simplify),
                margin: margin.unwrap_or(d.margin),
                spacing: spacing.unwrap_or(d.spacing),
            };
            contour_automesh(&alpha, &knobs, &uv_map, origin)?
        }
        AutoMesh::Grid {
            threshold,
            cols,
            rows,
        } => grid_automesh(
            &alpha,
            threshold.unwrap_or_else(|| ContourKnobs::default().threshold),
            cols.unwrap_or(AUTOMESH_GRID.0),
            rows.unwrap_or(AUTOMESH_GRID.1),
            &uv_map,
            origin,
        )?,
    };
    // A trace that found no contour at all leaves nothing to triangulate, and
    // an empty mesh would silently blank the part.
    if working.vertex_count() < 3 {
        return Err(MeshError::NothingToMesh.into());
    }
    Ok(working.to_mesh(&uv_map, Some(&alpha))?)
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

/// Whether `weld` is the one joining these two seams. A weld has no Id of its
/// own — its two ends are what names it — so either order finds it.
fn joins(weld: &ModelWeld, a: &SeamAddr, b: &SeamAddr) -> bool {
    let is = |end: &(NodeId, SeamId), addr: &SeamAddr| end.0 == addr.node && end.1 == addr.seam;
    (is(weld.a(), a) && is(weld.b(), b)) || (is(weld.a(), b) && is(weld.b(), a))
}

/// In-process document view handed to observers (the GUI); refs are stable for
/// the session's lifetime.
#[derive(Debug, Clone)]
pub struct DocSnapshot {
    pub rev: u64,
    pub root: TreeNode,
    pub params: Vec<ParamInfo>,
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
        .map(|s| BlendMode::from_name(s).ok_or_else(|| EditorError::unknown("blend mode", s)))
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
) -> Result<catchlight_core::interpolate::InterpolateMode, EditorError> {
    use catchlight_core::interpolate::InterpolateMode as I;
    match s.to_ascii_lowercase().as_str() {
        "nearest" => Ok(I::Nearest),
        "stepped" => Ok(I::Stepped),
        "linear" => Ok(I::Linear),
        "cubic" => Ok(I::Cubic),
        other => Err(EditorError::unknown("interpolation mode", other)),
    }
}

fn parse_mask_mode(s: &str) -> Result<MaskMode, EditorError> {
    match s.to_ascii_lowercase().as_str() {
        "mask" => Ok(MaskMode::Mask),
        "dodge" | "dodge_mask" | "dodgemask" => Ok(MaskMode::DodgeMask),
        other => Err(EditorError::unknown("mask mode", other)),
    }
}

/// The encoding a storage key's extension implies. A key is opaque to
/// everything else; this is the one place its tail is read, and only to
/// pick a decoder.
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

    /// An Id is validated where the request is decoded, so a string outside
    /// the charset never reaches a command. It still has to come back as a
    /// structured error carrying the *request's* id: a client blocks reading
    /// until it sees its own id, so an error correlated to 0 would hang it.
    #[cfg(unix)]
    #[test]
    fn an_invalid_id_on_the_wire_is_an_error_against_the_requests_own_id() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let editor = Editor::new();
            serve_connection(&editor, server);
        });
        // `a b` has a space; `.hidden` opens with a dot. Both are refused by
        // `catchlight_core::id`, and an unknown command shares the code.
        for line in [
            br#"{"id":11,"cmd":"node_delete","session":1,"node":"a b"}"#.as_slice(),
            br#"{"id":12,"cmd":"node_delete","session":1,"node":".hidden"}"#.as_slice(),
            br#"{"id":13,"cmd":"no_such_command","session":1}"#.as_slice(),
        ] {
            client.write_all(line).unwrap();
            client.write_all(b"\n").unwrap();
        }
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut reader = BufReader::new(client);
        for want in [11u64, 12, 13] {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            match serde_json::from_str::<Reply>(&line).unwrap() {
                Reply::Err { id, code, message } => {
                    assert_eq!(id, want, "the reply correlates to the request");
                    assert_eq!(code, ErrorCode::BadRequest);
                    assert!(message.starts_with("bad request:"), "{message}");
                }
                other => panic!("expected an error, got {other:?}"),
            }
        }
        server_thread.join().unwrap();
    }

    /// A well-formed Id the model does not carry is a different answer: the
    /// command parsed, it just names nothing.
    #[test]
    fn a_well_formed_id_the_model_lacks_is_a_no_node_error() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        assert!(matches!(
            ed.handle(req(
                2,
                Command::NodeDelete {
                    session: s,
                    node: NodeId::new("root/part-deadbeef").unwrap(),
                },
            )),
            Reply::Err {
                id: 2,
                code: ErrorCode::NoNode,
                ..
            }
        ));
    }

    /// Events, and who hears them.
    ///
    /// The observer reads the editor from inside the callback, which is the
    /// point: an emission that still held the sessions map or a session guard
    /// would deadlock here rather than in whatever transport ships first.
    #[test]
    fn an_observer_hears_every_change_until_it_unsubscribes() {
        let ed = Arc::new(Editor::new());
        let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let weak = Arc::downgrade(&ed);
        let handle = ed.subscribe(Box::new(move |event| {
            if let (Event::DocumentChanged { session, .. }, Some(ed)) = (event, weak.upgrade()) {
                // Re-entering the editor from an observer is the normal case.
                ed.with_model(*session, |m| m.node_count()).unwrap();
            }
            lock(&sink).push(event.clone());
        }));

        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        assert!(
            matches!(lock(&seen).as_slice(), [Event::SessionsChanged]),
            "a new session changes the set of open sessions",
        );
        lock(&seen).clear();

        body(ed.handle(req(
            2,
            Command::NodeAdd {
                session: s,
                parent: NodeId::new("root").unwrap(),
                kind: NodeKindArg::Group,
                name: None,
                node: None,
            },
        )));
        match lock(&seen).as_slice() {
            [Event::DocumentChanged { session, rev }] => {
                assert_eq!(*session, s);
                assert_eq!(*rev, 1, "the event carries the revision after the edit");
            }
            other => panic!("expected one DocumentChanged, got {other:?}"),
        }
        lock(&seen).clear();

        body(ed.handle(req(3, Command::SessionClose { session: s })));
        assert!(matches!(lock(&seen).as_slice(), [Event::SessionsChanged]));
        lock(&seen).clear();

        ed.unsubscribe(handle);
        let s = session_of(body(ed.handle(req(4, Command::SessionNew { name: None }))));
        body(ed.handle(req(
            5,
            Command::NodeAdd {
                session: s,
                parent: NodeId::new("root").unwrap(),
                kind: NodeKindArg::Group,
                name: None,
                node: None,
            },
        )));
        assert!(
            lock(&seen).is_empty(),
            "an unsubscribed observer hears nothing",
        );
    }

    /// Every reply that names a session says which revision it reflects, so a
    /// client can tell a stale read from a fresh one without asking again.
    #[test]
    fn a_reply_carries_the_revision_it_reflects() {
        let ed = Editor::new();
        let (s, rev) = match ed.handle(req(1, Command::SessionNew { name: None })) {
            Reply::Ok { body, rev, .. } => (session_of(body), rev),
            other => panic!("{other:?}"),
        };
        assert_eq!(rev, Some(0), "a new session reports the rev it starts at");
        assert!(
            matches!(
                ed.handle(req(2, Command::SessionList)),
                Reply::Ok { rev: None, .. }
            ),
            "an editor-level read names no session and so no revision",
        );

        let add = |id| {
            ed.handle(req(
                id,
                Command::NodeAdd {
                    session: s,
                    parent: NodeId::new("root").unwrap(),
                    kind: NodeKindArg::Group,
                    name: None,
                    node: None,
                },
            ))
        };
        assert!(matches!(add(3), Reply::Ok { rev: Some(1), .. }));
        // A read of the same session reports it too, unmoved.
        assert!(matches!(
            ed.handle(req(4, Command::NodeTree { session: s })),
            Reply::Ok { rev: Some(1), .. }
        ));
        assert!(matches!(
            ed.handle(req(5, Command::SessionClose { session: s })),
            Reply::Ok { rev: None, .. }
        ));
    }

    /// What a browser asks before it stages bytes: exactly the keys the import
    /// will read, resolved against the manifest's own key.
    #[test]
    fn manifest_requirements_names_the_keys_the_import_reads() {
        let dir = std::env::temp_dir().join(format!("catchlight-reqs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(8, 8);
        for px in img.pixels_mut() {
            *px = image::Rgba([10, 20, 30, 255]);
        }
        img.save(dir.join("face.png")).unwrap();
        let manifest = dir.join("m.json");
        std::fs::write(
            &manifest,
            r#"{"textures":[{"id":"face","path":"face.png"}],
               "nodes":[{"id":"face","kind":"part","texture":"face","mesh":{"auto":"quad"}}]}"#,
        )
        .unwrap();

        let ed = Editor::new();
        let manifest_path = manifest.display().to_string();
        match body(ed.handle(req(
            1,
            Command::ManifestRequirements {
                manifest_path: manifest_path.clone(),
            },
        ))) {
            ResponseBody::ManifestRequirements { textures } => assert_eq!(
                textures,
                vec![dir.join("face.png").display().to_string()],
                "a texture reference resolves against the manifest's key",
            ),
            other => panic!("{other:?}"),
        }
        // The keys are the import's keys: it reads them and nothing else.
        session_of(body(
            ed.handle(req(2, Command::SessionImport { manifest_path })),
        ));

        let _ = std::fs::remove_dir_all(&dir);
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

    /// An undo snapshot is a shallow clone, so 64 of them hold one model's
    /// textures once, not 64 times. Charging each snapshot the full
    /// `estimated_size_bytes` would bill them 64 times and collapse the
    /// history of any model whose textures approach the cap.
    #[test]
    fn a_texture_shared_by_every_snapshot_is_counted_once() {
        let mut model = Model::new();
        let mut hex = SeededHex::new(7);
        let root = model.root().unwrap().clone();
        let part = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
                ),
                &mut hex,
            )
            .unwrap();
        model
            .add_texture(
                &part,
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: vec![0u8; 4 * 1024 * 1024].into(),
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
            .len();
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

    /// A delete that cascades into a texture leaves the payload in the older
    /// snapshots and nowhere else. The ledger counts a payload while any
    /// snapshot holds it, so it stays billed until the last one holding it is
    /// trimmed — and an edit that frees megabytes does not make the history
    /// look free while undo can still bring them back.
    #[test]
    fn a_cascaded_texture_delete_is_billed_until_the_last_snapshot_holding_it_goes() {
        let mut model = Model::new();
        let mut hex = SeededHex::new(7);
        let root = model.root().unwrap().clone();
        let part = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
                ),
                &mut hex,
            )
            .unwrap();
        model
            .add_texture(
                &part,
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: vec![0u8; 4 * 1024 * 1024].into(),
                },
                &mut hex,
            )
            .unwrap();
        let payload = model.texture(&model.texture_ids()[0]).unwrap().data.len();

        let mut history = History::default();
        history.push_undo(model.clone());

        // Deleting the only part drawing it takes the texture with it.
        model.delete_node(&part).unwrap();
        assert!(model.texture_ids().is_empty());
        history.push_undo(model.clone());

        assert!(
            history.bytes() > payload,
            "the snapshot that can undo the delete still holds the bytes"
        );
        history.trim(1, usize::MAX);
        assert!(
            history.bytes() < payload,
            "nothing holds the payload once that snapshot is gone: {}",
            history.bytes()
        );
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
                node: None,
            },
        ))) {
            ResponseBody::Node { node, .. } => node,
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

    /// A 1x1 PNG, which is what an upload validates and what the dimensions
    /// come from. The bytes are never looked at again.
    fn one_pixel_png(rgba: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_raw(1, 1, rgba.to_vec()).expect("1x1");
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    fn new_part(ed: &Editor, s: SessionId, id: u64) -> NodeId {
        let root = match body(ed.handle(req(id, Command::NodeTree { session: s }))) {
            ResponseBody::Tree { root } => root.id,
            other => panic!("{other:?}"),
        };
        match body(ed.handle(req(
            id + 1,
            Command::NodeAdd {
                session: s,
                parent: root,
                kind: NodeKindArg::Part,
                name: None,
                node: None,
            },
        ))) {
            ResponseBody::Node { node, .. } => node,
            other => panic!("{other:?}"),
        }
    }

    fn texture_ids(ed: &Editor, s: SessionId, id: u64) -> Vec<TexId> {
        match body(ed.handle(req(id, Command::TextureList { session: s }))) {
            ResponseBody::Textures { textures } => textures.into_iter().map(|t| t.id).collect(),
            other => panic!("{other:?}"),
        }
    }

    /// An upload names the part it is for and both land in one edit: one
    /// revision, and no moment where the session holds a texture nothing
    /// draws.
    #[test]
    fn an_upload_adds_the_texture_and_assigns_it_in_one_edit() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        let part = new_part(&ed, s, 2);
        let rev = ed.doc_snapshot(s).expect("a snapshot").rev;

        let (tex, dropped) = ed
            .add_texture_bytes(s, &part, TextureEncoding::Png, one_pixel_png([9; 4]))
            .expect("upload");

        assert!(dropped.is_empty(), "nothing was displaced");
        assert_eq!(texture_ids(&ed, s, 4), vec![tex.clone()]);
        assert_eq!(
            ed.with_model(s, |m| match m.node(&part).map(|n| &n.kind) {
                Some(ModelNodeKind::Part(p)) => p.albedo().cloned(),
                _ => None,
            })
            .expect("session"),
            Some(tex),
        );
        assert_eq!(
            ed.doc_snapshot(s).expect("a snapshot").rev,
            rev + 1,
            "one edit, one revision"
        );
    }

    /// The cascade is in the reply: an edit that leaves a texture with no part
    /// drawing it deletes it, and a client that has to tell an author what a
    /// click cost reads that off the reply rather than diffing the session.
    #[test]
    fn a_reply_names_the_textures_the_edit_deleted() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(1, Command::SessionNew { name: None }))));
        let part = new_part(&ed, s, 2);
        let (first, _) = ed
            .add_texture_bytes(s, &part, TextureEncoding::Png, one_pixel_png([1; 4]))
            .expect("upload");

        // A second upload to the same part displaces the first, which nothing
        // else draws.
        let (second, dropped) = ed
            .add_texture_bytes(s, &part, TextureEncoding::Png, one_pixel_png([2; 4]))
            .expect("upload");
        assert_eq!(dropped, vec![first]);
        assert_eq!(texture_ids(&ed, s, 4), vec![second.clone()]);

        // Unmapping the part through the wire says the same thing.
        let other = new_part(&ed, s, 5);
        let (third, _) = ed
            .add_texture_bytes(s, &other, TextureEncoding::Png, one_pixel_png([3; 4]))
            .expect("upload");
        match body(ed.handle(req(
            7,
            Command::NodeSet {
                session: s,
                node: other,
                patch: NodePatch {
                    texture: Some(second.clone()),
                    ..Default::default()
                },
            },
        ))) {
            ResponseBody::Node { dropped, .. } => assert_eq!(dropped, vec![third]),
            other => panic!("{other:?}"),
        }

        // And so does deleting the last part drawing one.
        match body(ed.handle(req(
            8,
            Command::NodeDelete {
                session: s,
                node: part,
            },
        ))) {
            ResponseBody::Node { dropped, .. } => {
                assert!(dropped.is_empty(), "the other part still draws it")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(texture_ids(&ed, s, 9), vec![second]);
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
                node: None,
            },
        ))) {
            ResponseBody::Node { node, .. } => node,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            body(ed.handle(req(4, Command::NodeDelete { session: s, node }))),
            ResponseBody::Node { dropped, .. } if dropped.is_empty()
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
                    node: None,
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
                node: None,
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
