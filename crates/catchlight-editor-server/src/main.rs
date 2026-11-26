//! Headless standalone server: create an [`Editor`] and serve it on a Unix
//! socket, over HTTP, or both.
//!
//! A unix socket path is bounded at around 100 bytes (`SUN_LEN`). The socket is
//! Unix-only; elsewhere `--http` is required.

// The server runs in-tab through `catchlight-editor-wasm` on wasm32; this
// stub keeps `cargo build --workspace` building.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::io::Result<()> {
    native::main()
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use catchlight_editor_protocol::{Command, Reply, Request};
    use catchlight_editor_server::connection::ConnectionFile;
    use catchlight_editor_server::{
        Editor, FileStorage, HttpOptions, KeptStore, SidecarKeptStore, UserKeptStore, bind_http,
        user_kept_dir,
    };

    const USAGE: &str = "usage: catchlight-editor-server [--socket <path>] [--root <dir>] \
                         [--keep <user|beside|dir>] [--http <addr> [--allow-remote]] \
                         [--allow-origin <origin>]... [--connection-file <path>] \
                         [<model.clm>]

  --root     the only directory the editor reads and writes models in; a
             relative path resolves under it (default: the model's directory,
             else this one)
  --allow-remote  let --http listen on an address other machines reach; the
                  token is then never served, so hand it to the client yourself

  --keep     where each model's review threads and saved versions are kept:
             `user` (default) in the per-user data directory, `beside` next to
             each model, or a directory; CATCHLIGHT_KEEP sets it when the flag
             is absent";

    /// Where reviews and versions are kept, from `--keep` or `CATCHLIGHT_KEEP`.
    #[derive(Debug, PartialEq)]
    enum Keep {
        User,
        Beside,
        In(PathBuf),
    }

    impl Keep {
        fn parse(value: &str) -> Self {
            match value {
                "user" => Self::User,
                "beside" => Self::Beside,
                dir => Self::In(PathBuf::from(dir)),
            }
        }

        /// The flag wins over the environment.
        fn chosen(flag: Option<Self>) -> Self {
            flag.or_else(|| {
                let value = std::env::var_os("CATCHLIGHT_KEEP").filter(|v| !v.is_empty())?;
                Some(match value.to_str() {
                    Some(value) => Self::parse(value),
                    None => Self::In(PathBuf::from(value)),
                })
            })
            .unwrap_or(Self::User)
        }

        fn store(self, root: &Path) -> Arc<dyn KeptStore> {
            let dir = match self {
                Self::Beside => return Arc::new(SidecarKeptStore::new(root)),
                Self::In(dir) => dir,
                Self::User => match user_kept_dir() {
                    Some(dir) => dir,
                    None => {
                        eprintln!(
                            "catchlight-editor-server: no per-user data directory; keeping \
                             reviews and versions beside each model"
                        );
                        return Arc::new(SidecarKeptStore::new(root));
                    }
                },
            };
            Arc::new(UserKeptStore::new(root, dir))
        }
    }

    #[derive(Debug)]
    struct Args {
        /// `None` means the default socket path.
        socket: Option<PathBuf>,
        /// `None` means the model's directory, else the current one.
        root: Option<PathBuf>,
        /// `None` means `CATCHLIGHT_KEEP`, else the per-user directory.
        keep: Option<Keep>,
        http: Option<SocketAddr>,
        allow_remote: bool,
        allowed_origins: Vec<String>,
        connection_file: Option<PathBuf>,
        model: Option<String>,
    }

    pub fn main() -> std::io::Result<()> {
        let args = match parse(std::env::args().skip(1)) {
            Ok(args) => args,
            Err(message) => {
                eprintln!("catchlight-editor-server: {message}");
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        };

        let store = rooted(args.root, args.model)
            .and_then(|(root, model)| Ok((FileStorage::new(root)?, model)));
        let (store, model) = match store {
            Ok(rooted) => rooted,
            Err(e) => {
                eprintln!("catchlight-editor-server: {e}");
                std::process::exit(2);
            }
        };
        let root = store.root().to_path_buf();
        let kept = Keep::chosen(args.keep).store(&root);
        let editor = Arc::new(Editor::with_storage(Arc::new(store)).with_kept_store(kept));

        if let Some(path) = model {
            // Through `handle` so the session shows up in `session_list`.
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
                allow_remote: args.allow_remote,
                ..HttpOptions::default()
            },
        )?;
        eprintln!("catchlight-editor-server: http://{}", server.addr);
        eprintln!("catchlight-editor-server: token: {}", server.token);
        if let Some(path) = &args.connection_file {
            let file = std::sync::Mutex::new(Some(ConnectionFile::write(path, &server, &root)?));
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

    /// The root, and the key the model on the command line opens under it.
    /// Without `--root`, a model roots the editor in its own directory and
    /// is named relative to this one.
    fn rooted(
        root: Option<PathBuf>,
        model: Option<String>,
    ) -> std::io::Result<(PathBuf, Option<String>)> {
        match (root, model) {
            (Some(root), model) => Ok((root, model)),
            (None, Some(model)) => {
                let path = std::path::absolute(&model)?;
                let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
                Ok((dir, Some(path.display().to_string())))
            }
            (None, None) => Ok((std::env::current_dir()?, None)),
        }
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
            root: None,
            keep: None,
            http: None,
            allow_remote: false,
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
                "--root" => {
                    let value = args.next().ok_or("--root needs a directory")?;
                    parsed.root = Some(PathBuf::from(value));
                }
                "--keep" => {
                    let value = args
                        .next()
                        .ok_or("--keep needs user, beside or a directory")?;
                    parsed.keep = Some(Keep::parse(&value));
                }
                "--http" => {
                    let value = args.next().ok_or("--http needs an address")?;
                    parsed.http = Some(
                        value
                            .parse()
                            .map_err(|_| format!("--http {value}: not a host:port address"))?,
                    );
                }
                "--allow-remote" => parsed.allow_remote = true,
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
        match parsed.http {
            None if parsed.allow_remote => {
                return Err("--allow-remote opens the http door; pass --http too".into());
            }
            Some(addr) if !addr.ip().is_loopback() && !parsed.allow_remote => {
                return Err(format!(
                    "--http {addr}: not a loopback address; pass --allow-remote to let other \
                     machines reach it"
                ));
            }
            _ => {}
        }
        Ok(parsed)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn parse_args(args: &[&str]) -> Result<Args, String> {
            parse(args.iter().map(|s| (*s).to_string()))
        }

        #[test]
        fn the_socket_is_the_default_until_a_flag_names_one() {
            assert_eq!(parse_args(&[]).ok().and_then(|a| a.socket), None);
            let args = parse_args(&["--socket", "/run/catchlight-test/server.sock"])
                .expect("a socket path");
            assert_eq!(
                args.socket,
                Some(PathBuf::from("/run/catchlight-test/server.sock"))
            );
            assert!(parse_args(&["--socket"]).is_err());
        }

        #[test]
        fn the_root_is_named_else_the_models_directory_else_this_one() {
            assert_eq!(parse_args(&[]).ok().and_then(|a| a.root), None);
            let args = parse_args(&["--root", "models"]).expect("a root directory");
            assert_eq!(args.root, Some(PathBuf::from("models")));
            assert!(parse_args(&["--root"]).is_err());

            let named = rooted(Some("models".into()), Some("akari.clm".into())).unwrap();
            assert_eq!(named, ("models".into(), Some("akari.clm".into())));
            let here = std::env::current_dir().unwrap();
            let (root, model) = rooted(None, Some("sub/akari.clm".into())).unwrap();
            assert_eq!(root, here.join("sub"));
            assert_eq!(
                model,
                Some(here.join("sub/akari.clm").display().to_string())
            );
            assert_eq!(rooted(None, None).unwrap(), (here, None));
        }

        #[test]
        fn kept_files_live_per_user_beside_the_model_or_in_a_named_directory() {
            let keep = |list: &[&str]| parse_args(list).unwrap().keep;
            assert_eq!(keep(&[]), None);
            assert_eq!(keep(&["--keep", "user"]), Some(Keep::User));
            assert_eq!(keep(&["--keep", "beside"]), Some(Keep::Beside));
            assert_eq!(
                keep(&["--keep", "/srv/kept"]),
                Some(Keep::In(PathBuf::from("/srv/kept")))
            );
            assert!(parse_args(&["--keep"]).is_err());
            assert_eq!(Keep::chosen(Some(Keep::Beside)), Keep::Beside);
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
        fn a_non_loopback_address_needs_remote_clients_allowed() {
            let err = parse_args(&["--http", "0.0.0.0:9377"]).expect_err("a refusal");
            assert!(err.contains("--allow-remote"), "{err}");
            let args =
                parse_args(&["--http", "0.0.0.0:9377", "--allow-remote"]).expect("remote clients");
            assert!(args.allow_remote);
            assert!(
                !parse_args(&["--http", "127.0.0.1:9377"])
                    .unwrap()
                    .allow_remote
            );
            assert!(parse_args(&["--allow-remote"]).is_err());
        }

        #[test]
        fn the_one_positional_is_the_model_and_a_second_is_an_error() {
            let args = parse_args(&["--root", "models", "akari.clm"]).expect("a model");
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
}
