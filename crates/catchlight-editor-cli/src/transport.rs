//! The two doors the CLI reaches an editor through, behind one `call`.
//!
//! The Unix socket (Unix only) carries a request line plus sibling keys naming
//! the client's own files: `files` for attachments, `out` for a payload. The
//! HTTP door (`POST /request`, bearer token) shares no filesystem guarantee,
//! so here the same siblings are honoured client-side: each `files` path is
//! read and sent as a `multipart/form-data` part, and a payload that comes
//! back as the response body is written to `out`. A caller builds one request
//! and one sibling map either way.
//!
//! Which door: `--server` (with `--token`, or the token the connection file
//! records for that URL), then `--socket`, then the default socket if
//! something answers on it, then the connection file a running editor wrote
//! (see `catchlight_editor_protocol::default_connection_path`). The launcher
//! asks for the connection file before the socket, since its own editor is
//! the one recorded there.
//!
//! Invariants this module enforces:
//!
//! - **The token goes to loopback only.** It travels in clear text, so a URL
//!   naming any other host is refused before anything is sent.
//! - **A payload needs somewhere to go.** A command whose reply carries bytes
//!   is refused without `out` before it is sent, on both doors.

#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use catchlight_editor_protocol::{
    default_connection_path, Connection, Reply, Request, COMMAND_BYTES,
};
use serde_json::{Map, Value};

use super::MAX_COMMAND_BYTES;

/// How long a reply may take. A render under a software adapter is the slow
/// case.
const HTTP_TIMEOUT: Duration = Duration::from_secs(300);

/// What the command line said about where the editor is.
#[derive(Default)]
pub(super) struct Target {
    pub server: Option<String>,
    pub token: Option<String>,
    pub socket: Option<PathBuf>,
    /// Try the connection file before the default socket: what the launcher
    /// wants, whose own editor is the one in that file.
    pub connection_first: bool,
}

pub(super) enum Transport {
    #[cfg(unix)]
    Socket(UnixStream),
    Http(Http),
}

pub(super) struct Http {
    host: String,
    port: u16,
    token: String,
}

pub(super) fn connect(target: &Target) -> Result<Transport> {
    if let Some(url) = &target.server {
        let token = match &target.token {
            Some(token) => token.clone(),
            None => recorded()
                .filter(|(_, c)| c.url.trim_end_matches('/') == url.trim_end_matches('/'))
                .map(|(_, c)| c.token)
                .ok_or_else(|| {
                    anyhow!("--server {url} needs --token (or CATCHLIGHT_EDITOR_TOKEN)")
                })?,
        };
        return Ok(Transport::Http(Http::new(url, token)?));
    }
    if let Some(path) = &target.socket {
        return connect_socket(path);
    }
    let recorded = if target.connection_first {
        match from_connection_file() {
            Some(Ok(transport)) => return Ok(transport),
            other => other,
        }
    } else {
        None
    };
    #[cfg(unix)]
    {
        let path = catchlight_editor_protocol::default_socket_path();
        match UnixStream::connect(&path) {
            Ok(stream) => return Ok(Transport::Socket(stream)),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("cannot connect to the editor at {}", path.display()))
            }
        }
    }
    match recorded.or_else(from_connection_file) {
        Some(transport) => transport,
        None => bail!(
            "no editor is running: start `catchlight-editor`, or pass --server <url> --token <token>"
        ),
    }
}

#[cfg(unix)]
fn connect_socket(path: &Path) -> Result<Transport> {
    UnixStream::connect(path)
        .map(Transport::Socket)
        .with_context(|| format!("cannot connect to the editor at {}", path.display()))
}

#[cfg(not(unix))]
fn connect_socket(_: &Path) -> Result<Transport> {
    bail!("--socket is unix-only; pass --server <url> --token <token>")
}

/// The editor the connection file names: `None` without a file, an error when
/// the file names an editor that no longer answers.
fn from_connection_file() -> Option<Result<Transport>> {
    let (path, connection) = recorded()?;
    if connection.version != env!("CARGO_PKG_VERSION") {
        eprintln!(
            "warning: the editor at {} is version {}, this cli is {}",
            connection.url,
            connection.version,
            env!("CARGO_PKG_VERSION")
        );
    }
    let http = match Http::new(&connection.url, connection.token) {
        Ok(http) => http,
        Err(err) => return Some(Err(err)),
    };
    // Dial once now so a file left by a killed editor reads as stale rather
    // than as a failure of whatever command came first.
    let dialed = TcpStream::connect((http.host.as_str(), http.port)).with_context(|| {
        format!(
            "the editor recorded in {} ({}) is not answering; the file may be stale",
            path.display(),
            connection.url
        )
    });
    Some(dialed.map(|_| Transport::Http(http)))
}

fn recorded() -> Option<(PathBuf, Connection)> {
    let path = default_connection_path()?;
    let connection = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    Some((path, connection))
}

impl Transport {
    /// One request, answered by the reply that carries its id.
    pub fn call(&mut self, req: Request, siblings: Map<String, Value>) -> Result<Reply> {
        let tag = serde_json::to_value(&req.command)?
            .get("cmd")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_default();
        let carries_payload = COMMAND_BYTES
            .iter()
            .any(|(name, bytes)| *name == tag && bytes.payload);
        if carries_payload && !siblings.contains_key("out") {
            bail!("{tag} answers with bytes; give it an output path");
        }
        match self {
            #[cfg(unix)]
            Transport::Socket(stream) => call_socket(stream, req, siblings),
            Transport::Http(http) => http.call(req, siblings),
        }
    }
}

#[cfg(unix)]
pub(super) fn call_socket(
    stream: &mut UnixStream,
    req: Request,
    siblings: Map<String, Value>,
) -> Result<Reply> {
    let want = req.id;
    let mut line = match serde_json::to_value(&req)? {
        Value::Object(mut object) if !siblings.is_empty() => {
            object.extend(siblings);
            serde_json::to_string(&object)?
        }
        value => serde_json::to_string(&value)?,
    };
    line.push('\n');
    if line.len() > MAX_COMMAND_BYTES {
        bail!("request exceeds the 1 MiB command limit; split independent edits into explicit batches");
    }
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        let mut buf = String::new();
        if reader.read_line(&mut buf)? == 0 {
            bail!("server closed the connection");
        }
        let buf = buf.trim();
        if buf.is_empty() {
            continue;
        }
        let reply: Reply = serde_json::from_str(buf)?;
        match &reply {
            Reply::Event(_) => continue,
            Reply::Ok { id, .. } | Reply::Err { id, .. } if *id == want => return Ok(reply),
            _ => continue,
        }
    }
}

impl Http {
    fn new(url: &str, token: String) -> Result<Self> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| anyhow!("{url}: the editor's http door is plain http://"))?;
        let authority = rest.split('/').next().unwrap_or_default();
        let (host, port) = match authority.strip_prefix('[') {
            Some(bracketed) => bracketed
                .split_once(']')
                .map(|(host, rest)| (host, rest.strip_prefix(':')))
                .ok_or_else(|| anyhow!("{url}: unclosed [ in the host"))?,
            None => match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            },
        };
        let port = match port {
            Some(port) => port
                .parse::<u16>()
                .map_err(|_| anyhow!("{url}: bad port {port:?}"))?,
            None => 80,
        };
        if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
            bail!("{url}: the editor is reachable on loopback only, not {host:?}");
        }
        Ok(Self {
            host: host.to_owned(),
            port,
            token,
        })
    }

    fn call(&self, req: Request, mut siblings: Map<String, Value>) -> Result<Reply> {
        let json = serde_json::to_vec(&req)?;
        if json.len() + 1 > MAX_COMMAND_BYTES {
            bail!("request exceeds the 1 MiB command limit; split independent edits into explicit batches");
        }
        let out = match siblings.remove("out") {
            Some(Value::String(path)) => Some(PathBuf::from(path)),
            _ => None,
        };
        let mut parts = Vec::new();
        if let Some(Value::Object(files)) = siblings.remove("files") {
            for (name, path) in files {
                let path = path
                    .as_str()
                    .ok_or_else(|| anyhow!("attachment {name} is not a path"))?;
                let bytes =
                    std::fs::read(path).with_context(|| format!("reading attachment {path}"))?;
                parts.push((name, bytes));
            }
        }
        let (content_type, body) = if parts.is_empty() {
            ("application/json".to_owned(), json)
        } else {
            multipart(json, parts)?
        };

        let Response {
            status,
            headers,
            body: answer,
        } = self.exchange("/request", &content_type, &body)?;
        if status / 100 != 2 {
            bail!(
                "POST /request: {status}: {}",
                String::from_utf8_lossy(&answer).trim()
            );
        }
        let Some(header) = header(&headers, "x-catchlight-reply") else {
            return Ok(serde_json::from_slice(&answer)?);
        };
        let reply: Reply = serde_json::from_str(header)?;
        if matches!(reply, Reply::Ok { .. }) {
            let out = out.ok_or_else(|| anyhow!("the reply carried bytes and no output path"))?;
            write_payload(&out, &answer)?;
        }
        Ok(reply)
    }

    /// One round trip on a fresh connection; the editor closes every response.
    fn exchange(&self, path: &str, content_type: &str, body: &[u8]) -> Result<Response> {
        let authority = if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        };
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .with_context(|| format!("cannot reach the editor at http://{authority}"))?;
        stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
        stream.set_write_timeout(Some(HTTP_TIMEOUT))?;
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {authority}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.token,
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()?;
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .context("the editor went away mid-request")?;
        parse_response(&response)
    }
}

/// A status, lowercased headers, and the body cut to its `Content-Length`.
struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn parse_response(response: &[u8]) -> Result<Response> {
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("the editor sent a malformed http response"))?;
    let head = std::str::from_utf8(&response[..split])?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| anyhow!("the editor sent no http status"))?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let mut body = response[split + 4..].to_vec();
    if let Some(length) = header(&headers, "content-length").and_then(|v| v.parse().ok()) {
        if body.len() < length {
            bail!("the editor closed the response early");
        }
        body.truncate(length);
    }
    Ok(Response {
        status,
        headers,
        body,
    })
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The command and its bytes as one `multipart/form-data` body: a part named
/// `request` holding the command, then one part per attachment.
fn multipart(request: Vec<u8>, parts: Vec<(String, Vec<u8>)>) -> Result<(String, Vec<u8>)> {
    let mut seed = [0u8; 16];
    getrandom::fill(&mut seed).map_err(|e| anyhow!("no system randomness: {e}"))?;
    let boundary: String = seed.iter().fold("catchlight".into(), |mut acc, b| {
        acc.push_str(&format!("{b:02x}"));
        acc
    });
    let mut body = Vec::new();
    for (name, data) in std::iter::once(("request".to_owned(), request)).chain(parts) {
        if name.contains(['"', '\r', '\n']) {
            bail!("attachment name {name:?} cannot go in a part header");
        }
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(&data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), body))
}

fn write_payload(out: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(out, bytes).with_context(|| format!("writing {}", out.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_urls_carry_the_token() {
        for url in [
            "http://127.0.0.1:9",
            "http://localhost:9/",
            "http://[::1]:9",
        ] {
            let http = Http::new(url, "t".into()).unwrap();
            assert_eq!(http.port, 9, "{url}");
        }
        for url in [
            "http://example.com:9",
            "https://127.0.0.1:9",
            "http://10.0.0.1:9",
        ] {
            assert!(Http::new(url, "t".into()).is_err(), "{url}");
        }
    }

    #[test]
    fn a_response_splits_into_status_headers_and_a_sized_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nX-Catchlight-Reply: {}\r\n\r\nabcdef";
        let Response {
            status,
            headers,
            body,
        } = parse_response(raw).unwrap();
        assert_eq!(status, 200);
        assert_eq!(header(&headers, "x-catchlight-reply"), Some("{}"));
        assert_eq!(body, b"abc");
        assert!(parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nab").is_err());
    }

    #[test]
    fn a_multipart_body_leads_with_the_request() {
        let (kind, body) =
            multipart(b"{}".to_vec(), vec![("model".into(), b"clm".to_vec())]).unwrap();
        let boundary = kind.split("boundary=").nth(1).unwrap();
        let text = String::from_utf8(body).unwrap();
        let request = text.find("name=\"request\"").unwrap();
        let model = text.find("name=\"model\"").unwrap();
        assert!(request < model);
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
        assert!(multipart(vec![], vec![("a\"b".into(), vec![])]).is_err());
    }
}
