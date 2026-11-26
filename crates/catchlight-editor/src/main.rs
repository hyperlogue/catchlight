//! `catchlight-editor` — the Catchlight editor as one program.
//!
//! ```text
//! catchlight-editor [serve] [--port <port>] [--store <dir>] [--keep <where>] [--no-open] [<model.clm>]
//! catchlight-editor gc [--dry-run] [--unused-days <n>] [--keep <user|dir>]
//! catchlight-editor <command> ...
//! ```
//!
//! Each model's review threads and saved versions are kept per user (`--keep
//! user`, the default), beside each model (`beside`), or under a named
//! directory; `CATCHLIGHT_KEEP` supplies the value when the flag is absent.
//! `gc` removes what was kept for models that no longer exist; nothing
//! removes it on its own.
//!
//! A second `serve` that finds a live editor in the connection file opens that
//! one instead of starting another, so agents and tabs never split across two
//! processes.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use catchlight_editor_cli::Discovery;
use catchlight_editor_protocol::{Command, Reply, Request};
use catchlight_editor_server::connection::{
    Connection, ConnectionFile, default_connection_path, read_connection,
};
use catchlight_editor_server::{
    Editor, FileStorage, HttpOptions, KeptStore, SidecarKeptStore, UiFiles, UserKeptStore,
    bind_http, user_kept_dir,
};

mod ui {
    include!(concat!(env!("OUT_DIR"), "/ui.rs"));
}

const NAME: &str = "catchlight-editor";
/// Shipped in the binary so it always matches the commands it names.
const GUIDE: &str = include_str!("guide.md");
/// Tried first so a bookmark keeps working; any free port when it is taken.
const DEFAULT_PORT: u16 = 7323;

const USAGE: &str = "\
catchlight-editor: the Catchlight editor in your browser, drivable by local agents

usage:
  catchlight-editor [serve] [--port <port>] [--store <dir>] [--keep <where>] [--no-open] [<model.clm>]
  catchlight-editor gc [--dry-run] [--unused-days <n>] [--keep <user|dir>]
  catchlight-editor <command> ...

serve (the default) starts the editor on 127.0.0.1, opens it in the browser and
records where it listens so that commands can find it. With an editor already
running, it opens that one instead.

  --port <port>   listen here (default 7323, or any free port if that is taken)
  --store <dir>   where relative model paths resolve (default: this directory)
  --keep <where>  where each model's review threads and saved versions are
                  kept: `user` (default) in your per-user data directory,
                  `beside` next to each model, or a directory; CATCHLIGHT_KEEP
                  sets it when the flag is absent
  --no-open       do not open a browser

gc removes the threads and versions kept for models that no longer exist; see
`catchlight-editor gc --help`.

Every other command drives the running editor, e.g.
  catchlight-editor session list
  catchlight-editor node tree
Run `catchlight-editor help` for the full command list.
Agents: read `catchlight-editor guide` once per session before working.";

const GC_USAGE: &str = "\
usage: catchlight-editor gc [--dry-run] [--unused-days <n>] [--keep <user|dir>]

Removes the review threads and saved versions kept for models that no longer
exist and that have not changed for <n> days (default 30). It works on the
files directly; no editor needs to be running.

  --dry-run           list what would be removed, removing nothing
  --unused-days <n>   how long they must have gone unchanged (default 30)
  --keep <where>      `user` (default) or a directory; CATCHLIGHT_KEEP sets it
                      when the flag is absent";

#[derive(Debug, Default, PartialEq)]
struct Serve {
    port: Option<u16>,
    store: Option<PathBuf>,
    keep: Option<Keep>,
    open: bool,
    model: Option<PathBuf>,
}

impl Serve {
    /// The flags only a newly started server reads.
    fn server_flags(&self) -> Vec<&'static str> {
        [
            ("--port", self.port.is_some()),
            ("--store", self.store.is_some()),
            ("--keep", self.keep.is_some()),
        ]
        .into_iter()
        .filter_map(|(flag, given)| given.then_some(flag))
        .collect()
    }
}

fn main() -> Result<ExitCode> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let first = args.get(1).and_then(|a| a.to_str());
    match first {
        Some("-h" | "--help") => println!("{USAGE}"),
        Some("-V" | "--version") => println!("{NAME} {}", env!("CARGO_PKG_VERSION")),
        Some("guide") if args.len() == 2 => print!("{GUIDE}"),
        Some("gc") => gc_command(&args[2..])?,
        Some("serve") => return serve(parse_serve(&args[2..])?),
        None => return serve(parse_serve(&[])?),
        Some(first) if is_serve_argument(first) => return serve(parse_serve(&args[1..])?),
        Some(_) => return catchlight_editor_cli::run(NAME, Discovery::ConnectionFirst, args),
    }
    Ok(ExitCode::SUCCESS)
}

fn is_serve_argument(arg: &str) -> bool {
    matches!(arg, "--port" | "--store" | "--keep" | "--no-open")
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
            Some("--keep") => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--keep needs user, beside or a directory"))?;
                serve.keep = Some(Keep::parse(value));
            }
            Some("--no-open") => serve.open = false,
            Some(flag) if flag.starts_with('-') => bail!("unknown flag {flag}\n\n{USAGE}"),
            _ if serve.model.is_none() => serve.model = Some(PathBuf::from(arg)),
            _ => bail!("unexpected argument {}\n\n{USAGE}", arg.to_string_lossy()),
        }
    }
    Ok(serve)
}

fn serve(options: Serve) -> Result<ExitCode> {
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

    // Held from the liveness check until the new record is written, so two
    // launches at once cannot both start. The OS drops it with the process.
    let lock = launch_lock(&connection_path)?;
    if let Some(running) = read_connection(&connection_path).filter(is_live) {
        drop(lock);
        eprintln!("{NAME}: an editor is already running at {}", running.url);
        let ignored = options.server_flags();
        if !ignored.is_empty() {
            eprintln!(
                "{NAME}: ignoring {} because that editor is already running; stop it to change them",
                ignored.join(", ")
            );
        }
        if let Some(model) = model {
            let code = open_in_running(&running, &model)?;
            if code != ExitCode::SUCCESS {
                return Ok(code);
            }
        }
        if options.open {
            open_browser(&page_url(&running.url, &running.token));
        }
        return Ok(ExitCode::SUCCESS);
    }

    let store = match options.store {
        Some(dir) => FileStorage::new(dir),
        None => FileStorage::default(),
    };
    let kept = Keep::chosen(options.keep).store(store.root());
    let editor = Arc::new(Editor::with_storage(Arc::new(store)).with_kept_store(kept));
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
    // The page gets the token in its URL fragment, so `GET /token`, which
    // any local process can read, stays off.
    let http_options = || HttpOptions {
        ui: (!ui.is_empty()).then_some(ui),
        serve_token: false,
        ..HttpOptions::default()
    };
    let loopback = |port| SocketAddr::from(([127, 0, 0, 1], port));
    let server = match options.port {
        Some(port) => bind_http(editor, loopback(port), http_options())
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
    drop(lock);
    let page = page_url(&url, &server.token);
    let on_signal = file.clone();
    // Dropping the file is what removes it; a signal skips destructors.
    // `ctrlc`'s `termination` feature covers SIGTERM and SIGHUP too.
    ctrlc::set_handler(move || {
        if let Ok(mut file) = on_signal.lock() {
            file.take();
        }
        std::process::exit(130);
    })
    .context("installing the Ctrl-C handler")?;

    eprintln!("{NAME}: {page}");
    eprintln!(
        "{NAME}: connection recorded in {}",
        connection_path.display()
    );
    eprintln!("{NAME}: drive it with `{NAME} <command>`; Ctrl-C stops it");
    if options.open && !ui.is_empty() {
        open_browser(&page);
    }
    let served = server.serve();
    if let Ok(mut file) = file.lock() {
        file.take();
    }
    served.context("serving the editor")?;
    Ok(ExitCode::SUCCESS)
}

/// Where reviews and versions are kept, from `--keep` or `CATCHLIGHT_KEEP`.
#[derive(Debug, PartialEq)]
enum Keep {
    User,
    Beside,
    In(PathBuf),
}

impl Keep {
    fn parse(value: &OsString) -> Self {
        match value.to_str() {
            Some("user") => Self::User,
            Some("beside") => Self::Beside,
            _ => Self::In(PathBuf::from(value)),
        }
    }

    /// The flag wins over the environment.
    fn chosen(flag: Option<Self>) -> Self {
        flag.or_else(|| {
            std::env::var_os("CATCHLIGHT_KEEP")
                .filter(|v| !v.is_empty())
                .map(|v| Self::parse(&v))
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
                        "{NAME}: no per-user data directory; keeping reviews and versions \
                         beside each model"
                    );
                    return Arc::new(SidecarKeptStore::new(root));
                }
            },
        };
        Arc::new(UserKeptStore::new(root, dir))
    }
}

#[derive(Debug, PartialEq)]
struct Gc {
    dry_run: bool,
    unused_days: u64,
    keep: Option<Keep>,
}

fn gc_command(args: &[OsString]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{GC_USAGE}");
        return Ok(());
    }
    let mut gc = parse_gc(args)?;
    let dir = match Keep::chosen(gc.keep.take()) {
        Keep::Beside => bail!(
            "what is kept `beside` a model lives next to its file; there is nothing to collect"
        ),
        Keep::User => user_kept_dir()
            .ok_or_else(|| anyhow!("no per-user data directory; pass --keep <dir>"))?,
        Keep::In(dir) => dir,
    };
    let removed_all = collect(
        &gc,
        &dir,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )?;
    if !removed_all {
        std::process::exit(1);
    }
    Ok(())
}

fn parse_gc(args: &[OsString]) -> Result<Gc> {
    let mut gc = Gc {
        dry_run: false,
        unused_days: 30,
        keep: None,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--dry-run") => gc.dry_run = true,
            Some("--unused-days") => {
                let value = args.next().and_then(|v| v.to_str());
                let value = value.ok_or_else(|| anyhow!("--unused-days needs a number"))?;
                gc.unused_days = value
                    .parse()
                    .map_err(|_| anyhow!("--unused-days {value}: not a number of days"))?;
            }
            Some("--keep") => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--keep needs user or a directory"))?;
                gc.keep = Some(Keep::parse(value));
            }
            _ => bail!(
                "unexpected argument {}\n\n{GC_USAGE}",
                arg.to_string_lossy()
            ),
        }
    }
    Ok(gc)
}

/// Lists, and unless `dry_run` removes, what is kept unused under `dir`.
/// `false` when one could not be removed; the rest are removed regardless.
fn collect(gc: &Gc, dir: &Path, out: &mut dyn Write, err: &mut dyn Write) -> Result<bool> {
    const DAY: u64 = 24 * 60 * 60;
    let unused_for = gc
        .unused_days
        .checked_mul(DAY)
        .ok_or_else(|| anyhow!("--unused-days {}: too many", gc.unused_days))?;
    // Listing and removing never resolve a model key, so the root is unused.
    let store = UserKeptStore::new(".", dir);
    let unused = store
        .unused(Duration::from_secs(unused_for))
        .with_context(|| format!("listing {}", dir.display()))?;
    let now = SystemTime::now();
    let (mut count, mut bytes, mut removed_all) = (0, 0, true);
    for kept in &unused {
        if !gc.dry_run
            && let Err(e) = store.remove(kept)
        {
            writeln!(
                err,
                "{NAME}: could not remove what is kept for {}: {e}",
                kept.model.display()
            )?;
            removed_all = false;
            continue;
        }
        let days = now
            .duration_since(kept.changed)
            .unwrap_or_default()
            .as_secs()
            / DAY;
        writeln!(
            out,
            "{}  {}  unused {days} day{}",
            kept.model.display(),
            size(kept.bytes),
            if days == 1 { "" } else { "s" }
        )?;
        count += 1;
        bytes += kept.bytes;
    }
    writeln!(
        out,
        "{} what {count} model{} kept ({}) from {}",
        if gc.dry_run {
            "would remove"
        } else {
            "removed"
        },
        if count == 1 { "" } else { "s" },
        size(bytes),
        dir.display()
    )?;
    Ok(removed_all)
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KiB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    }
}

/// The browser never sends the fragment over the wire.
fn page_url(url: &str, token: &str) -> String {
    format!("{url}/#token={token}")
}

fn launch_lock(connection: &Path) -> Result<std::fs::File> {
    let parent = connection.parent().unwrap_or_else(|| Path::new("."));
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(parent)
        .with_context(|| format!("creating {}", parent.display()))?;
    let path = connection.with_extension("lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", path.display()))?;
    Ok(file)
}

/// A port reused by anything else, or a newer editor with another token, fails.
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
    let body = r#"{"id":0,"cmd":"session_list"}"#;
    let request = format!(
        "POST /request HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        connection.token,
        body.len()
    );
    let mut response = Vec::new();
    if stream.write_all(request.as_bytes()).is_err() || stream.read_to_end(&mut response).is_err() {
        return false;
    }
    response.starts_with(b"HTTP/1.1 200")
}

fn open_in_running(running: &Connection, model: &str) -> Result<ExitCode> {
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

    /// Otherwise the guide teaches an agent to fail.
    #[test]
    fn the_guide_names_only_real_commands() {
        let commands = catchlight_editor_cli::command_names();
        let mut checked = 0;
        for line in GUIDE.lines() {
            for (index, _) in line.match_indices("catchlight-editor ") {
                let rest = &line[index + NAME.len() + 1..];
                let word: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '/')
                    .collect();
                if word.is_empty() || word.starts_with('-') || word.starts_with('/') {
                    continue;
                }
                assert!(
                    commands.contains(&word) || matches!(word.as_str(), "guide" | "help" | "gc"),
                    "the guide names `{NAME} {word}`, which is no command"
                );
                checked += 1;
            }
        }
        assert!(checked > 10, "only {checked} commands found in the guide");
    }

    #[test]
    fn serve_flags_parse_and_a_second_model_is_refused() {
        let serve = parse_serve(&args(&["--port", "9000", "--no-open", "a.clm"])).unwrap();
        assert_eq!(
            serve,
            Serve {
                port: Some(9000),
                store: None,
                keep: None,
                open: false,
                model: Some(PathBuf::from("a.clm")),
            }
        );
        assert!(parse_serve(&args(&["a.clm", "b.clm"])).is_err());
        assert!(parse_serve(&args(&["--port", "x"])).is_err());
        assert!(parse_serve(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn only_server_flags_are_named_when_an_editor_already_runs() {
        let flags = |list: &[&str]| parse_serve(&args(list)).unwrap().server_flags();
        assert!(flags(&["--no-open", "a.clm"]).is_empty());
        assert_eq!(
            flags(&["--keep", "user", "--port", "9000", "--store", "s"]),
            ["--port", "--store", "--keep"]
        );
    }

    #[test]
    fn kept_files_live_per_user_beside_the_model_or_in_a_named_directory() {
        let keep = |list: &[&str]| parse_serve(&args(list)).unwrap().keep;
        assert_eq!(keep(&[]), None);
        assert_eq!(keep(&["--keep", "user"]), Some(Keep::User));
        assert_eq!(keep(&["--keep", "beside"]), Some(Keep::Beside));
        assert_eq!(
            keep(&["--keep", "/srv/kept", "a.clm"]),
            Some(Keep::In(PathBuf::from("/srv/kept")))
        );
        assert!(parse_serve(&args(&["--keep"])).is_err());
        assert!(is_serve_argument("--keep"));
        assert_eq!(Keep::chosen(Some(Keep::Beside)), Keep::Beside);
    }

    #[test]
    fn gc_flags_parse_with_thirty_days_by_default() {
        assert_eq!(
            parse_gc(&[]).unwrap(),
            Gc {
                dry_run: false,
                unused_days: 30,
                keep: None,
            }
        );
        assert_eq!(
            parse_gc(&args(&["--dry-run", "--unused-days", "0", "--keep", "/r"])).unwrap(),
            Gc {
                dry_run: true,
                unused_days: 0,
                keep: Some(Keep::In(PathBuf::from("/r"))),
            }
        );
        assert!(parse_gc(&args(&["--unused-days", "-1"])).is_err());
        assert!(parse_gc(&args(&["--unused-days"])).is_err());
        assert!(parse_gc(&args(&["a.clm"])).is_err());
        assert!(gc_command(&args(&["--keep", "beside"])).is_err());
        assert!(gc_command(&args(&["prune"])).is_err());
    }

    #[test]
    fn gc_lists_on_a_dry_run_and_removes_only_what_missing_models_kept() {
        let dir = std::env::temp_dir().join(format!("cl-editor-gc-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let kept = dir.join("kept");
        let live = dir.join("live.clm");
        let gone = dir.join("gone.clm");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&live, b"").unwrap();
        for (id, model) in [("a", &gone), ("b", &live)] {
            let one = kept.join(id);
            std::fs::create_dir_all(one.join("versions/chunks")).unwrap();
            std::fs::write(one.join("model"), model.to_str().unwrap()).unwrap();
            std::fs::write(one.join("review.json"), b"{}").unwrap();
            std::fs::write(one.join("versions/1.json"), b"{}").unwrap();
            std::fs::write(one.join("versions/chunks/ab"), b"chunk").unwrap();
        }
        let bytes = gone.to_str().unwrap().len() + 2 + 2 + 5;
        let run = |dry_run| {
            let gc = Gc {
                dry_run,
                unused_days: 0,
                keep: None,
            };
            let (mut out, mut err) = (Vec::new(), Vec::new());
            assert!(collect(&gc, &kept, &mut out, &mut err).unwrap());
            assert!(err.is_empty());
            String::from_utf8(out).unwrap()
        };

        let line = format!("{}  {bytes} B  unused 0 days", gone.display());
        assert_eq!(
            run(true),
            format!(
                "{line}\nwould remove what 1 model kept ({bytes} B) from {}\n",
                kept.display()
            )
        );
        assert!(kept.join("a").exists());
        assert_eq!(
            run(false),
            format!(
                "{line}\nremoved what 1 model kept ({bytes} B) from {}\n",
                kept.display()
            )
        );
        assert!(!kept.join("a").exists());
        assert!(kept.join("b").exists());
        assert_eq!(
            run(false),
            format!("removed what 0 models kept (0 B) from {}\n", kept.display())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_record_whose_port_answers_with_another_token_is_not_live() {
        let server = bind_http(
            Arc::new(Editor::new()),
            "127.0.0.1:0".parse().unwrap(),
            HttpOptions {
                serve_token: false,
                ..HttpOptions::default()
            },
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
