//! Headless standalone server: create an [`Editor`], listen on the canonical
//! socket, and optionally serve a browser tab over HTTP + WebSocket. The GUI
//! will instead embed the library and call `serve_unix` itself.
//!
//! ```text
//! catchlight-editor-server [--http <addr>] [--allow-origin <origin>]... [<model.clm>]
//! ```

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    unix_main::run()
}

#[cfg(unix)]
mod unix_main {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use catchlight_editor_protocol::{Command, Reply, Request};
    use catchlight_editor_server::{
        bind_http, default_socket_path, serve_unix, Editor, FileStorage, HttpOptions,
        StagingStorage,
    };

    const USAGE: &str = "usage: catchlight-editor-server [--http <addr>] \
                         [--allow-origin <origin>]... [<model.clm>]";

    struct Args {
        http: Option<SocketAddr>,
        allowed_origins: Vec<String>,
        model: Option<String>,
    }

    pub fn run() -> std::io::Result<()> {
        let args = match parse(std::env::args().skip(1)) {
            Ok(args) => args,
            Err(message) => {
                eprintln!("catchlight-editor-server: {message}");
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        };

        // Uploads arrive over HTTP and are named by the same `path` key a
        // `session_open` carries, so the editor reads through staging even
        // when there is a filesystem behind it.
        let staging = Arc::new(StagingStorage::new(Arc::new(FileStorage)));
        let editor = Arc::new(Editor::with_storage(staging.clone()));

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

        let socket = default_socket_path();
        let Some(addr) = args.http else {
            eprintln!(
                "catchlight-editor-server: listening on {}",
                socket.display()
            );
            return serve_unix(editor, &socket);
        };

        let server = bind_http(
            editor.clone(),
            addr,
            HttpOptions {
                allowed_origins: args.allowed_origins,
                token: None,
                staging: Some(staging),
                ..HttpOptions::default()
            },
        )?;
        eprintln!(
            "catchlight-editor-server: listening on {}",
            socket.display()
        );
        eprintln!("catchlight-editor-server: http://{}", server.addr);
        eprintln!("catchlight-editor-server: token: {}", server.token);
        // The socket keeps serving agents in the background; HTTP owns main.
        std::thread::spawn(move || {
            if let Err(err) = serve_unix(editor, &socket) {
                eprintln!("catchlight-editor-server: socket stopped: {err}");
            }
        });
        server.serve()
    }

    fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
        let mut parsed = Args {
            http: None,
            allowed_origins: Vec::new(),
            model: None,
        };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
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
                "-h" | "--help" => {
                    println!("{USAGE}");
                    std::process::exit(0);
                }
                other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
                other if parsed.model.is_none() => parsed.model = Some(other.to_string()),
                other => return Err(format!("unexpected argument {other}")),
            }
        }
        Ok(parsed)
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("catchlight-editor-server: the socket server is unix-only");
}
