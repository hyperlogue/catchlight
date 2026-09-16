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
//! - **Scratch stays local; a commit publishes once.** Browser and native
//!   frontends preview on their own Puppet. Exact cell writes and batches
//!   author model data through [`Editor::edit_session_captured`], the single
//!   revision/history publication path. Raw writes never seed a rest key;
//!   recording supplies that policy explicitly in one atomic edit batch.
//!
//! - **An observer never runs under a lock.** [`Editor::subscribe`] registers
//!   a callback for every [`Event`], and a callback's whole reason to exist is
//!   to read the session it was just told about. So every emission point
//!   collects what to say, drops the sessions map and the session guard, and
//!   only then calls out. Re-entering [`Editor`] from an observer is expected.
//!
//! - **History branches keep their creation revisions.** [`ModelHistory`]
//!   retains authored snapshots and navigation aliases within bounded storage,
//!   counting shared texture and binary-extension allocations once. A changed
//!   edit or navigation publishes a fresh live revision; final-content no-ops
//!   and failures leave the model, history and revision unchanged.
//!
//! - **A reply keeps the revision it captured.** [`Captured`] is stamped under
//!   the same session lock that reads or publishes its contents. Rendering,
//!   output IO and reentrant observers cannot replace that revision afterward.
//!
//! - **Close waits without holding the registry.** A session handle is cloned
//!   out of the registry before waiting on its mutex. Closing marks that handle
//!   closed under the same mutex before unregistering it, so queued operations
//!   cannot publish against a handle that has left the editor.
//!
//! - **Each session draws its own Ids.** See [`session_hex`]: the seed comes
//!   from the [`SessionId`], so two sessions open at once and edited the same
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
//!   store, and a command that read a key *into* a session releases it — so a
//!   session opened from a transient upload holds no `file` to save back to.
//!   [`Command::Preview`] is the one command that is still native — it
//!   needs the headless renderer, not just bytes.
//!
//! - **Bytes never enter except inside the command that uses them.** There is
//!   no upload-then-reference: a staged file nothing ever names cannot exist
//!   because nothing is ever staged. [`Editor::handle_with`] is the one call
//!   the whole editor sits behind — a [`Request`], the [`Attachments`] that
//!   came with it, a [`Reply`] and the [`Payload`] that goes back — and every
//!   transport is glue for those two halves and nothing else.
//!   [`COMMAND_BYTES`] says which names each command takes, so an attachment
//!   a command did not declare, or a fixed one that did not arrive, is
//!   refused before the command runs.
//!
//! - **An extension is carried, never interpreted.** A vendor's value goes in
//!   under a dotted key and comes back out untouched: a JSON one travels
//!   inline everywhere, including in the structure feed, while bytes travel
//!   as a `{size, hash}` marker and are fetched once, by hash, from
//!   [`http`]'s extension route. Setting one is an edit like any
//!   other — a revision, an undo entry, an event — because that is what makes
//!   it survive a save.
//!
//! - **A fit moves nothing the part draws.** See [`spine_fit`]: the spine
//!   goes between the part and its parent, and the strand's root is carried
//!   onto the spine by two translations of the same size in opposite
//!   directions — one onto the spine, one off the part — so every rest vertex
//!   draws exactly where it drew, however the part is rotated or scaled. A
//!   fit authors no binding: the spine turns the art by composing its joints.
//!
//! - **A model-only read has one implementation.** See [`query`]: the reads
//!   [`CommandKind::ReplicaQuery`] names are pure functions of the [`Model`],
//!   so a browser tab holding a replica answers them itself. `dispatch` routes
//!   those arms into the very same code rather than keeping a second copy.
//!
//! - **A browser gets the same protocol, plus bytes.** See [`http`]: `/ws`
//!   carries one JSON [`Request`] per text frame and answers each with its
//!   [`Reply`], exactly as [`serve_unix`] does, and additionally pushes every
//!   [`Event`] to the connections that are open; `POST /request` takes one
//!   [`Request`] and answers its [`Reply`] for a client that holds no socket;
//!   the structure and texture payloads a replica needs go over HTTP rather
//!   than through a frame. Loopback is not a permission — any page on any
//!   origin can reach that port — so a random per-launch token gates every
//!   door, and `GET /token` is readable only from an allowlisted origin.

mod edit;
#[cfg(test)]
mod history_tests;
#[cfg(not(target_arch = "wasm32"))]
mod http;
mod lifecycle;
mod limits;
#[cfg(not(target_arch = "wasm32"))]
mod preview;
mod query;
mod storage;
#[cfg(unix)]
mod transport;

#[cfg(not(target_arch = "wasm32"))]
pub use http::{bind_http, serve_http, HttpOptions, HttpServer};
pub use query::{extension_value_info, replica_query, replica_reply, slot_info, weld_info};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::FileStorage;
pub use storage::{join_key, key_stem, parent_key, NoStorage, Storage};

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use catchlight_core::components::BlendMode as CoreBlendMode;
use catchlight_core::formats::clm::{
    self as clm, ClmIndices, ClmMesh, TextureAlpha, TextureEncoding as CoreTextureEncoding,
};
use catchlight_core::id::{HexSource as _, Name, SeededHex};
use catchlight_core::LoadBudget;

use catchlight_core::Vec2;
use catchlight_core::{
    BindingKey, BindingTarget as CoreBindingTarget, ExtensionValue, InstallError, Mat4, Model,
    ModelChain, ModelComposite, ModelError, ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam,
    ModelPart, ModelPhysics, ModelSpine, ModelTexture, ModelWeld, Pose, Puppet, Required, Vec3,
};
// The wire's camera and the renderer's framing are the same shape and not the
// same type; the conversion happens here, at the edge.
use catchlight_editor_core::{
    contour_automesh, fit_strand, grid_automesh, AlphaMask, ContourKnobs, GridKnobs, HistoryError,
    HistoryLimits, HistoryNavigation, Manifest, ManifestError, MeshError, ModelHistory,
    ModelManifestExt as _, ModelMeshExt as _, TextureData, UvMap,
};
#[cfg(not(target_arch = "wasm32"))]
use catchlight_wgpu::Framing;

use catchlight_editor_protocol::*;

#[cfg(not(target_arch = "wasm32"))]
use preview::PreviewRenderer;

/// What a [`Command::Preview`] frames when it names no camera: this many world
/// units tall, centred on the origin.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_CAMERA_HEIGHT: f32 = 2000.0;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("operation {index}: {source}")]
    Operation {
        index: u32,
        source: Box<EditorError>,
    },
    #[error("{resource} requires {requested}, exceeding the limit {limit}")]
    Limit {
        resource: &'static str,
        limit: u64,
        requested: u64,
    },
    #[error("The model changed during this edit. Reload the draft or start the gesture again.")]
    RevisionConflict,
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
    /// The two parts named are not welded to each other.
    #[error("no weld pairs those two parts")]
    UnknownWeld,
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("revision {0} is not retained")]
    RevisionUnavailable(u64),
    #[error("the session publication revision is exhausted")]
    RevisionExhausted,
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
    /// Installing a fragment under a parent was refused — a colliding Id, a
    /// requirement the base does not have.
    #[error("install: {0}")]
    Install(#[from] InstallError),
    /// The model carries no extension under that key.
    #[error("no extension {0:?}")]
    NoExtension(String),
}

impl EditorError {
    fn limit_info(&self) -> Option<LimitInfo> {
        match self {
            Self::Operation { source, .. } => source.limit_info(),
            Self::Limit {
                resource,
                limit,
                requested,
            } => Some(LimitInfo {
                resource: (*resource).into(),
                limit: *limit,
                requested: *requested,
            }),
            Self::Edit(ModelError::LoadLimit(limit))
            | Self::Edit(ModelError::Clm(clm::ClmError::LoadLimit(limit)))
            | Self::Manifest(ManifestError::LoadLimit(limit))
            | Self::Manifest(ManifestError::Model(ModelError::LoadLimit(limit)))
            | Self::Manifest(ManifestError::Model(ModelError::Clm(clm::ClmError::LoadLimit(
                limit,
            )))) => Some(LimitInfo {
                resource: limit.resource.into(),
                limit: limit.limit,
                requested: limit.got,
            }),
            _ => None,
        }
    }

    /// The wire code a client branches on. The message stays for a person;
    /// this is what a commit gate or a mesh editor reacts to.
    pub fn code(&self) -> ErrorCode {
        if self.limit_info().is_some() {
            return ErrorCode::LimitExceeded;
        }
        match self {
            Self::Operation { source, .. } => source.code(),
            Self::Limit { .. } => ErrorCode::LimitExceeded,
            Self::RevisionConflict => ErrorCode::RevisionConflict,
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
            Self::RevisionUnavailable(_) => ErrorCode::RevisionUnavailable,
            Self::RevisionExhausted => ErrorCode::RevisionExhausted,
            Self::NoSavePath => ErrorCode::NoSavePath,
            Self::Manifest(_) => ErrorCode::Manifest,
            Self::Io(_) => ErrorCode::Io,
            Self::Image(_) => ErrorCode::Image,
            Self::Preview(_) => ErrorCode::Preview,
            Self::NativeOnly => ErrorCode::NativeOnly,
            Self::NoExtension(_) => ErrorCode::NoExtension,
            // Install's refusals are the model's refusals under other names,
            // so they answer with the codes the equivalent edit answers with
            // — a client that branches on `duplicate_id` for an add branches
            // on it for an import that collides.
            Self::Install(e) => match e {
                InstallError::Collision { .. } => ErrorCode::DuplicateId,
                InstallError::Missing { id, .. } => match id {
                    Required::Node(_) | Required::Part(_) => ErrorCode::NoNode,
                    Required::Param(_) => ErrorCode::NoParam,
                    Required::Slot(..) => ErrorCode::UnknownSlot,
                },
                InstallError::WeldUnknownSlot { .. } => ErrorCode::WeldUnknownSlot,
                InstallError::WeldSlotPairedTwice { .. } => ErrorCode::WeldSlotPairedTwice,
                InstallError::NotAnAddon { .. }
                | InstallError::CarriesParam { .. }
                | InstallError::CarriesExtension { .. }
                | InstallError::BindsOffAddon { .. } => ErrorCode::Edit,
            },
            // The model refuses an edit for many reasons; the ones a client
            // acts on differently get their own code, the rest are `Edit`.
            Self::Edit(e) => match e {
                ModelError::UnknownNode => ErrorCode::NoNode,
                ModelError::UnknownParam => ErrorCode::NoParam,
                ModelError::UnknownTexture => ErrorCode::NoTexture,
                ModelError::UnknownSlot => ErrorCode::UnknownSlot,
                ModelError::DuplicateId(_) => ErrorCode::DuplicateId,
                ModelError::DuplicateSlot(_) => ErrorCode::DuplicateSlot,
                ModelError::WeldUnknownSlot(_) => ErrorCode::WeldUnknownSlot,
                ModelError::WeldSlotPairedTwice(_) => ErrorCode::WeldSlotPairedTwice,
                ModelError::WeldSelfPaired | ModelError::WeldEndNotAPart => ErrorCode::BadWeldEnd,
                ModelError::DuplicateWeld => ErrorCode::DuplicateWeld,
                ModelError::WeldWeightOutOfRange => ErrorCode::WeldWeightOutOfRange,
                ModelError::UnknownWeld => ErrorCode::UnknownWeld,
                ModelError::Fragment => ErrorCode::Fragment,
                ModelError::UnknownExtension(_) => ErrorCode::NoExtension,
                ModelError::ReservedExtension(_) => ErrorCode::ReservedExtension,
                // Both are an argument that does not fit the node it names —
                // a chain command aimed at something that is not a chain, and
                // an output list of the wrong length for the chain it is
                // aimed at. That is what `bad_target` says, and it is the
                // code a client already branches on for a physics field set
                // on a node that is not a driver.
                ModelError::NotSpine => ErrorCode::BadTarget,
                ModelError::SpineTargetArity { .. } => ErrorCode::BadTarget,
                // The size cap has no code of its own: a client that hit it
                // has nothing to branch on, only a value to shrink.
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
    history: ModelHistory,
    /// Set while holding this handle before removing it from the registry.
    closed: bool,
    /// Lazily baked from `model` for preview. Rebaked by its own generation
    /// gate on the next use after an edit, so nothing has to invalidate it.
    puppet: Option<Puppet>,
    /// Navigation discards scratch and physics, but carries the preview pose
    /// into the next lazy runtime. Also held while a renderer owns the Puppet
    /// outside the session lock, so navigation cannot lose that captured pose.
    pending_preview_pose: Option<Pose>,
    /// rev-gated model view for in-process observers (the GUI).
    snapshot: Option<Arc<DocSnapshot>>,
    /// Latest shared view state — its own path, never touches the model/rev.
    presence: Option<Presence>,
}

impl Session {
    fn new(id: SessionId, model: Model, title: String, file: Option<String>) -> Self {
        let history = ModelHistory::new(model.clone(), HistoryLimits::default());
        Self {
            model,
            hex: session_hex(id),
            title,
            file,
            rev: 0,
            saved_rev: 0,
            history,
            closed: false,
            puppet: None,
            pending_preview_pose: None,
            snapshot: None,
            presence: None,
        }
    }

    fn ensure_open(&self, id: SessionId) -> Result<(), EditorError> {
        if self.closed {
            return Err(EditorError::NoSession(id));
        }
        Ok(())
    }

    fn next_revision(&self) -> Result<u64, EditorError> {
        self.rev
            .checked_add(1)
            .ok_or(EditorError::RevisionExhausted)
    }

    fn check_revision(&self, expected: Option<u64>) -> Result<(), EditorError> {
        if expected.is_some_and(|expected| expected != self.rev) {
            return Err(EditorError::RevisionConflict);
        }
        Ok(())
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

    fn slot_add_generated(&mut self, node: &NodeId) -> Result<SlotId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.slot_add_generated(node, hex)?)
    }

    fn duplicate_subtree(&mut self, node: &NodeId) -> Result<NodeId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.duplicate_subtree(node, hex)?)
    }

    fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    /// The session's puppet and the model it animates, baked on first use and
    /// rebaked by [`Puppet::sync`] when the model has moved since.
    fn puppet(&mut self) -> (&Model, &mut Puppet) {
        // Destructured so the model and the puppet are two borrows of two
        // fields rather than one of the session.
        let Self {
            model,
            puppet,
            pending_preview_pose,
            ..
        } = self;
        let puppet = puppet.get_or_insert_with(|| {
            let mut built = Puppet::new(model);
            // Frozen physics keeps the authoring preview deterministic (the
            // dt=0 tick can't integrate anyway) and lets physics-driven
            // params be posed by hand.
            built.set_physics_enabled(false);
            if let Some(pose) = pending_preview_pose.take() {
                built.apply_pose(&pose);
            }
            built
        });
        puppet.sync(model);
        (model, puppet)
    }

    /// A renderer may hold the runtime after releasing the session lock.
    /// Retain its pose so navigation can rebuild without awaiting the render.
    #[cfg(not(target_arch = "wasm32"))]
    fn take_preview_puppet(&mut self) -> Result<Puppet, EditorError> {
        self.puppet();
        let puppet = self
            .puppet
            .take()
            .ok_or_else(|| EditorError::Preview("puppet build failed".into()))?;
        self.pending_preview_pose = Some(puppet.pose());
        Ok(puppet)
    }
}

impl From<HistoryError> for EditorError {
    fn from(error: HistoryError) -> Self {
        match error {
            HistoryError::NothingToUndo => Self::NothingToUndo,
            HistoryError::NothingToRedo => Self::NothingToRedo,
            HistoryError::RevisionUnavailable(revision) => Self::RevisionUnavailable(revision),
            HistoryError::InvalidPublicationRevision { .. } => Self::RevisionConflict,
        }
    }
}

/// A result stamped while its addressed session is still locked. Never recover
/// this revision by looking up the session after running callbacks or IO.
#[derive(Debug)]
struct Captured<T> {
    value: T,
    rev: Option<u64>,
}

impl<T> Captured<T> {
    fn at(value: T, rev: u64) -> Self {
        Self {
            value,
            rev: Some(rev),
        }
    }

    fn unscoped(value: T) -> Self {
        Self { value, rev: None }
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

/// The bytes that came in beside one command, by attachment name.
///
/// Built by whatever transport carried them and handed to
/// [`Editor::handle_with`], which checks the names against [`COMMAND_BYTES`]
/// before any command runs. A dispatch arm then takes what its command
/// declared; nothing else can reach them, and nothing is left over anywhere
/// for a later command to find.
#[derive(Debug, Default)]
pub struct Attachments(std::collections::BTreeMap<String, Vec<u8>>);

impl Attachments {
    /// Nothing attached — every command that declares no bytes, and every
    /// transport that cannot carry any.
    pub fn none() -> Self {
        Self::default()
    }

    /// One attachment under `name`. Replaces a name given twice.
    pub fn insert(&mut self, name: impl Into<String>, bytes: Vec<u8>) {
        self.0.insert(name.into(), bytes);
    }

    /// Every name attached, sorted.
    pub fn names(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Take the bytes attached under exactly `name`.
    pub fn take(&mut self, name: &str) -> Option<Vec<u8>> {
        self.0.remove(name)
    }

    /// Take every `<family>:<suffix>` attachment, as `(suffix, bytes)` pairs
    /// in name order. Draining rather than borrowing: the suffixes are the
    /// command's to match against its own fields, and what it does not
    /// recognise it has already been handed.
    pub fn take_family(&mut self, family: &str) -> Vec<(String, Vec<u8>)> {
        let prefix = format!("{family}:");
        let names: Vec<String> = self
            .0
            .keys()
            .filter(|name| name.len() > prefix.len() && name.starts_with(&prefix))
            .cloned()
            .collect();
        names
            .into_iter()
            .filter_map(|name| {
                let bytes = self.0.remove(&name)?;
                Some((name[prefix.len()..].to_string(), bytes))
            })
            .collect()
    }
}

impl FromIterator<(String, Vec<u8>)> for Attachments {
    fn from_iter<I: IntoIterator<Item = (String, Vec<u8>)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// The bytes a reply carries back, beside the JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

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
            preview: Mutex::new(None),
        }
    }

    /// Apply one request that carries no bytes and wants none back.
    ///
    /// [`Self::handle_with`] with nothing attached, discarding a payload. For
    /// tests, for the in-process GUI, and for a transport that refuses a
    /// byte-bearing command before it ever gets here — a byte-bearing command
    /// sent this way is refused, because its attachment is missing.
    pub fn handle(&self, req: Request) -> Reply {
        self.handle_with(req, Attachments::none()).0
    }

    /// Apply one request and produce its reply, with the bytes that came in
    /// beside it and the bytes that go back.
    ///
    /// The one call the editor sits behind. Every transport is glue for its
    /// two halves: how `attachments` were framed on the way in, and how the
    /// [`Payload`] is framed on the way out.
    pub fn handle_with(&self, req: Request, attachments: Attachments) -> (Reply, Option<Payload>) {
        let mut attachments = attachments;
        let mut payload = None;
        let dispatched = limits::json_size(&req, "request_bytes", limits::MAX_REQUEST_JSON_BYTES)
            .and_then(|()| check_attachments(&req.command, &attachments))
            .and_then(|()| self.dispatch(req.command, &mut attachments, &mut payload));
        let reply = match dispatched {
            Ok(captured) => Reply::Ok {
                id: req.id,
                rev: captured.rev,
                body: captured.value,
            },
            Err(e) => Reply::Err {
                id: req.id,
                code: e.code(),
                message: e.to_string(),
                op_index: match &e {
                    EditorError::Operation { index, .. } => Some(*index),
                    _ => None,
                },
                limit: e.limit_info(),
            },
        };

        debug_assert!(
            payload.is_none() || matches!(&reply, Reply::Ok { .. }),
            "a refused command answered with a payload",
        );
        (reply, payload)
    }

    /// One session's revision, or `None` if it is not open — which is what a
    /// `session_close` reply reports, its session having just gone.
    fn rev(&self, id: SessionId) -> Option<u64> {
        self.with_session(id, |s| Ok(s.rev)).ok()
    }

    /// The current live revision. To stamp model data coherently, use
    /// [`Self::with_model_revision`] instead of reading the counter separately.
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

    /// Say that `session`'s model now reads as `rev`. Every revision move
    /// routes through here, and so does a save — a title bar reads `dirty` the
    /// same way it reads the tree.
    fn notify_model_changed(&self, session: SessionId, rev: u64) {
        self.notify(Event::ModelChanged { session, rev });
    }

    /// Say that the set of open sessions changed. Carries nothing: an
    /// observer that cares re-reads the list.
    fn notify_sessions(&self) {
        self.notify(Event::SessionsChanged);
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
        self.notify_model_changed(id, rev);
        Ok(())
    }

    /// Undo/redo stack depths — the history panel's scrub range.
    pub fn history(&self, id: SessionId) -> Result<(usize, usize), EditorError> {
        self.with_session(id, |s| Ok(s.history.depths()))
    }

    /// Is the session dirty (unsaved edits since the last save/save_bytes)?
    pub fn is_dirty(&self, id: SessionId) -> Result<bool, EditorError> {
        self.with_session(id, |s| Ok(s.dirty()))
    }

    /// Read the session's model directly — the in-process observer path for
    /// panels that need more than the tree snapshot (inspector, textures).
    /// Deep-read protocol commands wait until a remote client exists.
    pub fn with_model<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model) -> R,
    ) -> Result<R, EditorError> {
        self.with_session(id, |s| Ok(f(&s.model)))
    }

    /// Read model data and its live revision inside one session lock.
    pub fn with_model_revision<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model, u64) -> R,
    ) -> Result<R, EditorError> {
        self.with_session(id, |s| Ok(f(&s.model, s.rev)))
    }

    /// Register an already-encoded PNG/TGA texture from bytes (the browser
    /// picker path; native flows can keep handing paths to `TextureAdd`).
    pub fn add_texture_bytes(
        &self,
        id: SessionId,
        part: &NodeId,
        encoding: CoreTextureEncoding,
        bytes: Vec<u8>,
    ) -> Result<(TexId, Vec<TexId>), EditorError> {
        image_dims(&bytes, encoding)?;
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
            Ok(added)
        })
    }

    /// Read session state without publishing a model edit. The captured helper
    /// also carries the revision for protocol replies and asynchronous output.
    fn with_session<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<R, EditorError> {
        self.with_session_captured(id, f)
            .map(|captured| captured.value)
    }

    fn with_session_captured<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<Captured<R>, EditorError> {
        let handle = self.session(id)?;
        let mut session = lock(&handle);
        session.ensure_open(id)?;
        let rev = session.rev;
        let result = f(&mut session);
        debug_assert_eq!(session.rev, rev, "read helper published a model edit");
        result.map(|value| Captured::at(value, rev))
    }

    fn edit_session<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<R, EditorError> {
        self.edit_session_captured(id, f)
            .map(|captured| captured.value)
    }

    /// The single publication seam for authored edits. The same exclusive
    /// lock covers guards inside `f`, snapshot, execution, content comparison,
    /// history and revision publication. Callbacks run only after unlocking.
    /// A failed or final-content no-op edit restores its original model/rev.
    fn edit_session_captured<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<Captured<R>, EditorError> {
        let handle = self.session(id)?;
        let (result, moved) = {
            let mut session = lock(&handle);
            session.ensure_open(id)?;
            let before = session.rev;
            let snapshot = session.model.clone();
            let result = f(&mut session).and_then(|value| {
                if snapshot.authored_eq(&session.model)? {
                    session.model = snapshot.clone();
                    session.rev = before;
                } else {
                    let next = before
                        .checked_add(1)
                        .ok_or(EditorError::RevisionExhausted)?;
                    let model = session.model.clone();
                    session.history.record(model, next)?;
                    session.rev = next;
                    session.snapshot = None;
                }
                Ok(Captured::at(value, session.rev))
            });
            if result.is_err() {
                session.model = snapshot;
                session.rev = before;
            }
            let moved = (session.rev != before).then_some(session.rev);
            (result, moved)
        };
        if let Some(rev) = moved {
            self.notify_model_changed(id, rev);
        }
        result
    }

    fn navigate_history(
        &self,
        id: SessionId,
        if_rev: u64,
        action: HistoryNavigation,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let handle = self.session(id)?;
        let (captured, changed) = {
            let mut session = lock(&handle);
            session.ensure_open(id)?;
            session.check_revision(Some(if_rev))?;
            let next = session.next_revision()?;
            let restored = session.history.navigate(action, next)?;
            let changed = restored.is_some();
            if let Some(restored) = restored {
                session.model.replace_from(&restored);
                session.rev = next;
                if let Some(puppet) = session.puppet.take() {
                    session.pending_preview_pose = Some(puppet.pose());
                }
                session.snapshot = None;
            }
            (Captured::at(ResponseBody::Empty, session.rev), changed)
        };
        if changed {
            self.notify_model_changed(id, captured.rev.unwrap_or(if_rev));
        }
        Ok(captured)
    }

    /// Current model view for an in-process observer (the GUI), rebuilt only
    /// when the session's revision changed.
    pub fn doc_snapshot(&self, id: SessionId) -> Option<Arc<DocSnapshot>> {
        let session = self.session(id).ok()?;
        let mut session = lock(&session);
        session.ensure_open(id).ok()?;
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
        self.with_session(id, |s| {
            s.presence = Some(presence);
            Ok(())
        })
        .is_ok()
    }

    pub fn presence(&self, id: SessionId) -> Option<Presence> {
        self.with_session(id, |s| Ok(s.presence.clone()))
            .ok()
            .flatten()
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
        s.ensure_open(id)?;
        let (model, puppet) = s.puppet();
        Ok(f(model, puppet))
    }

    /// The one match over every command. `attachments` holds the bytes that
    /// came in with it, already checked against what it declared; an arm that
    /// answers with bytes writes them into `payload`.
    fn dispatch(
        &self,
        cmd: Command,
        attachments: &mut Attachments,
        // Export and extension reads are portable; preview rendering is native.
        payload: &mut Option<Payload>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        match cmd {
            Command::SessionNew { name, source } => self.create_session(name, source, attachments),
            Command::SessionOpen { path } => self.open_session(path),
            Command::SessionFork {
                session,
                if_rev,
                name,
            } => self.fork_session(session, if_rev, name),
            Command::ModelExport { session, if_rev } => self.export_model(session, if_rev, payload),
            Command::SessionList => {
                let handles: Vec<_> = lock(&self.sessions)
                    .iter()
                    .map(|(&id, session)| (id, session.clone()))
                    .collect();
                let mut sessions = Vec::with_capacity(handles.len());
                for (id, handle) in handles {
                    let s = lock(&handle);
                    if s.closed {
                        continue;
                    }
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
                Ok(Captured::unscoped(ResponseBody::Sessions { sessions }))
            }
            Command::SessionClose { session } => {
                // Never hold the registry lock while waiting for this handle.
                // Queued operations may already own an Arc; the closed marker
                // makes those handles fail after this publication wins the lock.
                let handle = self.session(session)?;
                {
                    let mut state = lock(&handle);
                    state.ensure_open(session)?;
                    state.closed = true;
                    lock(&self.sessions).remove(&session);
                }
                self.notify_sessions();
                Ok(Captured::unscoped(ResponseBody::Empty))
            }
            Command::Save { session, path } => {
                let handle = self.session(session)?;
                let (key, rev) = {
                    let mut s = lock(&handle);
                    s.ensure_open(session)?;
                    let key = match path {
                        Some(p) => p,
                        None => s.file.clone().ok_or(EditorError::NoSavePath)?,
                    };
                    let bytes = s.model.to_clm_bytes()?;
                    // Save publication includes its persistent write. Closing
                    // and later saves wait; failure leaves session metadata
                    // untouched. Other sessions retain their independent locks.
                    self.storage.write(&key, &bytes)?;
                    s.file = Some(key.clone());
                    s.saved_rev = s.rev;
                    (key, s.rev)
                };
                // A save moves no revision but does flip `dirty`, which a
                // title bar reads the same way it reads the tree.
                self.notify_model_changed(session, rev);
                Ok(Captured::at(ResponseBody::Saved { path: key }, rev))
            }
            Command::ExportManifest { session, path } => {
                let handle = self.session(session)?;
                let (manifest, textures, rev) = {
                    let s = lock(&handle);
                    s.ensure_open(session)?;
                    let manifest = s.model.to_manifest().to_json()?;
                    let textures = s
                        .model
                        .texture_ids()
                        .iter()
                        .enumerate()
                        .filter_map(|(i, tid)| {
                            let texture = s.model.texture(tid)?;
                            let ext = match texture.encoding {
                                CoreTextureEncoding::Tga => "tga",
                                CoreTextureEncoding::Png => "png",
                            };
                            Some((format!("tex{i}.{ext}"), texture.data.clone()))
                        })
                        .collect::<Vec<_>>();
                    (manifest, textures, s.rev)
                };
                self.storage.write(&path, manifest.as_bytes())?;
                // Textures land beside the manifest, as its own references
                // expect them.
                let base = parent_key(&path).to_string();
                for (name, data) in textures {
                    self.storage.write(&join_key(&base, &name), &data)?;
                }
                Ok(Captured::at(ResponseBody::Saved { path }, rev))
            }
            Command::Status { session } => self.with_session_captured(session, |s| {
                Ok(ResponseBody::Status {
                    status: StatusInfo {
                        title: s.title.clone(),
                        undo_steps: s.history.depths().0 as u32,
                        redo_steps: s.history.depths().1 as u32,
                        gravity: Some(s.model.physics().gravity),
                        pixels_per_meter: Some(s.model.physics().pixels_per_meter),
                        chain_substeps: Some(s.model.physics().chain_substeps.get()),
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
            Command::MeshGet { session, .. }
            | Command::ModelGet { session, .. }
            | Command::GeometryGet { session, .. }
            | Command::BindingCellsGet { session, .. }
            | Command::Check { session }
            | Command::NodeTree { session }
            | Command::NodeInfo { session, .. }
            | Command::TextureList { session }
            | Command::ParamList { session }
            | Command::BindingList { session, .. }
            | Command::Slots { session, .. }
            | Command::Welds { session }
            | Command::Extensions { session } => {
                let captured = self.with_session_captured(session, |s| {
                    query::check_revision(&cmd, s.rev)?;
                    Ok(s.model.clone())
                })?;
                let body = query::replica_query(&captured.value, &cmd)?;
                Ok(Captured {
                    value: body,
                    rev: captured.rev,
                })
            }
            Command::BindingCellsSet {
                session,
                if_rev,
                node,
                params,
                target,
                cells,
            } => self.edit_session_captured(session, |s| {
                s.check_revision(Some(if_rev))?;
                edit::apply(
                    &mut s.model,
                    EditOp::BindingCellsSet {
                        node,
                        params,
                        target,
                        cells,
                    },
                )
            }),
            Command::BindingCellsUnset {
                session,
                if_rev,
                node,
                params,
                target,
                cells,
            } => self.edit_session_captured(session, |s| {
                s.check_revision(Some(if_rev))?;
                edit::apply(
                    &mut s.model,
                    EditOp::BindingCellsUnset {
                        node,
                        params,
                        target,
                        cells,
                    },
                )
            }),
            Command::EditApply {
                session,
                if_rev,
                edits,
            } => {
                edit::check_batch(&edits)?;
                let captured = self.edit_session_captured(session, |s| {
                    s.check_revision(Some(if_rev))?;
                    edit::execute(&mut s.model, edits)
                })?;
                Ok(Captured {
                    value: ResponseBody::EditResults {
                        changed: captured.rev != Some(if_rev),
                        results: captured.value,
                    },
                    rev: captured.rev,
                })
            }
            Command::EditValidate {
                session,
                if_rev,
                edits,
            } => {
                edit::check_batch(&edits)?;
                let captured = self.with_session_captured(session, |s| {
                    s.check_revision(Some(if_rev))?;
                    Ok(s.model.clone())
                })?;
                let mut candidate = captured.value.clone();
                let results = edit::execute(&mut candidate, edits)?;
                let changed = !captured.value.authored_eq(&candidate)?;
                Ok(Captured {
                    value: ResponseBody::EditResults { changed, results },
                    rev: captured.rev,
                })
            }
            Command::NodeAdd {
                session,
                parent,
                kind,
                name,
                node,
            } => self.edit_session_captured(session, |s| {
                if let Some(node) = node {
                    edit::apply(
                        &mut s.model,
                        EditOp::NodeAdd {
                            parent,
                            kind,
                            name,
                            node,
                        },
                    )
                } else {
                    let node = s.add_node(
                        &parent,
                        None,
                        ModelNode::new(name.unwrap_or_else(|| default_name(kind)), make_kind(kind)),
                    )?;
                    Ok(ResponseBody::Node {
                        node,
                        dropped: Vec::new(),
                    })
                }
            }),
            Command::NodeSet {
                session,
                node,
                patch,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::NodeSet { node, patch })
            }),
            Command::NodeReparent { session, node, to } => self
                .edit_session_captured(session, |s| {
                    edit::apply(&mut s.model, EditOp::NodeReparent { node, to })
                }),
            Command::NodeReorder {
                session,
                node,
                index,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::NodeReorder { node, index })
            }),
            Command::NodeDuplicate { session, node } => self.edit_session_captured(session, |s| {
                let copy = s.duplicate_subtree(&node)?;
                Ok(ResponseBody::Node {
                    node: copy,
                    dropped: Vec::new(),
                })
            }),
            Command::RenameId { session, rename } => self.edit_session_captured(session, |s| {
                match rename {
                    Rename::Node { from, to } => s.model.rename_node_id(&from, to)?,
                    Rename::Param { from, to } => s.model.rename_param_id(&from, to)?,
                    Rename::Texture { from, to } => s.model.rename_tex_id(&from, to)?,
                    Rename::Slot { node, from, to } => s.model.rename_slot(&node, &from, to)?,
                }
                Ok(ResponseBody::Empty)
            }),
            Command::MaskAdd {
                session,
                node,
                source,
                mode,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::MaskAdd { node, source, mode })
            }),
            Command::MaskDelete {
                session,
                node,
                index,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::MaskDelete { node, index })
            }),
            Command::PhysicsSet {
                session,
                node,
                kind,
                map_mode,
                local_only,
                target_params,
                gravity,
                length,
                frequency,
                angle_damping,
                length_damping,
                output_scale,
            } => self.edit_session_captured(session, |s| {
                if let Some(t) = target_params {
                    let targets = physics_targets(&s.model, t)?;
                    s.model.set_physics_targets(&node, targets)?;
                }
                s.model.update_node(&node, |n| {
                    let ModelNodeKind::SimplePhysics(ph) = &mut n.kind else {
                        return Err(EditorError::BadTarget("not a physics node".into()));
                    };
                    if let Some(m) = kind {
                        ph.kind = m.into();
                    }
                    if let Some(m) = map_mode {
                        ph.map_mode = m.into();
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
                Ok(ResponseBody::Empty)
            }),
            Command::PhysicsGlobals {
                session,
                gravity,
                pixels_per_meter,
                chain_substeps,
            } => self.edit_session_captured(session, |s| {
                let mut physics = *s.model.physics();
                if let Some(g) = gravity {
                    physics.gravity = g;
                }
                if let Some(ppm) = pixels_per_meter {
                    physics.pixels_per_meter = ppm;
                }
                if let Some(steps) = chain_substeps {
                    physics.chain_substeps = std::num::NonZeroU8::new(steps).ok_or_else(|| {
                        EditorError::BadTarget("chain_substeps must be in 1..=255".into())
                    })?;
                }
                s.model.set_physics(physics);
                Ok(ResponseBody::Empty)
            }),
            Command::NodeDelete { session, node } => self.edit_session_captured(session, |s| {
                let dropped = s.model.textures_dropped_by_deleting(&node);
                s.model.delete_node(&node)?;
                Ok(ResponseBody::Node { node, dropped })
            }),
            Command::TextureAdd {
                session,
                node,
                encoding,
                texture: id,
            } => {
                // The gate already refused a command with no `texture`
                // attached, so this is the bytes the caller sent.
                let bytes = attachments.take("texture").unwrap_or_default();
                let encoding = encoding.into();
                self.edit_session_captured(session, move |s| {
                    image_dims(&bytes, encoding)?;
                    let (texture, dropped) = s.add_texture(
                        &node,
                        id,
                        ModelTexture {
                            encoding,
                            alpha: TextureAlpha::Straight,
                            data: bytes.into(),
                        },
                    )?;
                    Ok(ResponseBody::Texture { texture, dropped })
                })
            }
            Command::ParamAdd {
                session,
                name,
                min,
                max,
                default,
                param: id,
            } => self.edit_session_captured(session, |s| {
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
                    },
                )?;
                Ok(ResponseBody::Param { param })
            }),
            Command::ParamSet {
                session,
                param,
                name,
                min,
                max,
                default,
            } => self.edit_session_captured(session, |s| {
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
                Ok(ResponseBody::Empty)
            }),
            Command::ParamDelete { session, param } => self.edit_session_captured(session, |s| {
                s.model.delete_param(&param)?;
                Ok(ResponseBody::Empty)
            }),
            Command::BindingKeyInsert {
                session,
                if_rev,
                node,
                params,
                target,
                axis,
                value,
            } => self.edit_session_captured(session, |s| {
                s.check_revision(if_rev)?;
                edit::apply(
                    &mut s.model,
                    EditOp::BindingKeyInsert {
                        node,
                        params,
                        target,
                        axis,
                        value,
                    },
                )
            }),
            Command::BindingKeyDelete {
                session,
                if_rev,
                node,
                params,
                target,
                axis,
                index,
            } => self.edit_session_captured(session, |s| {
                s.check_revision(if_rev)?;
                edit::apply(
                    &mut s.model,
                    EditOp::BindingKeyDelete {
                        node,
                        params,
                        target,
                        axis,
                        index,
                    },
                )
            }),
            Command::BindingKeyMove {
                session,
                if_rev,
                node,
                params,
                target,
                axis,
                index,
                value,
            } => self.edit_session_captured(session, |s| {
                s.check_revision(if_rev)?;
                edit::apply(
                    &mut s.model,
                    EditOp::BindingKeyMove {
                        node,
                        params,
                        target,
                        axis,
                        index,
                        value,
                    },
                )
            }),
            Command::BindingAdd {
                session,
                node,
                params,
                target,
                key_positions,
            } => self.edit_session_captured(session, |s| {
                edit::apply(
                    &mut s.model,
                    EditOp::BindingAdd {
                        node,
                        params,
                        target,
                        key_positions,
                    },
                )
            }),
            Command::BindingDelete {
                session,
                node,
                params,
                target,
            } => self.edit_session_captured(session, |s| {
                edit::apply(
                    &mut s.model,
                    EditOp::BindingDelete {
                        node,
                        params,
                        target,
                    },
                )
            }),
            Command::BindingInterpolate {
                session,
                node,
                params,
                target,
                mode,
            } => self.edit_session_captured(session, |s| {
                edit::apply(
                    &mut s.model,
                    EditOp::BindingInterpolationSet {
                        node,
                        params,
                        target,
                        mode,
                    },
                )
            }),
            Command::MeshSet {
                session,
                if_rev,
                node,
                verts,
                uvs,
                indices,
                origin,
                deform_mapping,
            } => self.edit_session_captured(session, |s| {
                s.check_revision(if_rev)?;
                edit::apply(
                    &mut s.model,
                    EditOp::MeshSet {
                        node,
                        verts,
                        uvs,
                        indices,
                        origin,
                        deform_mapping,
                    },
                )
            }),
            Command::MeshAuto {
                session,
                node,
                mode,
            } => self.edit_session_captured(session, |s| {
                let mesh = automesh(&s.model, &node, mode)?;
                let emptied = s.model.set_mesh_with_refit(&node, mesh)?;
                Ok(emptied_reply(node, emptied))
            }),
            Command::SlotAdd {
                session,
                node,
                slot,
            } => self.edit_session_captured(session, |s| {
                if let Some(slot) = slot {
                    edit::apply(&mut s.model, EditOp::SlotAdd { node, slot })
                } else {
                    let slot = s.slot_add_generated(&node)?;
                    Ok(ResponseBody::Slot {
                        slot: SlotAddr { node, slot },
                    })
                }
            }),
            Command::SlotFill {
                session,
                node,
                slot,
                vertex,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::SlotFill { node, slot, vertex })
            }),
            Command::SlotClear {
                session,
                node,
                slot,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::SlotClear { node, slot })
            }),
            Command::SlotDelete {
                session,
                node,
                slot,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::SlotDelete { node, slot })
            }),
            Command::WeldSet {
                session,
                a,
                b,
                pairs,
            } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::WeldSet { a, b, pairs })
            }),
            Command::WeldDelete { session, a, b } => self.edit_session_captured(session, |s| {
                edit::apply(&mut s.model, EditOp::WeldDelete { a, b })
            }),
            Command::Undo { session, if_rev } => {
                self.navigate_history(session, if_rev, HistoryNavigation::Undo)
            }
            Command::Redo { session, if_rev } => {
                self.navigate_history(session, if_rev, HistoryNavigation::Redo)
            }
            Command::EditGoto {
                session,
                if_rev,
                revision,
            } => self.navigate_history(session, if_rev, HistoryNavigation::Goto(revision)),
            Command::EditHistoryGet { session, if_rev } => {
                self.with_session_captured(session, |s| {
                    s.check_revision(if_rev)?;
                    let metadata = s.history.metadata();
                    Ok(ResponseBody::EditHistory {
                        root: metadata.root,
                        current: metadata.current,
                        pruned: metadata.pruned,
                        entries: metadata
                            .entries
                            .into_iter()
                            .map(|entry| EditHistoryEntry {
                                revision: entry.revision,
                                parent: entry.parent,
                                redo: entry.redo,
                                revisions: entry.revisions,
                            })
                            .collect(),
                    })
                })
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
            } => self.edit_session_captured(session, |s| {
                let targets = physics_targets(&s.model, target_params)?;
                let mut phys = ModelPhysics::new(kind.into());
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
                Ok(ResponseBody::Node {
                    node,
                    dropped: Vec::new(),
                })
            }),
            Command::SpineAdd {
                session,
                parent,
                name,
                joints,
                targets,
                chain,
                node: id,
            } => self.edit_session_captured(session, |s| {
                let joints = spine_joints(joints)?;
                let count = joints.len();
                let node = ModelNode::new(
                    name.unwrap_or_else(|| "Spine".into()),
                    ModelNodeKind::Spine(ModelSpine::new(joints)),
                );
                let node = s.add_node(&parent, id, node)?;
                if let Some(targets) = targets {
                    s.model.set_spine_targets(&node, targets)?;
                }
                if let Some(chain) = chain {
                    s.model
                        .set_spine_chain(&node, Some(chain_of(&chain, count)?))?;
                }
                Ok(ResponseBody::Node {
                    node,
                    dropped: Vec::new(),
                })
            }),
            Command::SpineSet {
                session,
                node,
                joints,
                targets,
                chain,
            } => self.edit_session_captured(session, |s| {
                // Joints first: a set that reshapes and re-aims in one command
                // has its `targets` and its chain measured against the length
                // it just asked for rather than the one it replaced.
                if let Some(joints) = joints {
                    s.model.set_spine_joints(&node, spine_joints(joints)?)?;
                }
                if let Some(targets) = targets {
                    s.model.set_spine_targets(&node, targets)?;
                }
                if let Some(chain) = chain {
                    let count = spine_joint_count(&s.model, &node)?;
                    let chain = chain.map(|c| chain_of(&c, count)).transpose()?;
                    s.model.set_spine_chain(&node, chain)?;
                }
                Ok(ResponseBody::Empty)
            }),
            Command::SpineFit {
                session,
                part,
                links,
                axis,
                node: id,
                name,
                chain,
            } => self.edit_session_captured(session, |s| {
                spine_fit(s, &part, links, axis, id, name, chain.as_ref())
            }),
            Command::PresenceSet { session, presence } => {
                self.with_session_captured(session, |s| {
                    s.presence = Some(presence);
                    Ok(ResponseBody::Empty)
                })
            }
            Command::PresenceGet { session } => self.with_session_captured(session, |s| {
                Ok(ResponseBody::Presence {
                    presence: s.presence.clone(),
                })
            }),
            #[cfg(not(target_arch = "wasm32"))]
            Command::Preview {
                session,
                pose,
                size,
                camera,
            } => self.run_preview(session, pose, size, camera, payload),
            // Preview requires the native headless renderer.
            #[cfg(target_arch = "wasm32")]
            Command::Preview { .. } => Err(EditorError::NativeOnly),
            Command::ExtensionSet {
                session,
                key,
                value,
            } => {
                // The gate has already refused a `value` attachment the
                // command did not declare; which of the two kinds it is is
                // this arm's to decide, because only the command knows.
                let attached = attachments.take("value");
                let value = match (value, attached) {
                    (ExtensionSet::Json { .. }, Some(_)) => {
                        return Err(EditorError::BadRequest(
                            "a json extension carries its value inline; the `value` attachment \
                             is for kind \"bytes\""
                                .into(),
                        ))
                    }
                    (ExtensionSet::Json { value }, None) => ExtensionValue::Json(value),
                    (ExtensionSet::Bytes, Some(bytes)) => ExtensionValue::Bytes(bytes.into()),
                    (ExtensionSet::Bytes, None) => {
                        return Err(EditorError::BadRequest(
                            "a bytes extension needs the `value` attachment".into(),
                        ))
                    }
                };
                self.edit_session_captured(session, move |s| {
                    s.model.set_extension(key.clone(), value)?;
                    Ok(ResponseBody::Empty)
                })
            }
            Command::ExtensionDelete { session, key } => {
                self.edit_session_captured(session, move |s| {
                    s.model.delete_extension(&key)?;
                    Ok(ResponseBody::Empty)
                })
            }
            Command::ExtensionGet { session, key } => {
                let captured = self.with_session_captured(session, |s| {
                    s.model
                        .extension(&key)
                        .map(|value| (extension_value_info(value), value.bytes().cloned()))
                        .ok_or_else(|| EditorError::NoExtension(key.to_string()))
                })?;
                let (info, bytes) = captured.value;
                if let Some(bytes) = bytes {
                    // Bytes are the reply's payload, the way a preview's PNG
                    // is: what a `.clm` holds opaquely leaves the same way.
                    *payload = Some(Payload {
                        content_type: "application/octet-stream",
                        bytes: bytes.to_vec(),
                    });
                }
                Ok(Captured {
                    value: ResponseBody::Extension { key, value: info },
                    rev: captured.rev,
                })
            }
            Command::ImportJson {
                session,
                parent,
                if_rev,
                textures,
            } => self.import_structure(session, parent, if_rev, textures, attachments),
        }
    }

    /// Render one frame and hand back the PNG as the reply's payload.
    ///
    /// Nothing is written to a filesystem: the bytes go where the reply goes,
    /// so a caller with no shared disk — a browser tab, a script on the other
    /// side of HTTP — gets the same answer as one sitting next to the server.
    #[cfg(not(target_arch = "wasm32"))]
    fn run_preview(
        &self,
        session: SessionId,
        params: Vec<ParamPose>,
        size: Option<[u32; 2]>,
        camera: Option<Camera>,
        payload: &mut Option<Payload>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let [width, height] = size.unwrap_or([512, 512]);
        // Explicit or the default, never the presence: a script's output must
        // not depend on what some tab last looked at.
        let framing = camera.map_or(Framing::centered(DEFAULT_CAMERA_HEIGHT), |c| Framing {
            center: glam::Vec2::new(c.center[0], c.center[1]),
            height: c.height,
        });
        let handle = self.session(session)?;
        // The model is cloned rather than borrowed so the session lock is not
        // held across the GPU render; a clone is a shallow copy sharing every
        // heavy leaf, and it carries the same identity and generation, so the
        // puppet and the cache accept it as the model they were built from.
        let (model, mut puppet, rev) = {
            let mut s = lock(&handle);
            s.ensure_open(session)?;
            let puppet = s.take_preview_puppet()?;
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
            pr.render_png(session, &model, &mut puppet, &pose, width, height, framing)
                .map_err(|e| EditorError::Preview(e.to_string()))
        })();

        {
            let mut s = lock(&handle);
            if !s.closed && s.rev == rev && s.puppet.is_none() {
                s.puppet = Some(puppet);
                s.pending_preview_pose = None;
            }
        }
        *payload = Some(Payload {
            content_type: "image/png",
            bytes: render_result?,
        });
        Ok(Captured::at(
            ResponseBody::Preview {
                preview: PreviewInfo { width, height },
            },
            rev,
        ))
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
/// whose grid spans its two binding-owned position axes.
fn binding_key(
    params: BindingParams,
    node: NodeId,
    target: impl Into<BindingTarget>,
) -> Result<BindingKey, EditorError> {
    let target = CoreBindingTarget::from(target.into());
    Ok(match params.param_y {
        Some(y) => BindingKey::pair(params.param, y, node, target),
        None => BindingKey::new(params.param, node, target),
    })
}

/// [`PhysicsTargets`] as the model stores them: the driver's two outputs, in
/// order.
///
/// The wire type already rules out a third output and a positional hole, so
/// all that is left to check is that each param named exists.
/// `set_physics_targets` would refuse a dangling one anyway, but this says
/// which param was missing.
fn physics_targets(
    model: &Model,
    targets: PhysicsTargets,
) -> Result<[Option<ParamId>; 2], EditorError> {
    let check = |id: Option<ParamId>| match id {
        Some(id) if model.param(&id).is_none() => Err(EditorError::NoParam(id)),
        bound => Ok(bound),
    };
    Ok([check(targets.angle)?, check(targets.length)?])
}

/// A chain's authority over its params, refused at the door rather than on
/// the next load: a command that names a number the file format would not
/// take back is answered now, with the field in the message.
fn chain_weight(weight: Option<f32>) -> Result<Option<f32>, EditorError> {
    match weight {
        Some(w) if !w.is_finite() || w < 0.0 => Err(EditorError::BadTarget(
            "weight is not finite and at or above zero".into(),
        )),
        other => Ok(other),
    }
}

/// The wire's chain as the model holds it over `joints` joints, with every
/// refusal the file reader would make on the next load made here instead.
///
/// Writing is total, so a knob the model accepts is a knob the file carries,
/// and the reader is what would refuse it — on the next load, with the
/// command that named it long gone. Every command that hangs a chain comes
/// through here, so a script hears about a bad number now, with the field and
/// the link named the way `ClmLoadError::ChainLinkField` names them.
fn chain_of(arg: &ChainArg, joints: usize) -> Result<ModelChain, EditorError> {
    chain_weight(arg.weight)?;
    if let Some(g) = arg.gravity {
        if !g.is_finite() || g <= 0.0 {
            return Err(EditorError::BadTarget(
                "chain gravity is not finite and above zero".into(),
            ));
        }
    }
    for (i, feel) in arg.links.iter().flatten().enumerate() {
        let bad = |field: &str, reason: &str| {
            EditorError::BadTarget(format!("chain link {i}: {field} {reason}"))
        };
        if let Some(v) = feel.gravity_scale {
            if !v.is_finite() {
                return Err(bad("gravity_scale", "is not finite"));
            }
        }
        if let Some(v) = feel.damping {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err(bad("damping", "is outside 0..=1"));
            }
        }
        if let Some(v) = feel.stiffness {
            if !v.is_finite() || v < 0.0 {
                return Err(bad("stiffness", "is not finite and at or above zero"));
            }
        }
        if let Some(Some(v)) = feel.limit {
            if !v.is_finite() || v <= 0.0 || v > 1.0 {
                return Err(bad("limit", "is outside 0 exclusive to 1"));
            }
        }
    }
    Ok(arg.to_chain(joints))
}

/// How many joints a spine holds, or the refusal for a node that is not one.
fn spine_joint_count(model: &Model, node: &NodeId) -> Result<usize, EditorError> {
    match model.node(node).map(|n| &n.kind) {
        Some(ModelNodeKind::Spine(sp)) => Ok(sp.joints().len()),
        Some(_) => Err(EditorError::BadTarget(format!(
            "node {node} is not a spine"
        ))),
        None => Err(EditorError::NoNode(node.clone())),
    }
}

/// The joints of a spine the turn pass can compose, or the refusal that says
/// which one is wrong.
///
/// The same three rules the `.clm` reader keeps, checked here so a command
/// never authors a model the file would refuse: at least one joint, every
/// coordinate finite, and no joint repeating the point above it — the node's
/// own origin for the first one.
fn spine_joints(joints: Vec<[f32; 2]>) -> Result<Vec<[f32; 2]>, EditorError> {
    if joints.is_empty() {
        return Err(EditorError::BadTarget(
            "a spine needs at least one joint".into(),
        ));
    }
    let mut above = [0.0f32, 0.0];
    for (i, joint) in joints.iter().enumerate() {
        if !joint[0].is_finite() || !joint[1].is_finite() {
            return Err(EditorError::BadTarget(format!(
                "spine joint {i} has a coordinate that is not finite"
            )));
        }
        if joint == &above {
            return Err(EditorError::BadTarget(format!(
                "spine joint {i} repeats the point above it"
            )));
        }
        above = *joint;
    }
    Ok(joints)
}

/// A node's world matrix at rest, walking the model's own transforms up the
/// tree. `lock_to_root` is not honoured: a locked node's world rotation is the
/// root's, and the one caller here only wants an orientation to warn about.
fn rest_world(model: &Model, id: &NodeId) -> Mat4 {
    let mut chain: Vec<Mat4> = Vec::new();
    let mut at = Some(id.clone());
    while let Some(current) = at {
        let Some(node) = model.node(&current) else {
            break;
        };
        chain.push(clm_transform_matrix(&node.transform));
        at = node.parent().cloned();
    }
    chain.iter().rev().fold(Mat4::IDENTITY, |acc, m| acc * *m)
}

/// One authored transform as a matrix, through the core's own conversion so it
/// is the same composition a bake would build.
fn clm_transform_matrix(t: &catchlight_core::formats::clm::ClmTransform) -> Mat4 {
    catchlight_core::Transform::from(t).to_matrix()
}

/// The node's world linear map in the physics frame — what
/// `Arena::chain_carry` computes from a puppet's transforms, computed here
/// from the model's, so a command can warn about a fit before anything is
/// baked.
fn rest_carry(model: &Model, id: &NodeId) -> catchlight_core::Mat2 {
    use catchlight_core::Mat2;
    let world = rest_world(model, id);
    let m = Mat2::from_cols(
        Vec2::new(world.x_axis.x, world.x_axis.y),
        Vec2::new(world.y_axis.x, world.y_axis.y),
    );
    let flip = Mat2::from_cols(Vec2::new(1.0, 0.0), Vec2::new(0.0, -1.0));
    flip * m * flip
}

/// Ask the runtime where the chain settles, so authoring feedback uses the
/// same coupled forces and limits as playback.
fn chain_warnings(model: &Model, node: &NodeId) -> Vec<String> {
    let Some(ModelNodeKind::Spine(spine)) = model.node(node).map(|n| &n.kind) else {
        return Vec::new();
    };
    let Some(chain) = spine.chain() else {
        return Vec::new();
    };
    let g_scale = model.physics().pixels_per_meter * model.physics().gravity;
    let gravity = chain.gravity * g_scale;
    let carry = rest_carry(model, node);
    let data = catchlight_core::SpineData::new(
        spine
            .joints()
            .iter()
            .map(|j| Vec2::new(j[0], j[1]))
            .collect(),
    );
    let links = data
        .link_geometry()
        .zip(chain.links())
        .map(
            |((length, drawn), feel)| catchlight_core::physics::ChainLink {
                length,
                drawn,
                gravity_scale: feel.gravity_scale,
                damping: feel.damping,
                stiffness: feel.stiffness,
                limit: feel.limit,
            },
        )
        .collect();
    let mut runtime = catchlight_core::physics::ParticleChainData::new(links);
    runtime.gravity = gravity;
    runtime.settle_to_rest(Vec2::ZERO, carry, &[]);
    let local = carry.inverse();
    let inverse = Mat4::from_cols(
        local.x_axis.extend(0.0).extend(0.0),
        local.y_axis.extend(0.0).extend(0.0),
        Mat4::IDENTITY.z_axis,
        Mat4::IDENTITY.w_axis,
    );
    let mut bends = Vec::new();
    runtime.link_bends(inverse, &mut bends);
    bends
        .iter()
        .enumerate()
        .filter(|(_, bend)| bend.abs() > 1e-3)
        .map(|(i, _)| {
            format!(
                "link {} cannot rest as drawn under its gravity and bend response",
                i + 1
            )
        })
        .collect()
}

/// Rig a strand of art to a spine: the spine between the part and its parent,
/// the params its links read, and the chain that may drive them.
///
/// **The part keeps the world placement it had.** The spine is inserted at the
/// identity transform under the part's own parent and the part is reparented
/// under it untouched, so nothing about where the art sits can change — not
/// for a rotated part, not for a scaled one. The joints are then expressed in
/// the spine's own space, which is why the fit's vertex-space polyline goes
/// through the part's transform on the way in.
///
/// **A re-fit reuses the spine the part already hangs from.** Nesting a second
/// one would compose two turns where the rigger asked for one, so a part whose
/// parent is a spine has that spine re-measured: its chain keeps every knob,
/// its link list is refitted by repeating the last feel, and its params are
/// kept wherever the count still matches.
fn spine_fit(
    s: &mut Session,
    part: &NodeId,
    links: u32,
    axis: Option<[f32; 2]>,
    id: Option<NodeId>,
    name: Option<String>,
    chain: Option<&ChainArg>,
) -> Result<ResponseBody, EditorError> {
    if links == 0 {
        return Err(EditorError::BadTarget(
            "a spine needs at least one link".into(),
        ));
    }
    // Everything read off the part before the model is touched.
    let (fit, part_name, part_local, origin, parent) = {
        let node = s
            .model
            .node(part)
            .ok_or_else(|| EditorError::NoNode(part.clone()))?;
        let mesh = node
            .mesh()
            .ok_or_else(|| EditorError::BadTarget(format!("node {part} holds no mesh to fit")))?;
        let fit = fit_strand(mesh, links, axis)
            .map_err(|e| EditorError::BadTarget(format!("cannot fit a spine to {part}: {e}")))?;
        let parent = node.parent().cloned().ok_or_else(|| {
            EditorError::BadTarget(format!(
                "node {part} is the model's root, so a spine has nowhere to go above it"
            ))
        })?;
        (
            fit,
            node.name.to_string(),
            clm_transform_matrix(&node.transform),
            mesh.origin,
            parent,
        )
    };

    // A re-fit when the part already hangs from one; a fresh spine otherwise.
    let existing = matches!(
        s.model.node(&parent).map(|n| &n.kind),
        Some(ModelNodeKind::Spine(_))
    )
    .then(|| parent.clone());

    // Where the strand's root sits in the frame the spine will live in, and
    // the joints as offsets from it.
    //
    // The spine's own origin has to land on the strand's root, so the fit
    // moves the spine there and takes the same vector back off the part. The
    // two cancel — the spine carries only a translation — so every vertex
    // draws exactly where it drew, which is what
    // `a_fit_leaves_the_parts_world_placement_alone` measures.
    let root_offset = part_local.transform_point3(Vec3::new(
        fit.root[0] - origin[0],
        fit.root[1] - origin[1],
        0.0,
    ));
    let count = links as usize;
    let mut travelled = 0.0f32;
    let joints: Vec<[f32; 2]> = (0..count)
        .map(|i| {
            travelled += fit.lengths[i];
            // The linear part only: an offset from the root is a vector, and
            // the root's own place is what `root_offset` carries.
            let v = part_local.transform_vector3(Vec3::new(
                fit.axis[0] * travelled,
                fit.axis[1] * travelled,
                0.0,
            ));
            [v.x, v.y]
        })
        .collect();
    let joints = spine_joints(joints)?;

    // The params the spine being re-fitted already reads.
    let held_targets = match &existing {
        Some(spine) => match s.model.node(spine).map(|n| &n.kind) {
            Some(ModelNodeKind::Spine(sp)) => sp.targets().to_vec(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };

    // One param per link, reusing what the spine already read. A link that
    // read nothing, and every link past the old end, gets a fresh one. The
    // range is half turns outright — a spine reads a param's value and not
    // its position along a range — with a key at each end and nothing keyed
    // in between, because a spine carries no cells to key.
    let mut params = Vec::with_capacity(count);
    for i in 0..count {
        match held_targets.get(i).and_then(Option::as_ref) {
            Some(held) if s.model.param(held).is_some() => params.push(held.clone()),
            _ => {
                let param = s.add_param(
                    None,
                    ModelParam {
                        name: Name::truncated(format!("{part_name} bend {}", i + 1)),
                        min: -1.0,
                        max: 1.0,
                        default: 0.0,
                    },
                )?;
                params.push(param);
            }
        }
    }

    let node = match existing {
        Some(spine) => {
            s.model.set_spine_joints(&spine, joints)?;
            spine
        }
        None => {
            let node = ModelNode::new(
                name.unwrap_or_else(|| format!("{part_name} spine")),
                ModelNodeKind::Spine(ModelSpine::new(joints)),
            );
            let spine = s.add_node(&parent, id, node)?;
            s.model.reparent(part, &spine)?;
            spine
        }
    };
    // Move the spine onto the strand's root and take the same vector off the
    // part, in that order. Both are pure translations of the same size, so the
    // composition is unchanged and nothing the part draws moves.
    s.model.update_node(&node, |n| {
        n.transform.translation[0] += root_offset.x;
        n.transform.translation[1] += root_offset.y;
    })?;
    s.model.update_node(part, |n| {
        n.transform.translation[0] -= root_offset.x;
        n.transform.translation[1] -= root_offset.y;
    })?;
    s.model
        .set_spine_targets(&node, params.iter().cloned().map(Some).collect())?;
    // A named chain replaces what stood there; an absent one leaves a re-fit's
    // chain alone, and `set_spine_joints` has already refitted its link list.
    if let Some(arg) = chain {
        s.model
            .set_spine_chain(&node, Some(chain_of(arg, count)?))?;
    }

    let warnings = chain_warnings(&s.model, &node);
    Ok(ResponseBody::SpineFit {
        node,
        params,
        warnings,
    })
}

/// The wire's mesh as the model stores one.
///
/// The wire types carry the arity — a vertex is a pair and a triangle is a
/// triple — so the only thing left to check is that the two lists are the same
/// length and that every corner names a vertex that exists. `ClmMesh` is flat,
/// so this is also where the pairs are run together.
fn build_mesh(
    verts: Vec<[f32; 2]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<[u32; 3]>,
    origin: [f32; 2],
) -> Result<ClmMesh, EditorError> {
    let vcount = verts.len();
    if (!uvs.is_empty() && uvs.len() != vcount)
        || indices
            .iter()
            .any(|tri| tri.iter().any(|&i| i as usize >= vcount))
    {
        return Err(EditorError::BadTarget("malformed mesh".into()));
    }
    let flat: Vec<u32> = indices.concat();
    let indices = if flat.iter().max().copied().unwrap_or(0) <= u16::MAX as u32 {
        ClmIndices::U16(flat.iter().map(|&i| i as u16).collect())
    } else {
        ClmIndices::U32(flat)
    };
    Ok(ClmMesh {
        verts: verts.concat(),
        uvs: uvs.concat(),
        indices,
        origin,
    })
}

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
    let alpha = AlphaMask::decode_as(&texture.data, texture.encoding)
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
            rings,
            min_distance,
            mirror_x,
        } => {
            let d = ContourKnobs::default();
            let knobs = ContourKnobs {
                threshold: threshold.unwrap_or(d.threshold),
                simplify: simplify.unwrap_or(d.simplify),
                margin: margin.unwrap_or(d.margin),
                spacing: spacing.unwrap_or(d.spacing),
                rings: rings.unwrap_or(d.rings),
                min_distance: min_distance.unwrap_or(d.min_distance),
                // `or`, not `unwrap_or`: the editor's own answer here is
                // "no mirror line", so an absent knob stays absent.
                mirror_x: mirror_x.or(d.mirror_x),
            };
            contour_automesh(&alpha, &knobs, &uv_map, origin)?
        }
        AutoMesh::Grid {
            threshold,
            cols,
            rows,
            axes_x,
            axes_y,
            margin,
        } => {
            let d = GridKnobs::default();
            let knobs = GridKnobs {
                threshold: threshold.unwrap_or(d.threshold),
                cols: cols.unwrap_or(d.cols),
                rows: rows.unwrap_or(d.rows),
                axes_x: axes_x.unwrap_or(d.axes_x),
                axes_y: axes_y.unwrap_or(d.axes_y),
                // Absent stays absent: the default margin is one texel,
                // which no fraction of the box names.
                margin: margin.or(d.margin),
            };
            grid_automesh(&alpha, &knobs, &uv_map, origin)?
        }
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
fn emptied_reply(node: NodeId, emptied: Vec<SlotId>) -> ResponseBody {
    ResponseBody::Emptied {
        node,
        slots: emptied,
    }
}

/// A weld is two parts and the pairs between them, exactly as the wire says.
/// Empty `pairs` records the two parts as welded and joins nothing yet; the
/// model checks every pair against the slots each part carries.
fn build_weld(a: NodeId, b: NodeId, pairs: Vec<SlotPair>) -> ModelWeld {
    ModelWeld::new(
        a,
        b,
        pairs
            .into_iter()
            .map(|p| catchlight_core::SlotPair {
                a: p.a,
                b: p.b,
                weight: p.weight,
            })
            .collect(),
    )
}

/// Two welds join the same pair of parts, in either order.
fn pairs_the_same_parts(one: &ModelWeld, other: &ModelWeld) -> bool {
    (one.a() == other.a() && one.b() == other.b()) || (one.a() == other.b() && one.b() == other.a())
}

/// Whether `weld` is the one joining these two parts. A weld has no Id of its
/// own — its two ends are what names it — so either order finds it.
fn joins(weld: &ModelWeld, a: &NodeId, b: &NodeId) -> bool {
    (weld.a() == a && weld.b() == b) || (weld.a() == b && weld.b() == a)
}

/// In-process model view handed to observers (the GUI); refs are stable for
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
    let blend = patch.blend_mode.map(CoreBlendMode::from);
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

/// The encoding a storage key's extension implies. A key is opaque to
/// everything else; this is the one place its tail is read, and only to
/// pick a decoder.
/// Build a [`clm::ClmFile`] from a JSON structure and the images that came
/// with it.
///
/// Two pairings have to hold and both are the client's to get right, so both
/// are refused here naming what is wrong: every declared texture needs its
/// attachment, and every attachment needs a declaration. A texture the
/// *structure* references but nobody declared is left to the reader, which
/// already refuses a dangling albedo by Id — one check, in the one place that
/// knows what a structure references.
fn clm_file_from_json(
    structure: &[u8],
    textures: &[ImportTexture],
    images: Vec<(String, Vec<u8>)>,
) -> Result<clm::ClmFile, EditorError> {
    let json = std::str::from_utf8(structure).map_err(|e| {
        EditorError::BadRequest(format!("the structure attachment is not UTF-8: {e}"))
    })?;
    let doc: clm::ClmStructure = serde_json::from_str(json).map_err(|e| {
        EditorError::BadRequest(format!("the attachment is not a .clm structure: {e}"))
    })?;

    let mut attached: HashMap<String, Vec<u8>> = images.into_iter().collect();
    let mut table = Vec::with_capacity(textures.len());
    for want in textures {
        let data = attached.remove(want.texture.as_str()).ok_or_else(|| {
            EditorError::BadRequest(format!(
                "texture {:?} was declared but no attachment texture:{} arrived",
                want.texture.as_str(),
                want.texture
            ))
        })?;
        table.push(clm::ClmTexture {
            id: want.texture.clone(),
            encoding: want.encoding.into(),
            alpha: want.alpha.into(),
            data,
        });
    }
    // Whatever is left named a texture the command did not declare. Silently
    // dropping it would import a model missing an image the client thought it
    // had sent.
    if let Some(stray) = attached.keys().min() {
        return Err(EditorError::BadRequest(format!(
            "attachment texture:{stray} names no texture this import declared"
        )));
    }

    Ok(clm::ClmFile {
        doc,
        textures: table,
        // A byte extension's payload lives in a container section, and JSON
        // has none; a marker with no bytes is refused by key when
        // the file is read. Set such an extension after the import.
        extensions: Vec::new(),
    })
}

/// Read a decoded `.clm` as a fragment whose every root hangs from `parent`.
///
/// The two shapes are disjoint on the wire — a complete model has exactly one
/// parentless node and a fragment has none — so this re-parents at the
/// structure, before either reader sees it: a node whose named parent the
/// structure does not itself carry is one of its roots, and a complete model's
/// root, which names no parent at all, is the same thing. Both then read as a
/// fragment, and [`Model::install`] applies the same checks to either.
///
/// The `parent` on the wire wins over whatever a fragment's roots name: a
/// client says where a subtree goes, and an addon authored against another
/// base does not get to say it instead.
fn fragment_under(mut file: clm::ClmFile, parent: &NodeId) -> Result<Model, EditorError> {
    let carried: std::collections::HashSet<&NodeId> =
        file.doc.nodes.iter().map(|n| &n.id).collect();
    let roots: Vec<usize> = file
        .doc
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.parent.as_ref().is_none_or(|p| !carried.contains(p)))
        .map(|(at, _)| at)
        .collect();
    for at in roots {
        file.doc.nodes[at].parent = Some(parent.clone());
    }
    Ok(Model::from_clm_file_fragment(&file)?)
}

/// Hold one command's attachments to what it declared in [`COMMAND_BYTES`].
///
/// Two refusals, both before the command runs: a name the command does not
/// declare at all, and an [`Attachment::Fixed`] name that did not arrive. So a
/// dispatch arm can take what its command declared without deciding what to do
/// when it is not there.
fn check_attachments(cmd: &Command, attachments: &Attachments) -> Result<(), EditorError> {
    let declared: &[Attachment] = cmd.bytes().map_or(&[], |b| b.attachments);
    for name in attachments.names() {
        if !declared.iter().any(|a| a.admits(name)) {
            return Err(EditorError::BadRequest(format!(
                "{} takes no attachment named {name:?}",
                cmd.tag()
            )));
        }
    }
    for a in declared {
        if let Attachment::Fixed(name) = a {
            if !attachments.names().contains(name) {
                return Err(EditorError::BadRequest(format!(
                    "{} needs the attachment {name:?}",
                    cmd.tag()
                )));
            }
        }
    }
    Ok(())
}

/// What bytes this command carries, or `None` if it carries none.
///
/// The one question every transport asks, phrased once — and it is
/// [`Command::carries_bytes`], which answers for the command in hand rather
/// than for its tag. The two differ only where an attachment is optional: an
/// `extension_set` of a JSON value carries nothing, and the socket takes it
/// like any other small command.
///
/// A transport that cannot carry bytes refuses anything this answers `Some`
/// for with [`ErrorCode::BulkOverHttp`]; one that can reads the row to know
/// what to frame in, and whether a payload is coming back.
pub fn carries_bytes(cmd: &Command) -> Option<&'static Bytes> {
    cmd.carries_bytes()
}

/// How to read an image, from the tail of the reference that named it.
///
/// The one place a key's shape is read, and it is a *manifest's* references
/// rather than a command's: a manifest names files by path and says nothing
/// about how they are encoded. Every command carries an `encoding` field.
fn encoding_from_path(path: &str) -> CoreTextureEncoding {
    if path.to_ascii_lowercase().ends_with(".tga") {
        CoreTextureEncoding::Tga
    } else {
        CoreTextureEncoding::Png
    }
}

fn image_dims(bytes: &[u8], encoding: CoreTextureEncoding) -> Result<(u32, u32), EditorError> {
    catchlight_editor_core::image_dims_as(bytes, encoding).map_err(EditorError::Image)
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
    use std::path::PathBuf;

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
            Reply::Err { id: 0, code: ErrorCode::BadRequest, message, .. }
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
                Reply::Err {
                    id, code, message, ..
                } => {
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
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
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
            if let (Event::ModelChanged { session, .. }, Some(ed)) = (event, weak.upgrade()) {
                // Re-entering the editor from an observer is the normal case.
                ed.with_model(*session, |m| m.node_count()).unwrap();
            }
            lock(&sink).push(event.clone());
        }));

        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
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
            [Event::ModelChanged { session, rev }] => {
                assert_eq!(*session, s);
                assert_eq!(*rev, 1, "the event carries the revision after the edit");
            }
            other => panic!("expected one ModelChanged, got {other:?}"),
        }
        lock(&seen).clear();

        body(ed.handle(req(3, Command::SessionClose { session: s })));
        assert!(matches!(lock(&seen).as_slice(), [Event::SessionsChanged]));
        lock(&seen).clear();

        ed.unsubscribe(handle);
        let s = session_of(body(ed.handle(req(
            4,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
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
        let (s, rev) = match ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        )) {
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
        let first = session_of(body(editor.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
        let second = session_of(body(editor.handle(req(
            2,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
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
    fn session_node_save_reopen_lifecycle() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: Some("t".into()),
                source: None,
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
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
        let part = new_part(&ed, s, 2);
        let rev = ed.doc_snapshot(s).expect("a snapshot").rev;

        let (tex, dropped) = ed
            .add_texture_bytes(s, &part, CoreTextureEncoding::Png, one_pixel_png([9; 4]))
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
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
        let part = new_part(&ed, s, 2);
        let (first, _) = ed
            .add_texture_bytes(s, &part, CoreTextureEncoding::Png, one_pixel_png([1; 4]))
            .expect("upload");

        // A second upload to the same part displaces the first, which nothing
        // else draws.
        let (second, dropped) = ed
            .add_texture_bytes(s, &part, CoreTextureEncoding::Png, one_pixel_png([2; 4]))
            .expect("upload");
        assert_eq!(dropped, vec![first]);
        assert_eq!(texture_ids(&ed, s, 4), vec![second.clone()]);

        // Unmapping the part through the wire says the same thing.
        let other = new_part(&ed, s, 5);
        let (third, _) = ed
            .add_texture_bytes(s, &other, CoreTextureEncoding::Png, one_pixel_png([3; 4]))
            .expect("upload");
        match body(ed.handle(req(
            7,
            Command::NodeSet {
                session: s,
                node: other,
                patch: NodePatch {
                    texture: Some(Some(second.clone())),
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
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
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
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
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
            ed.handle(req(
                6,
                Command::Undo {
                    session: s,
                    if_rev: 2
                }
            )),
            Reply::Ok { .. }
        ));
        assert!(matches!(
            ed.handle(req(
                7,
                Command::Undo {
                    session: s,
                    if_rev: 3
                }
            )),
            Reply::Ok { .. }
        ));
        assert_eq!(node_count(&ed, s, 8), 1);
        assert!(matches!(
            ed.handle(req(
                9,
                Command::Undo {
                    session: s,
                    if_rev: 4
                }
            )),
            Reply::Err { .. }
        ));
        assert!(matches!(
            ed.handle(req(
                10,
                Command::Redo {
                    session: s,
                    if_rev: 4
                }
            )),
            Reply::Ok { .. }
        ));
        assert_eq!(node_count(&ed, s, 11), 2);
    }

    #[test]
    fn doc_snapshot_is_rev_gated() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
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
    fn presence_is_off_the_model_path() {
        let ed = Editor::new();
        let s = session_of(body(ed.handle(req(
            1,
            Command::SessionNew {
                name: None,
                source: None,
            },
        ))));
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
        // presence must not change the model: same cached snapshot Arc.
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
                assert!(!status.dirty, "presence does not dirty the model");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn preview_renders_png_smoke() {
        let mut img = image::RgbaImage::new(32, 32);
        for px in img.pixels_mut() {
            *px = image::Rgba([220, 40, 40, 255]);
        }
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();

        let ed = Editor::new();
        let mut attachments = Attachments::none();
        attachments.insert(
            "manifest",
            br#"{"textures":[{"id":"face","path":"face.png"}],
               "nodes":[{"id":"face","kind":"part","texture":"face","mesh":{"auto":"quad"}}]}"#
                .to_vec(),
        );
        attachments.insert("texture:face.png", png.into_inner());
        let s = session_of(body(
            ed.handle_with(
                req(
                    2,
                    Command::SessionNew {
                        name: None,
                        source: Some(SessionSource::Manifest {}),
                    },
                ),
                attachments,
            )
            .0,
        ));
        let (reply, payload) = ed.handle_with(
            req(
                3,
                Command::Preview {
                    session: s,
                    pose: vec![],
                    size: Some([128, 128]),
                    camera: None,
                },
            ),
            Attachments::none(),
        );
        match reply {
            Reply::Ok {
                body: ResponseBody::Preview { preview },
                ..
            } => {
                assert_eq!(preview.width, 128);
                let payload = payload.expect("preview answers with the png");
                assert_eq!(payload.content_type, "image/png");
                assert!(!payload.bytes.is_empty(), "preview png is empty");
            }
            other => panic!("preview failed: {other:?}"),
        }
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
        let ed = Editor::new();
        let mut attachments = Attachments::none();
        attachments.insert("model", bytes);
        let s = session_of(body(
            ed.handle_with(
                req(
                    2,
                    Command::SessionNew {
                        name: None,
                        source: Some(SessionSource::Clm {}),
                    },
                ),
                attachments,
            )
            .0,
        ));
        let shot = |id: u64, pose: Vec<ParamPose>| -> Vec<u8> {
            let (reply, payload) = ed.handle_with(
                req(
                    id,
                    Command::Preview {
                        session: s,
                        pose,
                        // welded_seam spans 300x240 world units, well inside
                        // the default 2000-unit camera.
                        size: Some([256, 256]),
                        camera: None,
                    },
                ),
                Attachments::none(),
            );
            match reply {
                Reply::Ok {
                    body: ResponseBody::Preview { .. },
                    ..
                } => payload.expect("preview answers with the png").bytes,
                other => panic!("preview failed: {other:?}"),
            }
        };

        let rest = shot(1, Vec::new());
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
            vec![ParamPose {
                param: pull,
                value: 1.0,
            }],
        );
        let back = shot(3, Vec::new());

        assert_ne!(rest, posed, "posing pull must change the preview");
        assert_eq!(rest, back, "leaving pull out must render it at its default");
    }
}
