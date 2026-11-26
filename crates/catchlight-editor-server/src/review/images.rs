//! Images uploaded for review comments: typed, held, claimed and freed.

use std::sync::Arc;

use catchlight_editor_protocol::*;

use crate::EditorError;

use super::state::ReviewState;

pub(super) const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_SESSION_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_COMMENT_IMAGES: usize = 4;
/// An upload whose comment never came must not pin memory for the session's
/// life.
pub(super) const MAX_HELD_IMAGES: usize = 16;

#[derive(Debug, Clone)]
pub(super) struct Image {
    pub(super) media_type: &'static str,
    pub(super) byte_length: usize,
    /// `None` for an image a kept review names and nobody has asked for yet.
    pub(super) bytes: Option<Arc<Vec<u8>>>,
}

pub(super) fn media_type_named(name: &str) -> Option<&'static str> {
    ["image/png", "image/jpeg", "image/webp"]
        .into_iter()
        .find(|known| *known == name)
}

/// What a [`KeptStore`](crate::KeptStore) knows an image by.
pub(super) fn image_name(id: u64, media_type: &str) -> String {
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

impl ReviewState {
    pub(super) fn image_info(&self, id: u64) -> Result<ReviewImage, EditorError> {
        let image = self.images.get(&id).ok_or(EditorError::NoImage(id))?;
        Ok(ReviewImage {
            image: id,
            media_type: image.media_type.to_owned(),
            byte_length: u32::try_from(image.byte_length).unwrap_or(u32::MAX),
        })
    }

    pub(super) fn hold_image(&mut self, bytes: Vec<u8>) -> Result<ReviewImage, EditorError> {
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
        let evicted = self
            .held
            .front()
            .copied()
            .filter(|_| self.held.len() == MAX_HELD_IMAGES);
        let total: usize = self
            .images
            .iter()
            .filter(|(id, _)| Some(**id) != evicted)
            .map(|(_, i)| i.byte_length)
            .sum();
        if total + bytes.len() > MAX_SESSION_IMAGE_BYTES {
            return Err(EditorError::Limit {
                resource: "review_session_image_bytes",
                limit: MAX_SESSION_IMAGE_BYTES as u64,
                requested: (total + bytes.len()) as u64,
            });
        }
        if let Some(oldest) = evicted {
            self.held.pop_front();
            self.images.remove(&oldest);
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
    pub(super) fn check_images(&self, images: &[u64]) -> Result<(), EditorError> {
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

    pub(super) fn claim(&mut self, images: &[u64]) -> Vec<ReviewImage> {
        self.held.retain(|id| !images.contains(id));
        if let Some(highest) = images.iter().max() {
            self.last_claimed_image = self.last_claimed_image.max(*highest);
        }
        images
            .iter()
            .filter_map(|id| self.image_info(*id).ok())
            .collect()
    }

    pub(super) fn free(&mut self, comments: &[ReviewComment]) {
        for image in comments.iter().flat_map(|c| &c.images) {
            self.images.remove(&image.image);
        }
    }
}
