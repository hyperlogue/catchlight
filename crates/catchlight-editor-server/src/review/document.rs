//! The kept review document, and what the store holds of it.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use catchlight_core::Model;
use catchlight_editor_protocol::*;
use serde::{Deserialize, Serialize};

use crate::kept_store::KeptStore;

use super::images::{Image, image_name, media_type_named};
use super::state::{Keyed, ReviewState, Thread};

const DOCUMENT_VERSION: u32 = 2;
pub(super) const DOCUMENT: &str = "review.json";

pub(super) fn kept_image(store: &dyn KeptStore, key: &str, name: &str) -> io::Result<Vec<u8>> {
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
pub(super) struct Document {
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
    const fn blank(&self) -> bool {
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
    /// The [`ReviewState::generation`] `written` holds.
    generation: Option<u64>,
    stored: BTreeMap<u64, String>,
    /// Set when the kept review could not be read. Nothing is written while
    /// it stands, so the file a person may still want is never replaced.
    pub(super) unreadable: Option<String>,
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
                // Read before following the model: targets that moved are
                // what the next write keeps.
                let generation = Some(state.generation);
                state.follow_model(model);
                let stored = state.claimed();
                (
                    state,
                    Self {
                        written: Some(bytes),
                        generation,
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
        if !store.keeps() || self.unreadable.is_some() || self.generation == Some(next.generation) {
            return Ok(());
        }
        if self.written.is_none() && next.blank() {
            return Ok(());
        }
        let document = next.document()?;
        if self.written.as_deref() == Some(document.as_slice()) {
            self.generation = Some(next.generation);
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
        self.generation = Some(next.generation);
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
