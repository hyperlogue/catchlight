//! The Unix socket and HTTP transports behind one `call`.
//!
//! The HTTP bearer token travels in clear text, so only loopback is dialled.

#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use catchlight_editor_discovery::{Connection, default_connection_path};
use catchlight_editor_protocol::{COMMAND_BYTES, Reply, Request};
use serde_json::{Map, Value};

use super::MAX_COMMAND_BYTES;

/// Sized for a render under a software adapter.
const HTTP_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Default)]
pub(super) struct Target {
    pub server: Option<String>,
    pub token: Option<String>,
    pub socket: Option<PathBuf>,
    pub connection_first: bool,
}

pub(super) enum Transport {
    /// One reader for the connection's life, so bytes read past a reply
    /// stay buffered for the next call.
    #[cfg(unix)]
    Socket(BufReader<UnixStream>),
    Http(Http),
}

pub(super) struct Http {
    host: String,
    port: u16,
    token: String,
}

/// Where a [`Transport`] reached the editor, kept so a later process can dial
/// the same one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Endpoint {
    Socket(PathBuf),
    Http { url: String, token: String },
}

#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "only the unix review listener uses it")
)]
impl Endpoint {
    pub fn connect(&self) -> Result<Transport> {
        match self {
            Self::Socket(path) => connect_socket(path),
            Self::Http { url, token } => Ok(Transport::Http(Http::new(url, token.clone())?)),
        }
    }

    /// What a person reads: never the token.
    pub fn describe(&self) -> String {
        match self {
            Self::Socket(path) => path.display().to_string(),
            Self::Http { url, .. } => url.clone(),
        }
    }
}

/// The transport and the endpoint it reached.
pub(super) fn resolve(target: &Target) -> Result<(Transport, Endpoint)> {
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
        let endpoint = Endpoint::Http {
            url: url.clone(),
            token: token.clone(),
        };
        return Ok((Transport::Http(Http::new(url, token)?), endpoint));
    }
    if let Some(path) = &target.socket {
        let transport = connect_socket(path)?;
        return Ok((transport, Endpoint::Socket(std::path::absolute(path)?)));
    }
    let recorded = if target.connection_first {
        match from_connection_file() {
            Some(Ok(found)) => return Ok(found),
            other => other,
        }
    } else {
        None
    };
    // The default socket is a guess: any failure on it falls through silently.
    #[cfg(unix)]
    let socket_error = {
        let path = catchlight_editor_discovery::default_socket_path();
        match UnixStream::connect(&path) {
            Ok(stream) => {
                let endpoint = Endpoint::Socket(std::path::absolute(&path)?);
                return Ok((Transport::Socket(BufReader::new(stream)), endpoint));
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                None
            }
            Err(err) => Some(anyhow::Error::new(err).context(format!(
                "cannot connect to the editor at {}",
                path.display()
            ))),
        }
    };
    #[cfg(not(unix))]
    let socket_error: Option<anyhow::Error> = None;
    match (recorded.or_else(from_connection_file), socket_error) {
        (Some(transport), _) => transport,
        (None, Some(err)) => Err(err),
        (None, None) => bail!(
            "no editor is running: start `catchlight-editor`, or pass --server <url> --token <token>"
        ),
    }
}

#[cfg(unix)]
fn connect_socket(path: &Path) -> Result<Transport> {
    let stream = match UnixStream::connect(path) {
        // An absolute path can outgrow `sun_path` (about 100 bytes) where the
        // relative one a person typed fit; dial it from its directory.
        Err(err) if err.kind() == std::io::ErrorKind::InvalidInput && path.is_absolute() => {
            connect_from_parent(path)
        }
        other => other,
    };
    stream
        .map(|stream| Transport::Socket(BufReader::new(stream)))
        .with_context(|| format!("cannot connect to the editor at {}", path.display()))
}

/// Changes the process's working directory for the dial and restores it.
#[cfg(unix)]
fn connect_from_parent(path: &Path) -> std::io::Result<UnixStream> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    let back = std::env::current_dir()?;
    std::env::set_current_dir(parent)?;
    let stream = UnixStream::connect(name);
    std::env::set_current_dir(back)?;
    stream
}

#[cfg(not(unix))]
fn connect_socket(_: &Path) -> Result<Transport> {
    bail!("--socket is unix-only; pass --server <url> --token <token>")
}

fn from_connection_file() -> Option<Result<(Transport, Endpoint)>> {
    let (path, connection) = recorded()?;
    if connection.version != env!("CARGO_PKG_VERSION") {
        eprintln!(
            "warning: the editor at {} is version {}, this cli is {}",
            connection.url,
            connection.version,
            env!("CARGO_PKG_VERSION")
        );
    }
    let endpoint = Endpoint::Http {
        url: connection.url.clone(),
        token: connection.token.clone(),
    };
    let http = match Http::new(&connection.url, connection.token) {
        Ok(http) => http,
        Err(err) => return Some(Err(err)),
    };
    // Dial now so a file left by a killed editor reports as stale.
    let dialed = TcpStream::connect((http.host.as_str(), http.port)).with_context(|| {
        format!(
            "the editor recorded in {} ({}) is not answering; the file may be stale",
            path.display(),
            connection.url
        )
    });
    Some(dialed.map(|_| (Transport::Http(http), endpoint)))
}

fn recorded() -> Option<(PathBuf, Connection)> {
    let path = default_connection_path()?;
    let connection = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    Some((path, connection))
}

impl Transport {
    pub fn call(&mut self, req: Request, siblings: Map<String, Value>) -> Result<Reply> {
        let tag = req.command.tag();
        let carries_payload = COMMAND_BYTES
            .iter()
            .any(|(name, bytes)| *name == tag && bytes.payload);
        if carries_payload && !siblings.contains_key("out") {
            bail!("{tag} answers with bytes; give it an output path");
        }
        match self {
            #[cfg(unix)]
            Self::Socket(stream) => call_socket(stream, req, siblings),
            Self::Http(http) => http.call(req, siblings),
        }
    }
}

#[cfg(unix)]
fn call_socket(
    stream: &mut BufReader<UnixStream>,
    req: Request,
    siblings: Map<String, Value>,
) -> Result<Reply> {
    let want = req.id;
    let mut line = if siblings.is_empty() {
        serde_json::to_string(&req)?
    } else {
        let Value::Object(mut object) = serde_json::to_value(&req)? else {
            bail!("a request did not serialize as an object");
        };
        object.extend(siblings);
        serde_json::to_string(&object)?
    };
    line.push('\n');
    if line.len() > MAX_COMMAND_BYTES {
        bail!(
            "request exceeds the 1 MiB command limit; split independent edits into explicit batches"
        );
    }
    stream.get_mut().write_all(line.as_bytes())?;
    loop {
        let mut buf = String::new();
        if stream.read_line(&mut buf)? == 0 {
            bail!("server closed the connection");
        }
        let buf = buf.trim();
        if buf.is_empty() {
            continue;
        }
        let reply: Reply = serde_json::from_str(buf)?;
        match &reply {
            Reply::Event(_) => {}
            Reply::Ok { id, .. } | Reply::Err { id, .. } if *id == want => return Ok(reply),
            _ => {}
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
        // By name, `localhost` may resolve to a `::1` listener that is not the
        // editor and would receive the token.
        let host = match host {
            "127.0.0.1" | "localhost" => "127.0.0.1",
            "::1" => "::1",
            _ => bail!("{url}: the editor is reachable on loopback only, not {host:?}"),
        };
        Ok(Self {
            host: host.to_owned(),
            port,
            token,
        })
    }

    fn call(&self, req: Request, mut siblings: Map<String, Value>) -> Result<Reply> {
        let json = serde_json::to_vec(&req)?;
        if json.len() + 1 > MAX_COMMAND_BYTES {
            bail!(
                "request exceeds the 1 MiB command limit; split independent edits into explicit batches"
            );
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
        let sent = stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body))
            .and_then(|()| stream.flush());
        // The editor may refuse on the head alone and close mid-body, so a
        // failed send can still have a readable refusal.
        let mut response = Vec::new();
        let read = stream.read_to_end(&mut response);
        if let Ok(parsed) = parse_response(&response) {
            return Ok(parsed);
        }
        sent.context("the editor went away mid-request")?;
        read.context("the editor went away mid-request")?;
        parse_response(&response)
    }
}

/// Header names are lowercased.
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

    #[cfg(unix)]
    #[test]
    fn a_reply_read_ahead_answers_the_next_call() {
        use catchlight_editor_protocol::{Command, ResponseBody, SessionId};
        let (client, server) = UnixStream::pair().unwrap();
        let editor = std::thread::spawn(move || {
            let mut server = BufReader::new(server);
            let mut line = String::new();
            server.read_line(&mut line).unwrap();
            assert!(line.starts_with(r#"{"id":1,"#), "{line}");
            let reply = |id| {
                serde_json::to_string(&Reply::Ok {
                    id,
                    rev: Some(id),
                    body: ResponseBody::Empty,
                })
                .unwrap()
            };
            write!(server.get_mut(), "{}\n{}\n", reply(1), reply(2)).unwrap();
            line.clear();
            server.read_line(&mut line).unwrap();
        });
        let mut transport = Transport::Socket(BufReader::new(client));
        for id in [1, 2] {
            let req = Request {
                id,
                command: Command::Status {
                    session: SessionId(1),
                },
            };
            let reply = transport.call(req, Map::new()).unwrap();
            assert!(matches!(reply, Reply::Ok { rev: Some(rev), .. } if rev == id));
        }
        editor.join().unwrap();
    }

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
        assert_eq!(
            Http::new("http://localhost:9", "t".into()).unwrap().host,
            "127.0.0.1"
        );
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
