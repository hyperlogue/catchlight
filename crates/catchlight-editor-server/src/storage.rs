//! Where a session's bytes live.
//!
//! A wire `path` is an opaque storage key, not a filesystem path. Keys are
//! `/`-separated by convention; nothing parses them further. A write must be
//! atomic: a reader sees the previous bytes or the new ones, never a truncated
//! file.

use std::io;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

pub trait Storage: Send + Sync + std::fmt::Debug {
    fn read(&self, key: &str) -> io::Result<Vec<u8>>;

    /// Must be atomic (see the module docs).
    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
}

pub fn parent_key(key: &str) -> &str {
    match key.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => "",
    }
}

/// An empty base yields `name` unchanged, so a flat store stays flat.
pub fn join_key(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }
}

/// For a session title only; nothing addresses anything by it.
pub fn key_stem(key: &str) -> String {
    let last = key.rsplit('/').next().unwrap_or(key);
    let stem = last.rsplit_once('.').map_or(last, |(stem, _)| stem);
    if stem.is_empty() {
        "untitled".to_string()
    } else {
        stem.to_string()
    }
}

/// The default on wasm, so a caller that forgot
/// [`with_storage`](crate::Editor::with_storage) gets a named error.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoStorage;

impl Storage for NoStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        Err(unconfigured(key))
    }

    fn write(&self, key: &str, _bytes: &[u8]) -> io::Result<()> {
        Err(unconfigured(key))
    }
}

fn unconfigured(key: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("no storage backend configured; cannot reach {key:?}"),
    )
}

/// Files under one root directory, and nothing outside it.
///
/// A key is a path relative to the root, or an absolute one inside it. The
/// root is resolved once at construction, so a later working-directory
/// change does not move a session's file.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct FileStorage {
    /// Canonical, so a resolved key is compared against it component-wise.
    root: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileStorage {
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        let resolved = std::fs::canonicalize(root)
            .map_err(|e| io::Error::new(e.kind(), format!("the root {}: {e}", root.display())))?;
        Ok(Self { root: resolved })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where `key` really is, symlinks followed; a file not written yet is
    /// resolved through its directory. A key that lands outside the root is
    /// refused with [`io::ErrorKind::PermissionDenied`].
    pub fn resolve(&self, key: &str) -> io::Result<PathBuf> {
        // `Path::join` returns an absolute argument unchanged.
        let joined = self.root.join(key);
        let resolved = match std::fs::canonicalize(&joined) {
            Ok(resolved) => resolved,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match (joined.parent(), joined.file_name()) {
                    (Some(parent), Some(name)) => std::fs::canonicalize(parent)?.join(name),
                    _ => return Err(e),
                }
            }
            Err(e) => return Err(e),
        };
        // An unresolved root is empty, and every path starts with the empty path.
        if !self.root.as_os_str().is_empty() && resolved.starts_with(&self.root) {
            Ok(resolved)
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{key} is outside the editor's root {}", self.root.display()),
            ))
        }
    }
}

/// The working directory; one that cannot be resolved roots nothing, so
/// every key is refused.
#[cfg(not(target_arch = "wasm32"))]
impl Default for FileStorage {
    fn default() -> Self {
        std::env::current_dir()
            .and_then(Self::new)
            .unwrap_or_else(|_| Self {
                root: PathBuf::new(),
            })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Storage for FileStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.resolve(key)?)
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        crate::atomic::write(&self.resolve(key)?, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_and_join_round_trip() {
        assert_eq!(parent_key("models/akari.clm"), "models");
        assert_eq!(parent_key("akari.clm"), "");
        assert_eq!(join_key("models", "tex0.png"), "models/tex0.png");
        assert_eq!(join_key("", "tex0.png"), "tex0.png");
        assert_eq!(join_key("models/", "tex0.png"), "models/tex0.png");
    }

    #[test]
    fn stem_drops_one_extension_and_every_segment() {
        assert_eq!(key_stem("models/akari.clm"), "akari");
        assert_eq!(key_stem("akari.clm"), "akari");
        assert_eq!(key_stem("akari"), "akari");
        assert_eq!(key_stem("a/b/c.tar.gz"), "c.tar");
        assert_eq!(key_stem(""), "untitled");
        assert_eq!(key_stem(".clm"), "untitled");
    }

    #[test]
    fn no_storage_names_the_key_it_could_not_reach() {
        let err = NoStorage.read("models/akari.clm").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("models/akari.clm"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_storage_write_is_visible_to_read() {
        let dir = scratch_dir("visible");
        let store = FileStorage::new(&dir).unwrap();
        let key = dir.join("m.clm").display().to_string();
        store.write(&key, b"hello").unwrap();
        assert_eq!(store.read(&key).unwrap(), b"hello");
        store.write(&key, b"world!").unwrap();
        assert_eq!(store.read(&key).unwrap(), b"world!");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_key_reaches_what_is_under_the_root_and_nothing_else() {
        let root = scratch_dir("root");
        let elsewhere = scratch_dir("elsewhere");
        std::fs::create_dir(root.join("sub")).unwrap();
        let store = FileStorage::new(&root).unwrap();

        store.write("m.clm", b"under the root").unwrap();
        assert_eq!(store.read("sub/../m.clm").unwrap(), b"under the root");
        let absolute = root.join("sub/m.clm").display().to_string();
        store.write(&absolute, b"absolute, inside").unwrap();
        assert_eq!(store.read("sub/m.clm").unwrap(), b"absolute, inside");

        std::fs::write(elsewhere.join("m.clm"), b"outside").unwrap();
        let outside = elsewhere.join("m.clm").display().to_string();
        let escape = format!(
            "../{}/m.clm",
            elsewhere.file_name().unwrap().to_string_lossy()
        );
        for key in [outside.as_str(), escape.as_str(), "..", "sub/../../x.clm"] {
            let read = store.read(key).unwrap_err();
            assert_eq!(
                read.kind(),
                io::ErrorKind::PermissionDenied,
                "{key}: {read}"
            );
            assert!(
                read.to_string().contains("outside the editor's root"),
                "{read}"
            );
            let written = store.write(key, b"escaped").unwrap_err();
            assert_eq!(written.kind(), io::ErrorKind::PermissionDenied, "{key}");
        }
        assert_eq!(std::fs::read(elsewhere.join("m.clm")).unwrap(), b"outside");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
            let followed = store.write("link/new.clm", b"escaped").unwrap_err();
            assert_eq!(followed.kind(), io::ErrorKind::PermissionDenied);
            assert!(!elsewhere.join("new.clm").exists());
            assert!(store.read("link/m.clm").is_err());
        }

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_unresolved_root_reaches_nothing() {
        let store = FileStorage {
            root: PathBuf::new(),
        };
        let here = std::env::temp_dir().display().to_string();
        let refused = store.resolve(&here).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_root_that_does_not_exist_is_refused_by_name() {
        let missing = scratch_dir("missing").join("not-here");
        let err = FileStorage::new(&missing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("not-here"), "{err}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn scratch_dir(what: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nth = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("cl-storage-{}-{what}-{nth}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).ok();
        dir
    }
}
