//! `catchlight-editor` — the Catchlight editor as one program.
//!
//! ```text
//! catchlight-editor [serve] [--port <port>] [--store <dir>] [--reviews <where>] [--no-open] [<model.clm>]
//! catchlight-editor reviews gc [--dry-run] [--unused-days <n>] [--reviews <user|dir>]
//! catchlight-editor <command> ...
//! ```
//!
//! Review threads are kept per user (`--reviews user`, the default), beside
//! each model (`beside`), or under a named directory; `CATCHLIGHT_REVIEWS`
//! supplies the value when the flag is absent. `reviews gc` removes the kept
//! threads of models that no longer exist; nothing removes them on its own.
//!
//! A second `serve` that finds a live editor in the connection file opens that
//! one instead of starting another, so agents and tabs never split across two
//! processes.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use catchlight_editor_cli::Discovery;
use catchlight_editor_protocol::{Command, Reply, Request};
use catchlight_editor_server::connection::{
    default_connection_path, read_connection, Connection, ConnectionFile,
};
use catchlight_editor_server::{
    bind_http, user_review_dir, Editor, FileStorage, HttpOptions, ReviewStore, SidecarReviewStore,
    UiFiles, UserReviewStore,
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
  catchlight-editor [serve] [--port <port>] [--store <dir>] [--reviews <where>] [--no-open] [<model.clm>]
  catchlight-editor reviews gc [--dry-run] [--unused-days <n>] [--reviews <user|dir>]
  catchlight-editor <command> ...

serve (the default) starts the editor on 127.0.0.1, opens it in the browser and
records where it listens so that commands can find it. With an editor already
running, it opens that one instead.

  --port <port>   listen here (default 7323, or any free port if that is taken)
  --store <dir>   where relative model paths resolve (default: this directory)
  --reviews <where>
                  where review threads are kept: `user` (default) in your
                  per-user data directory, `beside` next to each model, or a
                  directory; CATCHLIGHT_REVIEWS sets it when the flag is absent
  --no-open       do not open a browser

reviews gc removes the kept threads of models that no longer exist; see
`catchlight-editor reviews --help`.

Every other command drives the running editor, e.g.
  catchlight-editor session list
  catchlight-editor node tree
Run `catchlight-editor help` for the full command list.
Agents: read `catchlight-editor guide` once per session before working.";

const REVIEWS_USAGE: &str = "\
usage: catchlight-editor reviews gc [--dry-run] [--unused-days <n>] [--reviews <user|dir>]

Removes the review threads kept for models that no longer exist and that have
not changed for <n> days (default 30). It works on the files directly; no
editor needs to be running.

  --dry-run           list what would be removed, removing nothing
  --unused-days <n>   how long a review must have gone unchanged (default 30)
  --reviews <where>   `user` (default) or a directory; CATCHLIGHT_REVIEWS sets
                      it when the flag is absent";

#[derive(Debug, Default, PartialEq)]
struct Serve {
    port: Option<u16>,
    store: Option<PathBuf>,
    reviews: Option<Reviews>,
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
        Some("guide") if args.len() == 2 => {
            print!("{GUIDE}");
            Ok(())
        }
        Some("reviews") => reviews_command(&args[2..]),
        Some("serve") => serve(parse_serve(&args[2..])?),
        None => serve(parse_serve(&[])?),
        Some(first) if is_serve_argument(first) => serve(parse_serve(&args[1..])?),
        Some(_) => catchlight_editor_cli::run(NAME, Discovery::ConnectionFirst, args),
    }
}

fn is_serve_argument(arg: &str) -> bool {
    matches!(arg, "--port" | "--store" | "--reviews" | "--no-open")
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
            Some("--reviews") => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--reviews needs user, beside or a directory"))?;
                serve.reviews = Some(Reviews::parse(value));
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

    // Held from the liveness check until the new record is written, so two
    // launches at once cannot both start. The OS drops it with the process.
    let lock = launch_lock(&connection_path)?;
    if let Some(running) = read_connection(&connection_path).filter(is_live) {
        drop(lock);
        eprintln!("{NAME}: an editor is already running at {}", running.url);
        if let Some(model) = model {
            open_in_running(&running, &model)?;
        }
        if options.open {
            open_browser(&page_url(&running.url, &running.token));
        }
        return Ok(());
    }

    let store = match options.store {
        Some(dir) => FileStorage::new(dir),
        None => FileStorage::default(),
    };
    let reviews = Reviews::chosen(options.reviews).store(store.root());
    let editor = Arc::new(Editor::with_storage(Arc::new(store)).with_review_store(reviews));
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
    served.context("serving the editor")
}

/// Where review threads are kept, from `--reviews` or `CATCHLIGHT_REVIEWS`.
#[derive(Debug, PartialEq)]
enum Reviews {
    User,
    Beside,
    In(PathBuf),
}

impl Reviews {
    fn parse(value: &OsString) -> Self {
        match value.to_str() {
            Some("user") => Self::User,
            Some("beside") => Self::Beside,
            _ => Self::In(PathBuf::from(value)),
        }
    }

    /// The flag wins over the environment.
    fn chosen(flag: Option<Reviews>) -> Self {
        flag.or_else(|| {
            std::env::var_os("CATCHLIGHT_REVIEWS")
                .filter(|v| !v.is_empty())
                .map(|v| Self::parse(&v))
        })
        .unwrap_or(Self::User)
    }

    fn store(self, root: &Path) -> Arc<dyn ReviewStore> {
        let dir = match self {
            Self::Beside => return Arc::new(SidecarReviewStore::new(root)),
            Self::In(dir) => dir,
            Self::User => match user_review_dir() {
                Some(dir) => dir,
                None => {
                    eprintln!(
                        "{NAME}: no per-user data directory; keeping reviews beside each model"
                    );
                    return Arc::new(SidecarReviewStore::new(root));
                }
            },
        };
        Arc::new(UserReviewStore::new(root, dir))
    }
}

#[derive(Debug, PartialEq)]
struct Gc {
    dry_run: bool,
    unused_days: u64,
    reviews: Option<Reviews>,
}

fn reviews_command(args: &[OsString]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{REVIEWS_USAGE}");
        return Ok(());
    }
    match args.first().and_then(|a| a.to_str()) {
        Some("gc") => {}
        Some(other) => bail!("unknown command `reviews {other}`\n\n{REVIEWS_USAGE}"),
        None => bail!("reviews needs a command\n\n{REVIEWS_USAGE}"),
    }
    let mut gc = parse_gc(&args[1..])?;
    let dir = match Reviews::chosen(gc.reviews.take()) {
        Reviews::Beside => bail!(
            "reviews kept `beside` their models live next to each model file; there is \
             nothing to collect"
        ),
        Reviews::User => user_review_dir()
            .ok_or_else(|| anyhow!("no per-user data directory; pass --reviews <dir>"))?,
        Reviews::In(dir) => dir,
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
        reviews: None,
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
            Some("--reviews") => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--reviews needs user or a directory"))?;
                gc.reviews = Some(Reviews::parse(value));
            }
            _ => bail!(
                "unexpected argument {}\n\n{REVIEWS_USAGE}",
                arg.to_string_lossy()
            ),
        }
    }
    Ok(gc)
}

/// Lists, and unless `dry_run` removes, the unused reviews under `dir`.
/// `false` when one could not be removed; the rest are removed regardless.
fn collect(gc: &Gc, dir: &Path, out: &mut dyn Write, err: &mut dyn Write) -> Result<bool> {
    const DAY: u64 = 24 * 60 * 60;
    let unused_for = gc
        .unused_days
        .checked_mul(DAY)
        .ok_or_else(|| anyhow!("--unused-days {}: too many", gc.unused_days))?;
    // Listing and removing never resolve a model key, so the root is unused.
    let store = UserReviewStore::new(".", dir);
    let unused = store
        .unused(Duration::from_secs(unused_for))
        .with_context(|| format!("listing {}", dir.display()))?;
    let now = SystemTime::now();
    let (mut count, mut bytes, mut removed_all) = (0, 0, true);
    for review in &unused {
        if !gc.dry_run {
            if let Err(e) = store.remove(review) {
                writeln!(
                    err,
                    "{NAME}: could not remove the review of {}: {e}",
                    review.model.display()
                )?;
                removed_all = false;
                continue;
            }
        }
        let days = now
            .duration_since(review.changed)
            .unwrap_or_default()
            .as_secs()
            / DAY;
        writeln!(
            out,
            "{}  {}  unused {days} day{}",
            review.model.display(),
            size(review.bytes),
            if days == 1 { "" } else { "s" }
        )?;
        count += 1;
        bytes += review.bytes;
    }
    writeln!(
        out,
        "{} {count} review{} ({}) from {}",
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
    let parent = connection.parent().unwrap_or(Path::new("."));
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
                    commands.contains(&word)
                        || matches!(word.as_str(), "guide" | "help" | "reviews"),
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
                reviews: None,
                open: false,
                model: Some(PathBuf::from("a.clm")),
            }
        );
        assert!(parse_serve(&args(&["a.clm", "b.clm"])).is_err());
        assert!(parse_serve(&args(&["--port", "x"])).is_err());
        assert!(parse_serve(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn reviews_are_kept_per_user_beside_the_model_or_in_a_named_directory() {
        let reviews = |list: &[&str]| parse_serve(&args(list)).unwrap().reviews;
        assert_eq!(reviews(&[]), None);
        assert_eq!(reviews(&["--reviews", "user"]), Some(Reviews::User));
        assert_eq!(reviews(&["--reviews", "beside"]), Some(Reviews::Beside));
        assert_eq!(
            reviews(&["--reviews", "/srv/reviews", "a.clm"]),
            Some(Reviews::In(PathBuf::from("/srv/reviews")))
        );
        assert!(parse_serve(&args(&["--reviews"])).is_err());
        assert!(is_serve_argument("--reviews"));
        assert_eq!(Reviews::chosen(Some(Reviews::Beside)), Reviews::Beside);
    }

    #[test]
    fn gc_flags_parse_with_thirty_days_by_default() {
        assert_eq!(
            parse_gc(&[]).unwrap(),
            Gc {
                dry_run: false,
                unused_days: 30,
                reviews: None,
            }
        );
        assert_eq!(
            parse_gc(&args(&[
                "--dry-run",
                "--unused-days",
                "0",
                "--reviews",
                "/r"
            ]))
            .unwrap(),
            Gc {
                dry_run: true,
                unused_days: 0,
                reviews: Some(Reviews::In(PathBuf::from("/r"))),
            }
        );
        assert!(parse_gc(&args(&["--unused-days", "-1"])).is_err());
        assert!(parse_gc(&args(&["--unused-days"])).is_err());
        assert!(parse_gc(&args(&["a.clm"])).is_err());
        assert!(reviews_command(&args(&["gc", "--reviews", "beside"])).is_err());
        assert!(reviews_command(&args(&["prune"])).is_err());
    }

    #[test]
    fn gc_lists_on_a_dry_run_and_removes_only_reviews_of_missing_models() {
        let dir = std::env::temp_dir().join(format!("cl-editor-gc-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let reviews = dir.join("reviews");
        let live = dir.join("live.clm");
        let gone = dir.join("gone.clm");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&live, b"").unwrap();
        for (id, model) in [("a", &gone), ("b", &live)] {
            let review = reviews.join(id);
            std::fs::create_dir_all(&review).unwrap();
            std::fs::write(review.join("model"), model.to_str().unwrap()).unwrap();
            std::fs::write(review.join("review.json"), b"{}").unwrap();
        }
        let bytes = gone.to_str().unwrap().len() + 2;
        let run = |dry_run| {
            let gc = Gc {
                dry_run,
                unused_days: 0,
                reviews: None,
            };
            let (mut out, mut err) = (Vec::new(), Vec::new());
            assert!(collect(&gc, &reviews, &mut out, &mut err).unwrap());
            assert!(err.is_empty());
            String::from_utf8(out).unwrap()
        };

        let line = format!("{}  {bytes} B  unused 0 days", gone.display());
        assert_eq!(
            run(true),
            format!(
                "{line}\nwould remove 1 review ({bytes} B) from {}\n",
                reviews.display()
            )
        );
        assert!(reviews.join("a").exists());
        assert_eq!(
            run(false),
            format!(
                "{line}\nremoved 1 review ({bytes} B) from {}\n",
                reviews.display()
            )
        );
        assert!(!reviews.join("a").exists());
        assert!(reviews.join("b").exists());
        assert_eq!(
            run(false),
            format!("removed 0 reviews (0 B) from {}\n", reviews.display())
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
