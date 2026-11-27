//! Where a running editor is found when no flag names one.
//!
//! The server writes to its own copy of these paths;
//! `crates/catchlight-editor/tests/discovery.rs` keeps the two equal.

use std::path::PathBuf;

#[cfg(unix)]
use catchlight_editor_protocol::SOCKET_FILE;
use catchlight_editor_protocol::{CONNECTION_ENV, CONNECTION_FILE, EDITOR_DIR};

/// `$XDG_RUNTIME_DIR/catchlight-editor/server.sock`, else the temp dir.
#[cfg(unix)]
#[must_use]
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join(EDITOR_DIR).join(SOCKET_FILE)
}

/// `CATCHLIGHT_EDITOR_CONNECTION`, else the per-user runtime or cache dir.
#[must_use]
pub fn default_connection_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CONNECTION_ENV) {
        return Some(PathBuf::from(path));
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(base.join(EDITOR_DIR).join(CONNECTION_FILE))
}
