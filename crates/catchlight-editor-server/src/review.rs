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

use crate::review_store::ReviewStore;
use crate::{Attachments, Captured, Editor, EditorError, Payload};

/// A `review watch` polls about once a second; a few missed beats is a gone
/// agent, not a slow one.
const LISTENING_FOR: Duration = Duration::from_secs(5);
/// An agent that took a thread and died must not hold it forever.
const WORKING_FOR: Duration = Duration::from_secs(30 * 60);
const MAX_AGENT_NAME_BYTES: usize = 64;
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
struct Working {
    agent: String,
    since: Stamp,
}

#[derive(Debug, Clone)]
struct Thread {
    id: ThreadId,
    target: ReviewTarget,
    status: ThreadStatus,
    pose: Vec<ParamPose>,
    comments: Vec<ReviewComment>,
    working: Option<Working>,
}

impl Thread {
    fn handed_over(&self) -> bool {
        self.status == ThreadStatus::Open
            && self
                .comments
                .iter()
                .any(|c| c.delivery == ReviewDelivery::Sent)
    }

    fn wire(&self, model: &Model) -> ReviewThread {
        ReviewThread {
            thread: self.id,
            target_missing: !exists(model, &self.target),
            target: self.target.clone(),
            status: self.status,
            pose: self.pose.clone(),
            comments: self.comments.clone(),
            working: self
                .working
                .as_ref()
                .filter(|w| !w.since.older_than(WORKING_FOR))
                .map(|w| w.agent.clone()),
        }
    }
}

#[derive(Debug, Clone)]
struct Agent {
    name: String,
    polled: Option<Stamp>,
}

impl Agent {
    fn listening(&self) -> bool {
        self.polled.is_some_and(|at| at.within(LISTENING_FOR))
    }
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
    agent: Option<Agent>,
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

fn agent_name(name: &str) -> Result<&str, EditorError> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_AGENT_NAME_BYTES {
        return Err(EditorError::BadRequest(format!(
            "an agent name is 1 to {MAX_AGENT_NAME_BYTES} bytes"
        )));
    }
    Ok(name)
}

fn author_of(author: ReviewAuthor) -> Result<ReviewAuthor, EditorError> {
    Ok(match author {
        ReviewAuthor::Human => ReviewAuthor::Human,
        ReviewAuthor::Agent { name } => ReviewAuthor::Agent {
            name: agent_name(&name)?.to_owned(),
        },
    })
}

impl ReviewState {
    fn wire(&self, model: &Model, kept: ReviewKept, kept_problem: Option<String>) -> Review {
        Review {
            threads: self.threads.iter().map(|t| t.wire(model)).collect(),
            agent: self.agent.as_ref().map(|a| ReviewAgent {
                name: a.name.clone(),
                listening: a.listening(),
            }),
            kept,
            kept_problem,
        }
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
    ) -> ReviewComment {
        self.last_comment += 1;
        ReviewComment {
            comment: self.last_comment,
            delivery: match author {
                ReviewAuthor::Human => ReviewDelivery::Unsent,
                ReviewAuthor::Agent { .. } => ReviewDelivery::Delivered,
            },
            author,
            body,
            rev,
            images: self.claim(images),
        }
    }

    fn open(
        &mut self,
        model: &Model,
        rev: u64,
        target: ReviewTarget,
        draft: Draft,
        pose: Vec<ParamPose>,
    ) -> Result<ReviewThread, EditorError> {
        let body = body_of(draft.body, &draft.images)?;
        let author = author_of(draft.author)?;
        require(model, &target)?;
        self.check_images(&draft.images)?;
        let comment = self.comment(author, body, rev, &draft.images);
        self.last_thread += 1;
        let thread = Thread {
            id: ThreadId(self.last_thread),
            target,
            status: ThreadStatus::Open,
            pose,
            comments: vec![comment],
            working: None,
        };
        let wire = thread.wire(model);
        self.threads.push(thread);
        Ok(wire)
    }

    fn reply(
        &mut self,
        model: &Model,
        rev: u64,
        id: ThreadId,
        draft: Draft,
    ) -> Result<ReviewThread, EditorError> {
        let body = body_of(draft.body, &draft.images)?;
        let author = author_of(draft.author)?;
        self.thread_mut(id)?;
        self.check_images(&draft.images)?;
        let human = author == ReviewAuthor::Human;
        let comment = self.comment(author, body, rev, &draft.images);
        let thread = self.thread_mut(id)?;
        thread.comments.push(comment);
        if human {
            thread.status = ThreadStatus::Open;
        } else {
            thread.working = None;
        }
        Ok(thread.wire(model))
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
            thread.working = None;
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

    fn pending(&self) -> u32 {
        let count = self.threads.iter().filter(|t| t.handed_over()).count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// Whether what [`Self::wire`] says about the agent changed.
    fn poll(&mut self, name: &str) -> bool {
        let was = self
            .agent
            .as_ref()
            .is_some_and(|a| a.name == name && a.listening());
        let polled = Some(Stamp::now());
        let agent = Agent {
            name: name.to_owned(),
            polled,
        };
        let now = agent.listening();
        let renamed = self.agent.as_ref().is_none_or(|a| a.name != name);
        self.agent = Some(agent);
        renamed || was != now
    }

    /// The threads taken, as they were handed over: the comments this fetch
    /// delivers still read `sent`, which is how a reader tells them from the
    /// thread's history. Second is whether anything changed.
    fn fetch(&mut self, model: &Model, name: &str) -> (Vec<ReviewThread>, bool) {
        let mut taken = Vec::new();
        for thread in &mut self.threads {
            if !thread.handed_over() {
                continue;
            }
            thread.working = Some(Working {
                agent: name.to_owned(),
                since: Stamp::now(),
            });
            taken.push(thread.wire(model));
            for comment in &mut thread.comments {
                if comment.delivery == ReviewDelivery::Sent {
                    comment.delivery = ReviewDelivery::Delivered;
                }
            }
        }
        let known = self.agent.as_ref().is_some_and(|a| a.name == name);
        let changed = !taken.is_empty() || !known;
        if !known || !taken.is_empty() {
            // An agent that took work is no longer waiting for it.
            self.agent = Some(Agent {
                name: name.to_owned(),
                polled: None,
            });
        }
        (taken, changed)
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

const DOCUMENT_VERSION: u32 = 1;

/// The part of a review that outlives the session. Agents, leases and held
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
                    working: None,
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
    pub(crate) fn open(store: &dyn ReviewStore, key: &str, model: &Model) -> (ReviewState, Self) {
        if !store.keeps() {
            return Default::default();
        }
        let read = store.load(key).and_then(|found| match found {
            Some(bytes) => ReviewState::from_document(&bytes)
                .map(|state| Some((state, bytes)))
                .map_err(|e| bad_document(format!("{}: {e}", store.place(key)))),
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

    pub(crate) fn kept(&self, store: &dyn ReviewStore, key: Option<&str>) -> ReviewKept {
        if !store.keeps() {
            ReviewKept::Session
        } else if key.is_none() {
            ReviewKept::Unsaved
        } else if self.unreadable.is_some() {
            ReviewKept::Unreadable
        } else {
            ReviewKept::Stored
        }
    }

    /// Brings the store to `next`, writing only when what it keeps differs.
    /// On an error the store still reads as it did: images are put before the
    /// document that names them and dropped only after one that does not.
    pub(crate) fn write(
        &mut self,
        store: &dyn ReviewStore,
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
            store.put_image(key, name, bytes)?;
        }
        store.save(key, &document)?;
        for (id, name) in &self.stored {
            if !claimed.contains_key(id) {
                // An orphan file is harmless and the document no longer
                // names it; the comment's deletion already took effect.
                let _ = store.delete_image(key, name);
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
        store: &dyn ReviewStore,
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
                    let bytes = store.image(from, &image_name(*id, image.media_type))?;
                    image.bytes = Some(Arc::new(bytes));
                }
            }
        }
        *self = Self::default();
        self.write(store, Some(to), review)
    }
}

impl Editor {
    /// Runs `f` under the session lock and, when it reports a change, tells
    /// observers once the lock is released.
    fn review_session(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut ReviewState, &Model, u64) -> Result<(ResponseBody, bool), EditorError>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let captured = self.with_session_captured(id, |s| {
            let rev = s.rev;
            // The change is kept before it is published: a store that
            // refuses leaves the review as every client last read it.
            let mut next = s.review.clone();
            let out = f(&mut next, &s.model, rev)?;
            s.review_keeping
                .write(&*self.review_store, s.file.as_deref(), &next)?;
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
                        (None, Some(key)) => self
                            .review_store
                            .image(key, &image_name(image, stored.media_type))?,
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
                let kept = s
                    .review_keeping
                    .kept(&*self.review_store, s.file.as_deref());
                let problem = (kept == ReviewKept::Unreadable)
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
            } => self.review_session(session, |review, model, rev| {
                let thread = review.open(
                    model,
                    rev,
                    target,
                    Draft {
                        author,
                        body,
                        images,
                    },
                    pose,
                )?;
                Ok((ResponseBody::ReviewThread { thread }, true))
            }),
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
            } => self.review_session(session, |review, model, rev| {
                let thread = review.reply(
                    model,
                    rev,
                    thread,
                    Draft {
                        author,
                        body,
                        images,
                    },
                )?;
                Ok((ResponseBody::ReviewThread { thread }, true))
            }),
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
                self.review_session(session, |review, _, _| {
                    let changed = review.poll(agent_name(&agent)?);
                    Ok((
                        ResponseBody::ReviewPending {
                            pending: review.pending(),
                        },
                        changed,
                    ))
                })
            }
            Command::ReviewFetch { session, agent } => {
                self.review_session(session, |review, model, _| {
                    let (threads, changed) = review.fetch(model, agent_name(&agent)?);
                    Ok((ResponseBody::ReviewFetched { threads }, changed))
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

        fn open(&mut self, target: ReviewTarget, author: ReviewAuthor) -> ReviewThread {
            let session = self.session;
            match self.ok(Command::ReviewThreadAdd {
                session,
                target,
                author,
                body: "  look here \n".into(),
                pose: Vec::new(),
                images: Vec::new(),
            }) {
                ResponseBody::ReviewThread { thread } => thread,
                other => panic!("{other:?}"),
            }
        }
    }

    fn agent(name: &str) -> ReviewAuthor {
        ReviewAuthor::Agent { name: name.into() }
    }

    fn deliveries(thread: &ReviewThread) -> Vec<ReviewDelivery> {
        thread.comments.iter().map(|c| c.delivery).collect()
    }

    #[test]
    fn a_comment_travels_from_unsent_to_delivered_and_the_reply_ends_the_work() {
        let mut f = Fixture::new();
        let session = f.session;
        let rev = f.rev();

        let opened = f.open(ReviewTarget::Model, ReviewAuthor::Human);
        assert_eq!(opened.thread, ThreadId(1));
        assert_eq!(opened.comments[0].body, "look here");
        assert_eq!(opened.comments[0].comment, 1);
        assert_eq!(deliveries(&opened), [ReviewDelivery::Unsent]);

        assert!(matches!(
            f.ok(Command::ReviewPoll {
                session,
                agent: "claude".into()
            }),
            ResponseBody::ReviewPending { pending: 0 }
        ));
        f.ok(Command::ReviewSubmit { session });
        assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Sent]);
        assert!(matches!(
            f.ok(Command::ReviewPoll {
                session,
                agent: "claude".into()
            }),
            ResponseBody::ReviewPending { pending: 1 }
        ));
        assert_eq!(
            f.review().agent,
            Some(ReviewAgent {
                name: "claude".into(),
                listening: true
            })
        );

        let fetched = match f.ok(Command::ReviewFetch {
            session,
            agent: "claude".into(),
        }) {
            ResponseBody::ReviewFetched { threads } => threads,
            other => panic!("{other:?}"),
        };
        assert_eq!(fetched.len(), 1);
        assert_eq!(deliveries(&fetched[0]), [ReviewDelivery::Sent]);
        assert_eq!(fetched[0].working.as_deref(), Some("claude"));

        let review = f.review();
        assert_eq!(deliveries(&review.threads[0]), [ReviewDelivery::Delivered]);
        assert_eq!(review.threads[0].working.as_deref(), Some("claude"));
        assert_eq!(
            review.agent,
            Some(ReviewAgent {
                name: "claude".into(),
                listening: false
            })
        );
        assert!(matches!(
            f.ok(Command::ReviewFetch {
                session,
                agent: "claude".into()
            }),
            ResponseBody::ReviewFetched { threads } if threads.is_empty()
        ));

        let replied = match f.ok(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: agent("claude"),
            body: "done".into(),
            images: Vec::new(),
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(replied.working, None);
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
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
        assert_eq!(f.review_events(), 1);
        f.review();
        assert_eq!(f.review_events(), 1);

        let poll = Command::ReviewPoll {
            session,
            agent: "claude".into(),
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
            author: ReviewAuthor::Human,
            body: "x".into(),
            pose: Vec::new(),
            images: Vec::new(),
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
                author: ReviewAuthor::Human,
                body: " \n".into(),
                pose: Vec::new(),
                images: Vec::new(),
            }),
            ErrorCode::BadRequest
        );
        assert_eq!(
            f.refused(Command::ReviewCommentAdd {
                session,
                thread: ThreadId(9),
                author: ReviewAuthor::Human,
                body: "x".into(),
                images: Vec::new(),
            }),
            ErrorCode::NoThread
        );
        assert_eq!(
            f.refused(Command::ReviewPoll {
                session,
                agent: " ".into()
            }),
            ErrorCode::BadRequest
        );
        assert!(f.review().threads.is_empty());
    }

    #[test]
    fn deleting_the_only_comment_deletes_the_thread_and_ids_are_not_reused() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
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
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
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
            author: ReviewAuthor::Human,
            body: "still wrong".into(),
            images: Vec::new(),
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(reopened.status, ThreadStatus::Open);

        f.ok(Command::ReviewSubmit { session });
        f.ok(Command::ReviewFetch {
            session,
            agent: "claude".into(),
        });
        let resolved = match f.ok(Command::ReviewThreadSet {
            session,
            thread: ThreadId(1),
            status: ThreadStatus::Resolved,
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(resolved.working, None);
    }

    #[test]
    fn a_deleted_target_reads_missing_until_undo_brings_it_back() {
        let mut f = Fixture::new();
        let session = f.session;
        let node = f.group("g");
        f.open(
            ReviewTarget::Node { node: node.clone() },
            ReviewAuthor::Human,
        );
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
        f.open(
            ReviewTarget::Node { node: from.clone() },
            ReviewAuthor::Human,
        );

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
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
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
    fn a_silent_agent_stops_listening_and_stale_work_lapses() {
        let mut state = ReviewState::default();
        let model = Model::new();
        state
            .open(
                &model,
                0,
                ReviewTarget::Model,
                Draft {
                    author: ReviewAuthor::Human,
                    body: "x".into(),
                    images: Vec::new(),
                },
                Vec::new(),
            )
            .unwrap();
        state.submit();
        assert!(state.poll("claude"));
        assert!(!state.poll("claude"));
        let Some(long_ago) = std::time::Instant::now().checked_sub(WORKING_FOR) else {
            return;
        };
        state.agent.as_mut().unwrap().polled = Some(Stamp(long_ago));
        assert!(
            !state
                .wire(&model, ReviewKept::Session, None)
                .agent
                .unwrap()
                .listening
        );
        assert!(state.poll("claude"), "coming back is a change");

        state.fetch(&model, "claude");
        assert!(state.wire(&model, ReviewKept::Session, None).threads[0]
            .working
            .is_some());
        state.threads[0].working.as_mut().unwrap().since = Stamp(long_ago);
        assert_eq!(
            state.wire(&model, ReviewKept::Session, None).threads[0].working,
            None
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
                author: ReviewAuthor::Human,
                body: body.into(),
                pose: Vec::new(),
                images,
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
                author: ReviewAuthor::Human,
                body: "x".into(),
                images: vec![image],
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
    fn deleting_a_comment_or_thread_frees_its_images() {
        let mut f = Fixture::new();
        let session = f.session;
        let first = f.image(PNG).image;
        let second = f.image(PNG).image;
        assert!(matches!(f.say(vec![first], "one"), Reply::Ok { .. }));
        f.ok(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: ReviewAuthor::Human,
            body: "two".into(),
            images: vec![second],
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

    impl ReviewStore for MemoryStore {
        fn load(&self, model: &str) -> io::Result<Option<Vec<u8>>> {
            self.check()?;
            Ok(self.documents.lock().unwrap().get(model).cloned())
        }

        fn save(&self, model: &str, document: &[u8]) -> io::Result<()> {
            self.check()?;
            self.saves
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.documents
                .lock()
                .unwrap()
                .insert(model.into(), document.to_vec());
            Ok(())
        }

        fn put_image(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
            self.check()?;
            self.images
                .lock()
                .unwrap()
                .insert((model.into(), name.into()), bytes.to_vec());
            Ok(())
        }

        fn image(&self, model: &str, name: &str) -> io::Result<Vec<u8>> {
            self.check()?;
            self.images
                .lock()
                .unwrap()
                .get(&(model.to_owned(), name.to_owned()))
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, name.to_owned()))
        }

        fn delete_image(&self, model: &str, name: &str) -> io::Result<()> {
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
            let editor = Editor::with_storage(models.clone()).with_review_store(store.clone());
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
            let editor = Editor::with_storage(models.clone()).with_review_store(store.clone());
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
        assert_eq!(f.review().kept, ReviewKept::Unsaved);
        assert!(matches!(f.save("m.clm"), Reply::Ok { .. }));
        assert_eq!(f.review().kept, ReviewKept::Stored);

        let first = f.thread_with_image(PNG);
        let second = f.open(ReviewTarget::Model, ReviewAuthor::Human);
        f.ok(Command::ReviewSubmit { session });
        f.ok(Command::ReviewFetch {
            session,
            agent: "claude".into(),
        });
        f.ok(Command::ReviewThreadSet {
            session,
            thread: second.thread,
            status: ThreadStatus::Resolved,
        });
        let before = f.review();

        let mut g = Fixture::reopened(&models, &store, "m.clm");
        let after = g.review();
        assert_eq!(after.kept, ReviewKept::Stored);
        assert_eq!(
            after.agent, None,
            "an agent belongs to the session it joined"
        );
        assert_eq!(after.threads.len(), 2);
        for (was, now) in before.threads.iter().zip(&after.threads) {
            assert_eq!(was.thread, now.thread);
            assert_eq!(was.status, now.status);
            assert_eq!(was.comments, now.comments);
            assert_eq!(
                now.working, None,
                "a lease belongs to the session that took it"
            );
        }
        let image = first.comments[0].images[0].image;
        assert_eq!(g.bytes(image).unwrap().bytes, PNG);

        let next = g.thread_with_image(PNG);
        assert_eq!(next.thread, ThreadId(3), "thread Ids continue");
        assert_eq!(next.comments[0].comment, 3, "comment Ids continue");
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
            agent: "claude".into(),
        });
        f.review();
        assert_eq!(store.saves(), 0, "an empty review is not written");

        f.open(ReviewTarget::Model, ReviewAuthor::Human);
        assert_eq!(store.saves(), 1);
        f.image(PNG);
        f.ok(Command::ReviewPoll {
            session,
            agent: "claude".into(),
        });
        f.ok(Command::ReviewFetch {
            session,
            agent: "claude".into(),
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
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
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
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
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
            assert_eq!(review.kept, ReviewKept::Unreadable);
            assert!(review.kept_problem.is_some());
            assert!(review.threads.is_empty());

            f.open(ReviewTarget::Model, ReviewAuthor::Human);
            f.save("m.clm");
            assert_eq!(
                f.review().threads.len(),
                1,
                "comments still work, in memory"
            );
            assert_eq!(store.documents.lock().unwrap()["m.clm"], bad);

            assert!(matches!(f.save("copy.clm"), Reply::Ok { .. }));
            assert_eq!(
                f.review().kept,
                ReviewKept::Stored,
                "a new key has no bad file"
            );
        }
    }

    #[test]
    fn a_kept_thread_finds_a_target_renamed_after_the_last_save() {
        let (models, store) = Default::default();
        let mut f = Fixture::keeping(&models, &store);
        let session = f.session;
        let node = f.group("arm");
        f.save("m.clm");
        f.open(
            ReviewTarget::Node { node: node.clone() },
            ReviewAuthor::Human,
        );
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
        assert_eq!(f.review().kept, ReviewKept::Session);
    }

    #[test]
    fn the_sidecar_is_a_directory_beside_the_model() {
        use crate::{FileStorage, SidecarReviewStore};
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("cl-review-sidecar-{}-{nth}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        let editor = || {
            Editor::with_storage(Arc::new(FileStorage::new(&dir)))
                .with_review_store(Arc::new(SidecarReviewStore::new(&dir)))
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

        let sidecar = dir.join("models/m.clm.review");
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
        assert_eq!(review.kept, ReviewKept::Unreadable);
        assert!(
            review.kept_problem.unwrap().contains("review.json"),
            "the problem names the file to look at"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
