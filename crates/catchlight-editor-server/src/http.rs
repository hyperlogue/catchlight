//! The editor over HTTP: a WebSocket for a tab, one POST for a script, and
//! bytes for both.
//!
//! - **Loopback is not a permission.** Any page on any origin can reach this
//!   port, so a random per-launch token gates every door, and `GET /token`
//!   answers CORS only for an allowlisted origin, never `*`. `Host` must name
//!   loopback (421 otherwise) to defeat DNS rebinding.
//! - **A launch link carries a code, not the token.** `POST /launch` (with
//!   the token) mints a code; `POST /token` trades it for the token once,
//!   within a minute.
//! - **Another machine is opted into.** Only a loopback address binds, and
//!   only a loopback peer is served, unless [`HttpOptions::allow_remote`] is
//!   set; then the token is never served and the operator hands it over.
//! - **On `POST /request`, a status is a transport failure and a refusal is a
//!   200** carrying `Reply::Err`, exactly as the socket answers it.
//! - **An event can precede the reply that caused it.** Observers fire inside
//!   [`Editor::handle`]; clients should key on `rev`.
//! - **Only the writer thread touches the socket.** Two [`WebSocket`]s share
//!   one TCP stream; the reader's writes go through [`Bounce`].

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use catchlight_core::formats::clm::{TextureEncoding, extension_hash};
use catchlight_editor_protocol::{
    ErrorCode, ExtensionKey, Reply, Request, RequestId, SessionId, TexId,
};
use percent_encoding::percent_decode_str;
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Message, Role, WebSocket, WebSocketConfig};

use crate::{Attachments, Editor, EditorError, Payload};

/// Same cap as one Unix-socket line.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 64;
/// A WebSocket re-arms this as its keepalive interval instead, because an
/// idle tab is a live tab but a silent one is not.
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(30);
/// The whole request head must arrive within this, however it is dribbled:
/// the per-read timeout alone lets a client hold a connection permit forever.
const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// A launch link is opened as it is handed over; one left unopened, say by a
/// browser that never started, must not stay a way in.
const LAUNCH_CODE_LIFETIME: Duration = Duration::from_secs(60);
const ACCEPT_RETRY_PAUSE: Duration = Duration::from_millis(50);
const DEFAULT_MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
/// Two of these with no answer and the peer is treated as gone.
const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(30);
/// Body bytes drained after a 413 before closing. Closing with unread bytes
/// is a reset, which the sender reports instead of the status; past this,
/// draining costs more than the reset.
const MAX_DRAIN_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub allowed_origins: Vec<String>,
    pub token: Option<String>,
    pub max_upload_bytes: usize,
    /// Also half the silence tolerated before the connection is dropped.
    pub ping_interval: Duration,
    pub ui: Option<UiFiles>,
    /// CORS keeps the token from foreign pages, not from other local
    /// processes, so a launcher that passes the token another way turns this off.
    pub serve_token: bool,
    /// Bind a non-loopback address and serve any peer. `serve_token` is then
    /// ignored: the token is never served.
    pub allow_remote: bool,
}

pub type UiFiles = &'static [(&'static str, &'static [u8])];

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            token: None,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            ping_interval: DEFAULT_PING_INTERVAL,
            ui: None,
            serve_token: true,
            allow_remote: false,
        }
    }
}

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
    ui: Option<UiFiles>,
    serve_token: bool,
    allow_remote: bool,
    /// Minted and not yet traded for the token, with when each was minted.
    launch_codes: Mutex<Vec<(String, Instant)>>,
}

/// The allowlist includes the server's own loopback origins, so it can only be
/// built once the ephemeral port is known.
pub fn bind_http(
    editor: Arc<Editor>,
    addr: SocketAddr,
    options: HttpOptions,
) -> io::Result<HttpServer> {
    if !addr.ip().is_loopback() && !options.allow_remote {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{addr} is not a loopback address; allow remote clients to listen on it"),
        ));
    }
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
            ui: options.ui,
            serve_token: options.serve_token && !options.allow_remote,
            allow_remote: options.allow_remote,
            launch_codes: Mutex::new(Vec::new()),
        }),
    })
}

pub fn serve_http(editor: Arc<Editor>, addr: SocketAddr, options: HttpOptions) -> io::Result<()> {
    bind_http(editor, addr, options)?.serve()
}

impl HttpServer {
    /// A code for a launch link: `POST /token` trades it for the token once.
    pub fn launch_code(&self) -> io::Result<String> {
        self.state.mint_launch_code()
    }

    pub fn serve(self) -> io::Result<()> {
        for mut stream in accepted(self.listener.incoming()) {
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

/// The connections `incoming` yields, past any that failed to arrive: a
/// refused accept (an aborted handshake, no descriptors left) is no reason to
/// stop serving the tabs already open. The pause keeps a persistent failure
/// from spinning.
pub(crate) fn accepted<S>(
    incoming: impl Iterator<Item = io::Result<S>>,
) -> impl Iterator<Item = S> {
    incoming.filter_map(|stream| match stream {
        Ok(stream) => Some(stream),
        Err(err) => {
            tracing::warn!("accepting a connection failed: {err}");
            std::thread::sleep(ACCEPT_RETRY_PAUSE);
            None
        }
    })
}

impl ServerState {
    fn mint_launch_code(&self) -> io::Result<String> {
        self.mint_launch_code_at(Instant::now())
    }

    /// Expired codes go as a new one comes, so the set stays small.
    fn mint_launch_code_at(&self, now: Instant) -> io::Result<String> {
        let code = mint_token()?;
        let mut codes = crate::lock(&self.launch_codes);
        codes.retain(|(_, minted)| live(*minted, now));
        codes.push((code.clone(), now));
        Ok(code)
    }

    fn redeem_launch_code(&self, given: &str) -> Option<&str> {
        self.redeem_launch_code_at(given, Instant::now())
    }

    /// The code is spent whether or not the caller ever reads the token.
    fn redeem_launch_code_at(&self, given: &str, now: Instant) -> Option<&str> {
        let mut codes = crate::lock(&self.launch_codes);
        let at = codes
            .iter()
            .position(|(code, _)| secret_matches(given, code))?;
        let (_, minted) = codes.swap_remove(at);
        live(minted, now).then_some(self.token.as_str())
    }
}

fn live(minted: Instant, now: Instant) -> bool {
    now.saturating_duration_since(minted) < LAUNCH_CODE_LIFETIME
}

/// A guessable token is no token: every page the browser loads can reach
/// this port.
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

fn serve_connection(state: &ServerState, stream: TcpStream) {
    if !stream
        .peer_addr()
        .is_ok_and(|peer| peer_allowed(state.allow_remote, peer))
    {
        return;
    }
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

    let (mut request, body_len) = match read_head(&mut reader, Instant::now() + HEAD_DEADLINE) {
        Ok(head) => head,
        Err(BadRequest::Closed | BadRequest::Io) => return,
        Err(err) => {
            let _ = write_response(&mut writer, &err.response());
            return;
        }
    };
    if reader
        .get_ref()
        .set_read_timeout(Some(HTTP_IO_TIMEOUT))
        .is_err()
    {
        return;
    }

    // DNS rebinding: a non-loopback Host is someone else's name for this port.
    // A remote client names the machine, and the token it brings is the gate.
    if !state.allow_remote && !host_is_loopback(request.header("host")) {
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

    // Gate on the head alone: an unauthenticated caller never gets to spend
    // the upload ceiling.
    if !opens_without_a_token(state, &request)
        && !trades_a_launch_code(&request)
        && ui_file(state, &request).is_none()
        && !bearer_matches(&request, &state.token)
    {
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

    let response = route(state, &mut request, allowed.as_deref());
    let _ = write_response(&mut writer, &response);
}

/// Ungated: the CORS headers, not the token, keep a foreign page from reading
/// `GET /token`.
fn opens_without_a_token(state: &ServerState, request: &HttpRequest) -> bool {
    state.serve_token && request.method == "GET" && request.path == "/token"
}

/// Ungated: the code is the credential.
fn trades_a_launch_code(request: &HttpRequest) -> bool {
    request.method == "POST" && request.path == "/token"
}

fn token_response(token: &str) -> Response {
    Response::new(200, "OK")
        .with("Content-Type", "application/json")
        .with("X-Content-Type-Options", "nosniff")
        .with("Cache-Control", "no-store")
        .body(format!("{{\"token\":\"{token}\"}}").into_bytes())
}

fn route(state: &ServerState, request: &mut HttpRequest, allowed: Option<&str>) -> Response {
    if opens_without_a_token(state, request) {
        return cors(token_response(&state.token), allowed);
    }

    let segments: Vec<&str> = request
        .path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();

    let response = match (request.method.as_str(), segments.as_slice()) {
        ("POST", ["request"]) => command(state, request),
        ("POST", ["launch"]) => launch(state),
        ("POST", ["token"]) => redeem(state, request),
        ("GET", ["sessions", id, "structure"]) => structure(state, id),
        ("GET", ["sessions", id, "clm"]) => clm(state, id),
        ("GET", ["sessions", id, "textures", tex]) => texture(state, id, tex),
        ("GET", ["sessions", id, "extensions", key]) => extension(state, id, key),
        _ => match ui_file(state, request) {
            Some((path, bytes)) => return ui_response(path, bytes),
            None => Response::text(404, "Not Found", "no such endpoint"),
        },
    };
    cors(response, allowed)
}

fn ui_file(state: &ServerState, request: &HttpRequest) -> Option<(&'static str, &'static [u8])> {
    let ui = state.ui?;
    if request.method != "GET" || claimed_by_a_route(&request.path) {
        return None;
    }
    let wanted = match request.path.trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
    ui.iter().find(|(path, _)| *path == wanted).copied()
}

fn claimed_by_a_route(path: &str) -> bool {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    matches!(first, "token" | "launch" | "ws" | "request" | "sessions")
}

fn ui_response(path: &str, bytes: &'static [u8]) -> Response {
    let kind = match path.rsplit_once('.').map_or("", |(_, ext)| ext) {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    // Vite names everything under `assets/` by content hash, so only the
    // page itself has to be revalidated.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Response::new(200, "OK")
        .with("Content-Type", kind)
        .with("Cache-Control", cache)
        .with("X-Content-Type-Options", "nosniff")
        .with("X-Frame-Options", "DENY")
        .with("Content-Security-Policy", "frame-ancestors 'none'")
        .body(Body::Static(bytes))
}

fn launch(state: &ServerState) -> Response {
    match state.mint_launch_code() {
        Ok(code) => Response::new(200, "OK")
            .with("Content-Type", "application/json")
            .with("Cache-Control", "no-store")
            .body(format!("{{\"code\":\"{code}\"}}").into_bytes()),
        Err(e) => Response::text(500, "Internal Server Error", &e.to_string()),
    }
}

fn redeem(state: &ServerState, request: &HttpRequest) -> Response {
    #[derive(serde::Deserialize)]
    struct Redeem {
        code: String,
    }
    let Ok(Redeem { code }) = serde_json::from_slice(&request.body) else {
        return Response::text(400, "Bad Request", "expected {\"code\": \"...\"}");
    };
    match state.redeem_launch_code(&code) {
        Some(token) => token_response(token),
        None => Response::text(401, "Unauthorized", "unknown or already used launch code"),
    }
}

/// Only a body that is not a [`Request`] at all is a 400: there is no `id` to
/// answer against. Editor refusals are a 200 carrying `Reply::Err`.
fn command(state: &ServerState, request: &mut HttpRequest) -> Response {
    let body = std::mem::take(&mut request.body);
    let (parsed, attachments) = match multipart_boundary(request) {
        None => (
            serde_json::from_slice::<Request>(&body),
            Attachments::none(),
        ),
        Some(boundary) => match parse_multipart(&body, &boundary) {
            Err(why) => return Response::text(400, "Bad Request", why),
            Ok(parts) => {
                let mut attachments = Attachments::none();
                let mut command = None;
                for (name, bytes) in carve(body, parts) {
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
        // The bytes are the body, so the reply rides in a header, escaped to
        // ASCII because a header value is bytes and a model's name may be anything.
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

/// Escaping each non-ASCII char in place is safe: JSON puts them only inside
/// string literals, and `serde_json` already escaped the control characters.
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

/// Each part's name and where its bytes lie in `body`.
///
/// Strict on purpose (CRLF everywhere, a name on every part, a closing
/// boundary): anything else is a 400 rather than a guess. `filename` is
/// ignored; the part's name is the attachment's name.
fn parse_multipart(
    body: &[u8],
    boundary: &str,
) -> Result<Vec<(String, Range<usize>)>, &'static str> {
    let opening = format!("--{boundary}");
    let delimiter = format!("\r\n--{boundary}");
    if !body.starts_with(opening.as_bytes()) {
        return Err("the body does not open with its boundary");
    }
    let mut at = opening.len();
    let mut parts: Vec<(String, Range<usize>)> = Vec::new();
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
        parts.push((name, data_at..data_at + data_len));
        at = data_at + data_len + delimiter.len();
    }
}

/// The largest part keeps the body's own allocation, so the upload that is
/// most of a body is never held twice.
fn carve(mut body: Vec<u8>, mut parts: Vec<(String, Range<usize>)>) -> Vec<(String, Vec<u8>)> {
    let largest = (0..parts.len()).max_by_key(|&at| parts[at].1.len());
    let kept = largest.map(|at| parts.swap_remove(at));
    let mut carved: Vec<(String, Vec<u8>)> = parts
        .into_iter()
        .map(|(name, range)| (name, body[range].to_vec()))
        .collect();
    if let Some((name, range)) = kept {
        body.truncate(range.end);
        body.drain(..range.start);
        carved.push((name, body));
    }
    carved
}

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

/// Read under one session lock so bytes are never paired with another revision.
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

/// A JSON extension travels inline in the structure, so asking for one here
/// is a 404.
fn extension(state: &ServerState, id: &str, key: &str) -> Response {
    let Some(session) = parse_session(id) else {
        return Response::text(400, "Bad Request", "session id is not a number");
    };
    let Ok(key) = decode(key).parse::<ExtensionKey>() else {
        return Response::text(400, "Bad Request", "extension key is outside the charset");
    };
    let read = state.editor.with_model(session, |m| {
        m.extensions().get(&key).and_then(|v| v.bytes().cloned())
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

enum Outbound {
    Text(String),
    Raw(Vec<u8>),
    /// Only the writer may put a ping on the wire.
    Ping,
    Close,
}

/// Two `WebSocket`s share one TCP stream, so only the writer may write to it.
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

    // An expired read is a cue to ping rather than a failure; the write side
    // keeps its own timeout so a stuck peer cannot pin a thread.
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
        let _ = out_ws.get_mut().shutdown(Shutdown::Both);
    });

    let events = tx.clone();
    let subscription = state.editor.subscribe(Box::new(move |event| {
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
    // Any frame at all counts as proof of life.
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
                    op_index: None,
                    limit: None,
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

    drop(subscription);
    let _ = tx.send(Outbound::Close);
    drop(tx);
    let _ = pump.join();
    eprintln!("catchlight-editor-server: websocket closed");
}

/// A socket read timeout surfaces as `WouldBlock` on unix and `TimedOut` on
/// windows; tungstenite keeps the half-read frame either way.
fn timed_out(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// A command carrying bytes is refused: a megabyte-scale frame blocks
/// everything queued behind it on this channel.
fn answer(editor: &Editor, frame: &str) -> String {
    let reply = match serde_json::from_str::<Request>(frame) {
        Ok(request) if request.command.carries_bytes().is_some() => Reply::Err {
            op_index: None,
            limit: None,
            id: request.id,
            code: ErrorCode::BulkOverHttp,
            message: format!(
                "{} carries bytes; send it to POST /request as multipart/form-data",
                request.command.tag()
            ),
        },
        Ok(request) => editor.handle(request),
        Err(e) => Reply::Err {
            op_index: None,
            limit: None,
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

fn read_head(
    reader: &mut BufReader<TcpStream>,
    deadline: Instant,
) -> Result<(HttpRequest, usize), BadRequest> {
    let mut budget = MAX_HEADER_BYTES;
    let Some(start) = read_line(reader, &mut budget, deadline)? else {
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
        let Some(line) = read_line(reader, &mut budget, deadline)? else {
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

fn read_body(
    reader: &mut BufReader<TcpStream>,
    request: &HttpRequest,
    length: usize,
    max_body: usize,
) -> Result<Vec<u8>, BadRequest> {
    let bulk = request.method == "POST"
        && request.path == "/request"
        && multipart_boundary(request).is_some();
    let ceiling = if bulk { max_body } else { MAX_REQUEST_BYTES };
    if length > ceiling {
        drain_body(reader, length);
        return Err(BadRequest::BodyTooLarge);
    }
    // Grown as bytes arrive: a declared length costs nothing until it is sent.
    let mut body = Vec::new();
    reader
        .take(length as u64)
        .read_to_end(&mut body)
        .map_err(|_| BadRequest::Io)?;
    if body.len() != length {
        return Err(BadRequest::Io);
    }
    Ok(body)
}

/// Drained so the status is not overtaken by a reset. Best effort.
fn drain_body(reader: &mut BufReader<TcpStream>, length: usize) {
    if length > MAX_DRAIN_BYTES {
        return;
    }
    let _ = io::copy(&mut reader.by_ref().take(length as u64), &mut io::sink());
}

fn read_line(
    reader: &mut BufReader<TcpStream>,
    budget: &mut usize,
    deadline: Instant,
) -> Result<Option<String>, BadRequest> {
    let mut line = Vec::new();
    loop {
        if reader.buffer().is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(BadRequest::Io);
            }
            reader
                .get_ref()
                .set_read_timeout(Some(left.min(HTTP_IO_TIMEOUT)))
                .map_err(|_| BadRequest::Io)?;
        }
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

/// A texture is the model's own payload, held by refcount so the biggest
/// bodies are never copied.
enum Body {
    Owned(Vec<u8>),
    Shared(Arc<[u8]>),
    Static(&'static [u8]),
}

impl Body {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
            Self::Static(bytes) => bytes,
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
    // No keep-alive: one framing rule, and a refused body may be left unread.
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
            .with("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
            .with(
                "Access-Control-Expose-Headers",
                "X-Catchlight-Rev, X-Catchlight-Encoding, X-Catchlight-Reply",
            ),
        None => response,
    }
}

fn peer_allowed(allow_remote: bool, peer: SocketAddr) -> bool {
    allow_remote || peer.ip().to_canonical().is_loopback()
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

/// Constant time: a page on another origin can time this endpoint.
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
    fn a_dribbled_request_head_is_dropped_at_its_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dribbling = stop.clone();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(b"GET / HTTP/1.1\r\nX-Slow: ").unwrap();
            while !dribbling.load(Ordering::Relaxed) {
                if stream.write_all(b"a").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(HTTP_IO_TIMEOUT)).unwrap();
        let started = Instant::now();
        let head = read_head(
            &mut BufReader::new(stream),
            started + Duration::from_millis(300),
        );
        stop.store(true, Ordering::Relaxed);
        client.join().unwrap();
        assert!(
            matches!(head, Err(BadRequest::Io)),
            "the head was still read"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_failed_accept_does_not_end_the_connections_after_it() {
        let incoming = [
            Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
            Ok(1),
            Err(io::Error::other("no descriptors left")),
            Ok(2),
        ];
        assert_eq!(accepted(incoming.into_iter()).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn a_reply_in_a_header_is_ascii_and_still_the_same_json() {
        let json = b"{\"title\":\"\xe3\x81\x82 \xf0\x9f\x8e\xa8\"}";
        let escaped = ascii_json(json);
        assert!(escaped.is_ascii(), "{escaped}");
        let parsed: serde_json::Value =
            serde_json::from_str(&escaped).expect("the escaped form is the same JSON");
        assert_eq!(parsed["title"], "\u{3042} \u{1f3a8}");
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

    #[test]
    fn multipart_takes_only_what_it_says_it_takes() {
        let body = b"--b\r\nContent-Disposition: form-data; name=\"request\"\r\n\r\n{}\r\n\
                     --b\r\nContent-Disposition: form-data; name=\"texture\"; filename=\"a.png\"\r\n\
                     Content-Type: image/png\r\n\r\n\x00\x01\r\n--b--\r\n";
        let parts = parse_multipart(body, "b").expect("a well-formed body");
        assert_eq!(parts[0].0, "request");
        assert_eq!(&body[parts[0].1.clone()], b"{}");
        // The filename is ignored; the part's name is the attachment's.
        assert_eq!(parts[1].0, "texture");
        assert_eq!(&body[parts[1].1.clone()], b"\x00\x01");
        let mut carved = carve(body.to_vec(), parts);
        carved.sort();
        assert_eq!(
            carved,
            [
                ("request".to_string(), b"{}".to_vec()),
                ("texture".to_string(), b"\x00\x01".to_vec()),
            ]
        );

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
    fn a_launch_code_expires_a_minute_after_it_is_minted() {
        let server = bind_http(
            Arc::new(Editor::new()),
            "127.0.0.1:0".parse().unwrap(),
            HttpOptions::default(),
        )
        .unwrap();
        let state = &server.state;
        let minted = Instant::now();
        let fresh = state.mint_launch_code_at(minted).unwrap();
        let stale = state.mint_launch_code_at(minted).unwrap();
        let late = minted + LAUNCH_CODE_LIFETIME;
        assert_eq!(state.redeem_launch_code_at(&stale, late), None);
        assert_eq!(
            state.redeem_launch_code_at(&fresh, late - Duration::from_secs(1)),
            Some(server.token.as_str())
        );

        state.mint_launch_code_at(minted).unwrap();
        state.mint_launch_code_at(late).unwrap();
        assert_eq!(
            crate::lock(&state.launch_codes).len(),
            1,
            "the expired one went"
        );
    }

    #[test]
    fn only_a_loopback_peer_is_served_unless_remote_clients_are_allowed() {
        let peer = |addr: &str| addr.parse::<SocketAddr>().unwrap();
        assert!(peer_allowed(false, peer("127.0.0.1:9")));
        assert!(peer_allowed(false, peer("[::1]:9")));
        assert!(peer_allowed(false, peer("[::ffff:127.0.0.1]:9")));
        assert!(!peer_allowed(false, peer("192.0.2.7:9")));
        assert!(peer_allowed(true, peer("192.0.2.7:9")));
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
