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

/// The root is read once at construction, so a later working-directory change
/// does not move a session's file.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct FileStorage {
    root: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `Path::join` returns an absolute argument unchanged.
    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

/// Falls back to `.` if the working directory cannot be read.
#[cfg(not(target_arch = "wasm32"))]
impl Default for FileStorage {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Storage for FileStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.path(key))
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        // Temp-then-rename: an interrupted save must not truncate the only
        // copy. The temp sits beside the target so the rename stays on one fs.
        let path = self.path(key);
        let tmp = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => format!("{ext}.tmp"),
            None => "tmp".to_string(),
        });
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
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
        let store = FileStorage::default();
        let key = dir.join("m.clm").display().to_string();
        store.write(&key, b"hello").unwrap();
        assert_eq!(store.read(&key).unwrap(), b"hello");
        store.write(&key, b"world!").unwrap();
        assert_eq!(store.read(&key).unwrap(), b"world!");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_relative_key_lands_under_the_root_and_an_absolute_one_does_not() {
        let root = scratch_dir("root");
        let elsewhere = scratch_dir("elsewhere");
        let store = FileStorage::new(&root);

        store.write("m.clm", b"under the root").unwrap();
        assert_eq!(store.read("m.clm").unwrap(), b"under the root");
        assert_eq!(
            std::fs::read(root.join("m.clm")).unwrap(),
            b"under the root"
        );

        let absolute = elsewhere.join("m.clm").display().to_string();
        store.write(&absolute, b"its own path").unwrap();
        assert_eq!(store.read(&absolute).unwrap(), b"its own path");
        assert_eq!(std::fs::read(&absolute).unwrap(), b"its own path");
        let mut under_root: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        under_root.sort();
        assert_eq!(under_root, vec!["m.clm".to_string()]);

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
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
