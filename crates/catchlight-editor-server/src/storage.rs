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
//! **A relative key resolves against the store, not against the process.**
//! [`FileStorage`] carries a root directory fixed at construction — the
//! current directory when nobody says otherwise — and a relative key joins
//! onto it, so where a document lands is a property of the store a server was
//! built with rather than of whatever directory the process happens to be in
//! when a read arrives. An absolute key is already a path and joins onto
//! nothing, which is what keeps `session_open /abs/model.clm` meaning the file
//! it names.
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
//!
//! **Staging is released by the read that consumed it, and a released key was
//! never a file.** [`Storage::release`] is how a command that read a key *into
//! a document* says so. A staging map nothing empties holds a second whole
//! copy — textures and all — of every document the server was handed, for the
//! life of the process. The answer is also the fact the caller needs: a key
//! that released was a transient upload, not a file, so the session opened
//! from it has nowhere to save back to and a bare `save` refuses rather than
//! writing into the server's working directory. Only a command that succeeded
//! releases; a failed one leaves its bytes staged so the caller may retry.

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

    /// The bytes at `key` were read into a document; if they were transient,
    /// drop them and say so.
    ///
    /// `true` means the key named an upload this store was holding for exactly
    /// that read and is holding no longer — which is also how a caller learns
    /// the key was never a file. A store whose keys are durable keeps them and
    /// returns `false`, which is the default.
    fn release(&self, _key: &str) -> bool {
        false
    }
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

/// The local filesystem, with keys read as paths under a root directory.
///
/// A relative key joins onto the root; an absolute key is itself. The root is
/// read once, here, so nothing a process does to its working directory later
/// moves a session's document.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct FileStorage {
    root: PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileStorage {
    /// A store whose relative keys resolve against `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory relative keys resolve against.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path `key` names. `Path::join` returns an absolute argument
    /// unchanged, which is the whole of the absolute-key rule.
    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }
}

/// Rooted at the current directory, so a server nobody configured behaves as
/// it always did. A working directory that cannot be read leaves `.`, which
/// resolves the same way.
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
        // Temp-then-rename: an interrupted save must not truncate the user's
        // only copy. The temp sits beside the target so the rename stays on
        // one filesystem.
        let path = self.path(key);
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
/// `put` holds bytes under the key a later command will name; `take` and
/// [`release`](Storage::release) remove them again. Reads check the staging
/// map first, so
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

    fn release(&self, key: &str) -> bool {
        lock(&self.staged).remove(key).is_some()
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

    #[test]
    fn release_drops_an_upload_and_says_it_was_one() {
        let staging = StagingStorage::new(Arc::new(NoStorage));
        staging.put("model.clm", b"uploaded".to_vec());
        assert!(staging.release("model.clm"));
        assert!(staging.staged_keys().is_empty());
        // A key the store never staged is not this store's to drop, and the
        // `false` is what tells a caller the key may name a real file.
        assert!(!staging.release("model.clm"));
        assert!(!staging.release("on-disk.clm"));
        assert!(!NoStorage.release("model.clm"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_storage_keeps_the_keys_it_is_asked_to_release() {
        assert!(!FileStorage::default().release("models/akari.clm"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_storage_write_is_visible_to_read() {
        let dir = scratch_dir("visible");
        let store = FileStorage::default();
        let key = dir.join("m.clm").display().to_string();
        store.write(&key, b"hello").unwrap();
        assert_eq!(store.read(&key).unwrap(), b"hello");
        // Overwrite goes through the same temp-then-rename path.
        store.write(&key, b"world!").unwrap();
        assert_eq!(store.read(&key).unwrap(), b"world!");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The root is what a relative key means; an absolute key means itself.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_relative_key_lands_under_the_root_and_an_absolute_one_does_not() {
        let root = scratch_dir("root");
        let elsewhere = scratch_dir("elsewhere");
        let store = FileStorage::new(&root);

        store.write("m.clm", b"under the root").unwrap();
        assert_eq!(store.read("m.clm").unwrap(), b"under the root");
        // Not "wherever the process happens to be": the bytes are in the root.
        assert_eq!(
            std::fs::read(root.join("m.clm")).unwrap(),
            b"under the root"
        );

        let absolute = elsewhere.join("m.clm").display().to_string();
        store.write(&absolute, b"its own path").unwrap();
        assert_eq!(store.read(&absolute).unwrap(), b"its own path");
        assert_eq!(std::fs::read(&absolute).unwrap(), b"its own path");
        // The absolute key joined onto nothing, so the root has one file.
        let mut under_root: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        under_root.sort();
        assert_eq!(under_root, vec!["m.clm".to_string()]);

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    /// A directory of this run's own, named after nothing but the test.
    #[cfg(not(target_arch = "wasm32"))]
    fn scratch_dir(what: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nth = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("cl-storage-{}-{what}-{nth}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        // A failure here surfaces as the first write's error, inside the test.
        std::fs::create_dir_all(&dir).ok();
        dir
    }
}
