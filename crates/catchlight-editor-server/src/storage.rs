//! Where a session's bytes live.
//!
//! **A `path` on the wire is an opaque storage key, not a filesystem path.**
//! The protocol carries one string for "which document"; what that string
//! names is this trait's business. Natively it is a filesystem path
//! ([`FileStorage`]); in the browser it is an OPFS entry or a fetched URL; in
//! the cloud it is a blob key. That is why the path-taking commands
//! ([`Command::SessionOpen`](catchlight_editor_protocol::Command::SessionOpen)
//! and friends) are not native-only: one command set serves every deployment,
//! rather than a native surface and a browser surface that drift apart.
//!
//! Keys are `/`-separated by convention so [`parent_key`] and [`join_key`] can
//! resolve a manifest's texture references relative to the manifest. Nothing
//! parses a key beyond that: a backend is free to treat it as flat.
//!
//! A write is expected to be **atomic** — a reader either sees the previous
//! bytes or the new ones, never a truncated file. [`FileStorage`] gets this
//! from temp-then-rename; a backend that cannot must say so in its own docs,
//! because an interrupted save otherwise destroys the only copy.
//!
//! **An upload is not a file on disk.** A browser has no filesystem, so the
//! bytes a tab wants opened arrive over HTTP rather than sitting somewhere
//! [`FileStorage`] could read — yet the command that opens them still names
//! one `path` key. [`StagingStorage`] is that seam: `put` parks bytes under
//! the key the command will name, `read` finds them there before it asks the
//! backing store, and `write` always goes to the backing store, clearing the
//! staged copy so a save is visible to the next read.

use std::collections::HashMap;
use std::io;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A byte store addressed by opaque keys.
pub trait Storage: Send + Sync + std::fmt::Debug {
    /// Reads the whole value at `key`.
    fn read(&self, key: &str) -> io::Result<Vec<u8>>;

    /// Replaces the value at `key`, atomically (see the module docs).
    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()>;
}

/// The key a relative reference inside `key`'s document resolves against —
/// everything before the last `/`, or `""` when the key has no separator.
pub fn parent_key(key: &str) -> &str {
    match key.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => "",
    }
}

/// Resolves `name` against `base`. An empty base yields `name` unchanged, so a
/// flat store stays flat.
pub fn join_key(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }
}

/// The human-facing stem of `key`: its last segment with one extension
/// removed. A session title only — nothing addresses anything by it.
pub fn key_stem(key: &str) -> String {
    let last = key.rsplit('/').next().unwrap_or(key);
    let stem = last.rsplit_once('.').map_or(last, |(stem, _)| stem);
    if stem.is_empty() {
        "untitled".to_string()
    } else {
        stem.to_string()
    }
}

/// No backend configured: every key fails. The default where there is no
/// ambient filesystem (wasm), so a caller that forgot [`with_storage`] gets a
/// named error rather than a panic or a silent empty read.
///
/// [`with_storage`]: crate::Editor::with_storage
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

/// The local filesystem, with keys read as paths.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct FileStorage;

#[cfg(not(target_arch = "wasm32"))]
impl Storage for FileStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        std::fs::read(Path::new(key))
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        // Temp-then-rename: an interrupted save must not truncate the user's
        // only copy. The temp sits beside the target so the rename stays on
        // one filesystem.
        let path = PathBuf::from(key);
        let tmp = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
            Some(ext) => format!("{ext}.tmp"),
            None => "tmp".to_string(),
        });
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)
    }
}

/// A [`Storage`] with an in-memory staging area in front of it.
///
/// `put` holds bytes under the key a later command will name; `take` removes
/// them again. Reads check the staging map first, so
/// `session_open { path: "model.clm" }` resolves against an upload the server
/// never wrote anywhere. Writes never land in the map — a save is a save, so
/// it goes to `backing` and drops whatever was staged under that key, leaving
/// the saved bytes as the only answer a read can give.
///
/// Staging is not a cache: it holds what was put there and nothing else, and
/// never reaches for a key it was not given.
#[derive(Debug)]
pub struct StagingStorage {
    staged: Mutex<HashMap<String, Vec<u8>>>,
    backing: Arc<dyn Storage>,
}

impl StagingStorage {
    /// Stage in front of `backing`.
    pub fn new(backing: Arc<dyn Storage>) -> Self {
        Self {
            staged: Mutex::new(HashMap::new()),
            backing,
        }
    }

    /// Park `bytes` under `key`, replacing anything staged there.
    pub fn put(&self, key: &str, bytes: Vec<u8>) {
        lock(&self.staged).insert(key.to_string(), bytes);
    }

    /// Remove and return what was staged under `key`.
    pub fn take(&self, key: &str) -> Option<Vec<u8>> {
        lock(&self.staged).remove(key)
    }

    /// The staged keys, sorted. For diagnostics only — nothing addresses
    /// anything by this list.
    pub fn staged_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = lock(&self.staged).keys().cloned().collect();
        keys.sort();
        keys
    }
}

impl Storage for StagingStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        if let Some(bytes) = lock(&self.staged).get(key) {
            return Ok(bytes.clone());
        }
        self.backing.read(key)
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.backing.write(key, bytes)?;
        lock(&self.staged).remove(key);
        Ok(())
    }
}

/// A poisoned staging map means a panic already happened elsewhere; the bytes
/// are still whatever they were, so keep going rather than cascading a second
/// panic through a connection thread.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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

    #[test]
    fn staged_bytes_answer_a_read_the_backing_store_would_refuse() {
        let staging = StagingStorage::new(Arc::new(NoStorage));
        staging.put("model.clm", b"staged".to_vec());
        assert_eq!(staging.read("model.clm").unwrap(), b"staged");
        assert_eq!(staging.staged_keys(), vec!["model.clm".to_string()]);
        assert_eq!(staging.take("model.clm").unwrap(), b"staged");
        assert!(staging.read("model.clm").is_err());
    }

    #[test]
    fn a_save_lands_in_the_backing_store_and_clears_the_staged_copy() {
        #[derive(Debug, Default)]
        struct Mem(Mutex<HashMap<String, Vec<u8>>>);
        impl Storage for Mem {
            fn read(&self, key: &str) -> io::Result<Vec<u8>> {
                lock(&self.0)
                    .get(key)
                    .cloned()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, key.to_string()))
            }
            fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
                lock(&self.0).insert(key.to_string(), bytes.to_vec());
                Ok(())
            }
        }

        let backing = Arc::new(Mem::default());
        let staging = StagingStorage::new(backing.clone());
        staging.put("model.clm", b"uploaded".to_vec());
        staging.write("model.clm", b"saved").unwrap();
        assert_eq!(backing.read("model.clm").unwrap(), b"saved");
        assert!(staging.staged_keys().is_empty());
        assert_eq!(staging.read("model.clm").unwrap(), b"saved");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_storage_write_is_visible_to_read() {
        let dir = std::env::temp_dir().join(format!("cl-storage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("m.clm").display().to_string();
        FileStorage.write(&key, b"hello").unwrap();
        assert_eq!(FileStorage.read(&key).unwrap(), b"hello");
        // Overwrite goes through the same temp-then-rename path.
        FileStorage.write(&key, b"world!").unwrap();
        assert_eq!(FileStorage.read(&key).unwrap(), b"world!");
        std::fs::remove_dir_all(&dir).ok();
    }
}
