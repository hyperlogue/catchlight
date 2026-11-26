//! The editor: its sessions, its observers, and the requests it answers.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use catchlight_core::LoadLimits;
use catchlight_core::extension::{IdRef, Payload as ExtensionPayload, RawExtension};
use catchlight_core::formats::clm::TextureAlpha;
#[cfg(test)]
use catchlight_core::formats::clm::TextureEncoding as CoreTextureEncoding;
use catchlight_core::id::Name;

#[cfg(not(target_arch = "wasm32"))]
use catchlight_core::Pose;
use catchlight_core::{
    ExtensionValue, Model, ModelError, ModelNode, ModelNodeKind, ModelParam, ModelSpine,
    ModelTexture, Puppet,
};
// The wire's camera and the renderer's framing are the same shape and not the
// same type; the conversion happens here, at the edge.
use catchlight_editor_core::{HistoryNavigation, ModelMeshExt as _};
#[cfg(not(target_arch = "wasm32"))]
use catchlight_wgpu::Framing;

use catchlight_editor_protocol::*;

#[cfg(target_arch = "wasm32")]
use crate::NoStorage;
use crate::ops::{
    automesh, check_spine_joints, default_name, emptied_reply, image_dims, make_kind, spine_fit,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::preview::PreviewRenderer;
use crate::session::{Captured, Session};
use crate::{
    EditorError, KeptStore, NoKeptStore, Storage, edit, extension_value_info, limits, official,
    query, replica_query, versions,
};
#[cfg(not(target_arch = "wasm32"))]
use crate::{FileStorage, PreviewError, SidecarKeptStore};

/// What a [`Command::Preview`] frames when it names no camera: this many world
/// units tall, centred on the origin.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_CAMERA_HEIGHT: f32 = 2000.0;

/// The most pixels one [`Command::Preview`] renders: its RGBA readback and
/// PNG encode are held in memory whole.
#[cfg(not(target_arch = "wasm32"))]
const MAX_PREVIEW_PIXELS: u64 = 8192 * 8192;

/// Called with no editor lock held, so it may re-enter [`Editor`].
pub type Observer = Box<dyn Fn(&Event) + Send + Sync>;

type SharedObserver = Arc<dyn Fn(&Event) + Send + Sync>;

type Observers = Mutex<Vec<(u64, SharedObserver)>>;

/// Its observer hears events until this is dropped.
#[must_use = "dropping a subscription unsubscribes its observer"]
#[derive(Debug)]
pub struct Subscription {
    observers: Weak<Observers>,
    id: u64,
}

impl Subscription {
    /// Keeps the observer subscribed for as long as the editor lives.
    pub const fn detach(self) {
        std::mem::forget(self);
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(observers) = self.observers.upgrade() {
            lock(&observers).retain(|(id, _)| *id != self.id);
        }
    }
}

/// Their names are checked against [`COMMAND_BYTES`] and their total against
/// [`LoadLimits::encoded_bytes`] before any command runs.
#[derive(Debug, Default)]
pub struct Attachments(pub(crate) std::collections::BTreeMap<String, Vec<u8>>);

impl Attachments {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: impl Into<String>, bytes: Vec<u8>) {
        self.0.insert(name.into(), bytes);
    }

    pub fn names(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }

    #[must_use]
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
    pub(crate) sessions: Mutex<HashMap<SessionId, Arc<Mutex<Session>>>>,
    /// Closed because a panic poisoned their lock, so a command naming one
    /// is told why rather than that it never existed.
    pub(crate) poisoned: Mutex<HashSet<SessionId>>,
    pub(crate) next_id: AtomicU64,
    pub(crate) observers: Arc<Observers>,
    pub(crate) next_observer: AtomicU64,
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) kept_store: Arc<dyn KeptStore>,
    /// Milliseconds since the Unix epoch, for a version's time.
    pub(crate) clock: fn() -> u64,
    /// Told to clients with the session list; see [`mint_instance`].
    pub(crate) instance: String,
    pub(crate) version_limits: versions::VersionLimits,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) preview: Mutex<Option<PreviewRenderer>>,
}

/// Reads no lock, so it is safe while a session is held.
impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor")
            .field("instance", &self.instance)
            .field("next_id", &self.next_id)
            .field("version_limits", &self.version_limits)
            .finish_non_exhaustive()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn system_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// `SystemTime` panics on wasm32-unknown-unknown; the host supplies a clock.
#[cfg(target_arch = "wasm32")]
#[expect(
    clippy::missing_const_for_fn,
    reason = "shares the signature of the native clock, which cannot be const"
)]
fn system_clock() -> u64 {
    0
}

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(0);

/// Unique to this editor and the process it runs in, so a session id a
/// client remembers is never mistaken for one a later editor handed out.
/// Nothing about it is secret.
#[cfg(not(target_arch = "wasm32"))]
fn mint_instance() -> String {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!(
        "{:x}-{started:x}-{:x}",
        std::process::id(),
        NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Only the page that holds an in-tab editor reaches it.
#[cfg(target_arch = "wasm32")]
fn mint_instance() -> String {
    format!("tab-{:x}", NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed))
}

impl Editor {
    /// Filesystem rooted at the cwd natively, with reviews and versions in
    /// sidecars there; [`NoStorage`](crate::NoStorage) on wasm, where the
    /// host must use [`Editor::with_storage`].
    #[must_use]
    pub fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let editor = Self::with_storage(Arc::new(FileStorage::default()))
            .with_kept_store(Arc::new(SidecarKeptStore::default()));
        #[cfg(target_arch = "wasm32")]
        let editor = Self::with_storage(Arc::new(NoStorage));
        editor
    }

    /// Where reviews and versions are kept between runs; without this,
    /// nowhere. Model keys are the ones `storage` is given.
    pub fn with_kept_store(mut self, store: Arc<dyn KeptStore>) -> Self {
        self.kept_store = store;
        self
    }

    /// Milliseconds since the Unix epoch; a wasm host must supply one for
    /// versions to carry their time.
    pub fn with_clock(mut self, clock: fn() -> u64) -> Self {
        self.clock = clock;
        self
    }

    #[cfg(test)]
    pub(crate) const fn with_version_limits(mut self, versions: u64, bytes: u64) -> Self {
        self.version_limits = versions::VersionLimits { versions, bytes };
        self
    }

    pub fn with_storage(storage: Arc<dyn Storage>) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            poisoned: Mutex::new(HashSet::new()),
            next_id: AtomicU64::new(1),
            observers: Arc::default(),
            next_observer: AtomicU64::new(1),
            storage,
            kept_store: Arc::new(NoKeptStore),
            clock: system_clock,
            instance: mint_instance(),
            version_limits: versions::VersionLimits::default(),
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
            .and_then(|()| check_attachment_budget(&attachments))
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
    pub fn revision(&self, id: SessionId) -> Option<u64> {
        self.with_session(id, |s| Ok(s.rev)).ok()
    }

    /// The callback runs on the command's thread with no editor lock held.
    pub fn subscribe(&self, observer: Observer) -> Subscription {
        let id = self.next_observer.fetch_add(1, Ordering::Relaxed);
        lock(&self.observers).push((id, Arc::from(observer)));
        Subscription {
            observers: Arc::downgrade(&self.observers),
            id,
        }
    }

    /// The list is copied out before the first call so an observer may
    /// (un)subscribe re-entrantly without deadlocking.
    pub(crate) fn notify(&self, event: Event) {
        let observers: Vec<SharedObserver> = {
            let guard = lock(&self.observers);
            guard.iter().map(|(_, o)| o.clone()).collect()
        };
        for observer in observers {
            observer(&event);
        }
    }

    pub(crate) fn notify_model_changed(&self, session: SessionId, rev: u64) {
        self.notify(Event::ModelChanged { session, rev });
    }

    pub(crate) fn notify_sessions(&self) {
        self.notify(Event::SessionsChanged);
    }

    /// Leaves the dirty flag alone: the bytes have landed nowhere yet.
    pub fn save_bytes(&self, id: SessionId) -> Result<Vec<u8>, EditorError> {
        self.with_session(id, |s| Ok(s.model.to_clm_bytes()?))
    }

    #[cfg(test)]
    pub(crate) fn is_dirty(&self, id: SessionId) -> Result<bool, EditorError> {
        self.with_session(id, |s| Ok(s.dirty()))
    }

    pub fn with_model<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model) -> R,
    ) -> Result<R, EditorError> {
        self.with_session(id, |s| Ok(f(&s.model)))
    }

    #[cfg(test)]
    fn add_texture_bytes(
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

    #[cfg(test)]
    pub(crate) fn with_model_revision<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model, u64) -> R,
    ) -> Result<R, EditorError> {
        self.with_session(id, |s| Ok(f(&s.model, s.rev)))
    }

    pub fn with_puppet<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Model, &mut Puppet) -> R,
    ) -> Result<R, EditorError> {
        let session = self.session(id)?;
        let mut s = self.enter(id, &session)?;
        let (model, puppet) = s.puppet();
        Ok(f(model, puppet))
    }

    fn dispatch(
        &self,
        cmd: Command,
        attachments: &mut Attachments,
        payload: &mut Option<Payload>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let cmd = match SessionEdit::try_from(cmd) {
            Ok(SessionEdit {
                session,
                if_rev,
                op,
            }) => {
                return self.edit_session_captured(session, |s| {
                    s.check_revision(if_rev)?;
                    edit::apply(&mut s.model, op)
                });
            }
            Err(cmd) => cmd,
        };
        match cmd {
            Command::SessionNew { name, source } => self.create_session(name, source, attachments),
            Command::SessionOpen { path } => self.open_session(path),
            Command::SessionFork {
                session,
                if_rev,
                name,
            } => self.fork_session(session, if_rev, name),
            Command::ModelExport { session, if_rev } => self.export_model(session, if_rev, payload),
            Command::SessionList => self.list_sessions(),
            Command::SessionClose { session } => self.close_session(session),
            Command::Save { session, path } => self.save(session, path),
            Command::ExportManifest { session, path } => self.export_manifest(session, path),
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
            | Command::Extensions { session }
            | Command::ExtensionWarnings { session }
            | Command::HumanoidGet { session }
            | Command::ExpressionList { session }
            | Command::ClipList { session } => self.with_session_captured(session, |s| {
                query::check_revision(&cmd, s.rev)?;
                replica_query(&s.model, &cmd)
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
                self.with_session_captured(session, |s| {
                    s.check_revision(Some(if_rev))?;
                    let mut candidate = s.model.clone();
                    let results = edit::execute(&mut candidate, edits)?;
                    let changed = !s.model.authored_eq(&candidate)?;
                    Ok(ResponseBody::EditResults { changed, results })
                })
            }
            Command::NodeAdd {
                session,
                parent,
                kind,
                name,
                node: None,
            } => self.edit_session_captured(session, |s| {
                let node = s.add_node(
                    &parent,
                    None,
                    ModelNode::new(name.unwrap_or_else(|| default_name(kind)), make_kind(kind)),
                )?;
                Ok(ResponseBody::Node {
                    node,
                    dropped: Vec::new(),
                })
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
                        s.model.rename_slot(node, from, to.clone())?;
                    }
                }
                s.review.rename(&rename);
                Ok(ResponseBody::Empty)
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
                    let p = s
                        .model
                        .param(&param)
                        .ok_or_else(|| ModelError::UnknownParam(param.clone()))?;
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
                slot: None,
            } => self.edit_session_captured(session, |s| {
                let slot = s.slot_add_generated(&node)?;
                Ok(ResponseBody::Slot {
                    slot: SlotAddr { node, slot },
                })
            }),
            // Taken as a `SessionEdit` above.
            Command::NodeAdd { node: Some(_), .. }
            | Command::SlotAdd { slot: Some(_), .. }
            | Command::NodeSet { .. }
            | Command::NodeReparent { .. }
            | Command::NodeReorder { .. }
            | Command::MaskAdd { .. }
            | Command::MaskDelete { .. }
            | Command::MeshSet { .. }
            | Command::BindingKeyInsert { .. }
            | Command::BindingKeyDelete { .. }
            | Command::BindingKeyMove { .. }
            | Command::BindingCellsSet { .. }
            | Command::BindingCellsUnset { .. }
            | Command::BindingAdd { .. }
            | Command::BindingDelete { .. }
            | Command::BindingInterpolationSet { .. }
            | Command::SlotFill { .. }
            | Command::SlotClear { .. }
            | Command::SlotDelete { .. }
            | Command::WeldSet { .. }
            | Command::WeldDelete { .. } => Err(EditorError::BadRequest(format!(
                "`{}` is a batch edit",
                cmd.tag()
            ))),
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
            | Command::ReviewFetch { .. }
            | Command::ReviewAck { .. }
            | Command::ReviewClaim { .. }
            | Command::ReviewRelease { .. }) => self.review(command, attachments, payload),
            command @ (Command::VersionSave { .. }
            | Command::VersionList { .. }
            | Command::VersionGet { .. }
            | Command::VersionRestore { .. }
            | Command::VersionOpen { .. }) => self.version(command, payload),
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
                        ));
                    }
                    (ExtensionSet::Json { value }, None) => ExtensionValue::Json(value),
                    (ExtensionSet::Bytes, Some(bytes)) => ExtensionValue::Bytes(bytes.into()),
                    (ExtensionSet::Bytes, None) => {
                        return Err(EditorError::BadRequest(
                            "a bytes extension needs the `value` attachment".into(),
                        ));
                    }
                    (
                        ExtensionSet::Maintained {
                            version,
                            refs,
                            value,
                        },
                        attached,
                    ) => {
                        let payload = match (value, attached) {
                            (Some(value), None) => ExtensionPayload::Json(value),
                            (None, Some(bytes)) => ExtensionPayload::Bytes(bytes.into()),
                            (Some(_), Some(_)) => {
                                return Err(EditorError::BadRequest(
                                    "a maintained extension's payload is its inline `value` \
                                     or the `value` attachment, not both"
                                        .into(),
                                ));
                            }
                            (None, None) => {
                                return Err(EditorError::BadRequest(
                                    "a maintained extension needs an inline `value` or the \
                                     `value` attachment"
                                        .into(),
                                ));
                            }
                        };
                        let refs = refs
                            .into_iter()
                            .map(IdRef::try_from)
                            .collect::<Result<_, _>>()
                            .map_err(|e| EditorError::BadRequest(e.to_string()))?;
                        ExtensionValue::Maintained(RawExtension {
                            version,
                            refs,
                            payload,
                        })
                    }
                };
                self.edit_session_captured(session, move |s| {
                    s.model.set_extension(key.clone(), value)?;
                    Ok(ResponseBody::Empty)
                })
            }
            Command::HumanoidMap {
                session,
                control,
                param,
                stops,
                nonstandard,
            } => self.edit_session_captured(session, |s| {
                official::humanoid_map(&mut s.model, &control, &param, stops, nonstandard)
            }),
            Command::HumanoidUnmap { session, control } => self
                .edit_session_captured(session, |s| {
                    official::humanoid_unmap(&mut s.model, &control)
                }),
            Command::ExpressionSet {
                session,
                group,
                name,
                settings,
                nonstandard,
            } => self.edit_session_captured(session, |s| {
                official::expression_set(&mut s.model, group, &name, settings, nonstandard)
            }),
            Command::ExpressionDelete {
                session,
                group,
                name,
            } => self.edit_session_captured(session, |s| {
                official::expression_delete(&mut s.model, group, &name)
            }),
            Command::VisemeAddStandard { session, replace } => self
                .edit_session_captured(session, |s| {
                    official::viseme_add_standard(&mut s.model, replace)
                }),
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
        let pixels = u64::from(width) * u64::from(height);
        if pixels > MAX_PREVIEW_PIXELS {
            return Err(EditorError::Limit {
                resource: "preview_pixels",
                limit: MAX_PREVIEW_PIXELS,
                requested: pixels,
            });
        }
        // Explicit or the default, never the presence: a script's output must
        // not depend on what some tab last looked at.
        let framing = camera.map_or_else(
            || Framing::centered(DEFAULT_CAMERA_HEIGHT),
            |c| Framing {
                center: glam::Vec2::new(c.center[0], c.center[1]),
                height: c.height,
            },
        );
        let handle = self.session(session)?;
        // Cloned (shallow, same identity and generation) so the session lock
        // is not held across the GPU render.
        let (model, mut puppet, rev) = {
            let mut s = self.enter(session, &handle)?;
            let puppet = s.take_preview_puppet()?;
            (s.model.clone(), puppet, s.rev)
        };
        let pose: Pose = params.into_iter().map(|p| (p.param, p.value)).collect();

        let render_result = (|| {
            let mut prev = lock(&self.preview);
            if prev.is_none() {
                *prev = Some(PreviewRenderer::new()?);
            }
            let pr = prev.as_mut().ok_or(PreviewError::RendererUnavailable)?;
            let max_side = pr.max_texture_dimension();
            if width.max(height) > max_side {
                return Err(EditorError::Limit {
                    resource: "preview_dimension",
                    limit: max_side.into(),
                    requested: width.max(height).into(),
                });
            }
            Ok(pr.render_png(session, &model, &mut puppet, &pose, width, height, framing)?)
        })();

        if let Ok(mut s) = self.enter(session, &handle)
            && s.rev == rev
            && s.puppet.is_none()
        {
            s.puppet = Some(puppet);
            s.pending_preview_pose = None;
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

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
        if let Attachment::Fixed(name) = a
            && !attachments.names().contains(name)
        {
            return Err(EditorError::BadRequest(format!(
                "{} needs the attachment {name:?}",
                cmd.tag()
            )));
        }
    }
    Ok(())
}

fn check_attachment_budget(attachments: &Attachments) -> Result<(), EditorError> {
    let requested = attachments.0.values().fold(0u64, |total, bytes| {
        total.saturating_add(bytes.len() as u64)
    });
    let limit = LoadLimits::default().encoded_bytes;
    if requested > limit {
        return Err(EditorError::Limit {
            resource: "encoded_bytes",
            limit,
            requested,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
