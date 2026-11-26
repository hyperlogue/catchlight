//! Where a local client finds a running Catchlight editor: its Unix socket
//! and the connection file its HTTP door is written to.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `$XDG_RUNTIME_DIR/catchlight-editor/server.sock`, else the temp dir.
#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("catchlight-editor").join("server.sock")
}

/// Where a running editor writes its [`Connection`] for local clients;
/// `CATCHLIGHT_EDITOR_CONNECTION` overrides it. Never a shared temp dir: the
/// file holds a bearer token.
pub fn default_connection_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CATCHLIGHT_EDITOR_CONNECTION") {
        return Some(PathBuf::from(path));
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }?;
    Some(base.join("catchlight-editor").join("connection.json"))
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// `http://127.0.0.1:<port>`, the base every route hangs off.
    pub url: String,
    pub token: String,
    /// The process that wrote the file, so it removes only its own.
    pub pid: u32,
    pub version: String,
    /// The directory the editor's files are confined to.
    pub root: PathBuf,
}
