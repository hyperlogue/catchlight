//! `catchlight-editor` — the Catchlight editor as one program.
//!
//! ```text
//! catchlight-editor [serve] [--port <port>] [--store <dir>] [--no-open] [<model.clm>]
//! catchlight-editor <command> ...
//! ```
//!
//! `serve` (the default, and what a bare `.clm` argument means) runs the
//! editor server on loopback, serves the editor page compiled into this
//! binary at the same origin, records the HTTP door in the connection file
//! and opens the browser. Every other command is `catchlight-editor-cli`'s,
//! which finds that editor through the connection file: an agent runs
//! `catchlight-editor node tree` against the session a person is looking at,
//! and the tab sees each edit as it lands.
//!
//! Invariants this binary keeps:
//!
//! - **One editor per user.** A second `serve` that finds a live editor in
//!   the connection file opens that one (and the model, if given) instead of
//!   starting another, so agents and tabs never split across two processes.
//!   "Live" means the recorded URL answers `GET /token` with the recorded
//!   token, not merely that something listens on the port.
//! - **Loopback only.** The listener binds `127.0.0.1`; the server's token,
//!   `Host` and `Origin` checks are unchanged.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use catchlight_editor_cli::Discovery;
use catchlight_editor_protocol::{Command, Reply, Request};
use catchlight_editor_server::connection::{
    default_connection_path, read_connection, Connection, ConnectionFile,
};
use catchlight_editor_server::{bind_http, Editor, FileStorage, HttpOptions, UiFiles};

mod ui {
    include!(concat!(env!("OUT_DIR"), "/ui.rs"));
}

const NAME: &str = "catchlight-editor";
/// Tried first so a bookmark keeps working; any free port when it is taken.
const DEFAULT_PORT: u16 = 7323;

const USAGE: &str = "\
catchlight-editor: the Catchlight editor in your browser, drivable by local agents

usage:
  catchlight-editor [serve] [--port <port>] [--store <dir>] [--no-open] [<model.clm>]
  catchlight-editor <command> ...

serve (the default) starts the editor on 127.0.0.1, opens it in the browser and
records where it listens so that commands can find it. With an editor already
running, it opens that one instead.

  --port <port>   listen here (default 7323, or any free port if that is taken)
  --store <dir>   where relative model paths resolve (default: this directory)
  --no-open       do not open a browser

Every other command drives the running editor, e.g.
  catchlight-editor session list
  catchlight-editor node tree
Run `catchlight-editor help` for the full command list.";

#[derive(Debug, Default, PartialEq)]
struct Serve {
    port: Option<u16>,
    store: Option<PathBuf>,
    open: bool,
    model: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let first = args.get(1).and_then(|a| a.to_str());
    match first {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("serve") => serve(parse_serve(&args[2..])?),
        None => serve(parse_serve(&[])?),
        Some(first) if is_serve_argument(first) => serve(parse_serve(&args[1..])?),
        Some(_) => catchlight_editor_cli::run(NAME, Discovery::ConnectionFirst, args),
    }
}

/// Whether a first argument starts a `serve` without naming it: a launcher
/// flag, or a model file. Anything else is a cli command.
fn is_serve_argument(arg: &str) -> bool {
    matches!(arg, "--port" | "--store" | "--no-open")
        || Path::new(arg)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("clm"))
}

fn parse_serve(args: &[OsString]) -> Result<Serve> {
    let mut serve = Serve {
        open: true,
        ..Serve::default()
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--port") => {
                let value = args.next().and_then(|v| v.to_str());
                let value = value.ok_or_else(|| anyhow!("--port needs a number"))?;
                serve.port = Some(
                    value
                        .parse()
                        .map_err(|_| anyhow!("--port {value}: not a port"))?,
                );
            }
            Some("--store") => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--store needs a directory"))?;
                serve.store = Some(PathBuf::from(value));
            }
            Some("--no-open") => serve.open = false,
            Some(flag) if flag.starts_with('-') => bail!("unknown flag {flag}\n\n{USAGE}"),
            _ if serve.model.is_none() => serve.model = Some(PathBuf::from(arg)),
            _ => bail!("unexpected argument {}\n\n{USAGE}", arg.to_string_lossy()),
        }
    }
    Ok(serve)
}

fn serve(options: Serve) -> Result<()> {
    let model = match &options.model {
        Some(path) => Some(
            std::path::absolute(path)
                .with_context(|| format!("resolving {}", path.display()))?
                .display()
                .to_string(),
        ),
        None => None,
    };
    let connection_path = default_connection_path()
        .ok_or_else(|| anyhow!("no per-user directory for the connection file; set HOME"))?;

    if let Some(running) = read_connection(&connection_path).filter(is_live) {
        eprintln!("{NAME}: an editor is already running at {}", running.url);
        if let Some(model) = model {
            open_in_running(&running, &model)?;
        }
        if options.open {
            open_browser(&running.url);
        }
        return Ok(());
    }

    let store = match options.store {
        Some(dir) => FileStorage::new(dir),
        None => FileStorage::default(),
    };
    let editor = Arc::new(Editor::with_storage(Arc::new(store)));
    if let Some(model) = model {
        match editor.handle(Request {
            id: 0,
            command: Command::SessionOpen {
                path: model.clone(),
            },
        }) {
            Reply::Err { message, .. } => bail!("could not open {model}: {message}"),
            _ => eprintln!("{NAME}: opened {model}"),
        }
    }

    let ui: UiFiles = ui::UI;
    if ui.is_empty() {
        eprintln!(
            "{NAME}: this build has no editor page (run `cargo xtask bundle-ui` and rebuild); \
             serving the API only"
        );
    }
    let http_options = || HttpOptions {
        ui: (!ui.is_empty()).then_some(ui),
        ..HttpOptions::default()
    };
    let loopback = |port| SocketAddr::from(([127, 0, 0, 1], port));
    let server = match options.port {
        Some(port) => bind_http(editor.clone(), loopback(port), http_options())
            .with_context(|| format!("listening on 127.0.0.1:{port}"))?,
        None => bind_http(editor.clone(), loopback(DEFAULT_PORT), http_options())
            .or_else(|_| bind_http(editor.clone(), loopback(0), http_options()))
            .context("listening on 127.0.0.1")?,
    };
    let url = format!("http://{}", server.addr);

    let file = Arc::new(Mutex::new(Some(ConnectionFile::write(
        &connection_path,
        &server,
    )?)));
    let on_signal = file.clone();
    // Dropping the file is what removes it; a signal skips destructors.
    ctrlc::set_handler(move || {
        if let Ok(mut file) = on_signal.lock() {
            file.take();
        }
        std::process::exit(130);
    })
    .context("installing the Ctrl-C handler")?;

    eprintln!("{NAME}: {url}");
    eprintln!(
        "{NAME}: connection recorded in {}",
        connection_path.display()
    );
    eprintln!("{NAME}: drive it with `{NAME} <command>`; Ctrl-C stops it");
    if options.open && !ui.is_empty() {
        open_browser(&url);
    }
    let served = server.serve();
    if let Ok(mut file) = file.lock() {
        file.take();
    }
    served.context("serving the editor")
}

/// Whether the recorded editor is still the process that wrote the record:
/// the URL answers `GET /token` with the recorded token. A port reused by
/// anything else fails the comparison.
fn is_live(connection: &Connection) -> bool {
    let Some(addr) = connection
        .url
        .strip_prefix("http://")
        .and_then(|a| a.parse::<SocketAddr>().ok())
    else {
        return false;
    };
    if !addr.ip().is_loopback() {
        return false;
    }
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let request = format!("GET /token HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    let mut response = Vec::new();
    if stream.write_all(request.as_bytes()).is_err() || stream.read_to_end(&mut response).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&response);
    text.starts_with("HTTP/1.1 200") && text.contains(&format!("\"{}\"", connection.token))
}

/// Ask the running editor to open `model` through the cli, which already
/// speaks its HTTP door; the session becomes the current one for later
/// commands.
fn open_in_running(running: &Connection, model: &str) -> Result<()> {
    catchlight_editor_cli::run(
        NAME,
        Discovery::ConnectionFirst,
        [
            NAME,
            "--server",
            running.url.as_str(),
            "--token",
            running.token.as_str(),
            "session",
            "open",
            model,
        ],
    )
}

fn open_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    let spawned = command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if spawned.is_err() {
        eprintln!("{NAME}: open {url} in a browser");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_model_or_a_launcher_flag_means_serve_and_anything_else_is_a_command() {
        assert!(is_serve_argument("lan.clm"));
        assert!(is_serve_argument("dir/Lan.CLM"));
        assert!(is_serve_argument("--no-open"));
        assert!(!is_serve_argument("node"));
        assert!(!is_serve_argument("--json"));
        assert!(!is_serve_argument("status"));
    }

    #[test]
    fn serve_flags_parse_and_a_second_model_is_refused() {
        let serve = parse_serve(&args(&["--port", "9000", "--no-open", "a.clm"])).unwrap();
        assert_eq!(
            serve,
            Serve {
                port: Some(9000),
                store: None,
                open: false,
                model: Some(PathBuf::from("a.clm")),
            }
        );
        assert!(parse_serve(&args(&["a.clm", "b.clm"])).is_err());
        assert!(parse_serve(&args(&["--port", "x"])).is_err());
        assert!(parse_serve(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn a_record_whose_port_answers_with_another_token_is_not_live() {
        let server = bind_http(
            Arc::new(Editor::new()),
            "127.0.0.1:0".parse().unwrap(),
            HttpOptions::default(),
        )
        .unwrap();
        let mut record = Connection {
            url: format!("http://{}", server.addr),
            token: server.token.clone(),
            pid: 1,
            version: String::new(),
        };
        std::thread::spawn(move || server.serve());
        assert!(is_live(&record));
        record.token = "someone-else".into();
        assert!(!is_live(&record));
        record.url = "http://127.0.0.1:1".into();
        assert!(!is_live(&record));
    }
}
