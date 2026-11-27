use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The environment variable that names the connection file outright.
pub const CONNECTION_ENV: &str = "CATCHLIGHT_EDITOR_CONNECTION";
/// The editor's directory under a per-user runtime or cache directory.
pub const EDITOR_DIR: &str = "catchlight-editor";
/// The default Unix socket's file name inside [`EDITOR_DIR`].
pub const SOCKET_FILE: &str = "server.sock";
/// The connection file's name inside [`EDITOR_DIR`].
pub const CONNECTION_FILE: &str = "connection.json";

/// What a running editor writes to its connection file so local clients can
/// find its HTTP door.
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
