//! A session's review threads. Kept beside the model, never in it: no
//! revision, no history entry, nothing in the model file. A session with a
//! file writes each change through to the editor's [`ReviewStore`].

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::time::Duration;

use catchlight_core::{Model, ModelError};
use catchlight_editor_protocol::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::kept_store::KeptStore;
use crate::{Attachments, Captured, Editor, EditorError, Payload};

/// A `review watch` polls about once a second; a few missed beats is a gone
/// agent, not a slow one.
const LISTENING_FOR: Duration = Duration::from_secs(5);
/// An agent that claimed a thread and died must not hold it forever.
const LEASE: Duration = Duration::from_secs(60 * 60);
/// How long an agent stays listed after it was last heard from.
const HEARD_FOR: Duration = LEASE;
const MAX_AGENTS: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 64;
const MAX_KEY_BYTES: usize = 128;
const MAX_RENAMES: usize = 256;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SESSION_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_COMMENT_IMAGES: usize = 4;
/// An upload whose comment never came must not pin memory for the session's
/// life.
const MAX_HELD_IMAGES: usize = 16;

/// `Instant` panics on wasm32-unknown-unknown, where no agent can attach
/// anyway: there nothing is ever recent and nothing ever lapses.
#[derive(Debug, Clone, Copy)]
struct Stamp(#[cfg(not(target_arch = "wasm32"))] std::time::Instant);

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
struct Claim {
    holder: ReviewIdentity,
    since: Stamp,
}

/// A retry key, kept as long as the comment it wrote. `digest` is what was
/// written under it, so a repeat with other content is told apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Keyed {
    comment: u64,
    key: String,
    digest: String,
}

#[derive(Debug, Clone)]
struct Thread {
    id: ThreadId,
    target: ReviewTarget,
    status: ThreadStatus,
    pose: Vec<ParamPose>,
    comments: Vec<ReviewComment>,
    claim: Option<Claim>,
    keys: Vec<Keyed>,
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
struct Agent {
    identity: ReviewIdentity,
    heard: Stamp,
    polled: Option<Stamp>,
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

#[derive(Debug, Clone)]
struct Image {
    media_type: &'static str,
    byte_length: usize,
    /// `None` for an image a kept review names and nobody has asked for yet.
    bytes: Option<Arc<Vec<u8>>>,
}

fn media_type_named(name: &str) -> Option<&'static str> {
    ["image/png", "image/jpeg", "image/webp"]
        .into_iter()
        .find(|known| *known == name)
}

/// What a [`ReviewStore`] knows an image by.
fn image_name(id: u64, media_type: &str) -> String {
    let ext = match media_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    };
    format!("{id}.{ext}")
}

/// Read from the magic bytes alone; the image is never decoded here.
fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ReviewState {
    threads: Vec<Thread>,
    last_thread: u64,
    last_comment: u64,
    last_image: u64,
    /// The highest image Id a comment ever held: what a kept review must not
    /// hand out again. Held images come and go without touching it.
    last_claimed_image: u64,
    images: BTreeMap<u64, Image>,
    /// Uploaded and not yet on a comment, oldest first.
    held: VecDeque<u64>,
    /// Most recently heard from last.
    agents: Vec<Agent>,
    /// What [`Self::follow_model`] walks to find where a renamed target went
    /// once history navigation moves the model under the threads.
    renames: Vec<Rename>,
    /// Set by [`Self::rename`], which runs inside an edit and cannot notify.
    touched: bool,
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
            model
                .slot(node, slot)
                .map(drop)
                .ok_or(EditorError::Edit(ModelError::UnknownSlot))
        }
    }
}

/// `target` as `rename` leaves it, run forward or (`back`) undone.
fn renamed(target: &ReviewTarget, rename: &Rename, back: bool) -> Option<ReviewTarget> {
    fn pick<'a, T>(from: &'a T, to: &'a T, back: bool) -> (&'a T, &'a T) {
        if back {
            (to, from)
        } else {
            (from, to)
        }
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
struct Draft {
    author: ReviewAuthor,
    body: String,
    images: Vec<u64>,
    key: Option<String>,
    version: Option<u64>,
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

fn identity(who: ReviewIdentity) -> Result<ReviewIdentity, EditorError> {
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
    fn wire(&self, model: &Model, kept: Kept, kept_problem: Option<String>) -> Review {
        Review {
            threads: self.threads.iter().map(|t| t.wire(model)).collect(),
            agents: self.presence(),
            kept,
            kept_problem,
        }
    }

    fn presence(&self) -> Vec<ReviewAgent> {
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
    fn attendance(&self) -> (Vec<ReviewAgent>, Vec<Option<String>>) {
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

    fn image_info(&self, id: u64) -> Result<ReviewImage, EditorError> {
        let image = self.images.get(&id).ok_or(EditorError::NoImage(id))?;
        Ok(ReviewImage {
            image: id,
            media_type: image.media_type.to_owned(),
            byte_length: u32::try_from(image.byte_length).unwrap_or(u32::MAX),
        })
    }

    fn hold_image(&mut self, bytes: Vec<u8>) -> Result<ReviewImage, EditorError> {
        let media_type = media_type(&bytes).ok_or_else(|| {
            EditorError::BadRequest("a review image is a PNG, JPEG or WebP file".into())
        })?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(EditorError::Limit {
                resource: "review_image_bytes",
                limit: MAX_IMAGE_BYTES as u64,
                requested: bytes.len() as u64,
            });
        }
        if self.held.len() == MAX_HELD_IMAGES {
            if let Some(oldest) = self.held.pop_front() {
                self.images.remove(&oldest);
            }
        }
        let total: usize = self.images.values().map(|i| i.byte_length).sum();
        if total + bytes.len() > MAX_SESSION_IMAGE_BYTES {
            return Err(EditorError::Limit {
                resource: "review_session_image_bytes",
                limit: MAX_SESSION_IMAGE_BYTES as u64,
                requested: (total + bytes.len()) as u64,
            });
        }
        self.last_image += 1;
        let id = self.last_image;
        self.images.insert(
            id,
            Image {
                media_type,
                byte_length: bytes.len(),
                bytes: Some(Arc::new(bytes)),
            },
        );
        self.held.push_back(id);
        self.image_info(id)
    }

    /// Refuses without changing anything; [`Self::claim`] cannot then fail.
    fn check_images(&self, images: &[u64]) -> Result<(), EditorError> {
        if images.len() > MAX_COMMENT_IMAGES {
            return Err(EditorError::Limit {
                resource: "review_comment_images",
                limit: MAX_COMMENT_IMAGES as u64,
                requested: images.len() as u64,
            });
        }
        for (at, id) in images.iter().enumerate() {
            if !self.held.contains(id) || images[..at].contains(id) {
                return Err(EditorError::NoImage(*id));
            }
        }
        Ok(())
    }

    fn claim(&mut self, images: &[u64]) -> Vec<ReviewImage> {
        self.held.retain(|id| !images.contains(id));
        if let Some(highest) = images.iter().max() {
            self.last_claimed_image = self.last_claimed_image.max(*highest);
        }
        images
            .iter()
            .filter_map(|id| self.image_info(*id).ok())
            .collect()
    }

    fn free(&mut self, comments: &[ReviewComment]) {
        for image in comments.iter().flat_map(|c| &c.images) {
            self.images.remove(&image.image);
        }
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
    fn open(
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
    fn reply(
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
    fn set_status(
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

    fn delete_thread(&mut self, id: ThreadId) -> Result<(), EditorError> {
        let at = self
            .threads
            .iter()
            .position(|t| t.id == id)
            .ok_or(EditorError::NoThread(id))?;
        let thread = self.threads.remove(at);
        self.free(&thread.comments);
        Ok(())
    }

    fn delete_comment(&mut self, id: ThreadId, comment: u64) -> Result<(), EditorError> {
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

    fn submit(&mut self) -> bool {
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

    fn pending(&self, agent: &str) -> u32 {
        let count = self.threads.iter().filter(|t| t.waiting_for(agent)).count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    fn poll(&mut self, who: &ReviewIdentity) -> (u32, Option<String>) {
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
    fn fetch(
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
    fn ack(
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

    fn claim_threads(
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

    fn release(&mut self, who: &ReviewIdentity, threads: &[ThreadId]) -> Result<(), EditorError> {
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
        moved
    }
}

const DOCUMENT_VERSION: u32 = 2;
const DOCUMENT: &str = "review.json";

fn kept_image(store: &dyn KeptStore, key: &str, name: &str) -> io::Result<Vec<u8>> {
    store.read(key, name)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is missing", store.place(key, name)),
        )
    })
}

/// The part of a review that outlives the session. Agents, claims and held
/// images are the session's own.
#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    last_thread: u64,
    last_comment: u64,
    last_image: u64,
    threads: Vec<DocumentThread>,
    /// Kept so a thread written against a rename the model file never saw
    /// still finds its target ([`ReviewState::follow_model`]).
    #[serde(default)]
    renames: Vec<Rename>,
}

#[derive(Serialize, Deserialize)]
struct DocumentThread {
    thread: ThreadId,
    target: ReviewTarget,
    status: ThreadStatus,
    #[serde(default)]
    pose: Vec<ParamPose>,
    comments: Vec<ReviewComment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    keys: Vec<Keyed>,
}

fn bad_document(why: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_string())
}

impl ReviewState {
    fn blank(&self) -> bool {
        self.threads.is_empty()
            && self.last_thread == 0
            && self.last_comment == 0
            && self.last_claimed_image == 0
    }

    fn document(&self) -> io::Result<Vec<u8>> {
        let document = Document {
            version: DOCUMENT_VERSION,
            last_thread: self.last_thread,
            last_comment: self.last_comment,
            last_image: self.last_claimed_image,
            threads: self
                .threads
                .iter()
                .map(|t| DocumentThread {
                    thread: t.id,
                    target: t.target.clone(),
                    status: t.status,
                    pose: t.pose.clone(),
                    comments: t.comments.clone(),
                    keys: t.keys.clone(),
                })
                .collect(),
            renames: self.renames.clone(),
        };
        serde_json::to_vec_pretty(&document).map_err(bad_document)
    }

    fn from_document(bytes: &[u8]) -> io::Result<Self> {
        #[derive(Deserialize)]
        struct Versioned {
            version: u32,
        }
        let Versioned { version } = serde_json::from_slice(bytes).map_err(bad_document)?;
        if version != DOCUMENT_VERSION {
            return Err(bad_document(format!(
                "review document version {version}; this editor reads version {DOCUMENT_VERSION}"
            )));
        }
        let document: Document = serde_json::from_slice(bytes).map_err(bad_document)?;
        let mut images = BTreeMap::new();
        for image in document
            .threads
            .iter()
            .flat_map(|t| &t.comments)
            .flat_map(|c| &c.images)
        {
            let media_type = media_type_named(&image.media_type).ok_or_else(|| {
                bad_document(format!(
                    "review image {} has type {:?}",
                    image.image, image.media_type
                ))
            })?;
            images.insert(
                image.image,
                Image {
                    media_type,
                    byte_length: image.byte_length as usize,
                    bytes: None,
                },
            );
        }
        Ok(Self {
            threads: document
                .threads
                .into_iter()
                .map(|t| Thread {
                    id: t.thread,
                    target: t.target,
                    status: t.status,
                    pose: t.pose,
                    comments: t.comments,
                    claim: None,
                    keys: t.keys,
                })
                .collect(),
            last_thread: document.last_thread,
            last_comment: document.last_comment,
            last_image: document.last_image,
            last_claimed_image: document.last_image,
            images,
            renames: document.renames,
            ..Self::default()
        })
    }

    /// Every image a comment holds, by the name a store keeps it under.
    fn claimed(&self) -> BTreeMap<u64, String> {
        self.images
            .iter()
            .filter(|(id, _)| !self.held.contains(id))
            .map(|(id, image)| (*id, image_name(*id, image.media_type)))
            .collect()
    }
}

/// What the store is known to hold for one session's review.
#[derive(Debug, Default)]
pub(crate) struct ReviewKeeping {
    /// The document as last read or written; `None` forces the next write.
    written: Option<Vec<u8>>,
    stored: BTreeMap<u64, String>,
    /// Set when the kept review could not be read. Nothing is written while
    /// it stands, so the file a person may still want is never replaced.
    unreadable: Option<String>,
}

impl ReviewKeeping {
    /// The review `key` was left with, and what is known of the store. A
    /// review that cannot be read never blocks opening its model.
    pub(crate) fn open(store: &dyn KeptStore, key: &str, model: &Model) -> (ReviewState, Self) {
        if !store.keeps() {
            return Default::default();
        }
        let read = store.read(key, DOCUMENT).and_then(|found| match found {
            Some(bytes) => ReviewState::from_document(&bytes)
                .map(|state| Some((state, bytes)))
                .map_err(|e| bad_document(format!("{}: {e}", store.place(key, DOCUMENT)))),
            None => Ok(None),
        });
        match read {
            Ok(None) => Default::default(),
            Ok(Some((mut state, bytes))) => {
                state.follow_model(model);
                let stored = state.claimed();
                (
                    state,
                    Self {
                        written: Some(bytes),
                        stored,
                        unreadable: None,
                    },
                )
            }
            Err(e) => (
                ReviewState::default(),
                Self {
                    unreadable: Some(e.to_string()),
                    ..Self::default()
                },
            ),
        }
    }

    pub(crate) fn kept(&self, store: &dyn KeptStore, key: Option<&str>) -> Kept {
        if !store.keeps() {
            Kept::Session
        } else if key.is_none() {
            Kept::Unsaved
        } else if self.unreadable.is_some() {
            Kept::Unreadable
        } else {
            Kept::Stored
        }
    }

    /// Brings the store to `next`, writing only when what it keeps differs.
    /// On an error the store still reads as it did: images are put before the
    /// document that names them and dropped only after one that does not.
    pub(crate) fn write(
        &mut self,
        store: &dyn KeptStore,
        key: Option<&str>,
        next: &ReviewState,
    ) -> io::Result<()> {
        let Some(key) = key else { return Ok(()) };
        if !store.keeps() || self.unreadable.is_some() {
            return Ok(());
        }
        if self.written.is_none() && next.blank() {
            return Ok(());
        }
        let document = next.document()?;
        if self.written.as_deref() == Some(document.as_slice()) {
            return Ok(());
        }
        let claimed = next.claimed();
        for (id, name) in &claimed {
            if self.stored.contains_key(id) {
                continue;
            }
            let bytes = next
                .images
                .get(id)
                .and_then(|image| image.bytes.as_ref())
                .ok_or_else(|| bad_document(format!("review image {id} has no bytes to keep")))?;
            store.write(key, name, bytes)?;
        }
        store.write(key, DOCUMENT, &document)?;
        for (id, name) in &self.stored {
            if !claimed.contains_key(id) {
                // An orphan file is harmless and the document no longer
                // names it; the comment's deletion already took effect.
                let _ = store.delete(key, name);
            }
        }
        self.stored = claimed;
        self.written = Some(document);
        Ok(())
    }

    /// The session's model now lives at `to`: read what only the store under
    /// `from` holds, then write the whole review under the new key.
    pub(crate) fn rekey(
        &mut self,
        store: &dyn KeptStore,
        from: Option<&str>,
        to: &str,
        review: &mut ReviewState,
    ) -> io::Result<()> {
        if !store.keeps() {
            return Ok(());
        }
        if let Some(from) = from {
            let held = review.held.clone();
            for (id, image) in &mut review.images {
                if image.bytes.is_none() && !held.contains(id) {
                    let bytes = kept_image(store, from, &image_name(*id, image.media_type))?;
                    image.bytes = Some(Arc::new(bytes));
                }
            }
        }
        *self = Self::default();
        self.write(store, Some(to), review)
    }
}

impl Editor {
    /// Versions are never removed, so one found here is there when the
    /// comment naming it lands.
    fn check_version(&self, session: SessionId, version: Option<u64>) -> Result<(), EditorError> {
        let Some(version) = version else {
            return Ok(());
        };
        self.with_session(session, |s| {
            if s.versions.has(version) {
                Ok(())
            } else {
                Err(EditorError::NoVersion(version))
            }
        })
    }

    /// Runs `f` under the session lock and, when it reports a change, tells
    /// observers once the lock is released.
    fn review_session(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut ReviewState, &Model, u64) -> Result<(ResponseBody, bool), EditorError>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let captured = self.with_session_captured(id, |s| {
            s.writable()?;
            let rev = s.rev;
            // The change is kept before it is published: a store that
            // refuses leaves the review as every client last read it.
            let mut next = s.review.clone();
            let attended = next.attendance();
            let (body, changed) = f(&mut next, &s.model, rev)?;
            let out = (body, changed || next.attendance() != attended);
            s.review_keeping
                .write(&*self.kept_store, s.file.as_deref(), &next)?;
            s.review = next;
            Ok(out)
        })?;
        let (body, changed) = captured.value;
        if changed {
            self.notify(Event::ReviewChanged { session: id });
        }
        Ok(Captured {
            value: body,
            rev: captured.rev,
        })
    }

    pub(crate) fn review(
        &self,
        command: Command,
        attachments: &mut Attachments,
        payload: &mut Option<Payload>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        match command {
            Command::ReviewImageAdd { session } => {
                let bytes = attachments.take("image").ok_or_else(|| {
                    EditorError::BadRequest(
                        "review_image_add needs the attachment \"image\"".into(),
                    )
                })?;
                self.review_session(session, |review, _, _| {
                    let image = review.hold_image(bytes)?;
                    Ok((ResponseBody::ReviewImage { image }, false))
                })
            }
            Command::ReviewImageGet { session, image } => {
                let captured = self.with_session_captured(session, |s| {
                    let info = s.review.image_info(image)?;
                    let stored = s
                        .review
                        .images
                        .get(&image)
                        .ok_or(EditorError::NoImage(image))?;
                    let bytes = match (&stored.bytes, s.file.as_deref()) {
                        (Some(bytes), _) => bytes.to_vec(),
                        (None, Some(key)) => kept_image(
                            &*self.kept_store,
                            key,
                            &image_name(image, stored.media_type),
                        )?,
                        (None, None) => return Err(EditorError::NoImage(image)),
                    };
                    Ok((info, stored.media_type, bytes))
                })?;
                let (image, content_type, bytes) = captured.value;
                *payload = Some(Payload {
                    content_type,
                    bytes,
                });
                Ok(Captured {
                    value: ResponseBody::ReviewImage { image },
                    rev: captured.rev,
                })
            }
            Command::ReviewGet { session } => self.with_session_captured(session, |s| {
                let kept = s.review_keeping.kept(&*self.kept_store, s.file.as_deref());
                let problem = (kept == Kept::Unreadable)
                    .then(|| s.review_keeping.unreadable.clone())
                    .flatten();
                Ok(ResponseBody::Review {
                    review: s.review.wire(&s.model, kept, problem),
                })
            }),
            Command::ReviewThreadAdd {
                session,
                target,
                author,
                body,
                pose,
                images,
                key,
                version,
            } => {
                self.check_version(session, version)?;
                self.review_session(session, |review, model, rev| {
                    let (thread, wrote) = review.open(
                        model,
                        rev,
                        target,
                        Draft {
                            author,
                            body,
                            images,
                            key,
                            version,
                        },
                        pose,
                    )?;
                    Ok((ResponseBody::ReviewThread { thread }, wrote))
                })
            }
            Command::ReviewThreadSet {
                session,
                thread,
                status,
            } => self.review_session(session, |review, model, _| {
                let (thread, changed) = review.set_status(model, thread, status)?;
                Ok((ResponseBody::ReviewThread { thread }, changed))
            }),
            Command::ReviewThreadDelete { session, thread } => {
                self.review_session(session, |review, _, _| {
                    review.delete_thread(thread)?;
                    Ok((ResponseBody::Empty, true))
                })
            }
            Command::ReviewCommentAdd {
                session,
                thread,
                author,
                body,
                images,
                key,
                version,
            } => {
                self.check_version(session, version)?;
                self.review_session(session, |review, model, rev| {
                    let (thread, wrote) = review.reply(
                        model,
                        rev,
                        thread,
                        Draft {
                            author,
                            body,
                            images,
                            key,
                            version,
                        },
                    )?;
                    Ok((ResponseBody::ReviewThread { thread }, wrote))
                })
            }
            Command::ReviewCommentDelete {
                session,
                thread,
                comment,
            } => self.review_session(session, |review, _, _| {
                review.delete_comment(thread, comment)?;
                Ok((ResponseBody::Empty, true))
            }),
            Command::ReviewSubmit { session } => self.review_session(session, |review, _, _| {
                let changed = review.submit();
                Ok((ResponseBody::Empty, changed))
            }),
            Command::ReviewPoll { session, agent } => {
                let agent = identity(agent)?;
                self.review_session(session, |review, _, _| {
                    let (pending, batch) = review.poll(&agent);
                    Ok((ResponseBody::ReviewPending { pending, batch }, false))
                })
            }
            Command::ReviewFetch { session, agent } => {
                let agent = identity(agent)?;
                self.review_session(session, |review, model, _| {
                    let (threads, batch) = review.fetch(model, &agent);
                    Ok((ResponseBody::ReviewFetched { threads, batch }, false))
                })
            }
            Command::ReviewAck {
                session,
                agent,
                batch,
            } => {
                let agent = identity(agent)?;
                self.review_session(session, |review, _, _| {
                    let (claimed, held) = review.ack(&agent, &batch)?;
                    Ok((ResponseBody::ReviewAcked { claimed, held }, true))
                })
            }
            Command::ReviewClaim {
                session,
                agent,
                threads,
            } => {
                let agent = identity(agent)?;
                self.review_session(session, |review, _, _| {
                    review.claim_threads(&agent, &threads)?;
                    Ok((
                        ResponseBody::ReviewClaimed {
                            threads,
                            lease_seconds: LEASE.as_secs() as u32,
                        },
                        false,
                    ))
                })
            }
            Command::ReviewRelease {
                session,
                agent,
                threads,
            } => {
                let agent = identity(agent)?;
                self.review_session(session, |review, _, _| {
                    review.release(&agent, &threads)?;
                    Ok((ResponseBody::Empty, false))
                })
            }
            other => Err(EditorError::BadRequest(format!(
                "{} is not a review command",
                other.tag()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use std::sync::{Arc, Mutex};

    use super::*;

    struct Fixture {
        editor: Editor,
        session: SessionId,
        events: Arc<Mutex<Vec<Event>>>,
        next: u64,
    }

    impl Fixture {
        fn new() -> Self {
            let editor = Editor::new();
            let session = match editor.handle(Request {
                id: 1,
                command: Command::SessionNew {
                    name: None,
                    source: None,
                },
            }) {
                Reply::Ok {
                    body: ResponseBody::Session { session },
                    ..
                } => session,
                other => panic!("{other:?}"),
            };
            let events = Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            editor.subscribe(Box::new(move |event| {
                sink.lock().unwrap().push(event.clone());
            }));
            Self {
                editor,
                session,
                events,
                next: 1,
            }
        }

        fn send(&mut self, command: Command) -> Reply {
            self.next += 1;
            self.editor.handle(Request {
                id: self.next,
                command,
            })
        }

        fn ok(&mut self, command: Command) -> ResponseBody {
            match self.send(command) {
                Reply::Ok { body, .. } => body,
                other => panic!("{other:?}"),
            }
        }

        fn refused(&mut self, command: Command) -> ErrorCode {
            match self.send(command) {
                Reply::Err { code, .. } => code,
                other => panic!("{other:?}"),
            }
        }

        fn rev(&mut self) -> u64 {
            let session = self.session;
            match self.send(Command::ReviewGet { session }) {
                Reply::Ok { rev: Some(rev), .. } => rev,
                other => panic!("{other:?}"),
            }
        }

        fn review(&mut self) -> Review {
            let session = self.session;
            match self.ok(Command::ReviewGet { session }) {
                ResponseBody::Review { review } => review,
                other => panic!("{other:?}"),
            }
        }

        fn review_events(&self) -> usize {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e, Event::ReviewChanged { .. }))
                .count()
        }

        fn group(&mut self, id: &str) -> NodeId {
            let node = NodeId::new(id).unwrap();
            let session = self.session;
            self.ok(Command::NodeAdd {
                session,
                parent: NodeId::new("root").unwrap(),
                kind: NodeKindArg::Group,
                name: None,
                node: Some(node.clone()),
            });
            node
        }

        fn fetch(&mut self, name: &str) -> (Vec<ReviewThread>, Option<String>) {
            let session = self.session;
            match self.ok(Command::ReviewFetch {
                session,
                agent: who(name),
            }) {
                ResponseBody::ReviewFetched { threads, batch } => (threads, batch),
                other => panic!("{other:?}"),
            }
        }

        fn ack(
            &mut self,
            name: &str,
            batch: &str,
        ) -> Result<(Vec<ThreadId>, Vec<ReviewHeld>), ErrorCode> {
            let session = self.session;
            match self.send(Command::ReviewAck {
                session,
                agent: who(name),
                batch: batch.into(),
            }) {
                Reply::Ok {
                    body: ResponseBody::ReviewAcked { claimed, held },
                    ..
                } => Ok((claimed, held)),
                Reply::Err { code, .. } => Err(code),
                other => panic!("{other:?}"),
            }
        }

        fn claim(&mut self, name: &str, threads: &[u64]) -> Reply {
            let session = self.session;
            self.send(Command::ReviewClaim {
                session,
                agent: who(name),
                threads: threads.iter().copied().map(ThreadId).collect(),
            })
        }

        fn holder(&mut self, thread: usize) -> Option<String> {
            self.review().threads[thread].holder.clone().map(|h| h.name)
        }

        /// Sends the person's comments and hands them to `name`.
        fn take(&mut self, name: &str) -> Vec<ReviewThread> {
            let session = self.session;
            self.ok(Command::ReviewSubmit { session });
            let (threads, batch) = self.fetch(name);
            self.ack(name, &batch.unwrap()).unwrap();
            threads
        }

        fn keyed(&mut self, author: ReviewAuthor, thread: u64, body: &str, key: &str) -> Reply {
            let session = self.session;
            self.send(Command::ReviewCommentAdd {
                session,
                thread: ThreadId(thread),
                author,
                body: body.into(),
                images: Vec::new(),
                key: Some(key.into()),
                version: None,
            })
        }

        fn open(&mut self, target: ReviewTarget, author: ReviewAuthor) -> ReviewThread {
            let session = self.session;
            match self.ok(Command::ReviewThreadAdd {
                session,
                target,
                author,
                body: "  look here \n".into(),
                pose: Vec::new(),
                images: Vec::new(),
                key: None,
                version: None,
            }) {
                ResponseBody::ReviewThread { thread } => thread,
                other => panic!("{other:?}"),
            }
        }
    }

    fn who(name: &str) -> ReviewIdentity {
        ReviewIdentity {
            id: format!("{name}:session"),
            name: name.into(),
        }
    }

    fn agent(name: &str) -> ReviewAuthor {
        let who = who(name);
        ReviewAuthor::Agent {
            id: who.id,
            name: who.name,
        }
    }

    fn human() -> ReviewAuthor {
        ReviewAuthor::Human {
            id: None,
            name: None,
        }
    }

    fn deliveries(thread: &ReviewThread) -> Vec<ReviewDelivery> {
        thread.comments.iter().map(|c| c.delivery).collect()
    }

    #[test]
    fn a_comment_travels_from_unsent_to_delivered_and_the_reply_ends_the_work() {
        let mut f = Fixture::new();
        let session = f.session;
        let rev = f.rev();

        let opened = f.open(ReviewTarget::Model, human());
        assert_eq!(opened.thread, ThreadId(1));
        assert_eq!(opened.comments[0].body, "look here");
        assert_eq!(opened.comments[0].comment, 1);
        assert_eq!(deliveries(&opened), [ReviewDelivery::Unsent]);

        assert!(matches!(
            f.ok(Command::ReviewPoll {
                session,
                agent: who("claude")
            }),
            ResponseBody::ReviewPending {
                pending: 0,
                batch: None
            }
        ));
        f.ok(Command::ReviewSubmit { session });
        assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Sent]);
        let polled = match f.ok(Command::ReviewPoll {
            session,
            agent: who("claude"),
        }) {
            ResponseBody::ReviewPending { pending: 1, batch } => batch,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            f.review().agents,
            [ReviewAgent {
                id: "claude:session".into(),
                name: "claude".into(),
                listening: true
            }]
        );

        let (fetched, batch) = f.fetch("claude");
        assert_eq!(polled, batch, "a poll names the batch a fetch would take");
        assert_eq!(fetched.len(), 1);
        assert_eq!(deliveries(&fetched[0]), [ReviewDelivery::Sent]);
        assert_eq!(fetched[0].holder, None);
        assert_eq!(
            f.fetch("claude"),
            (fetched, batch.clone()),
            "a fetch takes nothing"
        );
        assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Sent]);

        let (claimed, held) = f.ack("claude", batch.as_deref().unwrap()).unwrap();
        assert_eq!((claimed, held), (vec![ThreadId(1)], Vec::new()));
        let review = f.review();
        assert_eq!(deliveries(&review.threads[0]), [ReviewDelivery::Delivered]);
        assert_eq!(review.threads[0].holder, Some(who("claude")));
        assert!(
            !review.agents[0].listening,
            "it took work, so it is not waiting"
        );
        assert_eq!(f.fetch("claude"), (Vec::new(), None));

        let replied = match f.ok(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: agent("claude"),
            body: "done".into(),
            images: Vec::new(),
            key: None,
            version: None,
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(replied.holder, None);
        assert_eq!(replied.comments[1].comment, 2);
        assert_eq!(
            deliveries(&replied),
            [ReviewDelivery::Delivered, ReviewDelivery::Delivered]
        );

        assert_eq!(f.rev(), rev, "a review moves no revision");
        assert!(matches!(
            f.ok(Command::Status { session }),
            ResponseBody::Status { status } if !status.dirty
        ));
        assert_eq!(
            f.refused(Command::Undo {
                session,
                if_rev: rev
            }),
            ErrorCode::NothingToUndo
        );
    }

    #[test]
    fn every_change_is_announced_and_a_steady_heartbeat_is_not() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        assert_eq!(f.review_events(), 1);
        f.review();
        assert_eq!(f.review_events(), 1);

        let poll = Command::ReviewPoll {
            session,
            agent: who("claude"),
        };
        f.ok(poll.clone());
        assert_eq!(f.review_events(), 2);
        f.ok(poll.clone());
        f.ok(poll);
        assert_eq!(f.review_events(), 2);

        f.ok(Command::ReviewSubmit { session });
        assert_eq!(f.review_events(), 3);
        f.ok(Command::ReviewSubmit { session });
        assert_eq!(f.review_events(), 3, "nothing was left to send");

        f.ok(Command::ReviewThreadSet {
            session,
            thread: ThreadId(1),
            status: ThreadStatus::Resolved,
        });
        assert_eq!(f.review_events(), 4);
        f.ok(Command::ReviewThreadDelete {
            session,
            thread: ThreadId(1),
        });
        assert_eq!(f.review_events(), 5);
        assert!(f
            .events
            .lock()
            .unwrap()
            .iter()
            .all(|e| !matches!(e, Event::ModelChanged { .. })));
    }

    #[test]
    fn a_target_must_exist_and_a_body_must_say_something() {
        let mut f = Fixture::new();
        let session = f.session;
        let add = |target| Command::ReviewThreadAdd {
            session,
            target,
            author: human(),
            body: "x".into(),
            pose: Vec::new(),
            images: Vec::new(),
            key: None,
            version: None,
        };
        let node = NodeId::new("nope").unwrap();
        assert_eq!(
            f.refused(add(ReviewTarget::Node { node: node.clone() })),
            ErrorCode::NoNode
        );
        assert_eq!(
            f.refused(add(ReviewTarget::Param {
                param: ParamId::new("nope").unwrap()
            })),
            ErrorCode::NoParam
        );
        assert_eq!(
            f.refused(add(ReviewTarget::Texture {
                texture: TexId::new("nope").unwrap()
            })),
            ErrorCode::NoTexture
        );
        let group = f.group("g");
        assert_eq!(
            f.refused(add(ReviewTarget::Slot {
                node: group,
                slot: SlotId::new("nope").unwrap()
            })),
            ErrorCode::UnknownSlot
        );
        assert_eq!(
            f.refused(Command::ReviewThreadAdd {
                session,
                target: ReviewTarget::Model,
                author: human(),
                body: " \n".into(),
                pose: Vec::new(),
                images: Vec::new(),
                key: None,
                version: None,
            }),
            ErrorCode::BadRequest
        );
        assert_eq!(
            f.refused(Command::ReviewCommentAdd {
                session,
                thread: ThreadId(9),
                author: human(),
                body: "x".into(),
                images: Vec::new(),
                key: None,
                version: None,
            }),
            ErrorCode::NoThread
        );
        assert_eq!(
            f.refused(Command::ReviewPoll {
                session,
                agent: ReviewIdentity {
                    id: " ".into(),
                    name: "claude".into()
                }
            }),
            ErrorCode::BadRequest
        );
        assert!(f.review().threads.is_empty());
    }

    #[test]
    fn deleting_the_only_comment_deletes_the_thread_and_ids_are_not_reused() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        assert_eq!(
            f.refused(Command::ReviewCommentDelete {
                session,
                thread: ThreadId(1),
                comment: 7
            }),
            ErrorCode::NoComment
        );
        f.ok(Command::ReviewCommentDelete {
            session,
            thread: ThreadId(1),
            comment: 1,
        });
        assert!(f.review().threads.is_empty());
        assert_eq!(
            f.refused(Command::ReviewThreadDelete {
                session,
                thread: ThreadId(1)
            }),
            ErrorCode::NoThread
        );
        let next = f.open(ReviewTarget::Model, agent("claude"));
        assert_eq!(next.thread, ThreadId(2));
        assert_eq!(next.comments[0].comment, 2);
        assert_eq!(deliveries(&next), [ReviewDelivery::Delivered]);
    }

    #[test]
    fn a_resolved_thread_is_not_handed_over_and_a_human_reply_reopens_it() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        f.ok(Command::ReviewThreadSet {
            session,
            thread: ThreadId(1),
            status: ThreadStatus::Resolved,
        });
        f.ok(Command::ReviewSubmit { session });
        assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Unsent]);

        let reopened = match f.ok(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: human(),
            body: "still wrong".into(),
            images: Vec::new(),
            key: None,
            version: None,
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(reopened.status, ThreadStatus::Open);

        f.take("claude");
        assert_eq!(f.holder(0).as_deref(), Some("claude"));
        let resolved = match f.ok(Command::ReviewThreadSet {
            session,
            thread: ThreadId(1),
            status: ThreadStatus::Resolved,
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(resolved.holder, None);
    }

    #[test]
    fn a_deleted_target_reads_missing_until_undo_brings_it_back() {
        let mut f = Fixture::new();
        let session = f.session;
        let node = f.group("g");
        f.open(ReviewTarget::Node { node: node.clone() }, human());
        let before = f.review_events();

        f.ok(Command::NodeDelete { session, node });
        assert!(f.review().threads[0].target_missing);
        assert_eq!(f.review_events(), before + 1);

        let rev = f.rev();
        f.ok(Command::Undo {
            session,
            if_rev: rev,
        });
        assert!(!f.review().threads[0].target_missing);
        assert_eq!(f.review_events(), before + 2);
    }

    #[test]
    fn a_thread_follows_a_rename_through_undo_and_redo() {
        let mut f = Fixture::new();
        let session = f.session;
        let from = f.group("g");
        let to = NodeId::new("torso").unwrap();
        f.open(ReviewTarget::Node { node: from.clone() }, human());

        f.ok(Command::RenameId {
            session,
            rename: Rename::Node {
                from: from.clone(),
                to: to.clone(),
            },
        });
        let thread = &f.review().threads[0];
        assert_eq!(thread.target, ReviewTarget::Node { node: to.clone() });
        assert!(!thread.target_missing);

        let rev = f.rev();
        f.ok(Command::Undo {
            session,
            if_rev: rev,
        });
        let thread = &f.review().threads[0];
        assert_eq!(thread.target, ReviewTarget::Node { node: from });
        assert!(!thread.target_missing);

        let rev = f.rev();
        f.ok(Command::Redo {
            session,
            if_rev: rev,
        });
        let thread = &f.review().threads[0];
        assert_eq!(thread.target, ReviewTarget::Node { node: to });
        assert!(!thread.target_missing);
    }

    #[test]
    fn a_fork_starts_with_no_review() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        let rev = f.rev();
        let fork = match f.ok(Command::SessionFork {
            session,
            if_rev: rev,
            name: None,
        }) {
            ResponseBody::SessionFork { session, .. } => session,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            f.ok(Command::ReviewGet { session: fork }),
            ResponseBody::Review { review } if review.threads.is_empty()
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_silent_agent_stops_listening_and_a_stale_claim_lapses() {
        let mut state = ReviewState::default();
        let model = Model::new();
        state
            .open(
                &model,
                0,
                ReviewTarget::Model,
                Draft {
                    author: human(),
                    body: "x".into(),
                    images: Vec::new(),
                    key: None,
                    version: None,
                },
                Vec::new(),
            )
            .unwrap();
        state.submit();
        assert_eq!(state.poll(&who("claude")).0, 1);
        let Some(long_ago) = std::time::Instant::now().checked_sub(LEASE) else {
            return;
        };
        state.agents[0].polled = Some(Stamp(long_ago));
        assert!(!state.presence()[0].listening);

        state.claim_threads(&who("claude"), &[ThreadId(1)]).unwrap();
        assert_eq!(state.pending(&who("codex").id), 0, "held for claude");
        state.threads[0].claim.as_mut().unwrap().since = Stamp(long_ago);
        assert_eq!(
            state.wire(&model, Kept::Session, None).threads[0].holder,
            None
        );
        assert_eq!(
            state.pending(&who("codex").id),
            1,
            "a lapsed claim frees it"
        );

        state.agents[0].heard = Stamp(long_ago);
        assert!(state.presence().is_empty(), "nothing heard for an hour");
    }

    #[test]
    fn a_fetch_is_acknowledged_whole_or_found_stale() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        f.open(ReviewTarget::Model, human());
        f.ok(Command::ReviewSubmit { session });

        let (first, batch) = f.fetch("claude");
        assert_eq!(first.len(), 2);
        let (again, also) = f.fetch("codex");
        assert_eq!((again, &also), (first, &batch), "both see what waits");
        let events = f.review_events();
        f.ack("claude", batch.as_deref().unwrap()).unwrap();
        assert_eq!(f.review_events(), events + 1);
        assert_eq!(
            f.ack("codex", also.as_deref().unwrap()),
            Err(ErrorCode::BatchStale),
            "claude took them first"
        );

        f.open(ReviewTarget::Model, human());
        f.ok(Command::ReviewSubmit { session });
        let (_, batch) = f.fetch("claude");
        f.ok(Command::ReviewThreadDelete {
            session,
            thread: ThreadId(3),
        });
        assert_eq!(
            f.ack("claude", batch.as_deref().unwrap()),
            Err(ErrorCode::BatchStale)
        );
        assert_eq!(f.ack("claude", "x"), Err(ErrorCode::BadRequest));
    }

    #[test]
    fn a_claim_keeps_a_thread_for_one_agent_until_its_reply() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        f.open(ReviewTarget::Model, human());
        let taken = f.take("claude");
        assert_eq!(taken.len(), 2);
        assert_eq!(f.holder(0).as_deref(), Some("claude"));

        let refused = f.claim("codex", &[2, 1]);
        assert!(
            matches!(
                &refused,
                Reply::Err { code: ErrorCode::ThreadClaimed, message, .. }
                    if message.contains("claude (claude:session)")
            ),
            "{refused:?}"
        );
        assert!(matches!(
            f.claim("claude", &[1]),
            Reply::Ok {
                body: ResponseBody::ReviewClaimed {
                    lease_seconds: 3600,
                    ..
                },
                ..
            }
        ));

        f.keyed(human(), 1, "and the other ear", "k");
        f.ok(Command::ReviewSubmit { session });
        assert!(f.fetch("codex").0.is_empty(), "claude holds thread 1");
        assert!(matches!(
            f.ok(Command::ReviewPoll {
                session,
                agent: who("codex")
            }),
            ResponseBody::ReviewPending {
                pending: 0,
                batch: None
            }
        ));
        let (mine, _) = f.fetch("claude");
        assert_eq!(mine.len(), 1);

        f.keyed(agent("codex"), 1, "not mine", "a");
        assert_eq!(
            f.holder(0).as_deref(),
            Some("claude"),
            "only its own reply releases"
        );
        f.keyed(agent("claude"), 1, "done", "b");
        assert_eq!(f.holder(0), None);

        f.ok(Command::ReviewRelease {
            session,
            agent: who("codex"),
            threads: vec![ThreadId(2)],
        });
        assert_eq!(
            f.holder(1).as_deref(),
            Some("claude"),
            "not codex's to release"
        );
        f.ok(Command::ReviewRelease {
            session,
            agent: who("claude"),
            threads: vec![ThreadId(2)],
        });
        assert_eq!(f.holder(1), None);
        assert!(matches!(f.claim("codex", &[2]), Reply::Ok { .. }));
        assert_eq!(f.holder(1).as_deref(), Some("codex"));
        assert!(matches!(
            f.claim("codex", &[9]),
            Reply::Err {
                code: ErrorCode::NoThread,
                ..
            }
        ));

        let names: Vec<String> = f.review().agents.into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["claude", "codex"]);
    }

    #[test]
    fn an_ack_leaves_a_thread_claimed_meanwhile_to_its_holder() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        f.open(ReviewTarget::Model, human());
        f.ok(Command::ReviewSubmit { session });
        let (_, batch) = f.fetch("codex");
        assert!(matches!(f.claim("claude", &[1]), Reply::Ok { .. }));

        let (claimed, held) = f.ack("codex", batch.as_deref().unwrap()).unwrap();
        assert_eq!(claimed, [ThreadId(2)]);
        assert_eq!(
            held,
            [ReviewHeld {
                thread: ThreadId(1),
                holder: who("claude"),
            }]
        );
        let review = f.review();
        assert_eq!(deliveries(&review.threads[0]), [ReviewDelivery::Sent]);
        assert_eq!(deliveries(&review.threads[1]), [ReviewDelivery::Delivered]);

        let (theirs, batch) = f.fetch("claude");
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].thread, ThreadId(1));
        f.ack("claude", batch.as_deref().unwrap()).unwrap();
        assert_eq!(
            deliveries(&f.review().threads[0]),
            [ReviewDelivery::Delivered]
        );
    }

    #[test]
    fn a_repeated_key_writes_once_and_a_reused_one_is_refused() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, human());
        let first = f.keyed(agent("claude"), 1, "done", "k1");
        let again = f.keyed(agent("claude"), 1, "done", "k1");
        let (Reply::Ok { body: first, .. }, Reply::Ok { body: again, .. }) = (first, again) else {
            panic!("a repeat is answered, not refused");
        };
        assert!(matches!((&first, &again), (
            ResponseBody::ReviewThread { thread: a },
            ResponseBody::ReviewThread { thread: b },
        ) if a == b && b.comments.len() == 2));

        assert!(matches!(
            f.keyed(agent("claude"), 1, "something else", "k1"),
            Reply::Err {
                code: ErrorCode::KeyConflict,
                ..
            }
        ));
        assert!(
            matches!(f.keyed(agent("codex"), 1, "done", "k1"), Reply::Ok { .. }),
            "a key is the author's own"
        );
        assert_eq!(f.review().threads[0].comments.len(), 3);

        let open = |key: &str| Command::ReviewThreadAdd {
            session,
            target: ReviewTarget::Model,
            author: agent("claude"),
            body: "a question".into(),
            pose: Vec::new(),
            images: Vec::new(),
            key: Some(key.into()),
            version: None,
        };
        let thread = |body| match body {
            ResponseBody::ReviewThread { thread } => thread.thread,
            other => panic!("{other:?}"),
        };
        let opened = thread(f.ok(open("t")));
        assert_eq!(thread(f.ok(open("t"))), opened);
        assert_eq!(f.review().threads.len(), 2);
        assert_eq!(
            f.refused(open("k1")),
            ErrorCode::KeyConflict,
            "k1 wrote a reply"
        );
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-not-really-a-png";

    impl Fixture {
        fn with(&mut self, command: Command, name: &str, bytes: &[u8]) -> (Reply, Option<Payload>) {
            self.next += 1;
            let mut attachments = Attachments::none();
            attachments.insert(name, bytes.to_vec());
            self.editor.handle_with(
                Request {
                    id: self.next,
                    command,
                },
                attachments,
            )
        }

        fn upload(&mut self, bytes: &[u8]) -> Reply {
            let session = self.session;
            self.with(Command::ReviewImageAdd { session }, "image", bytes)
                .0
        }

        fn image(&mut self, bytes: &[u8]) -> ReviewImage {
            match self.upload(bytes) {
                Reply::Ok {
                    body: ResponseBody::ReviewImage { image },
                    ..
                } => image,
                other => panic!("{other:?}"),
            }
        }

        fn bytes(&mut self, image: u64) -> Result<Payload, ErrorCode> {
            self.next += 1;
            let session = self.session;
            match self.editor.handle_with(
                Request {
                    id: self.next,
                    command: Command::ReviewImageGet { session, image },
                },
                Attachments::none(),
            ) {
                (Reply::Ok { .. }, Some(payload)) => Ok(payload),
                (Reply::Err { code, .. }, None) => Err(code),
                other => panic!("{other:?}"),
            }
        }

        fn say(&mut self, images: Vec<u64>, body: &str) -> Reply {
            let session = self.session;
            self.send(Command::ReviewThreadAdd {
                session,
                target: ReviewTarget::Model,
                author: human(),
                body: body.into(),
                pose: Vec::new(),
                images,
                key: None,
                version: None,
            })
        }
    }

    fn limit(reply: Reply) -> String {
        match reply {
            Reply::Err {
                code: ErrorCode::LimitExceeded,
                limit: Some(limit),
                ..
            } => limit.resource,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_image_is_typed_by_its_bytes_and_anything_else_is_refused() {
        let mut f = Fixture::new();
        assert_eq!(f.image(PNG).media_type, "image/png");
        assert_eq!(
            f.image(&[0xff, 0xd8, 0xff, 0xe0, 1]).media_type,
            "image/jpeg"
        );
        let webp = f.image(b"RIFF\x04\0\0\0WEBPVP8 ");
        assert_eq!((webp.image, webp.media_type.as_str()), (3, "image/webp"));
        assert!(matches!(
            f.upload(b"GIF89a"),
            Reply::Err {
                code: ErrorCode::BadRequest,
                ..
            }
        ));
        assert_eq!(f.review_events(), 0, "a held image is nobody's news yet");
        assert_eq!(f.rev(), 0);
    }

    #[test]
    fn an_image_rides_on_one_comment_and_reads_back_byte_for_byte() {
        let mut f = Fixture::new();
        let image = f.image(PNG);
        assert_eq!(image.byte_length as usize, PNG.len());
        let thread = match f.say(vec![image.image], "  ") {
            Reply::Ok {
                body: ResponseBody::ReviewThread { thread },
                ..
            } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(thread.comments[0].body, "");
        assert_eq!(thread.comments[0].images, vec![image.clone()]);
        let payload = f.bytes(image.image).unwrap();
        assert_eq!(
            (payload.content_type, payload.bytes.as_slice()),
            ("image/png", PNG)
        );

        assert!(matches!(
            f.say(vec![image.image], "again"),
            Reply::Err {
                code: ErrorCode::NoImage,
                ..
            }
        ));
        assert!(matches!(
            f.say(Vec::new(), " "),
            Reply::Err {
                code: ErrorCode::BadRequest,
                ..
            }
        ));
    }

    #[test]
    fn a_refused_comment_leaves_its_images_held() {
        let mut f = Fixture::new();
        let image = f.image(PNG).image;
        assert!(matches!(
            f.say(vec![image, 99], "x"),
            Reply::Err {
                code: ErrorCode::NoImage,
                ..
            }
        ));
        assert!(matches!(
            f.say(vec![image, image], "x"),
            Reply::Err {
                code: ErrorCode::NoImage,
                ..
            }
        ));
        let session = f.session;
        assert_eq!(
            f.refused(Command::ReviewCommentAdd {
                session,
                thread: ThreadId(9),
                author: human(),
                body: "x".into(),
                images: vec![image],
                key: None,
                version: None,
            }),
            ErrorCode::NoThread
        );
        assert!(matches!(f.say(vec![image], "x"), Reply::Ok { .. }));
    }

    #[test]
    fn image_limits_name_their_budget() {
        let mut f = Fixture::new();
        let mut big = PNG.to_vec();
        big.resize(MAX_IMAGE_BYTES + 1, 0);
        assert_eq!(limit(f.upload(&big)), "review_image_bytes");
        let five: Vec<u64> = (0..5).map(|_| f.image(PNG).image).collect();
        assert_eq!(limit(f.say(five, "x")), "review_comment_images");

        big.truncate(MAX_IMAGE_BYTES);
        let mut state = ReviewState::default();
        let fits = MAX_SESSION_IMAGE_BYTES / MAX_IMAGE_BYTES;
        for _ in 0..fits {
            let id = state.hold_image(big.clone()).unwrap().image;
            // Claimed, so the held-image cap does not make room.
            state.claim(&[id]);
        }
        assert!(matches!(
            state.hold_image(big),
            Err(EditorError::Limit {
                resource: "review_session_image_bytes",
                ..
            })
        ));
    }

    #[test]
    fn a_retry_with_images_is_told_apart_by_their_bytes() {
        let mut f = Fixture::new();
        let session = f.session;
        let reply = |f: &mut Fixture, image: u64| {
            f.send(Command::ReviewCommentAdd {
                session,
                thread: ThreadId(1),
                author: agent("claude"),
                body: "after".into(),
                images: vec![image],
                key: Some("k".into()),
                version: None,
            })
        };
        f.open(ReviewTarget::Model, human());
        let first = f.image(PNG).image;
        assert!(matches!(reply(&mut f, first), Reply::Ok { .. }));
        let again = f.image(PNG).image;
        assert!(matches!(reply(&mut f, again), Reply::Ok { .. }));
        assert_eq!(f.review().threads[0].comments.len(), 2);
        assert_eq!(
            f.bytes(again).unwrap_err(),
            ErrorCode::NoImage,
            "a repeat's upload is dropped"
        );
        let other = f.image(b"\x89PNG\r\n\x1a\nother").image;
        assert!(matches!(
            reply(&mut f, other),
            Reply::Err {
                code: ErrorCode::KeyConflict,
                ..
            }
        ));
    }

    #[test]
    fn deleting_a_comment_or_thread_frees_its_images() {
        let mut f = Fixture::new();
        let session = f.session;
        let first = f.image(PNG).image;
        let second = f.image(PNG).image;
        assert!(matches!(f.say(vec![first], "one"), Reply::Ok { .. }));
        f.ok(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: human(),
            body: "two".into(),
            images: vec![second],
            key: None,
            version: None,
        });
        f.ok(Command::ReviewCommentDelete {
            session,
            thread: ThreadId(1),
            comment: 2,
        });
        assert_eq!(f.bytes(second).unwrap_err(), ErrorCode::NoImage);
        assert!(f.bytes(first).is_ok());
        f.ok(Command::ReviewThreadDelete {
            session,
            thread: ThreadId(1),
        });
        assert_eq!(f.bytes(first).unwrap_err(), ErrorCode::NoImage);
    }

    #[test]
    fn an_image_nobody_claims_is_dropped_as_newer_ones_arrive() {
        let mut f = Fixture::new();
        let kept = f.image(PNG).image;
        assert!(matches!(f.say(vec![kept], "x"), Reply::Ok { .. }));
        let ids: Vec<u64> = (0..=MAX_HELD_IMAGES).map(|_| f.image(PNG).image).collect();
        assert_eq!(f.bytes(ids[0]).unwrap_err(), ErrorCode::NoImage);
        assert!(f.bytes(ids[1]).is_ok());
        assert!(f.bytes(kept).is_ok(), "a claimed image is not held");
        assert_eq!(*ids.last().unwrap(), kept + MAX_HELD_IMAGES as u64 + 1);
    }

    /// A store that is not the filesystem, as a cloud or browser one would be.
    #[derive(Debug, Default)]
    struct MemoryStore {
        documents: Mutex<BTreeMap<String, Vec<u8>>>,
        images: Mutex<BTreeMap<(String, String), Vec<u8>>>,
        saves: std::sync::atomic::AtomicUsize,
        refuse: std::sync::atomic::AtomicBool,
    }

    impl MemoryStore {
        fn check(&self) -> io::Result<()> {
            if self.refuse.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(io::Error::other("store is down"));
            }
            Ok(())
        }

        fn saves(&self) -> usize {
            self.saves.load(std::sync::atomic::Ordering::Relaxed)
        }

        fn image_names(&self, model: &str) -> Vec<String> {
            self.images
                .lock()
                .unwrap()
                .keys()
                .filter(|(m, _)| m == model)
                .map(|(_, name)| name.clone())
                .collect()
        }
    }

    impl KeptStore for MemoryStore {
        fn read(&self, model: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
            self.check()?;
            Ok(if name == DOCUMENT {
                self.documents.lock().unwrap().get(model).cloned()
            } else {
                self.images
                    .lock()
                    .unwrap()
                    .get(&(model.to_owned(), name.to_owned()))
                    .cloned()
            })
        }

        fn write(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
            self.check()?;
            if name == DOCUMENT {
                self.saves
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.documents
                    .lock()
                    .unwrap()
                    .insert(model.into(), bytes.to_vec());
            } else {
                self.images
                    .lock()
                    .unwrap()
                    .insert((model.into(), name.into()), bytes.to_vec());
            }
            Ok(())
        }

        fn delete(&self, model: &str, name: &str) -> io::Result<()> {
            self.check()?;
            self.images
                .lock()
                .unwrap()
                .remove(&(model.to_owned(), name.to_owned()));
            Ok(())
        }
    }

    /// Model bytes in memory, so a test's sessions have keys without files.
    #[derive(Debug, Default)]
    struct MemoryModels(Mutex<BTreeMap<String, Vec<u8>>>);

    impl crate::Storage for MemoryModels {
        fn read(&self, key: &str) -> io::Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, key.to_owned()))
        }

        fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
            self.0.lock().unwrap().insert(key.into(), bytes.to_vec());
            Ok(())
        }
    }

    impl Fixture {
        fn on(editor: Editor, session: SessionId) -> Self {
            let events = Arc::new(Mutex::new(Vec::new()));
            let sink = events.clone();
            editor.subscribe(Box::new(move |event| {
                sink.lock().unwrap().push(event.clone());
            }));
            Self {
                editor,
                session,
                events,
                next: 1,
            }
        }

        /// A new session on an editor that keeps reviews in `store`.
        fn keeping(models: &Arc<MemoryModels>, store: &Arc<MemoryStore>) -> Self {
            let editor = Editor::with_storage(models.clone()).with_kept_store(store.clone());
            let session = match editor.handle(Request {
                id: 1,
                command: Command::SessionNew {
                    name: None,
                    source: None,
                },
            }) {
                Reply::Ok {
                    body: ResponseBody::Session { session },
                    ..
                } => session,
                other => panic!("{other:?}"),
            };
            Self::on(editor, session)
        }

        /// The model at `key`, opened by a fresh editor on the same stores.
        fn reopened(models: &Arc<MemoryModels>, store: &Arc<MemoryStore>, key: &str) -> Self {
            let editor = Editor::with_storage(models.clone()).with_kept_store(store.clone());
            let session = match editor.handle(Request {
                id: 1,
                command: Command::SessionOpen { path: key.into() },
            }) {
                Reply::Ok {
                    body: ResponseBody::Session { session },
                    ..
                } => session,
                other => panic!("{other:?}"),
            };
            Self::on(editor, session)
        }

        fn save(&mut self, key: &str) -> Reply {
            let session = self.session;
            self.send(Command::Save {
                session,
                path: Some(key.into()),
            })
        }

        fn thread_with_image(&mut self, bytes: &[u8]) -> ReviewThread {
            let image = self.image(bytes).image;
            match self.say(vec![image], "see this") {
                Reply::Ok {
                    body: ResponseBody::ReviewThread { thread },
                    ..
                } => thread,
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_kept_review_is_there_when_another_editor_opens_the_model() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        let session = f.session;
        assert_eq!(f.review().kept, Kept::Unsaved);
        assert!(matches!(f.save("m.clm"), Reply::Ok { .. }));
        assert_eq!(f.review().kept, Kept::Stored);

        let first = f.thread_with_image(PNG);
        let second = f.open(ReviewTarget::Model, human());
        f.take("claude");
        assert!(matches!(
            f.keyed(agent("claude"), second.thread.0, "on it", "retry"),
            Reply::Ok { .. }
        ));
        f.ok(Command::ReviewThreadSet {
            session,
            thread: second.thread,
            status: ThreadStatus::Resolved,
        });
        let before = f.review();

        let mut g = Fixture::reopened(&models, &store, "m.clm");
        let after = g.review();
        assert_eq!(after.kept, Kept::Stored);
        assert_eq!(
            after.agents,
            [],
            "an agent belongs to the session it joined"
        );
        assert_eq!(after.threads.len(), 2);
        for (was, now) in before.threads.iter().zip(&after.threads) {
            assert_eq!(was.thread, now.thread);
            assert_eq!(was.status, now.status);
            assert_eq!(was.comments, now.comments);
            assert_eq!(
                now.holder, None,
                "a claim belongs to the session that took it"
            );
        }
        let image = first.comments[0].images[0].image;
        assert_eq!(g.bytes(image).unwrap().bytes, PNG);
        assert!(
            matches!(
                g.keyed(agent("claude"), second.thread.0, "on it", "retry"),
                Reply::Ok { body: ResponseBody::ReviewThread { thread }, .. } if thread.comments.len() == 2
            ),
            "a retry key outlives the editor"
        );

        let next = g.thread_with_image(PNG);
        assert_eq!(next.thread, ThreadId(3), "thread Ids continue");
        assert_eq!(next.comments[0].comment, 4, "comment Ids continue");
        assert_eq!(
            next.comments[0].images[0].image,
            image + 1,
            "image Ids continue"
        );
    }

    #[test]
    fn a_store_that_refuses_leaves_the_review_as_it_was() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        let session = f.session;
        f.save("m.clm");
        let thread = f.thread_with_image(PNG);
        let held = f.image(PNG).image;
        let before = f.review();
        let events = f.review_events();

        store
            .refuse
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            f.say(vec![held], "lost?"),
            Reply::Err {
                code: ErrorCode::Io,
                ..
            }
        ));
        assert_eq!(
            f.refused(Command::ReviewThreadDelete {
                session,
                thread: thread.thread
            }),
            ErrorCode::Io
        );
        assert_eq!(f.review(), before);
        assert_eq!(f.review_events(), events);

        store
            .refuse
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(
            matches!(f.say(vec![held], "kept"), Reply::Ok { .. }),
            "the refused comment's image is still held"
        );
        assert_eq!(
            Fixture::reopened(&models, &store, "m.clm")
                .review()
                .threads
                .len(),
            2
        );
    }

    #[test]
    fn only_a_change_to_what_is_kept_is_written() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        let session = f.session;
        f.save("m.clm");
        f.ok(Command::ReviewPoll {
            session,
            agent: who("claude"),
        });
        f.review();
        assert_eq!(store.saves(), 0, "an empty review is not written");

        f.open(ReviewTarget::Model, human());
        assert_eq!(store.saves(), 1);
        f.image(PNG);
        f.ok(Command::ReviewPoll {
            session,
            agent: who("claude"),
        });
        f.ok(Command::ReviewFetch {
            session,
            agent: who("claude"),
        });
        f.save("m.clm");
        assert_eq!(
            store.saves(),
            1,
            "presence, a held image and a re-save keep nothing new"
        );
        assert!(
            store.image_names("m.clm").is_empty(),
            "a held image is not kept"
        );

        f.ok(Command::ReviewSubmit { session });
        assert_eq!(store.saves(), 2);
    }

    #[test]
    fn deleting_a_comment_drops_its_kept_images() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        let session = f.session;
        f.save("m.clm");
        let thread = f.thread_with_image(PNG);
        f.thread_with_image(PNG);
        assert_eq!(store.image_names("m.clm"), ["1.png", "2.png"]);

        f.ok(Command::ReviewThreadDelete {
            session,
            thread: thread.thread,
        });
        assert_eq!(store.image_names("m.clm"), ["2.png"]);
    }

    #[test]
    fn a_review_follows_its_model_to_a_new_key() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        let thread = f.thread_with_image(PNG);
        assert_eq!(
            store.saves(),
            0,
            "a model with no file keeps its review in memory"
        );
        let events = f.review_events();

        assert!(matches!(f.save("a.clm"), Reply::Ok { .. }));
        assert_eq!(f.review_events(), events + 1, "how long it is kept changed");
        assert_eq!(store.image_names("a.clm"), ["1.png"]);

        // Reopened, the image is only in the store: saving elsewhere must
        // still carry it.
        let mut g = Fixture::reopened(&models, &store, "a.clm");
        assert!(matches!(g.save("b.clm"), Reply::Ok { .. }));
        let mut h = Fixture::reopened(&models, &store, "b.clm");
        assert_eq!(h.review().threads[0].comments, thread.comments);
        assert_eq!(
            h.bytes(thread.comments[0].images[0].image).unwrap().bytes,
            PNG
        );
        assert_eq!(
            Fixture::reopened(&models, &store, "a.clm")
                .review()
                .threads
                .len(),
            1,
            "the review left at the old key is untouched"
        );
    }

    #[test]
    fn a_save_whose_review_is_refused_still_saved_the_model() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        f.open(ReviewTarget::Model, human());
        store
            .refuse
            .store(true, std::sync::atomic::Ordering::Relaxed);
        match f.save("m.clm") {
            Reply::Err {
                code: ErrorCode::Io,
                message,
                ..
            } => assert!(message.contains("saved the model to m.clm"), "{message}"),
            other => panic!("{other:?}"),
        }
        assert!(models.0.lock().unwrap().contains_key("m.clm"));

        store
            .refuse
            .store(false, std::sync::atomic::Ordering::Relaxed);
        f.open(ReviewTarget::Model, human());
        assert_eq!(
            Fixture::reopened(&models, &store, "m.clm")
                .review()
                .threads
                .len(),
            2,
            "the next change writes everything the refused save did not"
        );
    }

    #[test]
    fn a_review_that_cannot_be_read_is_left_alone() {
        let (models, store): (Arc<MemoryModels>, Arc<MemoryStore>) = Default::default();
        Fixture::keeping(&models, &store).save("m.clm");
        for bad in [&b"{ not json"[..], br#"{"version": 99, "threads": []}"#] {
            store
                .documents
                .lock()
                .unwrap()
                .insert("m.clm".into(), bad.to_vec());
            let mut f = Fixture::reopened(&models, &store, "m.clm");
            let review = f.review();
            assert_eq!(review.kept, Kept::Unreadable);
            assert!(review.kept_problem.is_some());
            assert!(review.threads.is_empty());

            f.open(ReviewTarget::Model, human());
            f.save("m.clm");
            assert_eq!(
                f.review().threads.len(),
                1,
                "comments still work, in memory"
            );
            assert_eq!(store.documents.lock().unwrap()["m.clm"], bad);

            assert!(matches!(f.save("copy.clm"), Reply::Ok { .. }));
            assert_eq!(f.review().kept, Kept::Stored, "a new key has no bad file");
        }
    }

    #[test]
    fn a_kept_thread_finds_a_target_renamed_after_the_last_save() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        let session = f.session;
        let node = f.group("arm");
        f.save("m.clm");
        f.open(ReviewTarget::Node { node: node.clone() }, human());
        let renamed = NodeId::new("limb").unwrap();
        f.ok(Command::RenameId {
            session,
            rename: Rename::Node {
                from: node.clone(),
                to: renamed,
            },
        });

        // The file still calls it `arm`; the kept thread says `limb`.
        let mut g = Fixture::reopened(&models, &store, "m.clm");
        let thread = &g.review().threads[0];
        assert_eq!(thread.target, ReviewTarget::Node { node });
        assert!(!thread.target_missing);
    }

    #[test]
    fn an_editor_without_a_review_store_says_reviews_last_for_the_session() {
        let models: Arc<MemoryModels> = Default::default();
        let editor = Editor::with_storage(models.clone());
        let session = match editor.handle(Request {
            id: 1,
            command: Command::SessionNew {
                name: None,
                source: None,
            },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        let mut f = Fixture::on(editor, session);
        f.thread_with_image(PNG);
        assert!(matches!(f.save("m.clm"), Reply::Ok { .. }));
        assert_eq!(f.review().kept, Kept::Session);
    }

    #[test]
    fn the_sidecar_is_a_directory_beside_the_model() {
        use crate::{FileStorage, SidecarKeptStore};
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("cl-review-sidecar-{}-{nth}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        let editor = || {
            Editor::with_storage(Arc::new(FileStorage::new(&dir)))
                .with_kept_store(Arc::new(SidecarKeptStore::new(&dir)))
        };
        let first = editor();
        let session = match first.handle(Request {
            id: 1,
            command: Command::SessionNew {
                name: None,
                source: None,
            },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        let mut f = Fixture::on(first, session);
        f.save("models/m.clm");
        let thread = f.thread_with_image(PNG);

        let sidecar = dir.join("models/m.clm.kept");
        let mut names: Vec<String> = std::fs::read_dir(&sidecar)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["1.png", "review.json"]);
        assert_eq!(std::fs::read(sidecar.join("1.png")).unwrap(), PNG);

        let second = editor();
        let session = match second.handle(Request {
            id: 1,
            command: Command::SessionOpen {
                path: "models/m.clm".into(),
            },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        let mut g = Fixture::on(second, session);
        assert_eq!(g.review().threads[0].comments, thread.comments);
        assert_eq!(g.bytes(1).unwrap().bytes, PNG);
        g.ok(Command::ReviewThreadDelete {
            session,
            thread: thread.thread,
        });
        assert!(!sidecar.join("1.png").exists());

        std::fs::write(sidecar.join("review.json"), b"torn").unwrap();
        let third = editor();
        let session = match third.handle(Request {
            id: 1,
            command: Command::SessionOpen {
                path: "models/m.clm".into(),
            },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        let mut h = Fixture::on(third, session);
        let review = h.review();
        assert_eq!(review.kept, Kept::Unreadable);
        assert!(
            review.kept_problem.unwrap().contains("review.json"),
            "the problem names the file to look at"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_user_store_keeps_one_directory_per_model_and_prunes_deleted_ones() {
        use crate::{FileStorage, UserKeptStore};
        use std::time::Duration;
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("cl-review-user-{}-{nth}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let models = dir.join("models");
        let reviews = dir.join("reviews");
        std::fs::create_dir_all(&models).unwrap();
        let store = UserKeptStore::new(&models, &reviews);
        let editor = || {
            Editor::with_storage(Arc::new(FileStorage::new(&models)))
                .with_kept_store(Arc::new(store.clone()))
        };
        let open = |editor: Editor, path: &str| {
            let session = match editor.handle(Request {
                id: 1,
                command: Command::SessionOpen { path: path.into() },
            }) {
                Reply::Ok {
                    body: ResponseBody::Session { session },
                    ..
                } => session,
                other => panic!("{other:?}"),
            };
            Fixture::on(editor, session)
        };
        let listing = |dir: &std::path::Path| {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        let first = editor();
        let session = match first.handle(Request {
            id: 1,
            command: Command::SessionNew {
                name: None,
                source: None,
            },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        let mut f = Fixture::on(first, session);
        f.save("m.clm");
        let thread = f.thread_with_image(PNG);

        assert_eq!(listing(&models), ["m.clm"], "nothing beside the model");
        let ids = listing(&reviews);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].len(), 32);
        let kept = reviews.join(&ids[0]);
        assert_eq!(listing(&kept), ["1.png", "model", "review.json"]);
        assert_eq!(std::fs::read(kept.join("1.png")).unwrap(), PNG);
        let m = std::fs::canonicalize(models.join("m.clm")).unwrap();
        assert_eq!(
            std::fs::read_to_string(kept.join("model")).unwrap(),
            m.to_str().unwrap()
        );
        assert!(f.review().kept_problem.is_none());

        // A relative key and the absolute path reach the same review.
        for path in ["m.clm", m.to_str().unwrap()] {
            let mut g = open(editor(), path);
            assert_eq!(g.review().threads[0].comments, thread.comments);
            assert_eq!(g.bytes(1).unwrap().bytes, PNG);
        }
        assert_eq!(listing(&reviews), ids);

        // Another model gets its own directory.
        f.save("n.clm");
        let both = listing(&reviews);
        assert_eq!(both.len(), 2);
        let n = reviews.join(both.iter().find(|id| **id != ids[0]).unwrap());
        assert_eq!(listing(&n), ["1.png", "model", "review.json"]);

        // Neither a recently used review nor one whose model is still there
        // is pruned, nor a directory that does not say whose it is.
        std::fs::create_dir_all(reviews.join("unknown")).unwrap();
        std::fs::remove_file(models.join("n.clm")).unwrap();
        assert_eq!(store.unused(Duration::from_secs(3600)).unwrap(), []);
        let unused = store.unused(Duration::ZERO).unwrap();
        assert_eq!(unused.len(), 1);
        assert_eq!(unused[0].dir, n);
        assert_eq!(unused[0].model, m.with_file_name("n.clm"));
        assert_eq!(
            unused[0].bytes,
            ["1.png", "model", "review.json"]
                .map(|f| std::fs::metadata(n.join(f)).unwrap().len())
                .iter()
                .sum::<u64>()
        );
        assert!(n.exists(), "listing removes nothing");

        // A model that comes back between listing and removal keeps its review.
        std::fs::write(models.join("n.clm"), b"").unwrap();
        assert!(store.remove(&unused[0]).is_err());
        assert!(n.exists());
        std::fs::remove_file(models.join("n.clm")).unwrap();

        assert_eq!(store.prune(Duration::ZERO).unwrap(), unused);
        assert!(!n.exists());
        assert!(kept.exists());
        assert!(reviews.join("unknown").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
