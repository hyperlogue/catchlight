//! The connection file: where a running editor writes down its HTTP door so a
//! local client — the CLI, an agent — can find it without being told.
//!
//! It holds the bearer token, so it is `0600` inside a `0700` directory on
//! Unix and written via temp-then-rename. Drop removes it only while it still
//! names this process, leaving a newer server's file alone.

use std::io;
use std::path::{Path, PathBuf};

pub use catchlight_editor_discovery::{Connection, default_connection_path};

use crate::HttpServer;

/// The written file, removed again when this is dropped.
#[derive(Debug)]
pub struct ConnectionFile {
    path: PathBuf,
    pid: u32,
}

impl ConnectionFile {
    /// `root` is the editor's [`FileStorage::root`](crate::FileStorage::root).
    pub fn write(path: &Path, server: &HttpServer, root: &Path) -> io::Result<Self> {
        let connection = Connection {
            url: format!("http://{}", server.addr),
            token: server.token.clone(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            root: root.to_path_buf(),
        };
        write_private(path, &serde_json::to_vec_pretty(&connection)?)?;
        Ok(Self {
            path: path.to_path_buf(),
            pid: connection.pid,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ConnectionFile {
    fn drop(&mut self) {
        if read_connection(&self.path).is_some_and(|c| c.pid == self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn read_connection(path: &Path) -> Option<Connection> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(parent)?;

    crate::atomic::write_with_mode(path, bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{Editor, HttpOptions, bind_http};

    fn server() -> HttpServer {
        bind_http(
            Arc::new(Editor::new()),
            "127.0.0.1:0".parse().unwrap(),
            HttpOptions::default(),
        )
        .unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "catchlight-connection-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("nested").join("connection.json")
    }

    #[test]
    fn the_file_names_the_door_and_leaves_with_its_writer() {
        let path = scratch("roundtrip");
        let server = server();
        let file = ConnectionFile::write(&path, &server, Path::new("/models")).unwrap();
        let read = read_connection(&path).unwrap();
        assert_eq!(read.url, format!("http://{}", server.addr));
        assert_eq!(read.token, server.token);
        assert_eq!(read.pid, std::process::id());
        assert_eq!(read.root, Path::new("/models"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn a_file_another_process_took_over_is_left_alone() {
        let path = scratch("takeover");
        let file = ConnectionFile::write(&path, &server(), Path::new("/")).unwrap();
        let newer = Connection {
            url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            pid: std::process::id().wrapping_add(1),
            version: "0".into(),
            root: "/".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
        drop(file);
        assert_eq!(read_connection(&path), Some(newer));
    }
}
