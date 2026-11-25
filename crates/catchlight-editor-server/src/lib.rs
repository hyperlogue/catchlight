//! The catchlight editor server.

#[cfg(not(target_arch = "wasm32"))]
pub mod connection;
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
mod review;
mod review_store;
mod storage;
#[cfg(unix)]
mod transport;

#[cfg(not(target_arch = "wasm32"))]
pub use http::{bind_http, serve_http, HttpOptions, HttpServer, UiFiles};
pub use query::{extension_value_info, replica_query, replica_reply};
#[cfg(not(target_arch = "wasm32"))]
pub use review_store::{user_review_dir, SidecarReviewStore, UnusedReview, UserReviewStore};
pub use review_store::{NoReviewStore, ReviewStore};
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

use catchlight_core::{
    BindingKey, BindingTarget as CoreBindingTarget, ExtensionValue, InstallError, Mat4, Model,
    ModelComposite, ModelError, ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam, ModelPart,
    ModelSpine, ModelTexture, ModelWeld, Pose, Puppet, Required, Vec3,
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
    #[error("{0}")]
    BadRequest(String),
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
    #[error("node {0} is not a part")]
    NotAPart(NodeId),
    #[error("part {0} draws no texture")]
    NoAlbedo(NodeId),
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
    #[error("install: {0}")]
    Install(#[from] InstallError),
    #[error("no extension {0:?}")]
    NoExtension(String),
    #[error("no review thread {}", .0.0)]
    NoThread(ThreadId),
    #[error("no comment {0} on that thread")]
    NoComment(u64),
    #[error("no review image {0}")]
    NoImage(u64),
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
            Self::NoThread(_) => ErrorCode::NoThread,
            Self::NoComment(_) => ErrorCode::NoComment,
            Self::NoImage(_) => ErrorCode::NoImage,
            // Install's refusals answer with the codes the equivalent edit gets.
            Self::Install(e) => match e {
                InstallError::Collision { .. } => ErrorCode::DuplicateId,
                InstallError::Missing { id, .. } => match id {
                    Required::Node(_) | Required::Drawable(_) | Required::Part(_) => {
                        ErrorCode::NoNode
                    }
                    Required::Param(_) => ErrorCode::NoParam,
                    Required::Slot(..) => ErrorCode::UnknownSlot,
                },
                InstallError::WeldUnknownSlot { .. } => ErrorCode::WeldUnknownSlot,
                InstallError::WeldSlotPairedTwice { .. } => ErrorCode::WeldSlotPairedTwice,
                InstallError::WeldEndNotAPart { .. } => ErrorCode::BadWeldEnd,
                InstallError::DuplicateWeld { .. } => ErrorCode::DuplicateWeld,
                InstallError::Model(e) => model_error_code(e),
                // `InstallError` is non-exhaustive.
                _ => ErrorCode::Edit,
            },
            Self::Edit(e) => model_error_code(e),
        }
    }
}

fn model_error_code(e: &ModelError) -> ErrorCode {
    match e {
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
        ModelError::NotSpine | ModelError::NotMotionResponse => ErrorCode::BadTarget,
        ModelError::SpineTargetArity { .. } | ModelError::SpineJoints(_) => ErrorCode::BadTarget,
        // The size cap has no code of its own: a client that hit it
        // has nothing to branch on, only a value to shrink.
        _ => ErrorCode::Edit,
    }
}

/// Fixed, so a given [`SessionId`] always draws the same sequence.
const ID_SEED: u32 = 0x1d5e_ed01;

/// Deterministic so a replayed script rebuilds a byte-identical model;
/// per-session so two open sessions do not mint the same Ids for the same
/// edits. The seed is drawn through a `SeededHex` so consecutive session ids
/// land far apart in the sequence rather than one Weyl step apart.
fn session_hex(id: SessionId) -> SeededHex {
    SeededHex::new(SeededHex::new(ID_SEED ^ id.0 as u32).next_bits())
}

struct Session {
    model: Model,
    hex: SeededHex,
    title: String,
    /// Opaque storage key this session was opened from or last saved to.
    file: Option<String>,
    rev: u64,
    saved_rev: u64,
    history: ModelHistory,
    /// Set while holding this handle before removing it from the registry.
    closed: bool,
    puppet: Option<Puppet>,
    /// Carries the preview pose across navigation, and while a renderer owns
    /// the Puppet outside the session lock.
    pending_preview_pose: Option<Pose>,
    snapshot: Option<Arc<DocSnapshot>>,
    presence: Option<Presence>,
    review: review::ReviewState,
    review_keeping: review::ReviewKeeping,
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
            review: review::ReviewState::default(),
            review_keeping: review::ReviewKeeping::default(),
        }
    }

    /// For a thread that followed an edit, which cannot fail on the review's
    /// account. A write that is refused is retried by the next one, and a
    /// reopened review finds its targets through the renames it kept.
    fn keep_review(&mut self, store: &dyn ReviewStore) {
        let _ = self
            .review_keeping
            .write(store, self.file.as_deref(), &self.review);
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

    /// `id: None` draws a free Id; the charset was validated at decode.
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
            // Disabling motion responses keeps the authoring preview
            // deterministic and lets response-driven params be posed by hand.
            built.set_motion_responses_enabled(false);
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

/// Called with no editor lock held, so it may re-enter [`Editor`].
pub type Observer = Box<dyn Fn(&Event) + Send + Sync>;

type SharedObserver = Arc<dyn Fn(&Event) + Send + Sync>;

/// Checked against [`COMMAND_BYTES`] before any command runs.
#[derive(Debug, Default)]
pub struct Attachments(std::collections::BTreeMap<String, Vec<u8>>);

impl Attachments {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, bytes: Vec<u8>) {
        self.0.insert(name.into(), bytes);
    }

    pub fn names(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn take(&mut self, name: &str) -> Option<Vec<u8>> {
        self.0.remove(name)
    }

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

pub struct Editor {
    sessions: Mutex<HashMap<SessionId, Arc<Mutex<Session>>>>,
    next_id: AtomicU64,
    observers: Mutex<Vec<(u64, SharedObserver)>>,
    next_observer: AtomicU64,
    storage: Arc<dyn Storage>,
    review_store: Arc<dyn ReviewStore>,
    #[cfg(not(target_arch = "wasm32"))]
    preview: Mutex<Option<PreviewRenderer>>,
}

impl Editor {
    /// Filesystem rooted at the cwd natively, with reviews in sidecars there;
    /// [`NoStorage`] on wasm, where the host must use [`Editor::with_storage`].
    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let editor = Self::with_storage(Arc::new(FileStorage::default()))
            .with_review_store(Arc::new(SidecarReviewStore::default()));
        #[cfg(target_arch = "wasm32")]
        let editor = Self::with_storage(Arc::new(NoStorage));
        editor
    }

    /// Where reviews are kept between runs; without this, nowhere. Model keys
    /// are the ones `storage` is given.
    pub fn with_review_store(mut self, store: Arc<dyn ReviewStore>) -> Self {
        self.review_store = store;
        self
    }

    pub fn with_storage(storage: Arc<dyn Storage>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            observers: Mutex::new(Vec::new()),
            next_observer: AtomicU64::new(1),
            storage,
            review_store: Arc::new(NoReviewStore),
            #[cfg(not(target_arch = "wasm32"))]
            preview: Mutex::new(None),
        }
    }

    /// [`Self::handle_with`] with nothing attached; byte-bearing commands are
    /// refused.
    pub fn handle(&self, req: Request) -> Reply {
        self.handle_with(req, Attachments::none()).0
    }

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

    /// `None` if the session is not open.
    fn rev(&self, id: SessionId) -> Option<u64> {
        self.with_session(id, |s| Ok(s.rev)).ok()
    }

    /// To stamp model data coherently, use [`Self::with_model_revision`].
    pub fn revision(&self, id: SessionId) -> Option<u64> {
        self.rev(id)
    }

    /// The callback runs on the command's thread with no editor lock held.
    pub fn subscribe(&self, observer: Observer) -> u64 {
        let handle = self.next_observer.fetch_add(1, Ordering::Relaxed);
        lock(&self.observers).push((handle, Arc::from(observer)));
        handle
    }

    pub fn unsubscribe(&self, handle: u64) {
        lock(&self.observers).retain(|(h, _)| *h != handle);
    }

    /// The list is copied out before the first call so an observer may
    /// (un)subscribe re-entrantly without deadlocking.
    fn notify(&self, event: Event) {
        let observers: Vec<SharedObserver> = {
            let guard = lock(&self.observers);
            guard.iter().map(|(_, o)| o.clone()).collect()
        };
        for observer in observers {
            observer(&event);
        }
    }

    fn notify_model_changed(&self, session: SessionId, rev: u64) {
        self.notify(Event::ModelChanged { session, rev });
    }

    fn notify_sessions(&self) {
        self.notify(Event::SessionsChanged);
    }

    fn alloc_id(&self) -> SessionId {
        SessionId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

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

    /// The caller confirms with [`Self::mark_saved`] once the bytes actually
    /// land; a failed download must not clear the dirty flag.
    pub fn save_bytes(&self, id: SessionId) -> Result<Vec<u8>, EditorError> {
        self.with_session(id, |s| Ok(s.model.to_clm_bytes()?))
    }

    pub fn mark_saved(&self, id: SessionId) -> Result<(), EditorError> {
        let rev = self.with_session(id, |s| {
            s.saved_rev = s.rev;
            Ok(s.rev)
        })?;
        self.notify_model_changed(id, rev);
        Ok(())
    }

    pub fn history(&self, id: SessionId) -> Result<(usize, usize), EditorError> {
        self.with_session(id, |s| Ok(s.history.depths()))
    }

    pub fn is_dirty(&self, id: SessionId) -> Result<bool, EditorError> {
        self.with_session(id, |s| Ok(s.dirty()))
    }

    pub fn with_model<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model) -> R,
    ) -> Result<R, EditorError> {
        self.with_session(id, |s| Ok(f(&s.model)))
    }

    pub fn with_model_revision<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model, u64) -> R,
    ) -> Result<R, EditorError> {
        self.with_session(id, |s| Ok(f(&s.model, s.rev)))
    }

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

    /// The single publication seam for authored edits; callbacks run only
    /// after unlocking. A failed or no-op edit restores the original model/rev.
    fn edit_session_captured<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<Captured<R>, EditorError> {
        let handle = self.session(id)?;
        let (result, moved, reviewed) = {
            let mut session = lock(&handle);
            session.ensure_open(id)?;
            let before = session.rev;
            let snapshot = session.model.clone();
            let missing = session.review.missing(&session.model);
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
            let reviewed =
                session.review.take_touched() || missing != session.review.missing(&session.model);
            session.keep_review(&*self.review_store);
            (result, moved, reviewed)
        };
        if let Some(rev) = moved {
            self.notify_model_changed(id, rev);
        }
        if reviewed {
            self.notify(Event::ReviewChanged { session: id });
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
        let (captured, changed, reviewed) = {
            let mut session = lock(&handle);
            session.ensure_open(id)?;
            session.check_revision(Some(if_rev))?;
            let next = session.next_revision()?;
            let missing = session.review.missing(&session.model);
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
            let Session { review, model, .. } = &mut *session;
            let reviewed = review.follow_model(model) || missing != review.missing(model);
            session.keep_review(&*self.review_store);
            (
                Captured::at(ResponseBody::Empty, session.rev),
                changed,
                reviewed,
            )
        };
        if changed {
            self.notify_model_changed(id, captured.rev.unwrap_or(if_rev));
        }
        if reviewed {
            self.notify(Event::ReviewChanged { session: id });
        }
        Ok(captured)
    }

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

    fn dispatch(
        &self,
        cmd: Command,
        attachments: &mut Attachments,
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
                let (key, rev, moved, review) = {
                    let mut s = lock(&handle);
                    s.ensure_open(session)?;
                    let key = match path {
                        Some(p) => p,
                        None => s.file.clone().ok_or(EditorError::NoSavePath)?,
                    };
                    let bytes = s.model.to_clm_bytes()?;
                    // Save publication includes its persistent write; failure
                    // leaves session metadata untouched.
                    self.storage.write(&key, &bytes)?;
                    let from = s.file.replace(key.clone());
                    s.saved_rev = s.rev;
                    // The model is saved whatever becomes of its review: the
                    // review is written second and its failure is reported
                    // against a save that has fully taken effect.
                    let moved = from.as_deref() != Some(key.as_str());
                    let Session {
                        review,
                        review_keeping,
                        ..
                    } = &mut *s;
                    let kept = if moved {
                        review_keeping.rekey(&*self.review_store, from.as_deref(), &key, review)
                    } else {
                        review_keeping.write(&*self.review_store, Some(&key), review)
                    };
                    (key, s.rev, moved, kept)
                };
                // A save moves no revision but flips `dirty`.
                self.notify_model_changed(session, rev);
                if moved {
                    // What the review says of how long it is kept changed.
                    self.notify(Event::ReviewChanged { session });
                }
                review.map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("saved the model to {key}, but not its review: {e}"),
                    )
                })?;
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
                        node_count: s.model.node_count() as u32,
                        param_count: s.model.param_ids().len() as u32,
                        texture_count: s.model.texture_ids().len() as u32,
                        dirty: s.dirty(),
                        rev: s.rev,
                    },
                })
            }),
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
                match &rename {
                    Rename::Node { from, to } => s.model.rename_node_id(from, to.clone())?,
                    Rename::Param { from, to } => s.model.rename_param_id(from, to.clone())?,
                    Rename::Texture { from, to } => s.model.rename_tex_id(from, to.clone())?,
                    Rename::Slot { node, from, to } => {
                        s.model.rename_slot(node, from, to.clone())?
                    }
                }
                s.review.rename(&rename);
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
            Command::MotionResponseSet {
                session,
                node,
                response,
            } => self.edit_session_captured(session, |s| {
                s.model.set_motion_response(&node, response)?;
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
                // The attachment gate already refused a missing `texture`.
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
                    return Err(ModelError::InvalidParamRange.into());
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
            Command::SpineAdd {
                session,
                parent,
                name,
                joints,
                targets,
                node: id,
            } => self.edit_session_captured(session, |s| {
                check_spine_joints(&joints)?;
                let node = ModelNode::new(
                    name.unwrap_or_else(|| "Spine".into()),
                    ModelNodeKind::Spine(ModelSpine::new(joints)),
                );
                let node = s.add_node(&parent, id, node)?;
                if let Some(targets) = targets {
                    s.model.set_spine_targets(&node, targets)?;
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
            } => self.edit_session_captured(session, |s| {
                if let Some(joints) = joints {
                    check_spine_joints(&joints)?;
                    s.model.set_spine_joints(&node, joints)?;
                }
                if let Some(targets) = targets {
                    s.model.set_spine_targets(&node, targets)?;
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
            } => {
                self.edit_session_captured(session, |s| spine_fit(s, &part, links, axis, id, name))
            }
            Command::PresenceSet { session, presence } => {
                self.with_session_captured(session, |s| {
                    s.presence = Some(presence);
                    Ok(ResponseBody::Empty)
                })
            }
            command @ (Command::ReviewGet { .. }
            | Command::ReviewThreadAdd { .. }
            | Command::ReviewThreadSet { .. }
            | Command::ReviewThreadDelete { .. }
            | Command::ReviewCommentAdd { .. }
            | Command::ReviewCommentDelete { .. }
            | Command::ReviewImageAdd { .. }
            | Command::ReviewImageGet { .. }
            | Command::ReviewSubmit { .. }
            | Command::ReviewPoll { .. }
            | Command::ReviewFetch { .. }) => self.review(command, attachments, payload),
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
            #[cfg(target_arch = "wasm32")]
            Command::Preview { .. } => Err(EditorError::NativeOnly),
            Command::ExtensionSet {
                session,
                key,
                value,
            } => {
                // The gate refused undeclared attachments; which kind of value
                // this is depends on the command.
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
                        .extensions()
                        .get(&key)
                        .map(|value| (extension_value_info(value), value.bytes().cloned()))
                        .ok_or_else(|| EditorError::NoExtension(key.to_string()))
                })?;
                let (info, bytes) = captured.value;
                if let Some(bytes) = bytes {
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
        // Cloned (shallow, same identity and generation) so the session lock
        // is not held across the GPU render.
        let (model, mut puppet, rev) = {
            let mut s = lock(&handle);
            s.ensure_open(session)?;
            let puppet = s.take_preview_puppet()?;
            (s.model.clone(), puppet, s.rev)
        };
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

/// Checked before a command mints a param or node around the joints.
fn check_spine_joints(joints: &[[f32; 2]]) -> Result<(), EditorError> {
    ModelSpine::check_joints(joints).map_err(|e| EditorError::BadTarget(e.to_string()))
}

fn clm_transform_matrix(t: &catchlight_core::formats::clm::ClmTransform) -> Mat4 {
    catchlight_core::Transform::from(t).to_matrix()
}

/// A response chain can sit between the mesh and its spine.
fn fitted_spine<'a>(model: &'a Model, node: &'a ModelNode) -> Option<&'a NodeId> {
    node.mesh()?;
    let mut parent = node.parent()?;
    loop {
        let n = model.node(parent)?;
        match n.kind {
            ModelNodeKind::Spine(_) => return Some(parent),
            ModelNodeKind::MotionResponse(_) => parent = n.parent()?,
            _ => return None,
        }
    }
}

/// A root-locked node ends inheritance, so its matrix is already world space.
fn rest_to_frame(model: &Model, id: &NodeId, frame: Option<&NodeId>) -> Result<Mat4, EditorError> {
    let mut cursor = Some(id);
    let mut matrix = Mat4::IDENTITY;
    while cursor != frame {
        let Some(id) = cursor else {
            let Some(frame) = frame else { break };
            let world = rest_to_frame(model, frame, None)?;
            let inverse =
                catchlight_core::components::checked_affine_inverse(world).ok_or_else(|| {
                    EditorError::BadTarget(format!("cannot fit in the singular frame of {frame}"))
                })?;
            return Ok(inverse * matrix);
        };
        let node = model
            .node(id)
            .ok_or_else(|| EditorError::NoNode(id.clone()))?;
        matrix = clm_transform_matrix(&node.transform) * matrix;
        cursor = if node.lock_to_root {
            None
        } else {
            node.parent()
        };
    }
    Ok(matrix)
}

fn spine_fit(
    s: &mut Session,
    part: &NodeId,
    links: u32,
    axis: Option<[f32; 2]>,
    id: Option<NodeId>,
    name: Option<String>,
) -> Result<ResponseBody, EditorError> {
    if links == 0 {
        return Err(EditorError::BadTarget(
            "a spine needs at least one link".into(),
        ));
    }
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
    let part_name = node.name.to_string();
    let existing = fitted_spine(&s.model, node).cloned();
    let part_local = rest_to_frame(&s.model, part, Some(existing.as_ref().unwrap_or(&parent)))?;
    let root_offset = part_local.transform_point3(Vec3::new(
        fit.root[0] - mesh.origin[0],
        fit.root[1] - mesh.origin[1],
        0.,
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
    check_spine_joints(&joints)?;

    let held_targets = match &existing {
        Some(spine) => match s.model.node(spine).map(|n| &n.kind) {
            Some(ModelNodeKind::Spine(sp)) => sp.targets().to_vec(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };

    // Range is half turns, keyed only at its ends: a spine reads a param's
    // value, not its position along a range, and carries no cells to key.
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
    // Moving the pivot by d in spine space translates the spine by its
    // linear transform times d in parent space. Countertranslate each
    // inheriting child in spine space to preserve the artwork in every branch.
    s.model.update_node(&node, |n| {
        let shift = clm_transform_matrix(&n.transform).transform_vector3(root_offset);
        n.transform.translation = (Vec3::from(n.transform.translation) + shift).to_array();
    })?;
    let children = s
        .model
        .node(&node)
        .ok_or_else(|| EditorError::NoNode(node.clone()))?
        .children()
        .to_vec();
    for child in children {
        s.model.update_node(&child, |n| {
            if !n.lock_to_root {
                n.transform.translation =
                    (Vec3::from(n.transform.translation) - root_offset).to_array();
            }
        })?;
    }
    s.model
        .set_spine_targets(&node, params.iter().cloned().map(Some).collect())?;
    Ok(ResponseBody::SpineFit { node, params })
}

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

/// The UV mapping is fitted from the mesh being replaced when there is one:
/// cropped imported textures are not centred, and a wrong guess moves the art
/// rather than failing.
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

fn emptied_reply(node: NodeId, emptied: Vec<SlotId>) -> ResponseBody {
    ResponseBody::Emptied {
        node,
        slots: emptied,
    }
}

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

fn pairs_the_same_parts(one: &ModelWeld, other: &ModelWeld) -> bool {
    (one.a() == other.a() && one.b() == other.b()) || (one.a() == other.b() && one.b() == other.a())
}

fn joins(weld: &ModelWeld, a: &NodeId, b: &NodeId) -> bool {
    (weld.a() == a && weld.b() == b) || (weld.a() == b && weld.b() == a)
}

/// Refs are stable for the session's lifetime.
#[derive(Debug, Clone)]
pub struct DocSnapshot {
    pub rev: u64,
    pub root: TreeNode,
    pub params: Vec<ParamInfo>,
}

fn default_name(kind: NodeKindArg) -> String {
    match kind {
        NodeKindArg::MotionResponse => "MotionResponse",
        NodeKindArg::Group => "Group",
        NodeKindArg::Part => "Part",
        NodeKindArg::Composite => "Composite",
        NodeKindArg::MeshGroup => "MeshGroup",
    }
    .to_string()
}

fn make_kind(kind: NodeKindArg) -> ModelNodeKind {
    match kind {
        NodeKindArg::MotionResponse => ModelNodeKind::MotionResponse(Default::default()),
        NodeKindArg::Group => ModelNodeKind::Group,
        NodeKindArg::Part => ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
        NodeKindArg::Composite => ModelNodeKind::Composite(ModelComposite::new()),
        NodeKindArg::MeshGroup => ModelNodeKind::MeshGroup(ModelMeshGroup::new(ClmMesh::default())),
    }
}

fn apply_patch(model: &mut Model, id: &NodeId, patch: &NodePatch) -> Result<(), EditorError> {
    model.update_node(id, |n| {
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
    })?;
    if patch.opacity.is_none()
        && patch.blend_mode.is_none()
        && patch.tint.is_none()
        && patch.screen_tint.is_none()
        && patch.mask_threshold.is_none()
        && patch.propagate_meshgroup.is_none()
        && patch.mg_translate_children.is_none()
    {
        return Ok(());
    }
    model.update_node_kind(id, |kind| {
        if let Some(op) = patch.opacity {
            set_opacity(kind, op);
        }
        if let Some(mode) = patch.blend_mode.map(CoreBlendMode::from) {
            match kind {
                ModelNodeKind::Part(p) => p.blend_mode = mode,
                ModelNodeKind::Composite(c) => c.blend_mode = mode,
                _ => {}
            }
        }
        if let Some(t) = patch.tint {
            match kind {
                ModelNodeKind::Part(p) => p.tint = t,
                ModelNodeKind::Composite(c) => c.tint = t,
                _ => {}
            }
        }
        if let Some(t) = patch.screen_tint {
            match kind {
                ModelNodeKind::Part(p) => p.screen_tint = t,
                ModelNodeKind::Composite(c) => c.screen_tint = t,
                _ => {}
            }
        }
        if let Some(th) = patch.mask_threshold {
            match kind {
                ModelNodeKind::Part(p) => p.mask_threshold = th,
                ModelNodeKind::Composite(c) => c.mask_threshold = th,
                _ => {}
            }
        }
        if let Some(v) = patch.propagate_meshgroup {
            if let ModelNodeKind::Composite(c) = kind {
                c.propagate_meshgroup = v;
            }
        }
        if let Some(v) = patch.mg_translate_children {
            if let ModelNodeKind::MeshGroup(mg) = kind {
                mg.translate_children = v;
            }
        }
    })?;
    Ok(())
}

fn set_opacity(kind: &mut ModelNodeKind, op: f32) {
    match kind {
        ModelNodeKind::Part(p) => p.opacity = op,
        ModelNodeKind::Composite(c) => c.opacity = op,
        _ => {}
    }
}

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
    // Leftover attachments named undeclared textures; dropping them silently
    // would import a model missing an image.
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
        unknown_sections: Vec::new(),
    })
}

/// Re-parents every root (a complete model's root or a fragment's unparented
/// nodes) under `parent`; the wire's `parent` wins over what a fragment names.
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

pub fn carries_bytes(cmd: &Command) -> Option<&'static Bytes> {
    cmd.carries_bytes()
}

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

    /// A bad Id must be refused with the request's id: a client blocking on
    /// its own id would otherwise hang.
    #[cfg(unix)]
    #[test]
    fn an_invalid_id_on_the_wire_is_an_error_against_the_requests_own_id() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let editor = Editor::new();
            serve_connection(&editor, server);
        });
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

    /// The observer re-enters the editor; an emission holding a lock would
    /// deadlock here.
    #[test]
    fn an_observer_hears_every_change_until_it_unsubscribes() {
        let ed = Arc::new(Editor::new());
        let seen: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let weak = Arc::downgrade(&ed);
        let handle = ed.subscribe(Box::new(move |event| {
            if let (Event::ModelChanged { session, .. }, Some(ed)) = (event, weak.upgrade()) {
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

        let (second, dropped) = ed
            .add_texture_bytes(s, &part, CoreTextureEncoding::Png, one_pixel_png([2; 4]))
            .expect("upload");
        assert_eq!(dropped, vec![first]);
        assert_eq!(texture_ids(&ed, s, 4), vec![second.clone()]);

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

    /// Pixels prove the pose landed: a preview rendered at rest would still
    /// be a valid PNG.
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
