#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

//! The Unix socket's own glue: `files` in, `out` out.
//!
//! A socket client and the editor share a filesystem, so bytes never cross the
//! line — the client names its own files and the server reads them, and a
//! payload is written where the line said to put it. These drive a real
//! listener over a real socket, because the framing is what is being checked.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, NodeKindArg, Reply, Request, ResponseBody, SessionId,
    TextureEncoding,
};
use catchlight_editor_server::{serve_unix, Editor};
use serde_json::{json, Value};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A listener, its socket path, and a directory the test may write into.
struct Fixture {
    stream: UnixStream,
    dir: PathBuf,
    next_id: u64,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start() -> Fixture {
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "catchlight-socket-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("editor.sock");

    let editor = Arc::new(Editor::new());
    let listening = socket.clone();
    std::thread::spawn(move || {
        let _ = serve_unix(editor, &listening);
    });

    // The listener binds on another thread; connect as soon as it is there.
    let mut waited = Duration::ZERO;
    let stream = loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => break stream,
            Err(_) if waited < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(20));
                waited += Duration::from_millis(20);
            }
            Err(e) => panic!("the editor never listened at editor.sock: {e}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    Fixture {
        stream,
        dir,
        next_id: 1,
    }
}

impl Fixture {
    /// One line up, one reply back. `extra` is merged into the request object,
    /// which is how `files` and `out` ride beside a command.
    fn line(&mut self, command: Command, extra: Value) -> Reply {
        let id = self.next_id;
        self.next_id += 1;
        let mut object = match serde_json::to_value(Request { id, command }).unwrap() {
            Value::Object(object) => object,
            other => panic!("a request is an object, got {other:?}"),
        };
        if let Value::Object(extra) = extra {
            object.extend(extra);
        }
        let mut text = serde_json::to_string(&object).unwrap();
        text.push('\n');
        self.stream.write_all(text.as_bytes()).unwrap();

        let mut reader = BufReader::new(self.stream.try_clone().unwrap());
        loop {
            let mut buf = String::new();
            assert!(
                reader.read_line(&mut buf).unwrap() > 0,
                "the server closed the connection"
            );
            let buf = buf.trim();
            if buf.is_empty() {
                continue;
            }
            let reply: Reply = serde_json::from_str(buf).unwrap();
            match &reply {
                Reply::Ok { id: at, .. } | Reply::Err { id: at, .. } if *at == id => return reply,
                _ => continue,
            }
        }
    }

    fn send(&mut self, command: Command) -> Reply {
        self.line(command, json!({}))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

fn ok(reply: Reply) -> ResponseBody {
    match reply {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn err(reply: Reply) -> (u64, ErrorCode, String) {
    match reply {
        Reply::Err { id, code, message } => (id, code, message),
        other => panic!("expected Err, got {other:?}"),
    }
}

fn new_session(fixture: &mut Fixture) -> SessionId {
    match ok(fixture.send(Command::SessionNew { name: None })) {
        ResponseBody::Session { session } => session,
        other => panic!("expected Session, got {other:?}"),
    }
}

fn a_part(fixture: &mut Fixture, session: SessionId) -> NodeId {
    let root = match ok(fixture.send(Command::NodeTree { session })) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("expected Tree, got {other:?}"),
    };
    match ok(fixture.send(Command::NodeAdd {
        session,
        parent: root,
        kind: NodeKindArg::Part,
        name: Some("Face".into()),
        node: None,
    })) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("expected Node, got {other:?}"),
    }
}

fn write_png(path: &Path) {
    let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([12, 34, 56, 255]));
    image.save(path).expect("a 4x4 png writes");
}

#[test]
fn a_files_map_names_the_clients_own_image() {
    let mut fixture = start();
    let session = new_session(&mut fixture);
    let node = a_part(&mut fixture, session);
    let image = fixture.path("face.png");
    write_png(&image);

    let reply = fixture.line(
        Command::TextureAdd {
            session,
            node,
            encoding: TextureEncoding::Png,
            texture: None,
        },
        json!({ "files": { "texture": image.display().to_string() } }),
    );
    assert!(matches!(ok(reply), ResponseBody::Texture { .. }));
}

#[test]
fn a_file_that_cannot_be_read_is_answered_against_the_request() {
    let mut fixture = start();
    let session = new_session(&mut fixture);
    let node = a_part(&mut fixture, session);
    let missing = fixture.path("nowhere.png");

    let sent = fixture.next_id;
    let reply = fixture.line(
        Command::TextureAdd {
            session,
            node,
            encoding: TextureEncoding::Png,
            texture: None,
        },
        json!({ "files": { "texture": missing.display().to_string() } }),
    );
    let (id, code, message) = err(reply);
    assert_eq!(id, sent, "answered against the request that named it");
    assert_eq!(code, ErrorCode::Io);
    assert!(message.contains("nowhere.png"), "message was {message:?}");
}

#[test]
fn a_preview_writes_its_png_where_out_says() {
    let mut fixture = start();
    let session = new_session(&mut fixture);
    let out = fixture.path("shots/frame.png");

    let reply = fixture.line(
        Command::Preview {
            session,
            pose: Vec::new(),
            size: Some([40, 24]),
            camera: None,
        },
        json!({ "out": out.display().to_string() }),
    );
    match ok(reply) {
        ResponseBody::Preview { preview } => {
            assert_eq!([preview.width, preview.height], [40, 24]);
        }
        other => panic!("expected Preview, got {other:?}"),
    }
    // The parent directory did not exist; the glue made it.
    let written = std::fs::read(&out).expect("the payload landed at `out`");
    let decoded = image::load_from_memory(&written).expect("it is a png");
    assert_eq!((decoded.width(), decoded.height()), (40, 24));
}

#[test]
fn a_payload_command_with_nowhere_to_put_it_is_refused_before_it_runs() {
    let mut fixture = start();
    let session = new_session(&mut fixture);

    let reply = fixture.send(Command::Preview {
        session,
        pose: Vec::new(),
        size: Some([16, 16]),
        camera: None,
    });
    let (_, code, message) = err(reply);
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(message.contains("out"), "message was {message:?}");
}

/// The sibling keys are lifted off the line before it is read as a request, so
/// a command with neither behaves exactly as it did.
#[test]
fn a_line_with_no_siblings_is_the_request_it_always_was() {
    let mut fixture = start();
    let session = new_session(&mut fixture);
    assert!(matches!(
        ok(fixture.send(Command::Status { session })),
        ResponseBody::Status { .. }
    ));
}

#[test]
fn a_files_entry_that_is_not_a_path_is_a_bad_request() {
    let mut fixture = start();
    let session = new_session(&mut fixture);
    let node = a_part(&mut fixture, session);

    let reply = fixture.line(
        Command::TextureAdd {
            session,
            node,
            encoding: TextureEncoding::Png,
            texture: None,
        },
        json!({ "files": { "texture": 7 } }),
    );
    assert_eq!(err(reply).1, ErrorCode::BadRequest);
}
