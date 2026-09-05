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
//! resolve one document's references relative to it. Nothing parses a key
//! beyond that: a backend is free to treat it as flat.
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
//! **The store holds the server's files, and nothing else passes through it.**
//! Open, save, export: three commands, each naming a file that is the
//! server's to read or write. Bytes a *client* holds never become a key —
//! there is no upload, no staging map, and so no key that names something the
//! server never wrote. A tab that wants its own bytes opened sends
//! [`Command::SessionNew`](catchlight_editor_protocol::Command::SessionNew)
//! and then
//! [`Command::ImportFile`](catchlight_editor_protocol::Command::ImportFile)
//! with the file attached, and the session it gets has no file to save back
//! to because there is none.

use std::io;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

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
