//! Threads, agents and claims, and the deliveries between them.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use catchlight_core::{Model, ModelError};
use catchlight_editor_protocol::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EditorError;

use super::images::Image;

/// A `review watch` polls about once a second; a few missed beats is a gone
/// agent, not a slow one.
const LISTENING_FOR: Duration = Duration::from_secs(5);
/// An agent that claimed a thread and died must not hold it forever.
pub(super) const LEASE: Duration = Duration::from_secs(60 * 60);
/// How long an agent stays listed after it was last heard from.
const HEARD_FOR: Duration = LEASE;
const MAX_AGENTS: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 64;
const MAX_KEY_BYTES: usize = 128;
const MAX_RENAMES: usize = 256;

/// `Instant` panics on wasm32-unknown-unknown, where no agent can attach
/// anyway: there nothing is ever recent and nothing ever lapses.
#[derive(Debug, Clone, Copy)]
pub(super) struct Stamp(#[cfg(not(target_arch = "wasm32"))] pub(super) std::time::Instant);

impl Stamp {
    fn now() -> Self {
        Self(
            #[cfg(not(target_arch = "wasm32"))]
            std::time::Instant::now(),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn age(self) -> Option<Duration> {
        Some(self.0.elapsed())
    }

    #[cfg(target_arch = "wasm32")]
    fn age(self) -> Option<Duration> {
        None
    }

    fn within(self, window: Duration) -> bool {
        self.age().is_some_and(|age| age < window)
    }

    fn older_than(self, window: Duration) -> bool {
        self.age().is_some_and(|age| age >= window)
    }
}

#[derive(Debug, Clone)]
pub(super) struct Claim {
    holder: ReviewIdentity,
    pub(super) since: Stamp,
}

/// A retry key, kept as long as the comment it wrote. `digest` is what was
/// written under it, so a repeat with other content is told apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Keyed {
    comment: u64,
    key: String,
    digest: String,
}

#[derive(Debug, Clone)]
pub(super) struct Thread {
    pub(super) id: ThreadId,
    pub(super) target: ReviewTarget,
    pub(super) status: ThreadStatus,
    pub(super) pose: Vec<ParamPose>,
    pub(super) comments: Vec<ReviewComment>,
    pub(super) claim: Option<Claim>,
    pub(super) keys: Vec<Keyed>,
}

impl Thread {
    fn handed_over(&self) -> bool {
        self.status == ThreadStatus::Open
            && self
                .comments
                .iter()
                .any(|c| c.delivery == ReviewDelivery::Sent)
    }

    fn holder(&self) -> Option<&ReviewIdentity> {
        self.claim
            .as_ref()
            .filter(|c| !c.since.older_than(LEASE))
            .map(|c| &c.holder)
    }

    fn held_by_other(&self, agent: &str) -> bool {
        self.holder().is_some_and(|h| h.id != agent)
    }

    /// Another agent's live claim keeps new comments on its thread for it.
    fn waiting_for(&self, agent: &str) -> bool {
        self.handed_over() && !self.held_by_other(agent)
    }

    fn wire(&self, model: &Model) -> ReviewThread {
        ReviewThread {
            thread: self.id,
            target_missing: !exists(model, &self.target),
            target: self.target.clone(),
            status: self.status,
            pose: self.pose.clone(),
            comments: self.comments.clone(),
            holder: self.holder().cloned(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Agent {
    identity: ReviewIdentity,
    pub(super) heard: Stamp,
    pub(super) polled: Option<Stamp>,
}

impl Agent {
    fn listening(&self) -> bool {
        self.polled.is_some_and(|at| at.within(LISTENING_FOR))
    }

    fn listed(&self) -> bool {
        !self.heard.older_than(HEARD_FOR)
    }
}

/// What hearing from an agent says about whether it is waiting for comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    Polled,
    /// Leaves listening as the agent's own heartbeat last set it: a reply may
    /// come from another process while its watch runs.
    Acted,
    /// It took comments, so it is working, not waiting.
    Took,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ReviewState {
    pub(super) threads: Vec<Thread>,
    pub(super) last_thread: u64,
    pub(super) last_comment: u64,
    pub(super) last_image: u64,
    /// The highest image Id a comment ever held: what a kept review must not
    /// hand out again. Held images come and go without touching it.
    pub(super) last_claimed_image: u64,
    pub(super) images: BTreeMap<u64, Image>,
    /// Uploaded and not yet on a comment, oldest first.
    pub(super) held: VecDeque<u64>,
    /// Most recently heard from last.
    pub(super) agents: Vec<Agent>,
    /// What [`Self::follow_model`] walks to find where a renamed target went
    /// once history navigation moves the model under the threads.
    pub(super) renames: Vec<Rename>,
    /// Set by [`Self::rename`], which runs inside an edit and cannot notify.
    pub(super) touched: bool,
    /// Moves whenever what a [`Document`](super::document::Document) keeps
    /// may have changed, so a review already kept at this generation is not
    /// serialized again.
    pub(super) generation: u64,
}

fn exists(model: &Model, target: &ReviewTarget) -> bool {
    match target {
        ReviewTarget::Model => true,
        ReviewTarget::Node { node } => model.node(node).is_some(),
        ReviewTarget::Param { param } => model.param(param).is_some(),
        ReviewTarget::Texture { texture } => model.texture(texture).is_some(),
        ReviewTarget::Slot { node, slot } => model.slot(node, slot).is_some(),
    }
}

fn require(model: &Model, target: &ReviewTarget) -> Result<(), EditorError> {
    match target {
        ReviewTarget::Model => Ok(()),
        ReviewTarget::Node { node } => model
            .node(node)
            .map(drop)
            .ok_or_else(|| EditorError::NoNode(node.clone())),
        ReviewTarget::Param { param } => model
            .param(param)
            .map(drop)
            .ok_or_else(|| EditorError::NoParam(param.clone())),
        ReviewTarget::Texture { texture } => model
            .texture(texture)
            .map(drop)
            .ok_or_else(|| EditorError::NoTexture(texture.clone())),
        ReviewTarget::Slot { node, slot } => {
            if model.node(node).is_none() {
                return Err(EditorError::NoNode(node.clone()));
            }
            model.slot(node, slot).map(drop).ok_or_else(|| {
                EditorError::Edit(ModelError::UnknownSlot {
                    node: node.clone(),
                    slot: slot.clone(),
                })
            })
        }
    }
}

/// `target` as `rename` leaves it, run forward or (`back`) undone.
fn renamed(target: &ReviewTarget, rename: &Rename, back: bool) -> Option<ReviewTarget> {
    fn pick<'a, T>(from: &'a T, to: &'a T, back: bool) -> (&'a T, &'a T) {
        if back { (to, from) } else { (from, to) }
    }
    match (target, rename) {
        (ReviewTarget::Node { node }, Rename::Node { from, to }) => {
            let (old, new) = pick(from, to, back);
            (node == old).then(|| ReviewTarget::Node { node: new.clone() })
        }
        (ReviewTarget::Slot { node, slot }, Rename::Node { from, to }) => {
            let (old, new) = pick(from, to, back);
            (node == old).then(|| ReviewTarget::Slot {
                node: new.clone(),
                slot: slot.clone(),
            })
        }
        (ReviewTarget::Param { param }, Rename::Param { from, to }) => {
            let (old, new) = pick(from, to, back);
            (param == old).then(|| ReviewTarget::Param { param: new.clone() })
        }
        (ReviewTarget::Texture { texture }, Rename::Texture { from, to }) => {
            let (old, new) = pick(from, to, back);
            (texture == old).then(|| ReviewTarget::Texture {
                texture: new.clone(),
            })
        }
        (
            ReviewTarget::Slot { node, slot },
            Rename::Slot {
                node: part,
                from,
                to,
            },
        ) => {
            let (old, new) = pick(from, to, back);
            (node == part && slot == old).then(|| ReviewTarget::Slot {
                node: node.clone(),
                slot: new.clone(),
            })
        }
        _ => None,
    }
}

/// A comment as its author sent it.
pub(super) struct Draft {
    pub(super) author: ReviewAuthor,
    pub(super) body: String,
    pub(super) images: Vec<u64>,
    pub(super) key: Option<String>,
    pub(super) version: Option<u64>,
}

fn body_of(body: String, images: &[u64]) -> Result<String, EditorError> {
    let body = body.trim();
    if body.is_empty() && images.is_empty() {
        return Err(EditorError::BadRequest(
            "a comment needs a body or an image".into(),
        ));
    }
    Ok(body.to_owned())
}

fn bounded(what: &str, text: &str, max: usize) -> Result<String, EditorError> {
    let text = text.trim();
    if text.is_empty() || text.len() > max {
        return Err(EditorError::BadRequest(format!(
            "{what} is 1 to {max} bytes"
        )));
    }
    Ok(text.to_owned())
}

pub(super) fn identity(who: ReviewIdentity) -> Result<ReviewIdentity, EditorError> {
    Ok(ReviewIdentity {
        id: bounded("an agent id", &who.id, MAX_ID_BYTES)?,
        name: bounded("an agent name", &who.name, MAX_NAME_BYTES)?,
    })
}

pub(crate) fn author_of(author: ReviewAuthor) -> Result<ReviewAuthor, EditorError> {
    let optional = |what: &str, text: Option<String>, max: usize| {
        text.filter(|t| !t.trim().is_empty())
            .map(|t| bounded(what, &t, max))
            .transpose()
    };
    Ok(match author {
        ReviewAuthor::Human { id, name } => ReviewAuthor::Human {
            id: optional("a person's id", id, MAX_ID_BYTES)?,
            name: optional("a person's name", name, MAX_NAME_BYTES)?,
        },
        ReviewAuthor::Agent { id, name } => {
            let who = identity(ReviewIdentity { id, name })?;
            ReviewAuthor::Agent {
                id: who.id,
                name: who.name,
            }
        }
    })
}

/// Whether two comments came from the same writer, by id alone: a name may
/// change between them.
fn same_author(a: &ReviewAuthor, b: &ReviewAuthor) -> bool {
    match (a, b) {
        (ReviewAuthor::Human { id: a, .. }, ReviewAuthor::Human { id: b, .. }) => a == b,
        (ReviewAuthor::Agent { id: a, .. }, ReviewAuthor::Agent { id: b, .. }) => a == b,
        _ => false,
    }
}

fn agent_of(author: &ReviewAuthor) -> Option<ReviewIdentity> {
    match author {
        ReviewAuthor::Agent { id, name } => Some(ReviewIdentity {
            id: id.clone(),
            name: name.clone(),
        }),
        ReviewAuthor::Human { .. } => None,
    }
}

fn key_of(key: Option<String>) -> Result<Option<String>, EditorError> {
    match key {
        Some(key) if key.is_empty() || key.len() > MAX_KEY_BYTES => Err(EditorError::BadRequest(
            format!("a retry key is 1 to {MAX_KEY_BYTES} bytes"),
        )),
        key => Ok(key),
    }
}

/// A batch names the comments a fetch handed over; clients treat it as
/// opaque.
fn batch_of(comments: &[u64]) -> Option<String> {
    (!comments.is_empty()).then(|| {
        comments
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    })
}

fn comments_of(batch: &str) -> Result<Vec<u64>, EditorError> {
    batch
        .split('.')
        .map(|id| id.parse().ok())
        .collect::<Option<Vec<u64>>>()
        .ok_or_else(|| EditorError::BadRequest(format!("{batch:?} is not a fetched batch")))
}

impl ReviewState {
    pub(super) fn wire(&self, model: &Model, kept: Kept, kept_problem: Option<String>) -> Review {
        Review {
            threads: self.threads.iter().map(|t| t.wire(model)).collect(),
            agents: self.presence(),
            kept,
            kept_problem,
        }
    }

    pub(super) fn presence(&self) -> Vec<ReviewAgent> {
        self.agents
            .iter()
            .filter(|a| a.listed())
            .map(|a| ReviewAgent {
                id: a.identity.id.clone(),
                name: a.identity.name.clone(),
                listening: a.listening(),
            })
            .collect()
    }

    /// What clients see of agents: compared across a command to decide
    /// whether it changed the review.
    pub(super) fn attendance(&self) -> (Vec<ReviewAgent>, Vec<Option<String>>) {
        (
            self.presence(),
            self.threads
                .iter()
                .map(|t| t.holder().map(|h| h.id.clone()))
                .collect(),
        )
    }

    fn hear(&mut self, who: &ReviewIdentity, heard: Heard) {
        let was = self
            .agents
            .iter()
            .position(|a| a.identity.id == who.id)
            .map(|at| self.agents.remove(at));
        let polled = match heard {
            Heard::Polled => Some(Stamp::now()),
            Heard::Acted => was.and_then(|a| a.polled),
            Heard::Took => None,
        };
        self.agents.retain(Agent::listed);
        if self.agents.len() >= MAX_AGENTS {
            self.agents.remove(0);
        }
        self.agents.push(Agent {
            identity: who.clone(),
            heard: Stamp::now(),
            polled,
        });
    }

    /// Which targets the model no longer holds. Compared across an edit to
    /// decide whether clients must re-read.
    pub(crate) fn missing(&self, model: &Model) -> Vec<bool> {
        self.threads
            .iter()
            .map(|t| !exists(model, &t.target))
            .collect()
    }

    fn thread_mut(&mut self, id: ThreadId) -> Result<&mut Thread, EditorError> {
        self.threads
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(EditorError::NoThread(id))
    }

    fn comment(
        &mut self,
        author: ReviewAuthor,
        body: String,
        rev: u64,
        images: &[u64],
        version: Option<u64>,
    ) -> ReviewComment {
        self.last_comment += 1;
        ReviewComment {
            comment: self.last_comment,
            delivery: match author {
                ReviewAuthor::Human { .. } => ReviewDelivery::Unsent,
                ReviewAuthor::Agent { .. } => ReviewDelivery::Delivered,
            },
            author,
            body,
            rev,
            images: self.claim(images),
            version,
        }
    }

    /// What `draft` writes about `scope`, for telling a retry from a reuse
    /// of its key. Images are compared by their bytes: a retry uploads them
    /// again under new Ids.
    fn digest(&self, scope: &str, body: &str, images: &[u64], version: Option<u64>) -> String {
        let mut hash = Sha256::new();
        let version = version.map(|v| v.to_string()).unwrap_or_default();
        for part in [scope.as_bytes(), body.as_bytes(), version.as_bytes()] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part);
        }
        for id in images {
            let bytes = self
                .images
                .get(id)
                .and_then(|image| image.bytes.as_deref())
                .map_or(&[][..], Vec::as_slice);
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The thread `author` already wrote `key` on, when this is a repeat of
    /// that write; its fresh uploads are dropped.
    fn repeated(
        &mut self,
        model: &Model,
        author: &ReviewAuthor,
        key: Option<&str>,
        digest: &str,
        images: &[u64],
    ) -> Result<Option<ReviewThread>, EditorError> {
        let Some(key) = key else { return Ok(None) };
        let found = self.threads.iter().find_map(|thread| {
            thread
                .keys
                .iter()
                .filter(|keyed| keyed.key == key)
                .find(|keyed| {
                    thread
                        .comments
                        .iter()
                        .any(|c| c.comment == keyed.comment && same_author(&c.author, author))
                })
                .map(|keyed| (thread, keyed.digest == digest))
        });
        let Some((thread, same)) = found else {
            return Ok(None);
        };
        if !same {
            return Err(EditorError::KeyConflict(key.to_owned()));
        }
        let wire = thread.wire(model);
        self.held.retain(|id| !images.contains(id));
        for id in images {
            self.images.remove(id);
        }
        Ok(Some(wire))
    }

    /// The thread, and whether this wrote anything rather than repeat a
    /// keyed write.
    pub(super) fn open(
        &mut self,
        model: &Model,
        rev: u64,
        target: ReviewTarget,
        draft: Draft,
        pose: Vec<ParamPose>,
    ) -> Result<(ReviewThread, bool), EditorError> {
        let body = body_of(draft.body, &draft.images)?;
        let author = author_of(draft.author)?;
        let key = key_of(draft.key)?;
        self.check_images(&draft.images)?;
        let scope = format!(
            "open {}",
            serde_json::to_string(&target).unwrap_or_default()
        );
        let digest = self.digest(&scope, &body, &draft.images, draft.version);
        if let Some(thread) =
            self.repeated(model, &author, key.as_deref(), &digest, &draft.images)?
        {
            return Ok((thread, false));
        }
        require(model, &target)?;
        if let Some(agent) = agent_of(&author) {
            self.hear(&agent, Heard::Acted);
        }
        let comment = self.comment(author, body, rev, &draft.images, draft.version);
        self.last_thread += 1;
        let thread = Thread {
            id: ThreadId(self.last_thread),
            target,
            status: ThreadStatus::Open,
            pose,
            keys: key
                .map(|key| Keyed {
                    comment: comment.comment,
                    key,
                    digest,
                })
                .into_iter()
                .collect(),
            comments: vec![comment],
            claim: None,
        };
        let wire = thread.wire(model);
        self.threads.push(thread);
        Ok((wire, true))
    }

    /// As [`Self::open`].
    pub(super) fn reply(
        &mut self,
        model: &Model,
        rev: u64,
        id: ThreadId,
        draft: Draft,
    ) -> Result<(ReviewThread, bool), EditorError> {
        let body = body_of(draft.body, &draft.images)?;
        let author = author_of(draft.author)?;
        let key = key_of(draft.key)?;
        self.check_images(&draft.images)?;
        let digest = self.digest(
            &format!("reply {}", id.0),
            &body,
            &draft.images,
            draft.version,
        );
        if let Some(thread) =
            self.repeated(model, &author, key.as_deref(), &digest, &draft.images)?
        {
            return Ok((thread, false));
        }
        self.thread_mut(id)?;
        let agent = agent_of(&author);
        if let Some(agent) = &agent {
            self.hear(agent, Heard::Acted);
        }
        let comment = self.comment(author, body, rev, &draft.images, draft.version);
        let thread = self.thread_mut(id)?;
        if let Some(key) = key {
            thread.keys.push(Keyed {
                comment: comment.comment,
                key,
                digest,
            });
        }
        thread.comments.push(comment);
        match agent {
            None => thread.status = ThreadStatus::Open,
            Some(agent) => {
                if thread
                    .claim
                    .as_ref()
                    .is_some_and(|c| c.holder.id == agent.id)
                {
                    thread.claim = None;
                }
            }
        }
        Ok((thread.wire(model), true))
    }

    /// The reply and whether anything changed.
    pub(super) fn set_status(
        &mut self,
        model: &Model,
        id: ThreadId,
        status: ThreadStatus,
    ) -> Result<(ReviewThread, bool), EditorError> {
        let thread = self.thread_mut(id)?;
        let changed = thread.status != status;
        thread.status = status;
        if status == ThreadStatus::Resolved {
            thread.claim = None;
        }
        Ok((thread.wire(model), changed))
    }

    pub(super) fn delete_thread(&mut self, id: ThreadId) -> Result<(), EditorError> {
        let at = self
            .threads
            .iter()
            .position(|t| t.id == id)
            .ok_or(EditorError::NoThread(id))?;
        let thread = self.threads.remove(at);
        self.free(&thread.comments);
        Ok(())
    }

    pub(super) fn delete_comment(&mut self, id: ThreadId, comment: u64) -> Result<(), EditorError> {
        let thread = self.thread_mut(id)?;
        let at = thread
            .comments
            .iter()
            .position(|c| c.comment == comment)
            .ok_or(EditorError::NoComment(comment))?;
        let removed = thread.comments.remove(at);
        thread.keys.retain(|keyed| keyed.comment != comment);
        let emptied = thread.comments.is_empty();
        self.free(&[removed]);
        if emptied {
            self.delete_thread(id)?;
        }
        Ok(())
    }

    pub(super) fn submit(&mut self) -> bool {
        let mut changed = false;
        for thread in &mut self.threads {
            if thread.status != ThreadStatus::Open {
                continue;
            }
            for comment in &mut thread.comments {
                if comment.delivery == ReviewDelivery::Unsent {
                    comment.delivery = ReviewDelivery::Sent;
                    changed = true;
                }
            }
        }
        changed
    }

    pub(super) fn pending(&self, agent: &str) -> u32 {
        let count = self.threads.iter().filter(|t| t.waiting_for(agent)).count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    pub(super) fn poll(&mut self, who: &ReviewIdentity) -> (u32, Option<String>) {
        self.hear(who, Heard::Polled);
        (self.pending(&who.id), batch_of(&self.waiting(&who.id)))
    }

    /// The `sent` comments of the threads waiting for `agent`.
    fn waiting(&self, agent: &str) -> Vec<u64> {
        self.threads
            .iter()
            .filter(|t| t.waiting_for(agent))
            .flat_map(|t| &t.comments)
            .filter(|c| c.delivery == ReviewDelivery::Sent)
            .map(|c| c.comment)
            .collect()
    }

    /// The threads waiting for `who`, as they were handed over, and the batch
    /// that acknowledges their `sent` comments.
    pub(super) fn fetch(
        &mut self,
        model: &Model,
        who: &ReviewIdentity,
    ) -> (Vec<ReviewThread>, Option<String>) {
        self.hear(who, Heard::Acted);
        let threads = self
            .threads
            .iter()
            .filter(|t| t.waiting_for(&who.id))
            .map(|t| t.wire(model))
            .collect();
        (threads, batch_of(&self.waiting(&who.id)))
    }

    /// Refuses a stale batch whole. Otherwise delivers and claims for `who`
    /// each of its threads no other agent holds; on one another holds, the
    /// comments stay `sent` for that agent's next fetch.
    pub(super) fn ack(
        &mut self,
        who: &ReviewIdentity,
        batch: &str,
    ) -> Result<(Vec<ThreadId>, Vec<ReviewHeld>), EditorError> {
        let comments = comments_of(batch)?;
        let sent = |thread: &Thread, id: u64| {
            thread.status == ThreadStatus::Open
                && thread
                    .comments
                    .iter()
                    .any(|c| c.comment == id && c.delivery == ReviewDelivery::Sent)
        };
        if !comments
            .iter()
            .all(|id| self.threads.iter().any(|t| sent(t, *id)))
        {
            return Err(EditorError::BatchStale);
        }
        let mut claimed = Vec::new();
        let mut held = Vec::new();
        for thread in &mut self.threads {
            if !thread
                .comments
                .iter()
                .any(|c| comments.contains(&c.comment))
            {
                continue;
            }
            if let Some(holder) = thread.holder().filter(|h| h.id != who.id) {
                held.push(ReviewHeld {
                    thread: thread.id,
                    holder: holder.clone(),
                });
                continue;
            }
            for comment in &mut thread.comments {
                if comments.contains(&comment.comment) {
                    comment.delivery = ReviewDelivery::Delivered;
                }
            }
            thread.claim = Some(Claim {
                holder: who.clone(),
                since: Stamp::now(),
            });
            claimed.push(thread.id);
        }
        self.hear(who, Heard::Took);
        Ok((claimed, held))
    }

    pub(super) fn claim_threads(
        &mut self,
        who: &ReviewIdentity,
        threads: &[ThreadId],
    ) -> Result<(), EditorError> {
        if threads.is_empty() {
            return Err(EditorError::BadRequest("name a thread to claim".into()));
        }
        for id in threads {
            let thread = self
                .threads
                .iter()
                .find(|t| t.id == *id)
                .ok_or(EditorError::NoThread(*id))?;
            if let Some(holder) = thread.holder().filter(|h| h.id != who.id) {
                return Err(EditorError::Claimed {
                    thread: *id,
                    holder: holder.clone(),
                });
            }
        }
        for id in threads {
            self.thread_mut(*id)?.claim = Some(Claim {
                holder: who.clone(),
                since: Stamp::now(),
            });
        }
        self.hear(who, Heard::Acted);
        Ok(())
    }

    pub(super) fn release(
        &mut self,
        who: &ReviewIdentity,
        threads: &[ThreadId],
    ) -> Result<(), EditorError> {
        for id in threads {
            self.thread_mut(*id)?;
        }
        for id in threads {
            let thread = self.thread_mut(*id)?;
            if thread.claim.as_ref().is_some_and(|c| c.holder.id == who.id) {
                thread.claim = None;
            }
        }
        self.hear(who, Heard::Acted);
        Ok(())
    }

    pub(crate) fn take_touched(&mut self) -> bool {
        std::mem::take(&mut self.touched)
    }

    pub(crate) fn rename(&mut self, rename: &Rename) {
        for thread in &mut self.threads {
            if let Some(next) = renamed(&thread.target, rename, false) {
                thread.target = next;
                self.touched = true;
            }
        }
        if !self.threads.is_empty() {
            self.generation += 1;
            if self.renames.len() == MAX_RENAMES {
                self.renames.remove(0);
            }
            self.renames.push(rename.clone());
        }
    }

    /// After history navigation: a thread whose target is gone takes the Id
    /// a recorded rename, undone or redone, gives it, if the model holds that.
    pub(crate) fn follow_model(&mut self, model: &Model) -> bool {
        let mut moved = false;
        for thread in &mut self.threads {
            // A chain of renames is walked one link per pass.
            for _ in 0..self.renames.len() {
                if exists(model, &thread.target) {
                    break;
                }
                let next = self.renames.iter().rev().find_map(|rename| {
                    [true, false]
                        .into_iter()
                        .filter_map(|back| renamed(&thread.target, rename, back))
                        .find(|candidate| exists(model, candidate))
                });
                match next {
                    Some(next) => {
                        thread.target = next;
                        moved = true;
                    }
                    None => break,
                }
            }
        }
        if moved {
            self.generation += 1;
        }
        moved
    }
}
