//! The editor over a Unix socket: one JSON object per line, in order.
//!
//! A line may carry `files` (attachment name to a client-side path) and `out`
//! (where a payload is written) beside the [`Request`]: the socket client
//! shares this process's filesystem, so bytes never ride in the line.

use std::io::{BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use catchlight_core::LoadLimits;
use catchlight_editor_discovery::default_socket_path;
use catchlight_editor_protocol::{ErrorCode, Reply, Request};
use serde_json::Value;

use crate::doors::{
    ConnectionLimiter, Line, LineError, MAX_REQUEST_BYTES, accepted, read_line, refuse_line,
};
use crate::{Attachments, Editor, Payload};

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
        let mut budget = MAX_REQUEST_BYTES;
        let line = match read_line(&mut reader, &mut budget, |_| Ok(())) {
            Ok(Line::Terminated(line) | Line::Unterminated(line)) => line,
            Ok(Line::End) | Err(LineError::Io) => break,
            Err(LineError::TooLong) => {
                let _ = write_reply(
                    &mut writer,
                    &Reply::Err {
                        op_index: None,
                        limit: None,
                        id: 0,
                        code: ErrorCode::BadRequest,
                        message: format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
                    },
                );
                break;
            }
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
    let bad = |message: String| refuse_line(line, message);

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

fn write_reply(writer: &mut impl Write, reply: &Reply) -> std::io::Result<()> {
    let mut encoded = serde_json::to_vec(reply).map_err(std::io::Error::other)?;
    encoded.push(b'\n');
    writer.write_all(&encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SOCKET_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// In a directory of this user's own: the shared temp directory is one
    /// an explicit socket path is refused in.
    fn socket_test_path(label: &str) -> PathBuf {
        let sequence = SOCKET_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("catchlight-sockets-{}", std::process::id()));
        DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700)
            .create(&dir)
            .unwrap();
        dir.join(format!("{label}-{sequence}.sock"))
    }

    #[test]
    fn socket_startup_survives_the_lock_of_a_server_that_died() {
        let path = socket_test_path("lock");
        let lock = socket_lock_path(&path);
        std::fs::write(&lock, b"").unwrap();

        let listener = bind_unix_listener(&path).unwrap();
        drop(listener);
        std::fs::remove_file(&path).unwrap();

        let held = std::fs::File::open(&lock).unwrap();
        held.lock().unwrap();
        let err = bind_unix_listener(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        drop(held);
        std::fs::remove_file(lock).unwrap();
    }

    #[test]
    fn a_socket_directory_others_may_write_to_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = socket_test_path("private").with_extension("d");
        std::fs::create_dir(&dir).unwrap();
        let mode =
            |mode| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let current = || std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;

        for open in [0o777, 0o770, 0o702] {
            mode(open);
            let err = ensure_private_dir(&dir).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{open:o}");
            assert_eq!(current(), open, "a refused directory is left as it was");
        }
        mode(0o755);
        ensure_private_dir(&dir).unwrap();
        assert_eq!(current(), 0o700);
        std::fs::remove_dir(&dir).unwrap();

        // Root owns `/`; no test runs as root.
        if rustix::process::getuid().as_raw() != 0 {
            let err = ensure_private_dir(Path::new("/")).unwrap_err();
            assert!(err.to_string().contains("another user"), "{err}");
        }
    }

    #[test]
    fn a_named_socket_directory_is_refused_when_others_may_write_to_it_and_kept_as_it_is() {
        use std::os::unix::fs::PermissionsExt;
        let dir = socket_test_path("named").with_extension("d");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("server.sock");
        let mode =
            |mode| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        let current = || std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;

        mode(0o777);
        let err = bind_unix_listener(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("may write"), "{err}");
        mode(0o755);
        drop(bind_unix_listener(&path).unwrap());
        assert_eq!(
            current(),
            0o755,
            "a directory this user owns is not narrowed"
        );
        if rustix::process::getuid().as_raw() != 0 {
            let err = ensure_owned_dir(Path::new("/")).unwrap_err();
            assert!(err.to_string().contains("another user"), "{err}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn socket_startup_does_not_replace_a_regular_file() {
        let path = socket_test_path("regular");
        std::fs::write(&path, b"keep").unwrap();

        let err = bind_unix_listener(&path).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(socket_lock_path(&path)).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn socket_startup_preserves_a_live_listener() {
        let path = socket_test_path("live");
        let listener = bind_unix_listener(&path).unwrap();

        let err = bind_unix_listener(&path).unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        UnixStream::connect(&path).unwrap();
        drop(listener);
        std::fs::remove_file(socket_lock_path(&path)).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn socket_startup_replaces_a_stale_socket() {
        let path = socket_test_path("stale");
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);

        let listener = bind_unix_listener(&path).unwrap();

        UnixStream::connect(&path).unwrap();
        drop(listener);
        std::fs::remove_file(socket_lock_path(&path)).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn socket_connection_rejects_an_oversized_request() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let editor = Editor::new();
            serve_connection(&editor, server);
        });
        client
            .write_all(&vec![b'x'; MAX_REQUEST_BYTES + 1])
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();

        assert!(matches!(
            reply,
            Reply::Err { id: 0, code: ErrorCode::BadRequest, message, .. }
                if message.contains("request exceeds")
        ));
        server_thread.join().unwrap();
    }

    #[test]
    fn socket_files_must_name_regular_files() {
        let (client, server) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let editor = Editor::new();
            serve_connection(&editor, server);
        });
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        for (id, path) in [(1u64, "/dev/zero"), (2, "/")] {
            let line = serde_json::json!({
                "id": id,
                "cmd": "texture_add",
                "session": 1,
                "node": "face",
                "files": { "texture": path },
            });
            writer.write_all(format!("{line}\n").as_bytes()).unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            match serde_json::from_str::<Reply>(&reply).unwrap() {
                Reply::Err {
                    id: got,
                    code,
                    message,
                    ..
                } => {
                    assert_eq!(got, id);
                    assert_eq!(code, ErrorCode::BadRequest, "{message}");
                    assert!(message.contains("not a regular file"), "{message}");
                }
                other => panic!("{path} was read: {other:?}"),
            }
        }
        drop(writer);
        drop(reader);
        server_thread.join().unwrap();
    }

    /// A bad Id must be refused with the request's id: a client blocking on
    /// its own id would otherwise hang.
    #[test]
    fn an_invalid_id_on_the_wire_is_an_error_against_the_requests_own_id() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let editor = Editor::new();
            serve_connection(&editor, server);
        });
        for line in [
            br#"{"id":11,"cmd":"node_delete","session":1,"node":"a b"}"#.as_slice(),
            br#"{"id":12,"cmd":"node_delete","session":1,"node":".hidden"}"#.as_slice(),
            br#"{"id":13,"cmd":"no_such_command","session":1}"#.as_slice(),
        ] {
            client.write_all(line).unwrap();
            client.write_all(b"\n").unwrap();
        }
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut reader = BufReader::new(client);
        for want in [11u64, 12, 13] {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            match serde_json::from_str::<Reply>(&line).unwrap() {
                Reply::Err {
                    id, code, message, ..
                } => {
                    assert_eq!(id, want, "the reply correlates to the request");
                    assert_eq!(code, ErrorCode::BadRequest);
                    assert!(message.starts_with("bad request:"), "{message}");
                }
                other => panic!("expected an error, got {other:?}"),
            }
        }
        server_thread.join().unwrap();
    }
}
