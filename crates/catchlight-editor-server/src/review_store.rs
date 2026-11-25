//! Where a model's review is kept between editor runs.
//!
//! The editor owns the document's format and hands a store opaque bytes; a
//! store only has to keep them under the model's key. Callers hold to this
//! order, so a store never needs a transaction: an image is put before the
//! document that names it is saved, and deleted only after a document that no
//! longer names it has been saved.

use std::io;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

pub trait ReviewStore: Send + Sync + std::fmt::Debug {
    /// `false` for a store that keeps nothing, so the review can say it lasts
    /// only as long as the editor.
    fn keeps(&self) -> bool {
        true
    }

    /// Where a person would look for `model`'s review, for an error message.
    fn place(&self, model: &str) -> String {
        format!("the review kept for {model}")
    }

    /// `None` when `model` has no review yet.
    fn load(&self, model: &str) -> io::Result<Option<Vec<u8>>>;

    /// Must be atomic: a reader sees the previous document or the new one.
    fn save(&self, model: &str, document: &[u8]) -> io::Result<()>;

    /// `name` is unique within the model's review and safe as a file name.
    fn put_image(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()>;

    fn image(&self, model: &str, name: &str) -> io::Result<Vec<u8>>;

    /// Deleting a missing image is not an error.
    fn delete_image(&self, model: &str, name: &str) -> io::Result<()>;
}

/// Keeps nothing: every review lasts as long as its session.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoReviewStore;

impl ReviewStore for NoReviewStore {
    fn keeps(&self) -> bool {
        false
    }

    fn load(&self, _model: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn save(&self, model: &str, _document: &[u8]) -> io::Result<()> {
        Err(unkept(model))
    }

    fn put_image(&self, model: &str, _name: &str, _bytes: &[u8]) -> io::Result<()> {
        Err(unkept(model))
    }

    fn image(&self, model: &str, _name: &str) -> io::Result<Vec<u8>> {
        Err(unkept(model))
    }

    fn delete_image(&self, _model: &str, _name: &str) -> io::Result<()> {
        Ok(())
    }
}

fn unkept(model: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("no review store configured; cannot keep the review of {model:?}"),
    )
}

/// A directory beside the model, `<model key>.review/`, holding `review.json`
/// and one file per image. Keys resolve as [`FileStorage`](crate::FileStorage)
/// resolves them, so the sidecar follows the model file.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct SidecarReviewStore {
    root: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl SidecarReviewStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn dir(&self, model: &str) -> PathBuf {
        let mut path = self.root.join(model).into_os_string();
        path.push(".review");
        PathBuf::from(path)
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(name);
        // Beside the target so the rename stays on one filesystem.
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    }
}

#[cfg(not(target_arch = "wasm32"))]
const DOCUMENT: &str = "review.json";

/// Falls back to `.` if the working directory cannot be read.
#[cfg(not(target_arch = "wasm32"))]
impl Default for SidecarReviewStore {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn named(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

#[cfg(not(target_arch = "wasm32"))]
impl ReviewStore for SidecarReviewStore {
    fn place(&self, model: &str) -> String {
        self.dir(model).join(DOCUMENT).display().to_string()
    }

    fn load(&self, model: &str) -> io::Result<Option<Vec<u8>>> {
        let path = self.dir(model).join(DOCUMENT);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(named(&path, e)),
        }
    }

    fn save(&self, model: &str, document: &[u8]) -> io::Result<()> {
        let dir = self.dir(model);
        Self::write(&dir, DOCUMENT, document).map_err(|e| named(&dir.join(DOCUMENT), e))
    }

    fn put_image(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        let dir = self.dir(model);
        Self::write(&dir, name, bytes).map_err(|e| named(&dir.join(name), e))
    }

    fn image(&self, model: &str, name: &str) -> io::Result<Vec<u8>> {
        let path = self.dir(model).join(name);
        std::fs::read(&path).map_err(|e| named(&path, e))
    }

    fn delete_image(&self, model: &str, name: &str) -> io::Result<()> {
        let path = self.dir(model).join(name);
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(named(&path, e)),
            _ => Ok(()),
        }
    }
}
