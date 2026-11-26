//! An open session, and the locked reads and edits that publish its revisions.

use std::sync::{Arc, Mutex, MutexGuard};

use catchlight_core::id::{HexSource as _, SeededHex};
use catchlight_core::{Model, ModelNode, ModelParam, ModelTexture, Pose, Puppet};
use catchlight_editor_core::{HistoryLimits, HistoryNavigation, ModelHistory};
use catchlight_editor_protocol::*;

#[cfg(not(target_arch = "wasm32"))]
use crate::PreviewError;
use crate::editor::lock;
use crate::{Editor, EditorError, KeptStore, review, versions};

#[cfg(test)]
mod history_tests;

/// Fixed, so a given [`SessionId`] always draws the same sequence.
const ID_SEED: u32 = 0x1d5e_ed01;

/// Deterministic so a replayed script rebuilds a byte-identical model;
/// per-session so two open sessions do not mint the same Ids for the same
/// edits. The seed is drawn through a `SeededHex` so consecutive session ids
/// land far apart in the sequence rather than one Weyl step apart.
fn session_hex(id: SessionId) -> SeededHex {
    SeededHex::new(SeededHex::new(ID_SEED ^ id.0 as u32).next_bits())
}

pub(crate) struct Session {
    pub(crate) model: Model,
    pub(crate) hex: SeededHex,
    pub(crate) title: String,
    /// Opaque storage key this session was opened from or last saved to.
    pub(crate) file: Option<String>,
    pub(crate) rev: u64,
    pub(crate) saved_rev: u64,
    pub(crate) history: ModelHistory,
    /// Set while holding this handle before removing it from the registry.
    pub(crate) closed: bool,
    pub(crate) puppet: Option<Puppet>,
    /// Carries the preview pose across navigation, and while a renderer owns
    /// the Puppet outside the session lock.
    pub(crate) pending_preview_pose: Option<Pose>,
    pub(crate) presence: Option<Presence>,
    pub(crate) review: review::ReviewState,
    pub(crate) review_keeping: review::ReviewKeeping,
    pub(crate) versions: versions::Versions,
    /// Set on a read-only session showing a saved version.
    pub(crate) viewing: Option<VersionViewing>,
}

impl Session {
    pub(crate) fn new(id: SessionId, model: Model, title: String, file: Option<String>) -> Self {
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
            presence: None,
            review: review::ReviewState::default(),
            review_keeping: review::ReviewKeeping::default(),
            versions: versions::Versions::default(),
            viewing: None,
        }
    }

    pub(crate) fn writable(&self) -> Result<(), EditorError> {
        match self.viewing {
            Some(viewing) => Err(EditorError::ReadOnly(viewing)),
            None => Ok(()),
        }
    }

    /// For a thread that followed an edit, which cannot fail on the review's
    /// account. A write that is refused is retried by the next one, and a
    /// reopened review finds its targets through the renames it kept.
    pub(crate) fn keep_review(&mut self, store: &dyn KeptStore) {
        let _ = self
            .review_keeping
            .write(store, self.file.as_deref(), &self.review);
    }

    pub(crate) fn ensure_open(&self, id: SessionId) -> Result<(), EditorError> {
        if self.closed {
            return Err(EditorError::NoSession(id));
        }
        Ok(())
    }

    pub(crate) fn next_revision(&self) -> Result<u64, EditorError> {
        self.rev
            .checked_add(1)
            .ok_or(EditorError::RevisionExhausted)
    }

    pub(crate) fn check_revision(&self, expected: Option<u64>) -> Result<(), EditorError> {
        if expected.is_some_and(|expected| expected != self.rev) {
            return Err(EditorError::RevisionConflict);
        }
        Ok(())
    }

    /// `id: None` draws a free Id; the charset was validated at decode.
    pub(crate) fn add_node(
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

    pub(crate) fn add_param(
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

    pub(crate) fn add_texture(
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

    pub(crate) fn slot_add_generated(&mut self, node: &NodeId) -> Result<SlotId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.slot_add_generated(node, hex)?)
    }

    pub(crate) fn duplicate_subtree(&mut self, node: &NodeId) -> Result<NodeId, EditorError> {
        let Self { model, hex, .. } = self;
        Ok(model.duplicate_subtree(node, hex)?)
    }

    pub(crate) fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    pub(crate) fn puppet(&mut self) -> (&Model, &mut Puppet) {
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
    pub(crate) fn take_preview_puppet(&mut self) -> Result<Puppet, EditorError> {
        self.puppet();
        let puppet = self.puppet.take().ok_or(PreviewError::PuppetBuild)?;
        self.pending_preview_pose = Some(puppet.pose());
        Ok(puppet)
    }
}

/// A result stamped while its addressed session is still locked. Never recover
/// this revision by looking up the session after running callbacks or IO.
#[derive(Debug)]
pub(crate) struct Captured<T> {
    pub(crate) value: T,
    pub(crate) rev: Option<u64>,
}

impl<T> Captured<T> {
    pub(crate) fn at(value: T, rev: u64) -> Self {
        Self {
            value,
            rev: Some(rev),
        }
    }

    pub(crate) fn unscoped(value: T) -> Self {
        Self { value, rev: None }
    }
}

impl Editor {
    pub(crate) fn session(&self, id: SessionId) -> Result<Arc<Mutex<Session>>, EditorError> {
        let handle = lock(&self.sessions).get(&id).cloned();
        handle.ok_or_else(|| {
            if lock(&self.poisoned).contains(&id) {
                EditorError::SessionPoisoned(id)
            } else {
                EditorError::NoSession(id)
            }
        })
    }

    /// The open session, locked. A panic while it was locked may have left
    /// its model half-edited, so a poisoned one is closed for good instead.
    /// Never call this holding the registry lock.
    pub(crate) fn enter<'a>(
        &self,
        id: SessionId,
        handle: &'a Mutex<Session>,
    ) -> Result<MutexGuard<'a, Session>, EditorError> {
        match handle.lock() {
            Ok(session) => {
                session.ensure_open(id)?;
                Ok(session)
            }
            Err(poisoned) => {
                poisoned.into_inner().closed = true;
                lock(&self.poisoned).insert(id);
                if lock(&self.sessions).remove(&id).is_some() {
                    tracing::error!("closed session {} after an edit panicked", id.0);
                    self.notify_sessions();
                }
                Err(EditorError::SessionPoisoned(id))
            }
        }
    }

    pub(crate) fn with_session<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<R, EditorError> {
        self.with_session_captured(id, f)
            .map(|captured| captured.value)
    }

    pub(crate) fn with_session_captured<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<Captured<R>, EditorError> {
        let handle = self.session(id)?;
        let mut session = self.enter(id, &handle)?;
        let rev = session.rev;
        let result = f(&mut session);
        debug_assert_eq!(session.rev, rev, "read helper published a model edit");
        result.map(|value| Captured::at(value, rev))
    }

    #[cfg(test)]
    pub(crate) fn edit_session<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<R, EditorError> {
        self.edit_session_captured(id, f)
            .map(|captured| captured.value)
    }

    /// The single publication seam for authored edits; callbacks run only
    /// after unlocking. A failed or no-op edit restores the original model/rev.
    pub(crate) fn edit_session_captured<R>(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<R, EditorError>,
    ) -> Result<Captured<R>, EditorError> {
        let handle = self.session(id)?;
        let (result, moved, reviewed) = {
            let mut session = self.enter(id, &handle)?;
            session.writable()?;
            let before = session.rev;
            let snapshot = session.model.clone();
            let missing = session.review.missing(&session.model);
            let published = f(&mut session).and_then(|value| {
                if snapshot.authored_eq(&session.model)? {
                    return Ok((value, None));
                }
                let next = before
                    .checked_add(1)
                    .ok_or(EditorError::RevisionExhausted)?;
                let model = session.model.clone();
                session.history.record(model, next)?;
                Ok((value, Some(next)))
            });
            let result = match published {
                Ok((value, Some(next))) => {
                    session.rev = next;
                    Ok(Captured::at(value, next))
                }
                Ok((value, None)) => {
                    session.model = snapshot;
                    session.rev = before;
                    Ok(Captured::at(value, before))
                }
                Err(e) => {
                    session.model = snapshot;
                    session.rev = before;
                    Err(e)
                }
            };
            let moved = (session.rev != before).then_some(session.rev);
            let reviewed =
                session.review.take_touched() || missing != session.review.missing(&session.model);
            session.keep_review(&*self.kept_store);
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

    pub(crate) fn navigate_history(
        &self,
        id: SessionId,
        if_rev: u64,
        action: HistoryNavigation,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let handle = self.session(id)?;
        let (captured, changed, reviewed) = {
            let mut session = self.enter(id, &handle)?;
            session.writable()?;
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
            }
            let Session { review, model, .. } = &mut *session;
            let reviewed = review.follow_model(model) || missing != review.missing(model);
            session.keep_review(&*self.kept_store);
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
}
