//! The browser's transport: one WebSocket for messages, HTTP for bytes.
//!
//! A tab holds a replica of a session's model and talks to this listener.
//! `/ws` carries exactly what the Unix socket carries — one JSON [`Request`]
//! per text frame, answered with its [`Reply`] — plus the events the socket
//! has no way to push: every [`Event`] the editor emits while a connection is
//! open arrives on it as `Reply::Event`. Everything that is *bytes* rather
//! than a message goes over HTTP instead, because a replica wants the
//! structure and the textures as payloads, not as base64 inside a frame.
//!
//! Invariants this module enforces:
//!
//! - **Loopback is not a permission.** Any page on any origin may open
//!   `ws://localhost:<port>` or send a simple cross-origin request here; the
//!   same-origin policy gates neither. So the server mints a random token per
//!   launch and every door needs it: `Authorization: Bearer <token>` on HTTP,
//!   `?token=` on the handshake. `GET /token` hands it out unauthenticated but
//!   answers with CORS headers **only** for an allowlisted origin and never
//!   `*`, so a foreign page can send that request and still not read it.
//!
//! - **The `Host` header must name loopback.** A name that resolves to
//!   127.0.0.1 today and to an attacker's address tomorrow is how DNS
//!   rebinding turns a local server into a remote one; a request whose `Host`
//!   is anything but `localhost`, `127.0.0.1` or `[::1]` is refused with 421
//!   before it is routed.
//!
//! - **A WebSocket's `Origin` must be allowlisted, and its absence is fine.**
//!   Browsers always send one, so a foreign page is caught; non-browser
//!   clients (the CLI, a test) send none and are let through on the token
//!   alone.
//!
//! - **Only the writer thread touches the socket.** A connection reads on one
//!   thread and writes on another, over two [`WebSocket`]s on the same TCP
//!   stream — which would interleave half-frames the moment the reader
//!   answered a ping itself. So the reader's socket writes go to a channel
//!   ([`Bounce`]) and the writer replays those bytes between its own frames.
//!
//! - **An event can precede the reply that caused it.** Observers fire inside
//!   [`Editor::handle`], so a client's own `node_add` may see
//!   `document_changed` before its `ok`. A client keyed on `rev` does not
//!   care; one that assumes reply-then-event does.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::Duration;

use catchlight_core::formats::clm::TextureEncoding;
use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId, SessionId, TexId};
use percent_encoding::percent_decode_str;
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Message, Role, WebSocket, WebSocketConfig};

use crate::storage::StagingStorage;
use crate::{Editor, EditorError};

/// One frame, one request — the same cap the Unix socket puts on one line.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// A request line plus its headers. Bounded before anything is parsed.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Concurrent connections, WebSockets included. Same budget as the socket.
const MAX_CONNECTIONS: usize = 64;
/// Bounds the HTTP phase only: a WebSocket clears its read timeout, because an
/// idle tab is a live tab.
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Default ceiling on a `PUT /files/{key}` body: a `.clm` with its textures.
const DEFAULT_MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

/// How [`serve_http`] authenticates and who it answers.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// Origins a browser may read a response from, beyond the server's own.
    pub allowed_origins: Vec<String>,
    /// The bearer token. `None` mints a fresh random one for this launch.
    pub token: Option<String>,
    /// Ceiling on a `PUT /files/{key}` body.
    pub max_upload_bytes: usize,
    /// Where `PUT /files/{key}` parks its bytes. This must be the very
    /// [`StagingStorage`] the [`Editor`] was built on, or an upload will not
    /// be visible to the `session_open` that names it. `None` refuses uploads.
    pub staging: Option<Arc<StagingStorage>>,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            token: None,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            staging: None,
        }
    }
}

/// A bound listener whose address and token are already known, so a caller can
/// print them (or a test can connect) before [`HttpServer::serve`] blocks.
pub struct HttpServer {
    pub addr: SocketAddr,
    pub token: String,
    listener: TcpListener,
    state: Arc<ServerState>,
}

struct ServerState {
    editor: Arc<Editor>,
    token: String,
    origins: Vec<String>,
    max_upload_bytes: usize,
    staging: Option<Arc<StagingStorage>>,
    connections: Arc<ConnectionLimiter>,
}

/// Bind `addr` without serving it. The allowlist is the server's own two
/// loopback origins plus [`HttpOptions::allowed_origins`], which is why it can
/// only be built once the ephemeral port is known.
pub fn bind_http(
    editor: Arc<Editor>,
    addr: SocketAddr,
    options: HttpOptions,
) -> io::Result<HttpServer> {
    let listener = TcpListener::bind(addr)?;
    let addr = listener.local_addr()?;
    let token = match options.token {
        Some(token) => token,
        None => mint_token()?,
    };
    let mut origins = vec![
        format!("http://localhost:{}", addr.port()),
        format!("http://127.0.0.1:{}", addr.port()),
    ];
    origins.extend(options.allowed_origins);
    Ok(HttpServer {
        addr,
        token: token.clone(),
        listener,
        state: Arc::new(ServerState {
            editor,
            token,
            origins,
            max_upload_bytes: options.max_upload_bytes,
            staging: options.staging,
            connections: Arc::new(ConnectionLimiter::default()),
        }),
    })
}

/// Bind and serve, blocking the calling thread — the HTTP twin of
/// [`serve_unix`](crate::serve_unix).
pub fn serve_http(editor: Arc<Editor>, addr: SocketAddr, options: HttpOptions) -> io::Result<()> {
    bind_http(editor, addr, options)?.serve()
}

impl HttpServer {
    /// Accept forever, one thread per connection.
    pub fn serve(self) -> io::Result<()> {
        for stream in self.listener.incoming() {
            let mut stream = stream?;
            let Some(permit) = self.state.connections.try_acquire() else {
                let _ = write_response(
                    &mut stream,
                    &Response::text(503, "Service Unavailable", "too many editor connections"),
                );
                continue;
            };
            let state = self.state.clone();
            std::thread::spawn(move || {
                let _permit = permit;
                serve_connection(&state, stream);
            });
        }
        Ok(())
    }
}

/// 32 bytes of OS randomness as hex. A guessable token is no token: every page
/// the user's browser loads can reach this port.
fn mint_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| io::Error::other(format!("no system randomness for a token: {e}")))?;
    Ok(bytes.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    }))
}

#[derive(Default)]
struct ConnectionLimiter {
    active: AtomicUsize,
}

impl ConnectionLimiter {
    fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_CONNECTIONS).then_some(active + 1)
            })
            .ok()
            .map(|_| ConnectionPermit(self.clone()))
    }
}

struct ConnectionPermit(Arc<ConnectionLimiter>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------- connection

fn serve_connection(state: &ServerState, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    if stream.set_read_timeout(Some(HTTP_IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(HTTP_IO_TIMEOUT)).is_err()
    {
        return;
    }
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;

    let request = match read_request(&mut reader, state.max_upload_bytes) {
        Ok(request) => request,
        Err(BadRequest::Closed | BadRequest::Io) => return,
        Err(err) => {
            let _ = write_response(&mut writer, &err.response());
            return;
        }
    };

    // Before routing: a Host that is not loopback is someone else's name for
    // this port.
    if !host_is_loopback(request.header("host")) {
        let _ = write_response(
            &mut writer,
            &Response::text(
                421,
                "Misdirected Request",
                "this server answers loopback only",
            ),
        );
        return;
    }

    let origin = request.header("origin").map(str::to_owned);
    let allowed = origin
        .as_deref()
        .filter(|o| state.origins.iter().any(|a| a == o))
        .map(str::to_owned);

    if request.method == "OPTIONS" {
        let _ = write_response(
            &mut writer,
            &cors(Response::new(204, "No Content"), allowed.as_deref()),
        );
        return;
    }

    if request.method == "GET" && request.path == "/ws" {
        serve_websocket(
            state,
            request,
            reader,
            writer,
            origin.is_some(),
            allowed.is_some(),
        );
        return;
    }

    let response = route(state, &request, allowed.as_deref());
    let _ = write_response(&mut writer, &response);
}

fn route(state: &ServerState, request: &HttpRequest, allowed: Option<&str>) -> Response {
    // Unauthenticated on purpose: the CORS headers, not the token, are what
    // keep a foreign page from reading this.
    if request.method == "GET" && request.path == "/token" {
        return cors(
            Response::new(200, "OK")
                .with("Content-Type", "application/json")
                .with("X-Content-Type-Options", "nosniff")
                .body(format!("{{\"token\":\"{}\"}}", state.token).into_bytes()),
            allowed,
        );
    }

    if !bearer_matches(request, &state.token) {
        return cors(
            Response::text(401, "Unauthorized", "missing or wrong bearer token"),
            allowed,
        );
    }

    let segments: Vec<&str> = request
        .path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();

    let response = match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["sessions", id, "structure"]) => structure(state, id),
        ("GET", ["sessions", id, "textures", tex]) => texture(state, id, tex),
        ("PUT", ["files", ..]) => put_file(state, request),
        _ => Response::text(404, "Not Found", "no such endpoint"),
    };
    cors(response, allowed)
}

/// The structure-only container plus the revision it describes, read under one
/// session lock so a client can never pair bytes with a revision they are not.
fn structure(state: &ServerState, id: &str) -> Response {
    let Some(session) = parse_session(id) else {
        return Response::text(400, "Bad Request", "session id is not a number");
    };
    let read = state.editor.with_session(session, |s| {
        let bytes = s.model.to_structure_bytes()?;
        Ok((s.rev, bytes))
    });
    match read {
        Ok((rev, bytes)) => Response::new(200, "OK")
            .with("Content-Type", "application/octet-stream")
            .with("X-Catchlight-Rev", rev.to_string())
            .body(bytes),
        Err(EditorError::NoSession(_)) => Response::text(404, "Not Found", "no such session"),
        Err(err) => Response::text(500, "Internal Server Error", &err.to_string()),
    }
}

fn texture(state: &ServerState, id: &str, tex: &str) -> Response {
    let Some(session) = parse_session(id) else {
        return Response::text(400, "Bad Request", "session id is not a number");
    };
    let Ok(tex) = tex.parse::<TexId>() else {
        return Response::text(400, "Bad Request", "texture id is outside the Id charset");
    };
    let read = state.editor.with_model(session, |m| {
        m.texture(&tex).map(|t| (t.encoding, t.data.clone()))
    });
    match read {
        Ok(Some((encoding, data))) => {
            let (mime, name) = match encoding {
                TextureEncoding::Png => ("image/png", "png"),
                TextureEncoding::Tga => ("image/x-tga", "tga"),
            };
            Response::new(200, "OK")
                .with("Content-Type", mime)
                .with("X-Catchlight-Encoding", name)
                .body(data.as_ref().clone())
        }
        Ok(None) => Response::text(404, "Not Found", "no such texture"),
        Err(_) => Response::text(404, "Not Found", "no such session"),
    }
}

/// Stage an upload under the key a later `session_open` will name. The bytes
/// never touch the server's disk — see [`StagingStorage`].
fn put_file(state: &ServerState, request: &HttpRequest) -> Response {
    let Some(staging) = &state.staging else {
        return Response::text(503, "Service Unavailable", "this server stages no uploads");
    };
    let Some(raw) = request.path.strip_prefix("/files/") else {
        return Response::text(404, "Not Found", "no such endpoint");
    };
    let key = decode(raw);
    if key.is_empty() {
        return Response::text(400, "Bad Request", "empty file key");
    }
    staging.put(&key, request.body.clone());
    Response::new(204, "No Content")
}

fn parse_session(id: &str) -> Option<SessionId> {
    id.parse::<u64>().ok().map(SessionId)
}

// ----------------------------------------------------------------- websocket

/// What the writer thread owes the socket, in order.
enum Outbound {
    /// A reply or a pushed event, framed by the writer's own [`WebSocket`].
    Text(String),
    /// Bytes the reader's [`WebSocket`] produced — a pong, or the echo of a
    /// close. Already framed; the writer replays them verbatim.
    Raw(Vec<u8>),
    /// Send a close frame and stop.
    Close,
}

/// The reader's stream: reads come off the socket, writes go to the writer
/// thread. Two `WebSocket`s share one TCP stream, so exactly one of them may
/// write to it.
struct Bounce {
    read: TcpStream,
    out: Sender<Outbound>,
}

impl Read for Bounce {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read.read(buf)
    }
}

impl Write for Bounce {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out
            .send(Outbound::Raw(buf.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "websocket writer is gone"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serve_websocket(
    state: &ServerState,
    request: HttpRequest,
    reader: BufReader<TcpStream>,
    mut writer: TcpStream,
    has_origin: bool,
    origin_allowed: bool,
) {
    if !request
        .query
        .get("token")
        .is_some_and(|given| secret_matches(given, &state.token))
    {
        let _ = write_response(
            &mut writer,
            &Response::text(401, "Unauthorized", "missing or wrong token"),
        );
        return;
    }
    // No Origin is a non-browser client; a wrong one is a page that should not
    // be here.
    if has_origin && !origin_allowed {
        let _ = write_response(
            &mut writer,
            &Response::text(403, "Forbidden", "origin is not allowed"),
        );
        return;
    }
    let upgrading = request
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
        && request.header("connection").is_some_and(|c| {
            c.split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
        });
    let Some(key) = request.header("sec-websocket-key") else {
        let _ = write_response(
            &mut writer,
            &Response::text(400, "Bad Request", "not a websocket handshake"),
        );
        return;
    };
    if !upgrading || request.header("sec-websocket-version") != Some("13") {
        let _ = write_response(
            &mut writer,
            &Response::text(400, "Bad Request", "not a websocket handshake"),
        );
        return;
    }

    let accept = derive_accept_key(key.as_bytes());
    let handshake = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if writer.write_all(handshake.as_bytes()).is_err() || writer.flush().is_err() {
        return;
    }

    // An idle tab is a live tab, so the read side blocks indefinitely; the
    // write side keeps its timeout so a stuck peer cannot pin a thread.
    let leftover = reader.buffer().to_vec();
    let read_half = reader.into_inner();
    if read_half.set_read_timeout(None).is_err() {
        return;
    }

    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_REQUEST_BYTES))
        .max_frame_size(Some(MAX_REQUEST_BYTES));
    let (tx, rx) = mpsc::channel::<Outbound>();
    let mut out_ws = WebSocket::from_raw_socket(writer, Role::Server, Some(config));
    let pump = std::thread::spawn(move || {
        while let Ok(outbound) = rx.recv() {
            let alive = match outbound {
                Outbound::Text(text) => out_ws.send(Message::text(text)).is_ok(),
                Outbound::Raw(bytes) => {
                    out_ws.flush().is_ok() && out_ws.get_mut().write_all(&bytes).is_ok()
                }
                Outbound::Close => {
                    let _ = out_ws.close(None);
                    let _ = out_ws.flush();
                    false
                }
            };
            if !alive {
                break;
            }
        }
        // Whatever ended this, unblock the reader parked in `read()`.
        let _ = out_ws.get_mut().shutdown(Shutdown::Both);
    });

    let events = tx.clone();
    let observer = state.editor.subscribe(Box::new(move |event| {
        if let Ok(json) = serde_json::to_string(&Reply::Event(event.clone())) {
            let _ = events.send(Outbound::Text(json));
        }
    }));
    eprintln!("catchlight-editor-server: websocket open");

    let mut in_ws = WebSocket::from_partially_read(
        Bounce {
            read: read_half,
            out: tx.clone(),
        },
        leftover,
        Role::Server,
        Some(config),
    );
    loop {
        let text = match in_ws.read() {
            Ok(Message::Text(text)) => text.as_str().to_string(),
            // The protocol is JSON text; a binary frame is a client bug, and
            // it still deserves an answer rather than a silent drop.
            Ok(Message::Binary(_)) => {
                let err = Reply::Err {
                    id: 0,
                    code: ErrorCode::BadRequest,
                    message: "expected a text frame carrying one JSON request".into(),
                };
                let Ok(json) = serde_json::to_string(&err) else {
                    break;
                };
                if tx.send(Outbound::Text(json)).is_err() {
                    break;
                }
                continue;
            }
            Ok(Message::Close(_)) => break,
            // Ping and Pong are answered inside `read`, through `Bounce`.
            Ok(_) => continue,
            Err(_) => break,
        };
        if tx
            .send(Outbound::Text(answer(&state.editor, &text)))
            .is_err()
        {
            break;
        }
    }

    state.editor.unsubscribe(observer);
    let _ = tx.send(Outbound::Close);
    drop(tx);
    let _ = pump.join();
    eprintln!("catchlight-editor-server: websocket closed");
}

/// One frame in, one reply out. A frame that does not parse is still answered
/// against the id the client is blocked on, exactly as the socket does it.
fn answer(editor: &Editor, frame: &str) -> String {
    let reply = match serde_json::from_str::<Request>(frame) {
        Ok(request) => editor.handle(request),
        Err(e) => Reply::Err {
            id: serde_json::from_str::<RequestId>(frame).map_or(0, |r| r.id),
            code: ErrorCode::BadRequest,
            message: format!("bad request: {e}"),
        },
    };
    serde_json::to_string(&reply).unwrap_or_else(|_| {
        r#"{"reply":"err","id":0,"code":"bad_request","message":"reply could not be serialized"}"#
            .to_string()
    })
}

// ---------------------------------------------------------------------- http

struct HttpRequest {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    /// `name` must be lowercase; header names are folded on the way in.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

enum BadRequest {
    /// The peer went away without sending anything.
    Closed,
    Io,
    Malformed(&'static str),
    HeadersTooLarge,
    BodyTooLarge,
}

impl BadRequest {
    fn response(&self) -> Response {
        match self {
            Self::Closed | Self::Io => Response::new(400, "Bad Request"),
            Self::Malformed(why) => Response::text(400, "Bad Request", why),
            Self::HeadersTooLarge => {
                Response::text(431, "Request Header Fields Too Large", "headers too large")
            }
            Self::BodyTooLarge => Response::text(413, "Payload Too Large", "body too large"),
        }
    }
}

fn read_request(
    reader: &mut BufReader<TcpStream>,
    max_body: usize,
) -> Result<HttpRequest, BadRequest> {
    let mut budget = MAX_HEADER_BYTES;
    let Some(start) = read_line(reader, &mut budget)? else {
        return Err(BadRequest::Closed);
    };
    let mut parts = start.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or_default();
    if method.is_empty() || !target.starts_with('/') || !version.starts_with("HTTP/1.") {
        return Err(BadRequest::Malformed("bad request line"));
    }

    let mut headers = Vec::new();
    loop {
        let Some(line) = read_line(reader, &mut budget)? else {
            return Err(BadRequest::Malformed("headers ended without a blank line"));
        };
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(BadRequest::Malformed("header without a colon"));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    let find = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    };
    // One framing rule only: a body is exactly Content-Length bytes long.
    if find("transfer-encoding").is_some() {
        return Err(BadRequest::Malformed("transfer-encoding is not supported"));
    }
    let length = match find("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| BadRequest::Malformed("bad content-length"))?,
        None => 0,
    };
    // The generous ceiling belongs to the one endpoint that takes a payload;
    // every other route is buffered before it is authenticated, so it gets the
    // frame-sized cap instead.
    let ceiling = if method == "PUT" && target.starts_with("/files/") {
        max_body
    } else {
        MAX_REQUEST_BYTES
    };
    if length > ceiling {
        return Err(BadRequest::BodyTooLarge);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).map_err(|_| BadRequest::Io)?;

    let (path, raw_query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query),
        None => (target.clone(), ""),
    };
    let mut query = HashMap::new();
    for pair in raw_query.split('&').filter(|s| !s.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(decode(key), decode(value));
    }

    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn read_line(reader: &mut impl BufRead, budget: &mut usize) -> Result<Option<String>, BadRequest> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| BadRequest::Io)?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(BadRequest::Malformed("truncated header"))
            };
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |pos| pos + 1);
        if take > *budget {
            return Err(BadRequest::HeadersTooLarge);
        }
        *budget -= take;
        let terminated = available[take - 1] == b'\n';
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if terminated {
            break;
        }
    }
    while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        line.pop();
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| BadRequest::Malformed("header line is not utf-8"))
}

struct Response {
    status: u16,
    reason: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn text(status: u16, reason: &'static str, message: &str) -> Self {
        Self::new(status, reason)
            .with("Content-Type", "text/plain; charset=utf-8")
            .body(message.as_bytes().to_vec())
    }

    fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    fn body(mut self, bytes: Vec<u8>) -> Self {
        self.body = bytes;
        self
    }
}

fn write_response(writer: &mut impl Write, response: &Response) -> io::Result<()> {
    // Every response closes its connection: no keep-alive means no second
    // framing rule to get wrong.
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes())?;
    writer.write_all(&response.body)?;
    writer.flush()
}

/// Never `*`: the token lives behind these headers, so an origin is echoed
/// only when it is on the allowlist.
fn cors(response: Response, origin: Option<&str>) -> Response {
    let response = response.with("Vary", "Origin");
    match origin {
        Some(origin) => response
            .with("Access-Control-Allow-Origin", origin.to_string())
            .with(
                "Access-Control-Allow-Headers",
                "Authorization, Content-Type",
            )
            .with("Access-Control-Allow-Methods", "GET, PUT, OPTIONS")
            .with(
                "Access-Control-Expose-Headers",
                "X-Catchlight-Rev, X-Catchlight-Encoding",
            ),
        None => response,
    }
}

fn host_is_loopback(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let name = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((inner, _)) => inner,
            None => return false,
        },
        None => host.split(':').next().unwrap_or(host),
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

fn bearer_matches(request: &HttpRequest, token: &str) -> bool {
    request
        .header("authorization")
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .is_some_and(|(_, given)| secret_matches(given.trim(), token))
}

/// Constant time in the length it compares: a page on another origin can time
/// this endpoint even though it cannot read the answer.
fn secret_matches(given: &str, token: &str) -> bool {
    let (given, token) = (given.as_bytes(), token.as_bytes());
    given.len() == token.len()
        && given
            .iter()
            .zip(token)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn decode(raw: &str) -> String {
    percent_decode_str(raw).decode_utf8_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loopback_host_is_served() {
        assert!(host_is_loopback(Some("localhost:8080")));
        assert!(host_is_loopback(Some("127.0.0.1")));
        assert!(host_is_loopback(Some("[::1]:9")));
        assert!(!host_is_loopback(Some("evil.example")));
        assert!(!host_is_loopback(Some("localhost.evil.example:8080")));
        assert!(!host_is_loopback(None));
    }

    #[test]
    fn a_bearer_has_to_match_whole() {
        assert!(secret_matches("abc", "abc"));
        assert!(!secret_matches("ab", "abc"));
        assert!(!secret_matches("abd", "abc"));
    }

    #[test]
    fn a_minted_token_is_hex_and_not_the_previous_one() {
        let a = mint_token().unwrap();
        let b = mint_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
