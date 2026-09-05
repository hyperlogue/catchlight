//! The editor over a Unix socket: one JSON object per line, in order.
//!
//! One connection is one client, and it blocks: [`serve_connection`] reads a
//! line, answers it, and only then reads the next, so nothing is pipelined and
//! replies cannot reorder. There is no event channel — a socket client hears
//! only what it asked for; a tab that needs pushing holds a WebSocket instead
//! (see [`http`](crate::http)).
//!
//! Invariants this module enforces:
//!
//! - **The socket carries paths, never bytes.** A line may hold two sibling
//!   keys beside the [`Request`]'s own fields: `files`, mapping an attachment
//!   name to a path this process reads, and `out`, a path a payload is written
//!   to. Both name the *client's* own files — a socket client and the editor
//!   share a filesystem, which is the whole reason this transport exists — so
//!   nothing is staged, nothing is uploaded, and a line stays small enough to
//!   keep its [`MAX_REQUEST_BYTES`] cap however large the images it names are.
//!
//! - **A file that cannot be read is answered, not skipped.** Every path in
//!   `files` is read before the command runs, and a path that fails is
//!   [`ErrorCode::Io`] against the request's own id with the command never
//!   dispatched. So a half-attached command does not exist.
//!
//! - **A payload needs somewhere to go.** A command whose row in
//!   [`COMMAND_BYTES`](catchlight_editor_protocol::COMMAND_BYTES) says its
//!   reply carries bytes is [`ErrorCode::BadRequest`] without an `out`, before
//!   dispatch — rather than rendering a frame and dropping it. An `out` given
//!   for a command that answers with no payload is left alone.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId};
use serde_json::Value;

use super::{carries_bytes, Attachments, Editor, Payload};

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
        let reply = answer_line(editor, &line);
        if write_reply(&mut writer, &reply).is_err() {
            break;
        }
    }
}

/// One line in, one [`Reply`] out — the whole of what this transport is.
///
/// The line is read as a JSON object first so `files` and `out` can be lifted
/// off it; what is left is the [`Request`] every other transport sends.
fn answer_line(editor: &Editor, line: &[u8]) -> Reply {
    // A command that does not parse — an Id outside the charset, an unknown
    // `cmd`, a missing field — is still answered against the id the client is
    // blocked on, if the line carries a readable one. Without that a client
    // waiting for its own reply waits forever.
    let id = || serde_json::from_slice::<RequestId>(line).map_or(0, |r| r.id);
    let bad = |message: String| Reply::Err {
        id: id(),
        code: ErrorCode::BadRequest,
        message,
    };

    let mut object = match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(object)) => object,
        Ok(_) => return bad("a request is one JSON object".into()),
        Err(e) => return bad(format!("bad request: {e}")),
    };
    let files = object.remove("files");
    let out = object.remove("out");
    let request = match serde_json::from_value::<Request>(Value::Object(object)) {
        Ok(request) => request,
        Err(e) => return bad(format!("bad request: {e}")),
    };

    let out = match out {
        None => None,
        Some(Value::String(path)) => Some(PathBuf::from(path)),
        Some(_) => return bad("`out` is a path".into()),
    };
    let wants = carries_bytes(&request.command);
    if wants.is_some_and(|b| b.payload) && out.is_none() {
        return bad(format!(
            "{} answers with bytes and needs an `out` path to write them to",
            request.command.tag()
        ));
    }

    let attachments = match read_files(files) {
        Ok(attachments) => attachments,
        Err(reply) => {
            return match reply {
                FilesError::Bad(message) => bad(message),
                FilesError::Io(message) => Reply::Err {
                    id: request.id,
                    code: ErrorCode::Io,
                    message,
                },
            }
        }
    };

    let (reply, payload) = editor.handle_with(request, attachments);
    match (payload, out) {
        (Some(payload), Some(path)) => match write_payload(&path, &payload) {
            Ok(()) => reply,
            // The command ran; only the handoff failed. Every payload command
            // is a read, so there is no edit stranded behind this.
            Err(e) => Reply::Err {
                id: match &reply {
                    Reply::Ok { id, .. } | Reply::Err { id, .. } => *id,
                    Reply::Event(_) => 0,
                },
                code: ErrorCode::Io,
                message: format!("writing {}: {e}", path.display()),
            },
        },
        _ => reply,
    }
}

enum FilesError {
    Bad(String),
    Io(String),
}

/// Read every path in a line's `files` map into the bytes the command wants.
fn read_files(files: Option<Value>) -> Result<Attachments, FilesError> {
    let mut attachments = Attachments::none();
    let Some(files) = files else {
        return Ok(attachments);
    };
    let Value::Object(files) = files else {
        return Err(FilesError::Bad(
            "`files` maps an attachment name to a path".into(),
        ));
    };
    for (name, path) in files {
        let Value::String(path) = path else {
            return Err(FilesError::Bad(format!(
                "the `files` entry for {name:?} is not a path"
            )));
        };
        let bytes = std::fs::read(&path)
            .map_err(|e| FilesError::Io(format!("reading {name:?} from {path}: {e}")))?;
        attachments.insert(name, bytes);
    }
    Ok(attachments)
}

fn write_payload(path: &Path, payload: &Payload) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &payload.bytes)
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
