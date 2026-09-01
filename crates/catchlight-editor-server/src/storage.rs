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
