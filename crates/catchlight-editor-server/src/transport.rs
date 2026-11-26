//! The editor over a Unix socket: one JSON object per line, in order.
//!
//! A line may carry `files` (attachment name to a client-side path) and `out`
//! (where a payload is written) beside the [`Request`]: the socket client
//! shares this process's filesystem, so bytes never ride in the line.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use catchlight_core::LoadLimits;
use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId, default_socket_path};
use serde_json::Value;

use super::{Attachments, Editor, Payload};
use crate::http::accepted;

pub(super) const MAX_SOCKET_CONNECTIONS: usize = 64;
pub(super) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(30);

pub fn serve_unix(editor: Arc<Editor>, path: &Path) -> std::io::Result<()> {
    let listener = bind_unix_listener(path)?;
    let connections = Arc::new(ConnectionLimiter::default());
    for mut stream in accepted(listener.incoming()) {
        let Some(_permit) = connections.try_acquire() else {
            let _ = write_reply(
                &mut stream,
                &Reply::Err {
                    op_index: None,
                    limit: None,
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
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        // The default may fall back to a shared temp dir, where another user
        // could have made the directory first and swap the socket inside it.
        if path == default_socket_path() {
            ensure_private_dir(parent)?;
        } else {
            ensure_owned_dir(parent)?;
        }
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

/// Refuses a directory another user owns or may write to: they could swap
/// the socket inside it.
pub(super) fn ensure_owned_dir(dir: &Path) -> std::io::Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(dir)?;
    let refuse = |why: &str| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("refusing the socket directory {}: {why}", dir.display()),
        ))
    };
    if !metadata.is_dir() {
        return refuse("it is not a directory");
    }
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return refuse("another user owns it");
    }
    if metadata.mode() & 0o022 != 0 {
        return refuse("other users may write to it");
    }
    Ok(metadata)
}

/// [`ensure_owned_dir`], and narrowed to `0700` when others may read it:
/// nobody else can have put anything in it.
pub(super) fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    if ensure_owned_dir(dir)?.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Held while a server checks for and replaces a stale socket. The file is
/// never removed: the OS releases the lock when its holder exits, however it
/// exits, so a killed server cannot block the next one.
struct SocketStartupLock {
    _file: std::fs::File,
}

impl SocketStartupLock {
    fn acquire(socket_path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(socket_lock_path(socket_path))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("another server is starting at {}", socket_path.display()),
            )),
            Err(std::fs::TryLockError::Error(err)) => Err(err),
        }
    }
}

pub(super) fn socket_lock_path(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("sock.lock")
}

pub(super) fn serve_connection(editor: &Editor, stream: UnixStream) {
    if stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT)).is_err()
    {
        return;
    }
    let Ok(read_half) = stream.try_clone() else {
        return;
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
                        op_index: None,
                        limit: None,
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

fn answer_line(editor: &Editor, line: &[u8]) -> Reply {
    // An unparsable command is still answered against the id the client is
    // blocked on, or it would wait forever.
    let id = || serde_json::from_slice::<RequestId>(line).map_or(0, |r| r.id);
    let bad = |message: String| Reply::Err {
        op_index: None,
        limit: None,
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
    let wants = request.command.carries_bytes();
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
                    op_index: None,
                    limit: None,
                    id: request.id,
                    code: ErrorCode::Io,
                    message,
                },
            };
        }
    };

    let (reply, payload) = editor.handle_with(request, attachments);
    match (payload, out) {
        (Some(payload), Some(path)) => match write_payload(&path, &payload) {
            Ok(()) => reply,
            // Every payload command is a read, so no edit is stranded here.
            Err(e) => Reply::Err {
                op_index: None,
                limit: None,
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
    let limit = LoadLimits::default().encoded_bytes;
    let mut total = 0u64;
    for (name, path) in files {
        let Value::String(path) = path else {
            return Err(FilesError::Bad(format!(
                "the `files` entry for {name:?} is not a path"
            )));
        };
        let io = |e: std::io::Error| FilesError::Io(format!("reading {name:?} from {path}: {e}"));
        let file = std::fs::File::open(&path).map_err(io)?;
        if !file.metadata().map_err(io)?.is_file() {
            return Err(FilesError::Bad(format!(
                "the `files` entry for {name:?} is not a regular file: {path}"
            )));
        }
        // One byte past the budget is enough for the editor to refuse it.
        let room = limit.saturating_sub(total).saturating_add(1);
        let mut bytes = Vec::new();
        file.take(room).read_to_end(&mut bytes).map_err(io)?;
        total = total.saturating_add(bytes.len() as u64);
        attachments.insert(name, bytes);
        if total > limit {
            break;
        }
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
