//! Headless standalone server: create an [`Editor`], listen on a socket, and
//! optionally serve a browser tab over HTTP + WebSocket. The GUI will instead
//! embed the library and call `serve_unix` itself.
//!
//! ```text
//! catchlight-editor-server [--socket <path>] [--store <dir>] [--http <addr>]
//!                          [--allow-origin <origin>]... [--connection-file <path>]
//!                          [<model.clm>]
//! ```
//!
//! Both paths are flags rather than ambient state so a test harness can run a
//! server of its own: `--socket` inside a private temp directory does not
//! collide with the editor a person is already running on the default socket,
//! and `--store` says which directory a relative `path` key names instead of
//! leaving it to whatever the process was launched from. A unix socket path is
//! bounded by the OS at around 100 bytes (`SUN_LEN`), so a temp directory deep
//! enough to pass that costs an `InvalidInput` rather than a listener.
//!
//! The socket is Unix-only; elsewhere `--http` is required and is the only
//! door. `--connection-file` records the HTTP door for local clients (see
//! [`connection`](catchlight_editor_server::connection)); it is opt-in so a
//! test server never overwrites the file a person's editor wrote.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use catchlight_editor_protocol::{Command, Reply, Request};
use catchlight_editor_server::connection::ConnectionFile;
use catchlight_editor_server::{bind_http, Editor, FileStorage, HttpOptions};

const USAGE: &str = "usage: catchlight-editor-server [--socket <path>] [--store <dir>] \
                     [--http <addr>] [--allow-origin <origin>]... \
                     [--connection-file <path>] [<model.clm>]";

#[derive(Debug)]
struct Args {
    /// Where to listen. `None` means the default socket path.
    socket: Option<PathBuf>,
    /// What a relative `path` key resolves against. `None` means the
    /// current directory.
    store: Option<PathBuf>,
    http: Option<SocketAddr>,
    allowed_origins: Vec<String>,
    connection_file: Option<PathBuf>,
    model: Option<String>,
}

fn main() -> std::io::Result<()> {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("catchlight-editor-server: {message}");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    // The store holds the server's own files: what `session_open` reads
    // and what a save writes. Bytes a client holds arrive attached to the
    // command that uses them and never become a key here.
    let store = match args.store {
        Some(dir) => FileStorage::new(dir),
        None => FileStorage::default(),
    };
    let editor = Arc::new(Editor::with_storage(Arc::new(store)));

    if let Some(path) = args.model {
        // Through `handle`, not a side door: the session has to show up in
        // `session_list` like any other.
        let reply = editor.handle(Request {
            id: 0,
            command: Command::SessionOpen { path: path.clone() },
        });
        match reply {
            Reply::Err { message, .. } => {
                eprintln!("catchlight-editor-server: could not open {path}: {message}");
                std::process::exit(1);
            }
            _ => eprintln!("catchlight-editor-server: opened {path}"),
        }
    }

    let Some(addr) = args.http else {
        return serve_socket(editor, args.socket);
    };

    let server = bind_http(
        editor.clone(),
        addr,
        HttpOptions {
            allowed_origins: args.allowed_origins,
            token: None,
            ..HttpOptions::default()
        },
    )?;
    eprintln!("catchlight-editor-server: http://{}", server.addr);
    eprintln!("catchlight-editor-server: token: {}", server.token);
    if let Some(path) = &args.connection_file {
        let file = std::sync::Mutex::new(Some(ConnectionFile::write(path, &server)?));
        // `serve` never returns, so the file leaves on a signal or not at all.
        ctrlc::set_handler(move || {
            if let Ok(mut file) = file.lock() {
                file.take();
            }
            std::process::exit(130);
        })
        .map_err(std::io::Error::other)?;
    }
    // The socket keeps serving agents in the background; HTTP owns main.
    spawn_socket(editor, args.socket);
    server.serve()
}

#[cfg(unix)]
fn serve_socket(editor: Arc<Editor>, socket: Option<PathBuf>) -> std::io::Result<()> {
    let socket = socket.unwrap_or_else(catchlight_editor_server::default_socket_path);
    eprintln!(
        "catchlight-editor-server: listening on {}",
        socket.display()
    );
    catchlight_editor_server::serve_unix(editor, &socket)
}

#[cfg(not(unix))]
fn serve_socket(_: Arc<Editor>, _: Option<PathBuf>) -> std::io::Result<()> {
    eprintln!("catchlight-editor-server: the socket is unix-only; pass --http <addr>");
    eprintln!("{USAGE}");
    std::process::exit(2);
}

#[cfg(unix)]
fn spawn_socket(editor: Arc<Editor>, socket: Option<PathBuf>) {
    let socket = socket.unwrap_or_else(catchlight_editor_server::default_socket_path);
    eprintln!(
        "catchlight-editor-server: listening on {}",
        socket.display()
    );
    std::thread::spawn(move || {
        if let Err(err) = catchlight_editor_server::serve_unix(editor, &socket) {
            eprintln!("catchlight-editor-server: socket stopped: {err}");
        }
    });
}

#[cfg(not(unix))]
fn spawn_socket(_: Arc<Editor>, socket: Option<PathBuf>) {
    if socket.is_some() {
        eprintln!("catchlight-editor-server: --socket ignored: the socket is unix-only");
    }
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        socket: None,
        store: None,
        http: None,
        allowed_origins: Vec::new(),
        connection_file: None,
        model: None,
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                let value = args.next().ok_or("--socket needs a path")?;
                parsed.socket = Some(PathBuf::from(value));
            }
            "--store" => {
                let value = args.next().ok_or("--store needs a directory")?;
                parsed.store = Some(PathBuf::from(value));
            }
            "--http" => {
                let value = args.next().ok_or("--http needs an address")?;
                parsed.http = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--http {value}: not a host:port address"))?,
                );
            }
            "--allow-origin" => {
                parsed
                    .allowed_origins
                    .push(args.next().ok_or("--allow-origin needs an origin")?);
            }
            "--connection-file" => {
                let value = args.next().ok_or("--connection-file needs a path")?;
                parsed.connection_file = Some(PathBuf::from(value));
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other if parsed.model.is_none() => parsed.model = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    if parsed.connection_file.is_some() && parsed.http.is_none() {
        return Err("--connection-file records the http door; pass --http too".into());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| (*s).to_string()))
    }

    /// Absent means the default, which is the socket a person's editor is
    /// already on — so a harness that wants its own has to say so.
    #[test]
    fn the_socket_is_the_default_until_a_flag_names_one() {
        assert_eq!(parse_args(&[]).ok().and_then(|a| a.socket), None);
        let args =
            parse_args(&["--socket", "/run/catchlight-test/server.sock"]).expect("a socket path");
        assert_eq!(
            args.socket,
            Some(PathBuf::from("/run/catchlight-test/server.sock"))
        );
        assert!(parse_args(&["--socket"]).is_err());
    }

    #[test]
    fn the_store_is_a_directory_and_defaults_to_none() {
        assert_eq!(parse_args(&[]).ok().and_then(|a| a.store), None);
        let args = parse_args(&["--store", "models"]).expect("a store directory");
        assert_eq!(args.store, Some(PathBuf::from("models")));
        assert!(parse_args(&["--store"]).is_err());
    }

    #[test]
    fn http_takes_an_address_and_origins_accumulate() {
        let args = parse_args(&[
            "--http",
            "127.0.0.1:9377",
            "--allow-origin",
            "http://localhost:5173",
            "--allow-origin",
            "http://example.test",
        ])
        .expect("an http address");
        assert_eq!(args.http, Some("127.0.0.1:9377".parse().expect("an addr")));
        assert_eq!(
            args.allowed_origins,
            vec![
                "http://localhost:5173".to_string(),
                "http://example.test".to_string()
            ]
        );
        assert!(parse_args(&["--http", "9377"]).is_err());
        assert!(parse_args(&["--http"]).is_err());
    }

    #[test]
    fn the_one_positional_is_the_model_and_a_second_is_an_error() {
        let args = parse_args(&["--store", "models", "akari.clm"]).expect("a model");
        assert_eq!(args.model.as_deref(), Some("akari.clm"));
        assert!(parse_args(&["a.clm", "b.clm"]).is_err());
    }

    #[test]
    fn an_unknown_flag_is_refused_by_name() {
        let err = parse_args(&["--socket", "s.sock", "--nope"]).expect_err("a rejection");
        assert!(err.contains("--nope"), "{err}");
    }

    #[test]
    fn the_connection_file_needs_the_http_door() {
        let args = parse_args(&["--http", "127.0.0.1:0", "--connection-file", "c.json"])
            .expect("a connection file");
        assert_eq!(args.connection_file, Some(PathBuf::from("c.json")));
        assert!(parse_args(&["--connection-file", "c.json"]).is_err());
        assert!(parse_args(&["--http", "127.0.0.1:0", "--connection-file"]).is_err());
    }
}
