//! The editor over HTTP: a WebSocket for a tab, one POST for a script, and
//! bytes for both.
//!
//! A tab holds a replica of a session's model and talks to this listener.
//! `/ws` carries exactly what the Unix socket carries — one JSON [`Request`]
//! per text frame, answered with its [`Reply`] — plus the events the socket
//! has no way to push: every [`Event`] the editor emits while a connection is
//! open arrives on it as `Reply::Event`. Everything that is *bytes* rather
//! than a message goes over HTTP instead, because a replica wants the
//! structure, the whole file and the textures as payloads, not as base64
//! inside a frame.
//!
//! `POST /request` is the third door onto the same [`Editor::handle`]: one
//! JSON [`Request`] as the body, its [`Reply`] as the answer. It is there for
//! a client that only ever blocks on the command it just sent — a script, an
//! agent — and would otherwise have to implement WebSocket masking and a
//! reader thread in order to hear nothing it did not ask for. `/ws` stays, and
//! stays the only way to be *told* something: a route that answers a request
//! can push no event.
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
//! - **A texture is shared, not copied.** Payloads are the whole reason this
//!   listener exists, so a response body is either bytes built for the answer
//!   or the very [`Arc<[u8]>`](Arc) the model holds, cloned by refcount under
//!   the session lock and written straight to the socket.
//!
//! - **A byte extension is fetched by hash, like a texture by Id.** The
//!   structure feed carries a `{size, hash}` marker and never the bytes, so a
//!   replica that sees a hash it does not hold fetches that one key from
//!   `GET /sessions/{id}/extensions/{key}` and nothing else moves. A JSON
//!   extension has no route: it is already in the structure.
//!
//! - **The token is checked before a body is read.** A multipart
//!   `POST /request` may carry [`HttpOptions::max_upload_bytes`] — a quarter
//!   of a gigabyte by default — and buffering that for a request that turns
//!   out to have no token is memory any page on any origin could spend. So a
//!   connection
//!   reads the request line and the headers, settles `Host`, origin and
//!   bearer token against those alone, and only then reads the body. A
//!   request refused there is answered with its body still in the socket,
//!   which is one more reason no response here keeps its connection alive.
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
//! - **Bytes cross on `POST /request` and nowhere else.** There is no upload
//!   route and no key a client can put bytes under: a command that carries
//!   bytes goes to that one route as `multipart/form-data` — a part named
//!   `request` holding the command JSON, one part per attachment named for
//!   the attachment — so a blob nothing ever names cannot exist here. When
//!   the reply carries bytes back they *are* the response
//!   body, under the payload's own content type, and the JSON reply rides in
//!   `X-Catchlight-Reply` — escaped to ASCII (`\uXXXX`) so a header value can
//!   hold it and still parse as the same JSON. `/ws` refuses such a command
//!   with [`ErrorCode::BulkOverHttp`] instead of carrying it: that channel is
//!   text frames for low-latency commands and events, and one large frame
//!   blocks every frame queued behind it.
//!
//! - **On `POST /request`, a status is a transport failure and a refusal is a
//!   200.** The two are what a caller confuses otherwise. The status describes
//!   this listener only: 401 for a missing or wrong token, 413 for a body over
//!   the route's ceiling ([`MAX_REQUEST_BYTES`] for a JSON body,
//!   [`HttpOptions::max_upload_bytes`] for a multipart one), 400 for a body
//!   that is not one [`Request`], or multipart this parser will not take. Every
//!   refusal the editor itself decided — an unknown Id, a session that is not
//!   open — is a 200 carrying `Reply::Err`, exactly as the socket answers it.
//!
//! - **An event can precede the reply that caused it.** Observers fire inside
//!   [`Editor::handle`], so a client's own `node_add` may see
//!   `document_changed` before its `ok`. A client keyed on `rev` does not
//!   care; one that assumes reply-then-event does.
//!
//! - **An idle socket is proved live, not assumed.** A tab that is only
//!   watching sends nothing for minutes, and a proxy between it and here reaps
//!   a connection that carries no bytes. So the reader parks with a timeout of
//!   [`HttpOptions::ping_interval`] rather than forever: each expiry asks the
//!   writer for a ping, and silence lasting two intervals ends the connection
//!   from this side, where the observer can be unsubscribed, instead of
//!   leaving a thread parked on a socket nobody is on the other end of.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use catchlight_core::formats::clm::{extension_hash, TextureEncoding};
use catchlight_editor_protocol::{
    ErrorCode, ExtensionKey, Reply, Request, RequestId, SessionId, TexId,
};
use percent_encoding::percent_decode_str;
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Message, Role, WebSocket, WebSocketConfig};

use crate::{carries_bytes, Attachments, Editor, EditorError, Payload};

/// One frame, one request — the same cap the Unix socket puts on one line, and
/// the same one a `POST /request` body gets.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// A request line plus its headers. Bounded before anything is parsed.
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Concurrent connections, WebSockets included. Same budget as the socket.
const MAX_CONNECTIONS: usize = 64;
/// Bounds the HTTP phase. A WebSocket re-arms this as its keepalive interval
/// instead, because an idle tab is a live tab but a silent one is not.
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Default ceiling on a multipart `POST /request` body: a `.clm` with its
/// textures, or a manifest and every image it names.
const DEFAULT_MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
/// How long a WebSocket may say nothing before it is pinged; two of these with
/// no answer and the peer is treated as gone.
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(30);
/// How much of a body that has already earned a 413 is read and thrown away.
/// Closing a socket with bytes still unread in it is a reset, and a sender
/// reports that reset instead of the status it was sent, so draining first is
/// what makes the answer arrive at all; over loopback the discarded bytes cost
/// little more than a memcpy. Past this the trade flips — dragging hundreds of
/// megabytes across just to be polite costs more than the reset does — and the
/// connection closes under the sender instead.
const MAX_DRAIN_BYTES: usize = 8 * 1024 * 1024;

/// How [`serve_http`] authenticates and who it answers.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// Origins a browser may read a response from, beyond the server's own.
    pub allowed_origins: Vec<String>,
    /// The bearer token. `None` mints a fresh random one for this launch.
    pub token: Option<String>,
    /// Ceiling on a multipart `POST /request` body — the one route that
    /// carries a document rather than a command.
    pub max_upload_bytes: usize,
    /// How often an idle WebSocket is pinged, and half the silence it puts up
    /// with before the connection is dropped. The default suits a browser tab;
    /// shorten it only to exercise the keepalive.
    pub ping_interval: Duration,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            token: None,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            ping_interval: DEFAULT_PING_INTERVAL,
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
    ping_interval: Duration,
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
            ping_interval: options.ping_interval,
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

    let (mut request, body_len) = match read_head(&mut reader) {
        Ok(head) => head,
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
        // The handshake hands the reader to the socket, so a declared body
        // would be read as frames rather than skipped. No handshake has one.
        if body_len != 0 {
            let _ = write_response(
                &mut writer,
                &Response::text(400, "Bad Request", "a handshake carries no body"),
            );
            return;
        }
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

    // The gate closes on the head alone. A refusal here leaves the body
    // unread and the connection is dropped under it, which is what keeps an
    // unauthenticated caller from spending the upload ceiling.
    if !opens_without_a_token(&request) && !bearer_matches(&request, &state.token) {
        let _ = write_response(
            &mut writer,
            &cors(
                Response::text(401, "Unauthorized", "missing or wrong bearer token"),
                allowed.as_deref(),
            ),
        );
        return;
    }

    request.body = match read_body(&mut reader, &request, body_len, state.max_upload_bytes) {
        Ok(body) => body,
        Err(BadRequest::Closed | BadRequest::Io) => return,
        Err(err) => {
            let _ = write_response(&mut writer, &err.response());
            return;
        }
    };

    let response = route(state, &request, allowed.as_deref());
    let _ = write_response(&mut writer, &response);
}

/// The one door the token does not gate: the CORS headers, not the token, are
/// what keep a foreign page from reading `GET /token`.
fn opens_without_a_token(request: &HttpRequest) -> bool {
    request.method == "GET" && request.path == "/token"
}

/// Dispatch a request the gate in [`serve_connection`] has already let
/// through. Only [`opens_without_a_token`] arrives here unauthenticated.
fn route(state: &ServerState, request: &HttpRequest, allowed: Option<&str>) -> Response {
    if opens_without_a_token(request) {
        return cors(
            Response::new(200, "OK")
                .with("Content-Type", "application/json")
                .with("X-Content-Type-Options", "nosniff")
                .body(format!("{{\"token\":\"{}\"}}", state.token).into_bytes()),
            allowed,
        );
    }

    let segments: Vec<&str> = request
        .path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();

    let response = match (request.method.as_str(), segments.as_slice()) {
        ("POST", ["request"]) => command(state, request),
        ("GET", ["sessions", id, "structure"]) => structure(state, id),
        ("GET", ["sessions", id, "clm"]) => clm(state, id),
        ("GET", ["sessions", id, "textures", tex]) => texture(state, id, tex),
        ("GET", ["sessions", id, "extensions", key]) => extension(state, id, key),
        _ => Response::text(404, "Not Found", "no such endpoint"),
    };
    cors(response, allowed)
}

/// One request in the body, its reply in the answer. The same [`Editor`] entry
/// `/ws` and the Unix socket reach, so a caller gets the same answer here — a
/// refusal included, which is a 200 carrying `Reply::Err` rather than a status.
/// Only a body that is not a [`Request`] at all is a 400: at that point there
/// is no `id` to answer against and nothing the editor was asked to do.
///
/// Two body shapes by `Content-Type`. `application/json` (or anything else) is
/// the command alone. `multipart/form-data` is the command in a part named
/// `request` and one part per attachment, named for the attachment — which is
/// the only way bytes reach the editor over HTTP.
fn command(state: &ServerState, request: &HttpRequest) -> Response {
    let (parsed, attachments) = match multipart_boundary(request) {
        None => (
            serde_json::from_slice::<Request>(&request.body),
            Attachments::none(),
        ),
        Some(boundary) => match parse_multipart(&request.body, &boundary) {
            Err(why) => return Response::text(400, "Bad Request", why),
            Ok(parts) => {
                let mut attachments = Attachments::none();
                let mut command = None;
                for (name, bytes) in parts {
                    if name == "request" {
                        command = Some(bytes);
                    } else {
                        attachments.insert(name, bytes);
                    }
                }
                let Some(command) = command else {
                    return Response::text(400, "Bad Request", "no part named `request`");
                };
                (serde_json::from_slice::<Request>(&command), attachments)
            }
        },
    };
    let parsed = match parsed {
        Ok(parsed) => parsed,
        Err(e) => return Response::text(400, "Bad Request", &format!("bad request: {e}")),
    };

    let (reply, payload) = state.editor.handle_with(parsed, attachments);
    let json = match serde_json::to_vec(&reply) {
        Ok(json) => json,
        Err(e) => return Response::text(500, "Internal Server Error", &e.to_string()),
    };
    match payload {
        // The bytes are the body, so the reply rides in a header. It is
        // escaped to ASCII first: a header value is bytes, not UTF-8, and a
        // model's name may be anything.
        Some(Payload {
            content_type,
            bytes,
        }) => Response::new(200, "OK")
            .with("Content-Type", content_type)
            .with("X-Content-Type-Options", "nosniff")
            .with("X-Catchlight-Reply", ascii_json(&json))
            .body(bytes),
        None => Response::new(200, "OK")
            .with("Content-Type", "application/json")
            .with("X-Content-Type-Options", "nosniff")
            .body(json),
    }
}

/// One JSON document with every non-ASCII character written as a `\uXXXX`
/// escape, so it fits in a header value and still parses as the same JSON.
///
/// Safe as a byte-wise pass because JSON puts non-ASCII only inside string
/// literals, and `serde_json` has already escaped the control characters a
/// header could not carry.
fn ascii_json(json: &[u8]) -> String {
    let text = String::from_utf8_lossy(json);
    if text.is_ascii() {
        return text.into_owned();
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// The `boundary` parameter of a `multipart/form-data` content type, if the
/// request is one.
fn multipart_boundary(request: &HttpRequest) -> Option<String> {
    let content_type = request.header("content-type")?;
    let (kind, params) = content_type.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in params.split(';') {
        let (name, value) = param.split_once('=')?;
        if name.trim().eq_ignore_ascii_case("boundary") {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

/// `multipart/form-data`, strictly: `--boundary`, then one part per
/// `Content-Disposition: form-data; name="…"` with its headers, a blank line
/// and its bytes, and a closing `--boundary--`.
///
/// Hand-written because this server is: it speaks HTTP over a [`TcpStream`]
/// with no framework under it, and one form encoding is less to own than a
/// dependency. What that costs is strictness — CRLF everywhere, a name on
/// every part, the closing boundary required — and anything else is a 400
/// rather than a guess. A part's `filename` is ignored: the part's *name* is
/// the attachment's name, and the file it came from is the client's business.
fn parse_multipart(body: &[u8], boundary: &str) -> Result<Vec<(String, Vec<u8>)>, &'static str> {
    let opening = format!("--{boundary}");
    let delimiter = format!("\r\n--{boundary}");
    if !body.starts_with(opening.as_bytes()) {
        return Err("the body does not open with its boundary");
    }
    let mut at = opening.len();
    let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
    loop {
        let rest = &body[at..];
        if rest.starts_with(b"--") {
            return Ok(parts);
        }
        let Some(after) = rest.strip_prefix(b"\r\n") else {
            return Err("a boundary is followed by CRLF or by the closing --");
        };
        at += 2;
        let Some(head_len) = find(after, b"\r\n\r\n") else {
            return Err("a part's headers are not terminated");
        };
        let name = part_name(&after[..head_len])?;
        let data_at = at + head_len + 4;
        let Some(data_len) = find(&body[data_at..], delimiter.as_bytes()) else {
            return Err("a part is not terminated by its boundary");
        };
        if parts.iter().any(|(known, _)| *known == name) {
            return Err("two parts share one name");
        }
        parts.push((name, body[data_at..data_at + data_len].to_vec()));
        at = data_at + data_len + delimiter.len();
    }
}

/// The `name` of one part, from its `Content-Disposition` header.
fn part_name(headers: &[u8]) -> Result<String, &'static str> {
    let headers = std::str::from_utf8(headers).map_err(|_| "a part header is not UTF-8")?;
    for line in headers.split("\r\n") {
        let Some((field, value)) = line.split_once(':') else {
            return Err("a part header is not `name: value`");
        };
        if !field.trim().eq_ignore_ascii_case("content-disposition") {
            continue;
        }
        for param in value.split(';').skip(1) {
            let Some((key, raw)) = param.split_once('=') else {
                continue;
            };
            if !key.trim().eq_ignore_ascii_case("name") {
                continue;
            }
            let raw = raw.trim();
            let name = raw
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(raw);
            return if name.is_empty() {
                Err("a part is named with an empty string")
            } else {
                Ok(name.to_string())
            };
        }
    }
    Err("a part carries no `Content-Disposition` name")
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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

/// The whole document as a `.clm`, encoded on demand — what a tab downloads
/// or writes back to storage. The bytes are built for this answer, so the body
/// owns them.
fn clm(state: &ServerState, id: &str) -> Response {
    let Some(session) = parse_session(id) else {
        return Response::text(400, "Bad Request", "session id is not a number");
    };
    match state.editor.save_bytes(session) {
        Ok(bytes) => Response::new(200, "OK")
            .with("Content-Type", "application/octet-stream")
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
                .body(data)
        }
        Ok(None) => Response::text(404, "Not Found", "no such texture"),
        Err(EditorError::NoSession(_)) => Response::text(404, "Not Found", "no such session"),
        Err(err) => Response::text(500, "Internal Server Error", &err.to_string()),
    }
}

/// One byte extension's payload, for a replica catching up on a marker whose
/// hash it does not hold.
///
/// The counterpart of the texture route and shaped like it: a structure feed
/// carries `{size, hash}` and never the bytes, so this is where the bytes are
/// fetched once and only when the hash moved. A JSON extension is not here —
/// it travels inline in the structure — so asking for one is a 404, the same
/// answer a key the model does not carry gets.
fn extension(state: &ServerState, id: &str, key: &str) -> Response {
    let Some(session) = parse_session(id) else {
        return Response::text(400, "Bad Request", "session id is not a number");
    };
    let Ok(key) = decode(key).parse::<ExtensionKey>() else {
        return Response::text(400, "Bad Request", "extension key is outside the charset");
    };
    let read = state.editor.with_model(session, |m| {
        m.extension(&key).and_then(|v| v.bytes().cloned())
    });
    match read {
        Ok(Some(data)) => Response::new(200, "OK")
            .with("Content-Type", "application/octet-stream")
            .with("X-Catchlight-Extension-Hash", extension_hash(&data))
            .body(data),
        Ok(None) => Response::text(404, "Not Found", "no such byte extension"),
        Err(EditorError::NoSession(_)) => Response::text(404, "Not Found", "no such session"),
        Err(err) => Response::text(500, "Internal Server Error", &err.to_string()),
    }
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
    /// Keepalive. The reader asks for it on an idle timeout; only the writer
    /// may put it on the wire.
    Ping,
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

    // An idle tab is a live tab, so a read that expires is a cue to ping
    // rather than a failure; the write side keeps its own timeout so a stuck
    // peer cannot pin a thread.
    let ping_interval = state.ping_interval;
    // Two intervals: one ping lost to a hiccup is not a dead tab.
    let idle_timeout = ping_interval * 2;
    let leftover = reader.buffer().to_vec();
    let read_half = reader.into_inner();
    if read_half.set_read_timeout(Some(ping_interval)).is_err() {
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
                Outbound::Ping => out_ws.send(Message::Ping(Vec::new().into())).is_ok(),
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
    // Any frame at all counts as proof of life — a pong is only the cheapest
    // one, and a client that never idles is never pinged.
    let mut last_heard = Instant::now();
    loop {
        let message = match in_ws.read() {
            Ok(message) => {
                last_heard = Instant::now();
                message
            }
            Err(tungstenite::Error::Io(err)) if timed_out(&err) => {
                if last_heard.elapsed() >= idle_timeout {
                    break;
                }
                if tx.send(Outbound::Ping).is_err() {
                    break;
                }
                continue;
            }
            Err(_) => break,
        };
        let text = match message {
            Message::Text(text) => text.as_str().to_string(),
            // The protocol is JSON text; a binary frame is a client bug, and
            // it still deserves an answer rather than a silent drop.
            Message::Binary(_) => {
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
            Message::Close(_) => break,
            // Ping and Pong are answered inside `read`, through `Bounce`.
            _ => continue,
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

/// A read that expired rather than failed. A socket read timeout surfaces as
/// `WouldBlock` on unix and `TimedOut` on windows, and tungstenite keeps the
/// half-read frame either way, so the next read picks it up where it stopped.
fn timed_out(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// One frame in, one reply out. A frame that does not parse is still answered
/// against the id the client is blocked on, exactly as the socket does it.
///
/// A command that carries bytes is refused here rather than dispatched: this
/// channel is text frames for low-latency commands and events, and a
/// megabyte-scale frame on it blocks everything queued behind it. The same
/// client already speaks `POST /request`, where bulk belongs.
fn answer(editor: &Editor, frame: &str) -> String {
    let reply = match serde_json::from_str::<Request>(frame) {
        Ok(request) if carries_bytes(&request.command).is_some() => Reply::Err {
            id: request.id,
            code: ErrorCode::BulkOverHttp,
            message: format!(
                "{} carries bytes; send it to POST /request as multipart/form-data",
                request.command.tag()
            ),
        },
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

/// The request line and the headers, plus the body length they declare. The
/// body itself is left in the socket for [`read_body`], so nothing is buffered
/// on behalf of a request that has not been authenticated yet.
fn read_head(reader: &mut BufReader<TcpStream>) -> Result<(HttpRequest, usize), BadRequest> {
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

    let (path, raw_query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query),
        None => (target.clone(), ""),
    };
    let mut query = HashMap::new();
    for pair in raw_query.split('&').filter(|s| !s.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(decode(key), decode(value));
    }

    Ok((
        HttpRequest {
            method,
            path,
            query,
            headers,
            body: Vec::new(),
        },
        length,
    ))
}

/// The body the head declared, read once the request has earned it. The
/// generous ceiling belongs to the one endpoint that takes a payload; every
/// other body is one command, so it gets the cap the socket puts on a line.
fn read_body(
    reader: &mut BufReader<TcpStream>,
    request: &HttpRequest,
    length: usize,
    max_body: usize,
) -> Result<Vec<u8>, BadRequest> {
    // A multipart command carries a model or an image, so it gets the upload
    // ceiling; a JSON one is a command and nothing else.
    let bulk = request.method == "POST"
        && request.path == "/request"
        && multipart_boundary(request).is_some();
    let ceiling = if bulk { max_body } else { MAX_REQUEST_BYTES };
    if length > ceiling {
        drain_body(reader, length);
        return Err(BadRequest::BodyTooLarge);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).map_err(|_| BadRequest::Io)?;
    Ok(body)
}

/// Read and throw away a body that has already lost, so the status that
/// follows is not overtaken by a reset. Nothing is kept: the bytes go to
/// [`io::sink`] a block at a time, and a body over [`MAX_DRAIN_BYTES`] is not
/// read at all. Best effort — a sender that lied about its length, or stalled,
/// only costs the answer it was going to get anyway.
fn drain_body(reader: &mut BufReader<TcpStream>, length: usize) {
    if length > MAX_DRAIN_BYTES {
        return;
    }
    let _ = io::copy(&mut reader.by_ref().take(length as u64), &mut io::sink());
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
    body: Body,
}

/// What a response carries. A status line or a serialized document is built
/// for the answer and owned by it; a texture is the model's own payload, held
/// by refcount so the biggest bodies this server writes are never copied.
enum Body {
    Owned(Vec<u8>),
    Shared(Arc<[u8]>),
}

impl Body {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Owned(bytes)
    }
}

impl From<Arc<[u8]>> for Body {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self::Shared(bytes)
    }
}

impl Response {
    fn new(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            headers: Vec::new(),
            body: Body::Owned(Vec::new()),
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

    fn body(mut self, bytes: impl Into<Body>) -> Self {
        self.body = bytes.into();
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
        response.body.bytes().len()
    );
    for (name, value) in &response.headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes())?;
    writer.write_all(response.body.bytes())?;
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
            .with("Access-Control-Allow-Methods", "GET, POST, PUT, OPTIONS")
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

    /// A header value is bytes on the wire and a model's title is not, so the
    /// reply is escaped before it becomes one — and it has to still be the
    /// same JSON afterwards.
    #[test]
    fn a_reply_in_a_header_is_ascii_and_still_the_same_json() {
        let json = b"{\"title\":\"\xe3\x81\x82 \xf0\x9f\x8e\xa8\"}";
        let escaped = ascii_json(json);
        assert!(escaped.is_ascii(), "{escaped}");
        let parsed: serde_json::Value =
            serde_json::from_str(&escaped).expect("the escaped form is the same JSON");
        assert_eq!(parsed["title"], "\u{3042} \u{1f3a8}");
        // An ASCII reply is handed back untouched.
        assert_eq!(ascii_json(b"{\"id\":1}"), "{\"id\":1}");
    }

    #[test]
    fn a_boundary_is_read_off_the_content_type_and_nothing_else_is() {
        let with = |value: &str| HttpRequest {
            method: "POST".into(),
            path: "/request".into(),
            query: HashMap::new(),
            headers: vec![("content-type".into(), value.into())],
            body: Vec::new(),
        };
        assert_eq!(
            multipart_boundary(&with("multipart/form-data; boundary=abc")).as_deref(),
            Some("abc")
        );
        assert_eq!(
            multipart_boundary(&with(
                "Multipart/Form-Data; charset=utf-8; boundary=\"a b\""
            ))
            .as_deref(),
            Some("a b")
        );
        assert!(multipart_boundary(&with("application/json")).is_none());
        assert!(multipart_boundary(&with("multipart/form-data")).is_none());
        assert!(multipart_boundary(&with("multipart/form-data; boundary=")).is_none());
    }

    /// The parser is strict on purpose: every shape it will not take is a 400
    /// rather than a guess about what the sender meant.
    #[test]
    fn multipart_takes_only_what_it_says_it_takes() {
        let body = b"--b\r\nContent-Disposition: form-data; name=\"request\"\r\n\r\n{}\r\n\
                     --b\r\nContent-Disposition: form-data; name=\"texture\"; filename=\"a.png\"\r\n\
                     Content-Type: image/png\r\n\r\n\x00\x01\r\n--b--\r\n";
        let parts = parse_multipart(body, "b").expect("a well-formed body");
        assert_eq!(parts[0].0, "request");
        assert_eq!(parts[0].1, b"{}");
        // The filename is ignored; the part's name is the attachment's.
        assert_eq!(parts[1].0, "texture");
        assert_eq!(parts[1].1, b"\x00\x01");

        for bad in [
            // No closing boundary.
            &b"--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n"[..],
            // No opening boundary.
            &b"Content-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n--b--\r\n"[..],
            // A part with no name.
            &b"--b\r\nContent-Type: text/plain\r\n\r\nx\r\n--b--\r\n"[..],
            // Two parts sharing one name.
            &b"--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n\
               --b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\ny\r\n--b--\r\n"[..],
            // Bare LF where CRLF is required.
            &b"--b\nContent-Disposition: form-data; name=\"a\"\n\nx\n--b--\n"[..],
        ] {
            assert!(parse_multipart(bad, "b").is_err(), "{bad:?}");
        }
    }

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
