//! Where the editor keeps what belongs beside a model between runs: its
//! review and its saved versions.
//!
//! The editor owns every file's format and hands a store opaque bytes under
//! a name; a store only has to keep them per model. Callers hold to this
//! order, so a store never needs a transaction: a file is written before the
//! document that names it, and deleted only after a document that no longer
//! names it has been written.

use std::io;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, SystemTime};

pub trait KeptStore: Send + Sync + std::fmt::Debug {
    /// `false` for a store that keeps nothing, so a client can be told what
    /// it writes lasts only as long as the editor.
    fn keeps(&self) -> bool {
        true
    }

    /// Where a person would look for `model`'s file `name`, for an error
    /// message.
    fn place(&self, model: &str, name: &str) -> String {
        format!("{name} kept for {model}")
    }

    /// `None` when `model` has no such file.
    fn read(&self, model: &str, name: &str) -> io::Result<Option<Vec<u8>>>;

    /// Must be atomic: a reader sees the previous file or the new one.
    /// `name` is a relative path of the editor's own making: `/`-separated,
    /// each part safe as a file name.
    fn write(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()>;

    /// Deleting a missing file is not an error.
    fn delete(&self, model: &str, name: &str) -> io::Result<()>;
}

/// Keeps nothing: everything lasts as long as its session.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoKeptStore;

impl KeptStore for NoKeptStore {
    fn keeps(&self) -> bool {
        false
    }

    fn read(&self, _model: &str, _name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn write(&self, model: &str, _name: &str, _bytes: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("this editor keeps nothing beside {model:?}"),
        ))
    }

    fn delete(&self, _model: &str, _name: &str) -> io::Result<()> {
        Ok(())
    }
}

/// A directory beside the model, `<model key>.kept/`. Keys resolve as
/// [`FileStorage`](crate::FileStorage) resolves them, so the sidecar follows
/// the model file.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct SidecarKeptStore {
    root: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl SidecarKeptStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn dir(&self, model: &str) -> PathBuf {
        let mut path = self.root.join(model).into_os_string();
        path.push(".kept");
        PathBuf::from(path)
    }
}

/// Falls back to `.` if the working directory cannot be read.
#[cfg(not(target_arch = "wasm32"))]
impl Default for SidecarKeptStore {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl KeptStore for SidecarKeptStore {
    fn place(&self, model: &str, name: &str) -> String {
        files::place(&self.dir(model), name)
    }

    fn read(&self, model: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        files::read(&self.dir(model), name)
    }

    fn write(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        files::write(&self.dir(model), name, bytes)
    }

    fn delete(&self, model: &str, name: &str) -> io::Result<()> {
        files::delete(&self.dir(model), name)
    }
}

/// One directory per model under a per-user directory, named by a hash of
/// the model's canonical path and laid out as a [`SidecarKeptStore`]
/// directory, plus a `model` file naming the model so [`prune`](Self::prune)
/// can tell when it is gone. Moving or renaming the model file leaves what
/// was kept behind.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct UserKeptStore {
    root: PathBuf,
    kept: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
const MODEL: &str = "model";

#[cfg(not(target_arch = "wasm32"))]
impl UserKeptStore {
    /// `root` resolves relative model keys, as the
    /// [`FileStorage`](crate::FileStorage) beside it does; `kept` holds one
    /// directory per model.
    pub fn new(root: impl Into<PathBuf>, kept: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            kept: kept.into(),
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
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
            && let Ok(parent) = std::fs::canonicalize(parent)
        {
            return parent.join(name);
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
        self.kept.join(id)
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
            files::write(&dir, MODEL, text.as_bytes())?;
        }
        Ok(dir)
    }

    /// What is kept for models that no longer exist and that has gone
    /// unchanged for `unused_for`; both, so a model on an unmounted drive
    /// keeps what was recently kept for it. A directory that does not say
    /// whose it is, or that cannot be read, is never listed.
    pub fn unused(&self, unused_for: Duration) -> io::Result<Vec<UnusedKept>> {
        let entries = match std::fs::read_dir(&self.kept) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(named(&self.kept, e)),
        };
        let now = SystemTime::now();
        let mut unused = Vec::new();
        for entry in entries.flatten() {
            let Some(kept) = UnusedKept::of(entry.path()) else {
                continue;
            };
            if now.duration_since(kept.changed).unwrap_or(Duration::ZERO) >= unused_for {
                unused.push(kept);
            }
        }
        unused.sort_by(|a, b| a.model.cmp(&b.model));
        Ok(unused)
    }

    /// Removes what [`unused`](Self::unused) listed, unless its model has
    /// come back since.
    pub fn remove(&self, kept: &UnusedKept) -> io::Result<()> {
        if UnusedKept::of(kept.dir.clone()).is_none() {
            return Err(io::Error::other(format!(
                "{}: no longer unused",
                kept.dir.display()
            )));
        }
        std::fs::remove_dir_all(&kept.dir).map_err(|e| named(&kept.dir, e))
    }

    /// Removes what [`unused`](Self::unused) lists and returns what it
    /// removed; a directory it cannot remove is left out.
    pub fn prune(&self, unused_for: Duration) -> io::Result<Vec<UnusedKept>> {
        let mut unused = self.unused(unused_for)?;
        unused.retain(|kept| self.remove(kept).is_ok());
        Ok(unused)
    }
}

/// What is kept for a model that no longer exists, as
/// [`UserKeptStore::unused`] lists it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnusedKept {
    /// Where the model was when something was first kept for it.
    pub model: PathBuf,
    pub dir: PathBuf,
    /// The size of every file under [`dir`](Self::dir).
    pub bytes: u64,
    /// The newest modification time of any file under [`dir`](Self::dir).
    pub changed: SystemTime,
}

#[cfg(not(target_arch = "wasm32"))]
impl UnusedKept {
    /// `None` unless `dir` names a model that is certainly gone.
    fn of(dir: PathBuf) -> Option<Self> {
        let model = PathBuf::from(std::fs::read_to_string(dir.join(MODEL)).ok()?);
        if !matches!(model.try_exists(), Ok(false)) {
            return None;
        }
        let (bytes, changed) = Self::measure(&dir)?;
        Some(Self {
            model,
            dir,
            bytes,
            changed: changed?,
        })
    }

    fn measure(dir: &Path) -> Option<(u64, Option<SystemTime>)> {
        let mut bytes = 0;
        let mut changed = None;
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            if metadata.is_dir() {
                let (inner, when) = Self::measure(&entry.path())?;
                bytes += inner;
                changed = changed.max(when);
            } else {
                bytes += metadata.len();
                changed = changed.max(Some(metadata.modified().ok()?));
            }
        }
        Some((bytes, changed))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl KeptStore for UserKeptStore {
    fn place(&self, model: &str, name: &str) -> String {
        files::place(&self.dir(model), name)
    }

    fn read(&self, model: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        files::read(&self.dir(model), name)
    }

    fn write(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        files::write(&self.dir_for_writing(model)?, name, bytes)
    }

    fn delete(&self, model: &str, name: &str) -> io::Result<()> {
        files::delete(&self.dir(model), name)
    }
}

/// The platform's per-user directory for what is kept beside models:
/// `$XDG_STATE_HOME` (else `~/.local/state`) on Linux and other unixes,
/// `~/Library/Application Support` on macOS, `%LOCALAPPDATA%` on Windows,
/// each under `catchlight/kept`. `CATCHLIGHT_STATE_DIR` replaces
/// `<base>/catchlight` for everything the editor keeps per user. `None` when
/// the environment names none of them.
#[cfg(not(target_arch = "wasm32"))]
pub fn user_kept_dir() -> Option<PathBuf> {
    user_kept_dir_from(|name| std::env::var_os(name))
}

#[cfg(not(target_arch = "wasm32"))]
fn user_kept_dir_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    // Relative values are ignored, as the XDG spec requires of its variables.
    let absolute = |name: &str| {
        var(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(state) = absolute("CATCHLIGHT_STATE_DIR") {
        return Some(state.join("kept"));
    }
    let base = if cfg!(windows) {
        absolute("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        absolute("HOME")?.join("Library/Application Support")
    } else {
        absolute("XDG_STATE_HOME").or_else(|| Some(absolute("HOME")?.join(".local/state")))?
    };
    Some(base.join("catchlight").join("kept"))
}

/// The layout both file stores share: one file per name in one directory.
#[cfg(not(target_arch = "wasm32"))]
mod files {
    use std::io;
    use std::path::Path;

    use super::named;

    pub(super) fn place(dir: &Path, name: &str) -> String {
        dir.join(name).display().to_string()
    }

    pub(super) fn read(dir: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
        let path = dir.join(name);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(named(&path, e)),
        }
    }

    pub(super) fn write(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
        let path = dir.join(name);
        let write = || {
            std::fs::create_dir_all(path.parent().unwrap_or(dir))?;
            crate::atomic::write(&path, bytes)
        };
        write().map_err(|e| named(&path, e))
    }

    pub(super) fn delete(dir: &Path, name: &str) -> io::Result<()> {
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
        user_kept_dir_from(|name| vars.get(name).cloned())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn kept_files_live_under_xdg_state_home_else_the_home_state_directory() {
        let kept = Path::new("catchlight/kept");
        assert_eq!(
            dir_from(&[("XDG_STATE_HOME", "/state"), ("HOME", "/h")]),
            Some(Path::new("/state").join(kept))
        );
        for ignored in ["", "relative/state"] {
            assert_eq!(
                dir_from(&[("XDG_STATE_HOME", ignored), ("HOME", "/h")]),
                Some(Path::new("/h/.local/state").join(kept))
            );
        }
        assert_eq!(dir_from(&[("XDG_STATE_HOME", "state")]), None);
        assert_eq!(dir_from(&[("HOME", "")]), None);
        assert_eq!(dir_from(&[]), None);
        assert_eq!(
            dir_from(&[("CATCHLIGHT_STATE_DIR", "/s"), ("XDG_STATE_HOME", "/state")]),
            Some(PathBuf::from("/s/kept"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kept_files_live_in_application_support() {
        assert_eq!(
            dir_from(&[("HOME", "/h"), ("XDG_STATE_HOME", "/state")]),
            Some(PathBuf::from(
                "/h/Library/Application Support/catchlight/kept"
            ))
        );
        assert_eq!(dir_from(&[]), None);
    }

    #[cfg(windows)]
    #[test]
    fn kept_files_live_in_local_app_data() {
        assert_eq!(
            dir_from(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")]),
            Some(PathBuf::from(r"C:\Users\u\AppData\Local\catchlight\kept"))
        );
        assert_eq!(dir_from(&[("HOME", r"C:\Users\u")]), None);
    }
}
