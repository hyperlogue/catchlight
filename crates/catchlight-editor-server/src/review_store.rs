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
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, SystemTime};

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
}

/// Falls back to `.` if the working directory cannot be read.
#[cfg(not(target_arch = "wasm32"))]
impl Default for SidecarReviewStore {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ReviewStore for SidecarReviewStore {
    fn place(&self, model: &str) -> String {
        files::place(&self.dir(model))
    }

    fn load(&self, model: &str) -> io::Result<Option<Vec<u8>>> {
        files::load(&self.dir(model))
    }

    fn save(&self, model: &str, document: &[u8]) -> io::Result<()> {
        files::save(&self.dir(model), document)
    }

    fn put_image(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        files::put_image(&self.dir(model), name, bytes)
    }

    fn image(&self, model: &str, name: &str) -> io::Result<Vec<u8>> {
        files::image(&self.dir(model), name)
    }

    fn delete_image(&self, model: &str, name: &str) -> io::Result<()> {
        files::delete_image(&self.dir(model), name)
    }
}

/// One directory per model under a per-user directory, named by a hash of
/// the model's canonical path and laid out as a [`SidecarReviewStore`]
/// directory, plus a `model` file naming the model so [`prune`](Self::prune)
/// can tell when it is gone. Moving or renaming the model file leaves its
/// review behind.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct UserReviewStore {
    root: PathBuf,
    reviews: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
const MODEL: &str = "model";

#[cfg(not(target_arch = "wasm32"))]
impl UserReviewStore {
    /// `root` resolves relative model keys, as the
    /// [`FileStorage`](crate::FileStorage) beside it does; `reviews` holds
    /// one directory per model.
    pub fn new(root: impl Into<PathBuf>, reviews: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            reviews: reviews.into(),
        }
    }

    /// A model saved to a new name does not exist yet when it is first
    /// looked up, so its parent is canonicalized instead; the same file then
    /// lands in the same directory before and after it is written.
    fn model_path(&self, model: &str) -> PathBuf {
        let path = self.root.join(model);
        if let Ok(path) = std::fs::canonicalize(&path) {
            return path;
        }
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            if let Ok(parent) = std::fs::canonicalize(parent) {
                return parent.join(name);
            }
        }
        std::path::absolute(&path).unwrap_or(path)
    }

    fn dir_of(&self, model_path: &Path) -> PathBuf {
        use sha2::{Digest, Sha256};
        use std::fmt::Write;
        let digest = Sha256::digest(model_path.as_os_str().as_encoded_bytes());
        let mut id = String::with_capacity(32);
        for byte in &digest[..16] {
            let _ = write!(id, "{byte:02x}");
        }
        self.reviews.join(id)
    }

    fn dir(&self, model: &str) -> PathBuf {
        self.dir_of(&self.model_path(model))
    }

    /// Creates `model`'s directory, recording which model it belongs to.
    fn dir_for_writing(&self, model: &str) -> io::Result<PathBuf> {
        let model_path = self.model_path(model);
        let dir = self.dir_of(&model_path);
        let record = dir.join(MODEL);
        // A path that is not UTF-8 goes unrecorded, so prune never removes it.
        if let (false, Some(text)) = (record.exists(), model_path.to_str()) {
            files::write(&dir, MODEL, text.as_bytes()).map_err(|e| named(&record, e))?;
        }
        Ok(dir)
    }

    /// The reviews of models that no longer exist and that have gone
    /// unchanged for `unused_for`; both, so a model on an unmounted drive
    /// keeps its recent review. A directory that does not say whose review
    /// it holds, or that cannot be read, is never listed.
    pub fn unused(&self, unused_for: Duration) -> io::Result<Vec<UnusedReview>> {
        let entries = match std::fs::read_dir(&self.reviews) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(named(&self.reviews, e)),
        };
        let now = SystemTime::now();
        let mut unused = Vec::new();
        for entry in entries.flatten() {
            let Some(review) = UnusedReview::of(entry.path()) else {
                continue;
            };
            if now.duration_since(review.changed).unwrap_or(Duration::ZERO) >= unused_for {
                unused.push(review);
            }
        }
        unused.sort_by(|a, b| a.model.cmp(&b.model));
        Ok(unused)
    }

    /// Removes a review [`unused`](Self::unused) listed, unless its model
    /// has come back since.
    pub fn remove(&self, review: &UnusedReview) -> io::Result<()> {
        if UnusedReview::of(review.dir.clone()).is_none() {
            return Err(io::Error::other(format!(
                "{}: no longer unused",
                review.dir.display()
            )));
        }
        std::fs::remove_dir_all(&review.dir).map_err(|e| named(&review.dir, e))
    }

    /// Removes what [`unused`](Self::unused) lists and returns what it
    /// removed; a review it cannot remove is left out.
    pub fn prune(&self, unused_for: Duration) -> io::Result<Vec<UnusedReview>> {
        let mut unused = self.unused(unused_for)?;
        unused.retain(|review| self.remove(review).is_ok());
        Ok(unused)
    }
}

/// The review of a model that no longer exists, as
/// [`UserReviewStore::unused`] lists it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedReview {
    /// Where the model was when its review was first kept.
    pub model: PathBuf,
    pub dir: PathBuf,
    /// The size of every file in [`dir`](Self::dir).
    pub bytes: u64,
    /// The newest modification time of any file in [`dir`](Self::dir).
    pub changed: SystemTime,
}

#[cfg(not(target_arch = "wasm32"))]
impl UnusedReview {
    /// `None` unless `dir` names a model that is certainly gone.
    fn of(dir: PathBuf) -> Option<Self> {
        let model = PathBuf::from(std::fs::read_to_string(dir.join(MODEL)).ok()?);
        if !matches!(model.try_exists(), Ok(false)) {
            return None;
        }
        let mut bytes = 0;
        let mut changed = None;
        for entry in std::fs::read_dir(&dir).ok()? {
            let metadata = entry.ok()?.metadata().ok()?;
            bytes += metadata.len();
            changed = changed.max(Some(metadata.modified().ok()?));
        }
        Some(Self {
            model,
            dir,
            bytes,
            changed: changed?,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ReviewStore for UserReviewStore {
    fn place(&self, model: &str) -> String {
        files::place(&self.dir(model))
    }

    fn load(&self, model: &str) -> io::Result<Option<Vec<u8>>> {
        files::load(&self.dir(model))
    }

    fn save(&self, model: &str, document: &[u8]) -> io::Result<()> {
        files::save(&self.dir_for_writing(model)?, document)
    }

    fn put_image(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        files::put_image(&self.dir_for_writing(model)?, name, bytes)
    }

    fn image(&self, model: &str, name: &str) -> io::Result<Vec<u8>> {
        files::image(&self.dir(model), name)
    }

    fn delete_image(&self, model: &str, name: &str) -> io::Result<()> {
        files::delete_image(&self.dir(model), name)
    }
}

/// The platform's per-user directory for reviews: `$XDG_STATE_HOME` (else
/// `~/.local/state`) on Linux and other unixes, `~/Library/Application
/// Support` on macOS, `%LOCALAPPDATA%` on Windows, each under
/// `catchlight/reviews`. `None` when the environment names none of them.
#[cfg(not(target_arch = "wasm32"))]
pub fn user_review_dir() -> Option<PathBuf> {
    user_review_dir_from(|name| std::env::var_os(name))
}

#[cfg(not(target_arch = "wasm32"))]
fn user_review_dir_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    // Relative values are ignored, as the XDG spec requires of its variables.
    let absolute = |name: &str| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let base = if cfg!(windows) {
        absolute("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        absolute("HOME")?.join("Library/Application Support")
    } else {
        absolute("XDG_STATE_HOME").or_else(|| Some(absolute("HOME")?.join(".local/state")))?
    };
    Some(base.join("catchlight").join("reviews"))
}

/// The layout both file stores share: `review.json` and one file per image
/// in one directory.
#[cfg(not(target_arch = "wasm32"))]
mod files {
    use std::io;
    use std::path::Path;

    use super::named;

    const DOCUMENT: &str = "review.json";

    pub(super) fn write(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(name);
        // Beside the target so the rename stays on one filesystem.
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    }

    pub(super) fn place(dir: &Path) -> String {
        dir.join(DOCUMENT).display().to_string()
    }

    pub(super) fn load(dir: &Path) -> io::Result<Option<Vec<u8>>> {
        let path = dir.join(DOCUMENT);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(named(&path, e)),
        }
    }

    pub(super) fn save(dir: &Path, document: &[u8]) -> io::Result<()> {
        write(dir, DOCUMENT, document).map_err(|e| named(&dir.join(DOCUMENT), e))
    }

    pub(super) fn put_image(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        write(dir, name, bytes).map_err(|e| named(&dir.join(name), e))
    }

    pub(super) fn image(dir: &Path, name: &str) -> io::Result<Vec<u8>> {
        let path = dir.join(name);
        std::fs::read(&path).map_err(|e| named(&path, e))
    }

    pub(super) fn delete_image(dir: &Path, name: &str) -> io::Result<()> {
        let path = dir.join(name);
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(named(&path, e)),
            _ => Ok(()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn named(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    fn dir_from(vars: &[(&str, &str)]) -> Option<PathBuf> {
        let vars: HashMap<&str, OsString> = vars.iter().map(|(k, v)| (*k, v.into())).collect();
        user_review_dir_from(|name| vars.get(name).cloned())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn reviews_live_under_xdg_state_home_else_the_home_state_directory() {
        let reviews = Path::new("catchlight/reviews");
        assert_eq!(
            dir_from(&[("XDG_STATE_HOME", "/state"), ("HOME", "/h")]),
            Some(Path::new("/state").join(reviews))
        );
        for ignored in ["", "relative/state"] {
            assert_eq!(
                dir_from(&[("XDG_STATE_HOME", ignored), ("HOME", "/h")]),
                Some(Path::new("/h/.local/state").join(reviews))
            );
        }
        assert_eq!(dir_from(&[("XDG_STATE_HOME", "state")]), None);
        assert_eq!(dir_from(&[("HOME", "")]), None);
        assert_eq!(dir_from(&[]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reviews_live_in_application_support() {
        assert_eq!(
            dir_from(&[("HOME", "/h"), ("XDG_STATE_HOME", "/state")]),
            Some(PathBuf::from(
                "/h/Library/Application Support/catchlight/reviews"
            ))
        );
        assert_eq!(dir_from(&[]), None);
    }

    #[cfg(windows)]
    #[test]
    fn reviews_live_in_local_app_data() {
        assert_eq!(
            dir_from(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")]),
            Some(PathBuf::from(
                r"C:\Users\u\AppData\Local\catchlight\reviews"
            ))
        );
        assert_eq!(dir_from(&[("HOME", r"C:\Users\u")]), None);
    }
}
