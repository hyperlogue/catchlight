#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The browser's channels: messages over a WebSocket or one POST, bytes over
//! HTTP.
//!
//! Loopback is not a permission — any page the user's browser loads can reach
//! this port — so most of what follows is about who is refused. The rest
//! checks that a tab can do its job: send the same commands the Unix socket
//! takes, hear about edits another connection made, pull a session's structure
//! against the revision it belongs to, and hand up bytes it fetched itself.
//! `POST /request` is the same commands for a client that wants no socket, so
//! what it is checked on is where a status ends and a `Reply::Err` begins.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, NodeKindArg, Reply, Request, ResponseBody, SessionId, SessionInfo,
};
use catchlight_editor_server::{bind_http, Editor, HttpOptions, StagingStorage, Storage};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

/// Fixed so a failing run says nothing about the machine it ran on.
const TOKEN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const ALLOWED_ORIGIN: &str = "http://example.test";
const FOREIGN_ORIGIN: &str = "http://evil.test";

// -------------------------------------------------------------- the fixture

struct Server {
    addr: SocketAddr,
    /// The very staging map the uploads land in, so a test can ask what the
    /// server is still holding after a command read it.
    staging: Arc<StagingStorage>,
    /// What a save writes through to.
    store: Arc<Mem>,
}

fn start() -> Server {
    start_with(|_| {})
}

/// The fixture with one knob turned — an upload ceiling low enough to hit, or
/// a keepalive interval a test can afford to wait for.
fn start_with(tune: impl FnOnce(&mut HttpOptions)) -> Server {
    // Nothing here is allowed to touch the filesystem, so the staging layer
    // stands on a store in memory: an upload is the only way bytes get in, and
    // a save lands where a test can read it back.
    let store = Arc::new(Mem::default());
    let staging = Arc::new(StagingStorage::new(store.clone()));
    let editor = Arc::new(Editor::with_storage(staging.clone()));
    let mut options = HttpOptions {
        allowed_origins: vec![ALLOWED_ORIGIN.to_string()],
        token: Some(TOKEN.to_string()),
        max_upload_bytes: 8 * 1024 * 1024,
        staging: Some(staging.clone()),
        ..HttpOptions::default()
    };
    tune(&mut options);
    let server = bind_http(editor, "127.0.0.1:0".parse().unwrap(), options).unwrap();
    let addr = server.addr;
    std::thread::spawn(move || {
        let _ = server.serve();
    });
    Server {
        addr,
        staging,
        store,
    }
}

/// A byte store in memory. These tests write no files.
#[derive(Debug, Default)]
struct Mem(Mutex<HashMap<String, Vec<u8>>>);

impl Storage for Mem {
    fn read(&self, key: &str) -> std::io::Result<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, key.to_string()))
    }

    fn write(&self, key: &str, bytes: &[u8]) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }
}

// ---------------------------------------------------------- a tiny client

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

fn http(
    addr: SocketAddr,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut head = format!("{method} {target} HTTP/1.1\r\n");
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("host")) {
        head.push_str(&format!("Host: 127.0.0.1:{}\r\n", addr.port()));
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("content-length"))
    {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .expect("a status line")
        .parse()
        .unwrap();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').expect("a header");
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .map(|(_, v)| v.parse().unwrap())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();
    HttpResponse {
        status,
        headers,
        body,
    }
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

/// One command over `POST /request`, the way a client with no socket sends it.
fn post(addr: SocketAddr, request: &Request) -> HttpResponse {
    http(
        addr,
        "POST",
        "/request",
        &[
            ("Authorization", &bearer()),
            ("Content-Type", "application/json"),
        ],
        serde_json::to_vec(request).unwrap().as_slice(),
    )
}

/// The reply a 200 carries.
fn posted(response: &HttpResponse) -> Reply {
    assert_eq!(response.status, 200, "POST /request");
    assert_eq!(response.header("content-type"), Some("application/json"));
    serde_json::from_slice(&response.body).expect("the body is one reply")
}

type Socket = WebSocket<TcpStream>;

fn connect(addr: SocketAddr, token: &str, origin: Option<&str>) -> Result<Socket, String> {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = format!("ws://127.0.0.1:{}/ws?token={token}", addr.port())
        .into_client_request()
        .unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", origin.parse().unwrap());
    }
    tungstenite::client::client(request, stream)
        .map(|(socket, _)| socket)
        .map_err(|err| err.to_string())
}

fn send(socket: &mut Socket, request: Request) {
    socket
        .send(Message::text(serde_json::to_string(&request).unwrap()))
        .unwrap();
}

/// The next reply to `id`, skipping pushed events.
fn reply_to(socket: &mut Socket, id: u64) -> Reply {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let reply = read_frame(socket);
        let seen = match &reply {
            Reply::Ok { id, .. } | Reply::Err { id, .. } => *id,
            Reply::Event(_) => continue,
        };
        if seen == id {
            return reply;
        }
    }
    panic!("no reply to request {id}");
}

fn read_frame(socket: &mut Socket) -> Reply {
    loop {
        match socket.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(text.as_str()).unwrap(),
            Message::Close(_) => panic!("the server closed the connection"),
            _ => continue,
        }
    }
}

fn body_of(reply: Reply) -> ResponseBody {
    match reply {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn rev_of(reply: &Reply) -> u64 {
    match reply {
        Reply::Ok { rev, .. } => rev.expect("a session command reports its rev"),
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn new_session(socket: &mut Socket, id: u64) -> (SessionId, u64) {
    send(
        socket,
        Request {
            id,
            command: Command::SessionNew { name: None },
        },
    );
    let reply = reply_to(socket, id);
    let rev = rev_of(&reply);
    match body_of(reply) {
        ResponseBody::Session { session } => (session, rev),
        other => panic!("expected Session, got {other:?}"),
    }
}

fn root_of(socket: &mut Socket, id: u64, session: SessionId) -> NodeId {
    send(
        socket,
        Request {
            id,
            command: Command::NodeTree { session },
        },
    );
    match body_of(reply_to(socket, id)) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("expected Tree, got {other:?}"),
    }
}

/// Open the document staged under `key`, expecting it to succeed.
fn open_session(socket: &mut Socket, id: u64, key: &str) -> SessionId {
    send(
        socket,
        Request {
            id,
            command: Command::SessionOpen {
                path: key.to_string(),
            },
        },
    );
    match body_of(reply_to(socket, id)) {
        ResponseBody::Session { session } => session,
        other => panic!("expected Session, got {other:?}"),
    }
}

/// The `session_list` entry for `session`.
fn session_info(socket: &mut Socket, id: u64, session: SessionId) -> SessionInfo {
    send(
        socket,
        Request {
            id,
            command: Command::SessionList,
        },
    );
    match body_of(reply_to(socket, id)) {
        ResponseBody::Sessions { sessions } => sessions
            .into_iter()
            .find(|s| s.session == session)
            .expect("the session this test opened is listed"),
        other => panic!("expected Sessions, got {other:?}"),
    }
}

/// Stage `bytes` under `key`, the way a tab hands up a file it read.
fn upload(server: &Server, key: &str, bytes: &[u8]) {
    let put = http(
        server.addr,
        "PUT",
        &format!("/files/{key}"),
        &[("Authorization", &bearer())],
        bytes,
    );
    assert_eq!(put.status, 204, "PUT /files/{key}");
}

/// An empty model's `.clm` bytes, built in-process so no fixture is needed.
fn clm_bytes() -> Vec<u8> {
    let store = Arc::new(Mem::default());
    let editor = Editor::with_storage(store.clone());
    let session = match editor.handle(Request {
        id: 1,
        command: Command::SessionNew { name: None },
    }) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("expected Session, got {other:?}"),
    };
    editor.handle(Request {
        id: 2,
        command: Command::Save {
            session,
            path: Some("scratch.clm".into()),
        },
    });
    store.read("scratch.clm").unwrap()
}

// ---------------------------------------------------------------- the tests

#[test]
fn the_token_is_readable_only_from_an_allowlisted_origin() {
    let server = start();

    let allowed = http(
        server.addr,
        "GET",
        "/token",
        &[("Origin", ALLOWED_ORIGIN)],
        b"",
    );
    assert_eq!(allowed.status, 200);
    assert_eq!(
        allowed.header("access-control-allow-origin"),
        Some(ALLOWED_ORIGIN)
    );
    assert_eq!(allowed.header("x-content-type-options"), Some("nosniff"));
    assert!(String::from_utf8_lossy(&allowed.body).contains(TOKEN));

    // A foreign page may send this request; the browser will not let it read
    // the answer, because there is no header saying it may.
    let foreign = http(
        server.addr,
        "GET",
        "/token",
        &[("Origin", FOREIGN_ORIGIN)],
        b"",
    );
    assert_eq!(foreign.status, 200);
    assert_eq!(foreign.header("access-control-allow-origin"), None);
}

#[test]
fn a_handshake_needs_the_token_and_a_known_origin() {
    let server = start();

    assert!(connect(server.addr, "not-the-token", None).is_err());
    assert!(connect(server.addr, TOKEN, Some(FOREIGN_ORIGIN)).is_err());
    // No Origin at all is a non-browser client: the token is the whole gate.
    assert!(connect(server.addr, TOKEN, None).is_ok());
    assert!(connect(server.addr, TOKEN, Some(ALLOWED_ORIGIN)).is_ok());
}

#[test]
fn a_command_over_the_socket_answers_with_its_revision() {
    let server = start();
    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let (session, rev) = new_session(&mut socket, 1);
    assert_eq!(session, SessionId(1));
    assert_eq!(rev, 0);
}

#[test]
fn an_edit_on_one_connection_is_pushed_to_the_other() {
    let server = start();
    let mut watcher = connect(server.addr, TOKEN, None).unwrap();
    let mut editor = connect(server.addr, TOKEN, None).unwrap();

    let (session, _) = new_session(&mut editor, 1);
    let root = root_of(&mut editor, 2, session);
    send(
        &mut editor,
        Request {
            id: 3,
            command: Command::NodeAdd {
                session,
                parent: root,
                kind: NodeKindArg::Group,
                name: Some("Hat".into()),
                node: None,
            },
        },
    );
    let rev = rev_of(&reply_to(&mut editor, 3));

    // The watcher sent nothing, so everything it hears is pushed. It hears
    // about the session appearing first, then about the edit.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "no document_changed arrived");
        if let Reply::Event(catchlight_editor_protocol::Event::DocumentChanged {
            session: changed,
            rev: at,
        }) = read_frame(&mut watcher)
        {
            if changed == session && at == rev {
                break;
            }
        }
    }
}

// ------------------------------------------------------- POST /request

#[test]
fn a_command_over_post_answers_with_its_revision() {
    let server = start();

    let reply = posted(&post(
        server.addr,
        &Request {
            id: 1,
            command: Command::SessionNew { name: None },
        },
    ));
    assert_eq!(rev_of(&reply), 0);
    let session = match reply {
        Reply::Ok {
            id,
            body: ResponseBody::Session { session },
            ..
        } => {
            assert_eq!(id, 1, "the reply is answered against the id that was sent");
            session
        }
        other => panic!("expected Session, got {other:?}"),
    };

    // That POST closed its connection, and the session outlived it: a session
    // belongs to the editor, never to whatever carried the command that made
    // it.
    let reply = posted(&post(
        server.addr,
        &Request {
            id: 2,
            command: Command::NodeTree { session },
        },
    ));
    assert!(matches!(body_of(reply), ResponseBody::Tree { .. }));
}

/// The split this route exists to make: a status describes the transport, and
/// everything the editor itself decided comes back 200 carrying `Reply::Err`.
#[test]
fn a_command_the_editor_refuses_over_post_is_a_200_carrying_the_error() {
    let server = start();
    let response = post(
        server.addr,
        &Request {
            id: 7,
            command: Command::NodeTree {
                session: SessionId(999),
            },
        },
    );
    match posted(&response) {
        Reply::Err { id, code, .. } => {
            assert_eq!(id, 7);
            assert_eq!(code, ErrorCode::NoSession);
        }
        other => panic!("expected Err, got {other:?}"),
    }
}

/// A body that is not a request at all names no id to answer against, so there
/// is no reply to be made and the status carries it instead.
#[test]
fn a_post_that_is_not_a_request_is_a_400() {
    let server = start();
    for body in [&b"{}"[..], b"not json at all", b"[1,2,3]", b""] {
        let response = http(
            server.addr,
            "POST",
            "/request",
            &[("Authorization", &bearer())],
            body,
        );
        assert_eq!(
            response.status,
            400,
            "body {:?}",
            String::from_utf8_lossy(body)
        );
    }
}

/// The gate closes on the head alone here too: the length declared below is
/// well under the cap, so nothing but the token can decide, and the body it
/// promises is never sent.
#[test]
fn a_post_without_a_token_is_refused_before_its_body() {
    let server = start();
    let started = Instant::now();
    let response = http(
        server.addr,
        "POST",
        "/request",
        &[("Content-Length", "500000")],
        b"",
    );
    assert_eq!(response.status, 401);
    assert_eq!(response.header("connection"), Some("close"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the 401 waited on a body that was never sent"
    );

    // A wrong token is the same refusal, body and all.
    let refused = http(
        server.addr,
        "POST",
        "/request",
        &[("Authorization", "Bearer not-the-token")],
        &serde_json::to_vec(&Request {
            id: 1,
            command: Command::SessionNew { name: None },
        })
        .unwrap(),
    );
    assert_eq!(refused.status, 401);

    // And neither of those reached the editor: no session was made.
    match body_of(posted(&post(
        server.addr,
        &Request {
            id: 2,
            command: Command::SessionList,
        },
    ))) {
        ResponseBody::Sessions { sessions } => assert!(sessions.is_empty()),
        other => panic!("expected Sessions, got {other:?}"),
    }
}

/// One POST is one command, so it gets the cap the socket puts on one line.
/// Only a little over it, so the body is drained and the status arrives
/// instead of the reset that answering over unread bytes would be.
#[test]
fn a_post_over_the_request_cap_is_413() {
    let server = start();
    let response = http(
        server.addr,
        "POST",
        "/request",
        &[("Authorization", &bearer())],
        &vec![b'x'; 2 * 1024 * 1024],
    );
    assert_eq!(response.status, 413);
    assert_eq!(response.header("connection"), Some("close"));
    assert_eq!(String::from_utf8_lossy(&response.body), "body too large");
}

/// The reason `/ws` stays: a POST answers the caller and pushes nothing, while
/// the tab watching hears about the edit anyway. An observer is the editor's,
/// not the connection's, so the POST is long closed by the time this arrives.
#[test]
fn an_edit_made_over_post_reaches_a_websocket_subscriber() {
    let server = start();
    let mut watcher = connect(server.addr, TOKEN, None).unwrap();

    let session = match body_of(posted(&post(
        server.addr,
        &Request {
            id: 1,
            command: Command::SessionNew { name: None },
        },
    ))) {
        ResponseBody::Session { session } => session,
        other => panic!("expected Session, got {other:?}"),
    };
    let root = match body_of(posted(&post(
        server.addr,
        &Request {
            id: 2,
            command: Command::NodeTree { session },
        },
    ))) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("expected Tree, got {other:?}"),
    };
    let rev = rev_of(&posted(&post(
        server.addr,
        &Request {
            id: 3,
            command: Command::NodeAdd {
                session,
                parent: root,
                kind: NodeKindArg::Group,
                name: Some("Hat".into()),
                node: None,
            },
        },
    )));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "no document_changed arrived");
        if let Reply::Event(catchlight_editor_protocol::Event::DocumentChanged {
            session: changed,
            rev: at,
        }) = read_frame(&mut watcher)
        {
            if changed == session && at == rev {
                break;
            }
        }
    }
}

// -------------------------------------------------------------- byte routes

#[test]
fn the_structure_endpoint_pairs_its_bytes_with_the_revision_they_are() {
    let server = start();
    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let (session, _) = new_session(&mut socket, 1);
    let root = root_of(&mut socket, 2, session);
    send(
        &mut socket,
        Request {
            id: 3,
            command: Command::NodeAdd {
                session,
                parent: root,
                kind: NodeKindArg::Group,
                name: Some("Hat".into()),
                node: None,
            },
        },
    );
    let reply = reply_to(&mut socket, 3);
    let rev = rev_of(&reply);
    let added = match body_of(reply) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("expected Node, got {other:?}"),
    };

    let response = http(
        server.addr,
        "GET",
        &format!("/sessions/{}/structure", session.0),
        &[("Authorization", &bearer()), ("Origin", ALLOWED_ORIGIN)],
        b"",
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.header("x-catchlight-rev"), Some(&*rev.to_string()));
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some(ALLOWED_ORIGIN)
    );
    let structure = catchlight_core::formats::clm::decode_structure(&response.body).unwrap();
    assert!(structure.doc.nodes.iter().any(|node| node.id == added));

    let missing = http(
        server.addr,
        "GET",
        "/sessions/999/structure",
        &[("Authorization", &bearer())],
        b"",
    );
    assert_eq!(missing.status, 404);
}

#[test]
fn an_upload_is_what_session_open_reads() {
    let server = start();
    let bytes = clm_bytes();

    upload(&server, "model.clm", &bytes);

    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    send(
        &mut socket,
        Request {
            id: 1,
            command: Command::SessionOpen {
                path: "model.clm".into(),
            },
        },
    );
    match body_of(reply_to(&mut socket, 1)) {
        ResponseBody::Session { .. } => {}
        other => panic!("expected Session, got {other:?}"),
    }

    // Nothing was staged under this key, and there is no filesystem behind
    // the staging layer, so the open fails rather than reading somewhere else.
    send(
        &mut socket,
        Request {
            id: 2,
            command: Command::SessionOpen {
                path: "never-uploaded.clm".into(),
            },
        },
    );
    assert!(matches!(reply_to(&mut socket, 2), Reply::Err { .. }));
}

#[test]
fn an_open_releases_the_upload_it_read_and_claims_no_file() {
    let server = start();
    upload(&server, "model.clm", &clm_bytes());

    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let session = open_session(&mut socket, 1, "model.clm");

    // The model owns its own copy, so the server holds no second one. Without
    // this every `.clm` a tab opened stayed in memory for the process life.
    assert!(
        server.staging.staged_keys().is_empty(),
        "still staged: {:?}",
        server.staging.staged_keys()
    );
    // And an upload is not a file: the key named bytes in flight, not
    // something on the far side of the store.
    let info = session_info(&mut socket, 2, session);
    assert_eq!(info.file, None);
    // The title still comes from the key, which is what a person recognizes.
    assert_eq!(info.title, "model");
}

#[test]
fn a_session_opened_from_an_upload_has_nowhere_to_save_until_it_is_told() {
    let server = start();
    upload(&server, "model.clm", &clm_bytes());

    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let session = open_session(&mut socket, 1, "model.clm");

    // A bare save would otherwise write `model.clm` into the server's working
    // directory — a file the tab never asked for and cannot see.
    send(
        &mut socket,
        Request {
            id: 2,
            command: Command::Save {
                session,
                path: None,
            },
        },
    );
    match reply_to(&mut socket, 2) {
        Reply::Err { code, .. } => assert_eq!(code, ErrorCode::NoSavePath),
        other => panic!("expected Err, got {other:?}"),
    }

    // Named, it saves through to the backing store and keeps the key.
    send(
        &mut socket,
        Request {
            id: 3,
            command: Command::Save {
                session,
                path: Some("saved/model.clm".into()),
            },
        },
    );
    match body_of(reply_to(&mut socket, 3)) {
        ResponseBody::Saved { path } => assert_eq!(path, "saved/model.clm"),
        other => panic!("expected Saved, got {other:?}"),
    }
    let written = server.store.read("saved/model.clm").expect("a save landed");
    catchlight_core::Model::from_clm_bytes(&written).expect("the saved bytes load as a model");
    assert_eq!(
        session_info(&mut socket, 4, session).file.as_deref(),
        Some("saved/model.clm")
    );
}

#[test]
fn a_failed_open_leaves_its_bytes_staged_to_retry() {
    let server = start();
    upload(&server, "broken.clm", b"not a model file");

    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    send(
        &mut socket,
        Request {
            id: 1,
            command: Command::SessionOpen {
                path: "broken.clm".into(),
            },
        },
    );
    assert!(matches!(reply_to(&mut socket, 1), Reply::Err { .. }));

    // The upload is the caller's only copy: dropping it on a failure would
    // make a retry a re-upload.
    assert_eq!(server.staging.staged_keys(), vec!["broken.clm".to_string()]);
}

#[test]
fn a_texture_add_releases_the_image_it_read() {
    let server = start();
    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let (session, _) = new_session(&mut socket, 1);
    let root = root_of(&mut socket, 2, session);
    send(
        &mut socket,
        Request {
            id: 3,
            command: Command::NodeAdd {
                session,
                parent: root,
                kind: NodeKindArg::Part,
                name: Some("Face".into()),
                node: None,
            },
        },
    );
    let part = match body_of(reply_to(&mut socket, 3)) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("expected Node, got {other:?}"),
    };

    upload(&server, "face.png", &one_pixel_png());
    send(
        &mut socket,
        Request {
            id: 4,
            command: Command::TextureAdd {
                session,
                node: part.clone(),
                path: "face.png".into(),
                texture: None,
            },
        },
    );
    assert!(matches!(
        body_of(reply_to(&mut socket, 4)),
        ResponseBody::Texture { .. }
    ));
    assert!(server.staging.staged_keys().is_empty());

    // A texture that failed to decode keeps its upload for the retry.
    upload(&server, "torn.png", b"PNG-ish but not");
    send(
        &mut socket,
        Request {
            id: 5,
            command: Command::TextureAdd {
                session,
                node: part,
                path: "torn.png".into(),
                texture: None,
            },
        },
    );
    assert!(matches!(reply_to(&mut socket, 5), Reply::Err { .. }));
    assert_eq!(server.staging.staged_keys(), vec!["torn.png".to_string()]);
}

#[test]
fn a_texture_comes_back_as_the_payload_the_model_holds() {
    let server = start();
    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let (session, _) = new_session(&mut socket, 1);
    let root = root_of(&mut socket, 2, session);
    send(
        &mut socket,
        Request {
            id: 3,
            command: Command::NodeAdd {
                session,
                parent: root,
                kind: NodeKindArg::Part,
                name: Some("Face".into()),
                node: None,
            },
        },
    );
    let part = match body_of(reply_to(&mut socket, 3)) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("expected Node, got {other:?}"),
    };

    // The texture arrives the way a tab sends one: an upload, then a command
    // naming the key it was staged under.
    let png = one_pixel_png();
    assert_eq!(
        http(
            server.addr,
            "PUT",
            "/files/face.png",
            &[("Authorization", &bearer())],
            &png
        )
        .status,
        204
    );
    send(
        &mut socket,
        Request {
            id: 4,
            command: Command::TextureAdd {
                session,
                node: part,
                path: "face.png".into(),
                texture: None,
            },
        },
    );
    let texture = match body_of(reply_to(&mut socket, 4)) {
        ResponseBody::Texture { texture, .. } => texture,
        other => panic!("expected Texture, got {other:?}"),
    };

    let response = http(
        server.addr,
        "GET",
        &format!("/sessions/{}/textures/{texture}", session.0),
        &[("Authorization", &bearer())],
        b"",
    );
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("image/png"));
    assert_eq!(response.header("x-catchlight-encoding"), Some("png"));
    assert_eq!(response.body, png);

    // An Id outside the charset never reaches a lookup.
    assert_eq!(
        http(
            server.addr,
            "GET",
            &format!("/sessions/{}/textures/NOT AN ID", session.0),
            &[("Authorization", &bearer())],
            b""
        )
        .status,
        400
    );
}

fn one_pixel_png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("a 1x1 png encodes");
    bytes.into_inner()
}

#[test]
fn the_clm_endpoint_hands_back_a_file_that_loads() {
    let server = start();
    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let (session, _) = new_session(&mut socket, 1);
    let root = root_of(&mut socket, 2, session);

    let response = http(
        server.addr,
        "GET",
        &format!("/sessions/{}/clm", session.0),
        &[("Authorization", &bearer())],
        b"",
    );
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("application/octet-stream")
    );

    // The point of the route: what comes back is a model file, and it is this
    // session's model rather than any model.
    let model =
        catchlight_core::Model::from_clm_bytes(&response.body).expect("the bytes load as a model");
    assert!(model.node(&root).is_some());

    assert_eq!(
        http(
            server.addr,
            "GET",
            "/sessions/9999/clm",
            &[("Authorization", &bearer())],
            b""
        )
        .status,
        404
    );
}

#[test]
fn a_foreign_host_header_is_refused_before_anything_is_routed() {
    let server = start();
    let response = http(
        server.addr,
        "GET",
        "/token",
        &[("Host", "evil.example")],
        b"",
    );
    assert_eq!(response.status, 421);
}

#[test]
fn bytes_need_the_bearer_token() {
    let server = start();
    let mut socket = connect(server.addr, TOKEN, None).unwrap();
    let (session, _) = new_session(&mut socket, 1);

    let target = format!("/sessions/{}/structure", session.0);
    assert_eq!(http(server.addr, "GET", &target, &[], b"").status, 401);
    assert_eq!(
        http(
            server.addr,
            "GET",
            &target,
            &[("Authorization", "Bearer not-the-token")],
            b""
        )
        .status,
        401
    );
    assert_eq!(
        http(
            server.addr,
            "GET",
            &target,
            &[("Authorization", &bearer())],
            b""
        )
        .status,
        200
    );
}

#[test]
fn a_preflight_is_answered_for_an_allowlisted_origin_only() {
    let server = start();
    let allowed = http(
        server.addr,
        "OPTIONS",
        "/sessions/1/structure",
        &[
            ("Origin", ALLOWED_ORIGIN),
            ("Access-Control-Request-Method", "GET"),
        ],
        b"",
    );
    assert_eq!(allowed.status, 204);
    assert_eq!(
        allowed.header("access-control-allow-headers"),
        Some("Authorization, Content-Type")
    );
    // A cross-origin `POST /request` is preflighted, so the method has to be
    // named here or a browser never sends the command.
    assert_eq!(
        allowed.header("access-control-allow-methods"),
        Some("GET, POST, PUT, OPTIONS")
    );

    let foreign = http(
        server.addr,
        "OPTIONS",
        "/sessions/1/structure",
        &[("Origin", FOREIGN_ORIGIN)],
        b"",
    );
    assert_eq!(foreign.status, 204);
    assert_eq!(foreign.header("access-control-allow-origin"), None);
}

#[test]
fn an_upload_far_over_the_ceiling_is_refused_unread() {
    let server = start();
    let response = http(
        server.addr,
        "PUT",
        "/files/too-big.clm",
        &[
            ("Authorization", &bearer()),
            // The body is never sent, and at 200 MB it is far past what the
            // server will drain anyway: the length alone decides, and the
            // answer waits on no bytes.
            ("Content-Length", "200000000"),
        ],
        b"",
    );
    assert_eq!(response.status, 413);
}

/// A body only a little over the ceiling is read and thrown away before the
/// 413 goes out. Answering with bytes still unread closes the socket on the
/// sender, and a client reports that reset instead of the status it was sent.
#[test]
fn an_upload_over_the_ceiling_is_drained_so_its_413_arrives() {
    let server = start_with(|options| options.max_upload_bytes = 1024);
    // Far past what the socket buffers between here and there will hold, so
    // this write finishes only if the server is genuinely reading the body it
    // has already refused.
    let body = vec![0u8; 4 * 1024 * 1024];

    let response = http(
        server.addr,
        "PUT",
        "/files/too-big.clm",
        &[("Authorization", &bearer())],
        &body,
    );
    assert_eq!(response.status, 413);
    assert_eq!(response.header("connection"), Some("close"));
    assert_eq!(String::from_utf8_lossy(&response.body), "body too large");
}

/// An upload without a token is refused against its headers alone. The body
/// is never sent, so an answer that waits for one never arrives; the length
/// here is well under the ceiling, which leaves the token as the only thing
/// that can decide.
#[test]
fn an_unauthorized_upload_is_answered_before_its_body() {
    let server = start();
    let started = Instant::now();
    let response = http(
        server.addr,
        "PUT",
        "/files/not-yours.clm",
        &[
            ("Authorization", "Bearer not-the-token"),
            ("Content-Length", "4000000"),
        ],
        b"",
    );
    assert_eq!(response.status, 401);
    assert_eq!(response.header("connection"), Some("close"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the 401 waited on a body that was never sent"
    );
    // And nothing was staged under a key the caller had no right to name.
    assert!(server.staging.staged_keys().is_empty());
}

/// A tab that is only watching sends nothing for minutes, so the server keeps
/// the connection warm itself and takes the pong as proof the tab is there.
#[test]
fn an_idle_socket_is_pinged_and_survives_answering() {
    let server = start_with(|options| options.ping_interval = Duration::from_millis(500));
    let mut socket = connect(server.addr, TOKEN, None).unwrap();

    // Twice: the second ping only arrives if the first pong reset the idle
    // clock, rather than the connection simply not having run out yet.
    await_ping(&mut socket);
    await_ping(&mut socket);

    // And it is still a socket a command goes over.
    let (session, _) = new_session(&mut socket, 1);
    assert_eq!(session, SessionId(1));
}

/// Read until the keepalive ping arrives, then flush the pong tungstenite
/// queued for it — a queued pong goes out on the next call, not on its own.
fn await_ping(socket: &mut Socket) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Message::Ping(_) = socket.read().unwrap() {
            socket.flush().unwrap();
            return;
        }
    }
    panic!("no keepalive ping arrived");
}
