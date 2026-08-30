use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId};

use super::Editor;

pub(super) const MAX_SOCKET_CONNECTIONS: usize = 64;
pub(super) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Listen on a Unix socket, one thread per connection, each reading
/// newline-delimited [`Request`]s and writing [`Reply`]s.
pub fn serve_unix(editor: Arc<Editor>, path: &Path) -> std::io::Result<()> {
    let listener = bind_unix_listener(path)?;
    let connections = Arc::new(ConnectionLimiter::default());
    for stream in listener.incoming() {
        let mut stream = stream?;
        let Some(_permit) = connections.try_acquire() else {
            let _ = write_reply(
                &mut stream,
                &Reply::Err {
                    id: 0,
                    code: ErrorCode::Io,
                    message: "too many editor connections".into(),
                },
            );
            continue;
        };
        let editor = editor.clone();
        std::thread::spawn(move || {
            let _permit = _permit;
            serve_connection(&editor, stream);
        });
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct ConnectionLimiter {
    active: AtomicUsize,
}

impl ConnectionLimiter {
    pub(super) fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_SOCKET_CONNECTIONS).then_some(active + 1)
            })
            .ok()
            .map(|_| ConnectionPermit(self.clone()))
    }
}

pub(super) struct ConnectionPermit(Arc<ConnectionLimiter>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn bind_unix_listener(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let _startup_lock = SocketStartupLock::acquire(path)?;
    match UnixListener::bind(path) {
        Ok(listener) => return Ok(listener),
        Err(err) if err.kind() != std::io::ErrorKind::AddrInUse => return Err(err),
        Err(_) => {}
    }

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return UnixListener::bind(path);
        }
        Err(err) => return Err(err),
    };
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("refusing to replace non-socket path {}", path.display()),
        ));
    }

    match UnixStream::connect(path) {
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("editor server is already listening at {}", path.display()),
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return UnixListener::bind(path);
        }
        Err(err) => return Err(err),
    }

    std::fs::remove_file(path)?;
    UnixListener::bind(path)
}

struct SocketStartupLock {
    path: PathBuf,
}

impl SocketStartupLock {
    fn acquire(socket_path: &Path) -> std::io::Result<Self> {
        let lock_path = socket_path.with_extension("sock.lock");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock_path)
        {
            Ok(_) => Ok(Self { path: lock_path }),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!("another server is starting at {}", socket_path.display()),
                ))
            }
            Err(err) => Err(err),
        }
    }
}

impl Drop for SocketStartupLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn serve_connection(editor: &Editor, stream: UnixStream) {
    if stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT)).is_err()
    {
        return;
    }
    let read_half = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut writer = stream;
    let mut reader = BufReader::new(read_half);
    loop {
        let line = match read_request_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                let _ = write_reply(
                    &mut writer,
                    &Reply::Err {
                        id: 0,
                        code: ErrorCode::BadRequest,
                        message: err.to_string(),
                    },
                );
                break;
            }
            Err(_) => break,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let reply = match serde_json::from_slice::<Request>(&line) {
            Ok(req) => editor.handle(req),
            // A command that does not parse — an Id outside the charset, an
            // unknown `cmd`, a missing field — is still answered against the
            // id the client is blocked on, if the line carries a readable
            // one. Without that a client waiting for its own reply waits
            // forever.
            Err(e) => Reply::Err {
                id: serde_json::from_slice::<RequestId>(&line).map_or(0, |r| r.id),
                code: ErrorCode::BadRequest,
                message: format!("bad request: {e}"),
            },
        };
        if write_reply(&mut writer, &reply).is_err() {
            break;
        }
    }
}

fn read_request_line(reader: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |pos| pos + 1);
        if line.len().saturating_add(take) > MAX_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
            ));
        }
        let terminated = available[take - 1] == b'\n';
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if terminated {
            return Ok(Some(line));
        }
    }
}

fn write_reply(writer: &mut impl Write, reply: &Reply) -> std::io::Result<()> {
    let mut encoded = serde_json::to_vec(reply).map_err(std::io::Error::other)?;
    encoded.push(b'\n');
    writer.write_all(&encoded)
}
