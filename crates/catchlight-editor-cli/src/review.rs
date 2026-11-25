//! `review`: the thread a person and an agent keep about a session's model.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde_json::Value;

use super::*;

/// `review watch` ran out its `--timeout` with nothing sent. Apart from 1
/// (a refused command or failed delivery) and 2 (clap's usage error).
pub(super) const TIMED_OUT: i32 = 3;
/// What `fetch` or `watch` printed changed before it was acknowledged: the
/// comments were deleted, resolved or taken by another agent meanwhile.
pub(super) const SUPERSEDED: i32 = 4;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Who an agent is when its harness says nothing and
/// `CATCHLIGHT_AGENT_SESSION` is unset: every such agent is the same one.
const DEFAULT_AGENT: &str = "agent";

#[derive(clap::Args)]
pub(super) struct ReviewArgs {
    /// The name the person sees on your comments. Defaults to your harness
    /// (claude, codex, antigravity), else "agent". Who you are is separate:
    /// the harness session, or CATCHLIGHT_AGENT_SESSION.
    #[arg(long, global = true, env = "CATCHLIGHT_AGENT")]
    name: Option<String>,
    #[command(subcommand)]
    action: ReviewCmd,
}

/// The harness's name for an agent and its session id, as r3 reads them.
fn harness(env: &dyn Fn(&str) -> Option<String>) -> Option<(&'static str, String)> {
    let value = |name: &str| {
        env(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    if let Some(id) = value("CLAUDE_CODE_SESSION_ID") {
        return Some(("claude", format!("claude:{id}")));
    }
    if let Some(id) = value("ANTIGRAVITY_CONVERSATION_ID") {
        return Some(("antigravity", format!("antigravity:{id}")));
    }
    value("CODEX_THREAD_ID")
        .or_else(|| value("CODEX_SESSION_ID"))
        .map(|id| ("codex", format!("codex:{id}")))
}

/// The agent this process writes as, from its harness and environment.
pub(super) fn agent(name: Option<&str>) -> ReviewIdentity {
    identity(name, &|name| std::env::var(name).ok())
}

fn identity(name: Option<&str>, env: &dyn Fn(&str) -> Option<String>) -> ReviewIdentity {
    // As r3 decides: the harness `listen` would notify names the agent, so
    // the worker polls as the identity the agent fetches with; only without
    // one does the first harness id found count.
    #[cfg(unix)]
    let listening = crate::listen::detected_session(env).and_then(|id| {
        let label = ["claude", "codex", "antigravity"]
            .into_iter()
            .find(|h| id.strip_prefix(h).is_some_and(|r| r.starts_with(':')))?;
        Some((label, id))
    });
    #[cfg(not(unix))]
    let listening = None;
    let detected = listening.or_else(|| harness(env));
    let id = env("CATCHLIGHT_AGENT_SESSION")
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .or_else(|| detected.as_ref().map(|(_, id)| id.clone()))
        .unwrap_or_else(|| DEFAULT_AGENT.into());
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .or(detected.map(|(label, _)| label))
        .unwrap_or(DEFAULT_AGENT);
    ReviewIdentity {
        id,
        name: name.to_owned(),
    }
}

#[derive(Subcommand)]
enum ReviewCmd {
    /// Print the open threads; `--all` adds the resolved ones.
    List {
        #[arg(long)]
        all: bool,
        /// Save the threads' images here and print each one's path.
        #[arg(long, value_name = "DIR")]
        attachments_dir: Option<PathBuf>,
    },
    /// Wait until the person sends comments, then print and acknowledge
    /// them. Exits 0 once they are delivered, 3 if `--timeout` passes first,
    /// 4 if they changed before the acknowledgment (fetch again), 1 if they
    /// could not be saved, printed or acknowledged (they stay pending).
    Watch {
        /// Give up after this many seconds; without it, wait indefinitely.
        #[arg(long)]
        timeout: Option<u64>,
        /// Save the threads' images here and print each one's path.
        #[arg(long, value_name = "DIR")]
        attachments_dir: Option<PathBuf>,
    },
    /// Have each new batch of sent comments announced in your harness
    /// session (Claude Code, Codex, Antigravity) by a background worker,
    /// then return at once. Exit 0: registered; 5: no notification adapter
    /// here, use `watch`. Listening again replaces your registration.
    Listen,
    /// End what `listen` set up for this session.
    Unlisten,
    /// The background process behind `listen`, one per user.
    Worker {
        #[command(subcommand)]
        action: WorkerAction,
    },
    /// Print and acknowledge the comments sent so far, without waiting.
    /// Exits as `watch` does.
    Fetch {
        /// Save the threads' images here and print each one's path.
        #[arg(long, value_name = "DIR")]
        attachments_dir: Option<PathBuf>,
    },
    /// Reply on a thread. The message is read from stdin when `-m` is absent
    /// and nothing is attached.
    Reply {
        thread: u64,
        #[arg(short, long)]
        message: Option<String>,
        /// A PNG, JPEG or WebP to show with the comment; up to four.
        #[arg(long, value_name = "IMAGE")]
        attach: Vec<PathBuf>,
        /// Write as the person rather than as the agent.
        #[arg(long)]
        human: bool,
        #[command(flatten)]
        key: KeyArg,
        #[command(flatten)]
        version: VersionRef,
    },
    /// Start a thread on a node, param, texture, slot or the whole model.
    Open {
        #[command(flatten)]
        target: TargetArg,
        #[arg(short, long)]
        message: Option<String>,
        /// A PNG, JPEG or WebP to show with the comment; up to four.
        #[arg(long, value_name = "IMAGE")]
        attach: Vec<PathBuf>,
        /// Write as the person rather than as the agent.
        #[arg(long)]
        human: bool,
        #[command(flatten)]
        key: KeyArg,
        #[command(flatten)]
        version: VersionRef,
    },
    /// Take or renew a 60-minute claim on threads: the person sees you
    /// working on them, and other agents leave them to you.
    Claim {
        #[arg(required = true)]
        threads: Vec<u64>,
    },
    /// Give up your claims on threads. A reply releases its thread itself.
    Release {
        #[arg(required = true)]
        threads: Vec<u64>,
    },
    /// Hand the person's unsent comments to the agent, as their Send button does.
    Send,
    /// Mark a thread resolved. The person's call, not the agent's.
    Resolve {
        thread: u64,
    },
    Reopen {
        thread: u64,
    },
    Delete {
        thread: u64,
    },
}

#[cfg(unix)]
use crate::listen::WorkerCmd as WorkerAction;

/// Only so the command parses everywhere; it is refused off unix.
#[cfg(not(unix))]
#[derive(Subcommand)]
enum WorkerAction {
    Status,
    Start,
    Stop,
    #[command(hide = true)]
    Run,
}

#[derive(clap::Args)]
struct KeyArg {
    /// Repeating the command with the same key writes nothing new and prints
    /// the original result; the same key with other content is refused.
    #[arg(long, value_name = "KEY")]
    key: Option<String>,
}

/// The saved version a comment points at, such as the one holding the fix.
#[derive(clap::Args)]
#[group(multiple = false)]
struct VersionRef {
    /// Point at this saved version; the person can open it from the comment.
    #[arg(long, value_name = "N")]
    version: Option<u64>,
    /// Save the model as a new version first, with this label if given, and
    /// point at it. With `--key`, a retry saves nothing twice.
    #[arg(long, value_name = "LABEL", num_args = 0..=1, default_missing_value = "")]
    save_version: Option<String>,
}

impl VersionRef {
    /// The version to point at, saving it first when asked; `Err(reply)`
    /// when the save was refused.
    fn resolve(
        &self,
        client: &mut Client,
        session: SessionId,
        author: &ReviewAuthor,
        key: Option<&str>,
    ) -> Result<std::result::Result<Option<u64>, Reply>> {
        let Some(label) = &self.save_version else {
            return Ok(Ok(self.version));
        };
        let reply = client.send(Command::VersionSave {
            session,
            label: Some(label.clone()).filter(|l| !l.trim().is_empty()),
            summary: None,
            author: author.clone(),
            key: key.map(str::to_owned),
        })?;
        Ok(match reply {
            Reply::Ok {
                body: ResponseBody::Version { version },
                ..
            } => Ok(Some(version.version)),
            other => Err(other),
        })
    }
}

#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct TargetArg {
    #[arg(long)]
    node: Option<NodeId>,
    #[arg(long)]
    param: Option<ParamId>,
    #[arg(long)]
    texture: Option<TexId>,
    /// A slot is named with its part: `--slot <node> <slot>`.
    #[arg(long, num_args = 2, value_names = ["NODE", "SLOT"])]
    slot: Option<Vec<String>>,
    /// The model as a whole.
    #[arg(long)]
    model: bool,
}

impl TargetArg {
    fn target(&self) -> Result<ReviewTarget> {
        if let Some(node) = &self.node {
            return Ok(ReviewTarget::Node { node: node.clone() });
        }
        if let Some(param) = &self.param {
            return Ok(ReviewTarget::Param {
                param: param.clone(),
            });
        }
        if let Some(texture) = &self.texture {
            return Ok(ReviewTarget::Texture {
                texture: texture.clone(),
            });
        }
        if let Some([node, slot]) = self.slot.as_deref() {
            return Ok(ReviewTarget::Slot {
                node: node.parse().map_err(|e| anyhow!("--slot node: {e}"))?,
                slot: slot.parse().map_err(|e| anyhow!("--slot slot: {e}"))?,
            });
        }
        Ok(ReviewTarget::Model)
    }
}

struct Client<'a> {
    stream: &'a mut transport::Transport,
    next_id: u64,
}

impl Client<'_> {
    fn send(&mut self, command: Command) -> Result<Reply> {
        let id = self.next_id;
        self.next_id += 1;
        self.stream
            .call(Request { id, command }, serde_json::Map::new())
    }

    fn send_with(&mut self, command: Command, sibling: &str, value: Value) -> Result<Reply> {
        let id = self.next_id;
        self.next_id += 1;
        let mut siblings = serde_json::Map::new();
        siblings.insert(sibling.into(), value);
        self.stream.call(Request { id, command }, siblings)
    }

    /// The images' ids, or the reply that refused one.
    fn upload(
        &mut self,
        session: SessionId,
        paths: &[PathBuf],
    ) -> Result<std::result::Result<Vec<u64>, Reply>> {
        let mut ids = Vec::new();
        for path in paths {
            let path = path
                .to_str()
                .ok_or_else(|| anyhow!("{}: not a UTF-8 path", path.display()))?;
            let files = serde_json::json!({ "image": absolute(path)? });
            match self.send_with(Command::ReviewImageAdd { session }, "files", files)? {
                Reply::Ok {
                    body: ResponseBody::ReviewImage { image },
                    ..
                } => ids.push(image.image),
                other => return Ok(Err(other)),
            }
        }
        Ok(Ok(ids))
    }

    /// Saves every image on `threads` under `dir`.
    fn download(
        &mut self,
        session: SessionId,
        dir: &Path,
        threads: &[&ReviewThread],
    ) -> Result<Saved> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let dir = dir
            .to_str()
            .ok_or_else(|| anyhow!("{}: not a UTF-8 path", dir.display()))?;
        let dir = PathBuf::from(absolute(dir)?);
        let mut saved = Saved::new();
        for thread in threads {
            for comment in &thread.comments {
                for image in &comment.images {
                    let ext = match image.media_type.as_str() {
                        "image/png" => "png",
                        "image/jpeg" => "jpg",
                        "image/webp" => "webp",
                        _ => "bin",
                    };
                    let path = dir.join(format!(
                        "thread-{}-comment-{}-{}.{ext}",
                        thread.thread.0, comment.comment, image.image
                    ));
                    let out = Value::String(path.to_string_lossy().into_owned());
                    let get = Command::ReviewImageGet {
                        session,
                        image: image.image,
                    };
                    match self.send_with(get, "out", out)? {
                        Reply::Ok { .. } => {}
                        Reply::Err { message, .. } => bail!(
                            "image {} on thread {}: {message}",
                            image.image,
                            thread.thread.0
                        ),
                        Reply::Event(_) => bail!("image {}: no reply", image.image),
                    }
                    verify(&path, image)?;
                    saved.insert(image.image, path);
                }
            }
        }
        Ok(saved)
    }
}

/// Where each downloaded image landed.
type Saved = BTreeMap<u64, PathBuf>;

/// The file holds what the comment lists: an agent must not be told to look
/// at a truncated or foreign file.
fn verify(path: &Path, image: &ReviewImage) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading back {}", path.display()))?;
    let magic: &[u8] = match image.media_type.as_str() {
        "image/png" => b"\x89PNG\r\n\x1a\n",
        "image/jpeg" => &[0xff, 0xd8, 0xff],
        "image/webp" => b"RIFF",
        _ => b"",
    };
    if bytes.len() != image.byte_length as usize || !bytes.starts_with(magic) {
        bail!(
            "{}: not the {} of {} bytes that image {} is",
            path.display(),
            image.media_type,
            image.byte_length,
            image.image
        );
    }
    Ok(())
}

fn author(who: &ReviewIdentity, human: bool) -> ReviewAuthor {
    if human {
        ReviewAuthor::Human {
            id: None,
            name: None,
        }
    } else {
        ReviewAuthor::Agent {
            id: who.id.clone(),
            name: who.name.clone(),
        }
    }
}

fn threads(ids: &[u64]) -> Vec<ThreadId> {
    ids.iter().copied().map(ThreadId).collect()
}

fn listed(ids: &[ThreadId]) -> String {
    ids.iter()
        .map(|t| t.0.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn message(given: &Option<String>, attach: &[PathBuf]) -> Result<String> {
    match given {
        Some(text) => Ok(text.clone()),
        None if !attach.is_empty() => Ok(String::new()),
        None => {
            let mut text = String::new();
            std::io::stdin()
                .lock()
                .take(MAX_COMMAND_BYTES as u64)
                .read_to_string(&mut text)?;
            Ok(text)
        }
    }
}

/// Runs the commands that need no editor; `None` for the others.
#[cfg_attr(not(unix), allow(unused_variables))]
pub(super) fn run_local(cli: &Cli, args: &ReviewArgs) -> Result<Option<i32>> {
    match &args.action {
        #[cfg(unix)]
        ReviewCmd::Worker { action } => crate::listen::worker_command(action, cli.json).map(Some),
        #[cfg(unix)]
        ReviewCmd::Unlisten => {
            let editor = match (&cli.socket, &cli.server) {
                (Some(socket), _) => {
                    Some(transport::Endpoint::Socket(std::path::absolute(socket)?))
                }
                (None, Some(url)) => Some(transport::Endpoint::Http {
                    url: url.clone(),
                    token: String::new(),
                }),
                (None, None) => None,
            };
            let who = identity(args.name.as_deref(), &|name| std::env::var(name).ok());
            crate::listen::unlisten(editor.as_ref(), resolve_session(cli)?, &who, cli.json)
                .map(Some)
        }
        #[cfg(not(unix))]
        ReviewCmd::Listen | ReviewCmd::Unlisten | ReviewCmd::Worker { .. } => {
            eprintln!(
                "`review listen` is not supported on this platform; run `review watch` in the \
                 background instead"
            );
            Ok(Some(UNSUPPORTED))
        }
        _ => Ok(None),
    }
}

#[cfg(not(unix))]
const UNSUPPORTED: i32 = 5;

/// Prints a refusal; the exit code.
#[cfg(unix)]
pub(super) fn refused(reply: &Reply) -> i32 {
    match reply {
        Reply::Err { code, message, .. } => eprintln!("[{}] {message}", code_name(*code)),
        other => eprintln!("unexpected reply: {other:?}"),
    }
    1
}

/// How this command reached the editor, for `listen`.
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) struct Reached<'a> {
    pub program: &'a str,
    pub endpoint: &'a transport::Endpoint,
}

/// The process exit code.
#[cfg_attr(not(unix), allow(unused_variables))]
pub(super) fn run(
    cli: &Cli,
    args: &ReviewArgs,
    stream: &mut transport::Transport,
    reached: &Reached,
) -> Result<i32> {
    let session = resolve_session(cli)?;
    let who = identity(args.name.as_deref(), &|name| std::env::var(name).ok());
    let mut client = Client { stream, next_id: 1 };
    let mut all = true;
    let mut save_to = None;
    let command = match &args.action {
        ReviewCmd::List {
            all: resolved,
            attachments_dir,
        } => {
            all = *resolved;
            save_to = attachments_dir.as_deref();
            Command::ReviewGet { session }
        }
        ReviewCmd::Watch {
            timeout,
            attachments_dir,
        } => {
            let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(s));
            loop {
                let reply = client.send(Command::ReviewPoll {
                    session,
                    agent: who.clone(),
                })?;
                match reply {
                    Reply::Ok {
                        body: ResponseBody::ReviewPending { pending, .. },
                        ..
                    } => {
                        // Another agent may take what was waiting between
                        // the poll and the fetch; then keep waiting.
                        if pending > 0 {
                            if let Some(code) =
                                deliver(cli, &mut client, session, &who, attachments_dir, false)?
                            {
                                return Ok(code);
                            }
                        }
                    }
                    other => return Ok(finish(cli, &other, all, None)),
                }
                if deadline.is_some_and(|at| Instant::now() >= at) {
                    eprintln!("no comments were sent before the timeout; run `review watch` again to keep waiting");
                    return Ok(TIMED_OUT);
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
        #[cfg(unix)]
        ReviewCmd::Listen => {
            let mut reach = Vec::new();
            if let Some(socket) = &cli.socket {
                reach.push("--socket".into());
                reach.push(std::path::absolute(socket)?.to_string_lossy().into_owned());
            } else if let Some(server) = &cli.server {
                reach.push("--server".into());
                reach.push(server.clone());
            }
            let how = crate::listen::Listening {
                program: reached.program,
                reach,
                endpoint: reached.endpoint,
                session,
                who: &who,
                json: cli.json,
            };
            return crate::listen::listen(&how, client.stream);
        }
        #[cfg(unix)]
        ReviewCmd::Unlisten | ReviewCmd::Worker { .. } => unreachable!("run_local answers these"),
        #[cfg(not(unix))]
        ReviewCmd::Listen | ReviewCmd::Unlisten | ReviewCmd::Worker { .. } => {
            unreachable!("run_local answers these")
        }
        ReviewCmd::Fetch { attachments_dir } => {
            return Ok(
                deliver(cli, &mut client, session, &who, attachments_dir, true)?.unwrap_or(0),
            );
        }
        ReviewCmd::Reply {
            thread,
            message: text,
            attach,
            human,
            key,
            version,
        } => {
            let body = message(text, attach)?;
            let author = author(&who, *human);
            let version =
                match version.resolve(&mut client, session, &author, key.key.as_deref())? {
                    Ok(version) => version,
                    Err(refused) => return Ok(finish(cli, &refused, all, None)),
                };
            let images = match client.upload(session, attach)? {
                Ok(images) => images,
                Err(refused) => return Ok(finish(cli, &refused, all, None)),
            };
            Command::ReviewCommentAdd {
                session,
                thread: ThreadId(*thread),
                author,
                body,
                images,
                key: key.key.clone(),
                version,
            }
        }
        ReviewCmd::Open {
            target,
            message: text,
            attach,
            human,
            key,
            version,
        } => {
            let target = target.target()?;
            let body = message(text, attach)?;
            let author = author(&who, *human);
            let version =
                match version.resolve(&mut client, session, &author, key.key.as_deref())? {
                    Ok(version) => version,
                    Err(refused) => return Ok(finish(cli, &refused, all, None)),
                };
            let images = match client.upload(session, attach)? {
                Ok(images) => images,
                Err(refused) => return Ok(finish(cli, &refused, all, None)),
            };
            Command::ReviewThreadAdd {
                session,
                target,
                author,
                body,
                pose: Vec::new(),
                images,
                key: key.key.clone(),
                version,
            }
        }
        ReviewCmd::Claim { threads: ids } => Command::ReviewClaim {
            session,
            agent: who.clone(),
            threads: threads(ids),
        },
        ReviewCmd::Release { threads: ids } => {
            let reply = client.send(Command::ReviewRelease {
                session,
                agent: who.clone(),
                threads: threads(ids),
            })?;
            if matches!(reply, Reply::Ok { .. }) && !cli.json {
                println!("released thread {}", listed(&threads(ids)));
                return Ok(0);
            }
            return Ok(finish(cli, &reply, all, None));
        }
        ReviewCmd::Send => Command::ReviewSubmit { session },
        ReviewCmd::Resolve { thread } => Command::ReviewThreadSet {
            session,
            thread: ThreadId(*thread),
            status: ThreadStatus::Resolved,
        },
        ReviewCmd::Reopen { thread } => Command::ReviewThreadSet {
            session,
            thread: ThreadId(*thread),
            status: ThreadStatus::Open,
        },
        ReviewCmd::Delete { thread } => Command::ReviewThreadDelete {
            session,
            thread: ThreadId(*thread),
        },
    };
    let reply = client.send(command)?;
    let saved = match (save_to, &reply) {
        (Some(dir), Reply::Ok { body, .. }) => {
            Some(client.download(session, dir, &shown(body, all))?)
        }
        _ => None,
    };
    Ok(finish(cli, &reply, all, saved.as_ref()))
}

/// Fetches, saves every image, prints and flushes, and only then
/// acknowledges: whatever fails before the acknowledgment leaves the comments
/// pending. `None` when nothing waited and `empty_too` is unset.
fn deliver(
    cli: &Cli,
    client: &mut Client,
    session: SessionId,
    who: &ReviewIdentity,
    save_to: &Option<PathBuf>,
    empty_too: bool,
) -> Result<Option<i32>> {
    let reply = client.send(Command::ReviewFetch {
        session,
        agent: who.clone(),
    })?;
    let (threads, batch) = match &reply {
        Reply::Ok {
            body: ResponseBody::ReviewFetched { threads, batch },
            ..
        } => (threads, batch.clone()),
        other => return Ok(Some(finish(cli, other, true, None))),
    };
    let Some(batch) = batch else {
        return Ok(empty_too.then(|| finish(cli, &reply, true, None)));
    };
    const PENDING: &str = "the comments stay pending: fix this and fetch again";
    let saved = save_to
        .as_deref()
        .map(|dir| client.download(session, dir, &threads.iter().collect::<Vec<_>>()))
        .transpose()
        .context(PENDING)?;
    finish(cli, &reply, true, saved.as_ref());
    std::io::stdout().flush().context(PENDING)?;

    let acked = client
        .send(Command::ReviewAck {
            session,
            agent: who.clone(),
            batch,
        })
        .context("acknowledging what was printed; the comments stay pending: fetch again")?;
    if cli.json {
        print_reply(cli, &acked);
    }
    Ok(Some(match &acked {
        Reply::Ok {
            body: body @ ResponseBody::ReviewAcked { .. },
            ..
        } => {
            if !cli.json {
                print!("{}", describe(body, true, None).unwrap_or_default());
            }
            0
        }
        Reply::Err {
            code: ErrorCode::BatchStale,
            ..
        } => {
            eprintln!(
                "these comments changed or another agent took them before they were acknowledged; \
                 disregard them and run `review fetch` again"
            );
            SUPERSEDED
        }
        Reply::Err { code, message, .. } => {
            eprintln!(
                "acknowledging failed [{}]: {message}; the comments stay pending: fetch again",
                code_name(*code)
            );
            1
        }
        Reply::Ok { .. } | Reply::Event(_) => {
            bail!("acknowledging: an unexpected reply; the comments stay pending: fetch again")
        }
    }))
}

/// The threads [`describe`] prints for `body`.
fn shown(body: &ResponseBody, all: bool) -> Vec<&ReviewThread> {
    match body {
        ResponseBody::Review { review } => review
            .threads
            .iter()
            .filter(|t| all || t.status == ThreadStatus::Open)
            .collect(),
        ResponseBody::ReviewThread { thread } => vec![thread],
        ResponseBody::ReviewFetched { threads, .. } => threads.iter().collect(),
        _ => Vec::new(),
    }
}

fn finish(cli: &Cli, reply: &Reply, all: bool, saved: Option<&Saved>) -> i32 {
    let failed = matches!(reply, Reply::Err { .. });
    match reply {
        Reply::Ok { body, .. } if !cli.json => match describe(body, all, saved) {
            Some(text) => print!("{text}"),
            None => print_body(body),
        },
        _ => print_reply(cli, reply),
    }
    i32::from(failed)
}

/// `None` for a body that is not a review's. `saved` is `None` when no
/// `--attachments-dir` was given.
pub(super) fn describe(body: &ResponseBody, all: bool, saved: Option<&Saved>) -> Option<String> {
    let mut out = String::new();
    match body {
        ResponseBody::Review { review } => {
            let shown = shown(body, all);
            for agent in &review.agents {
                let state = if agent.listening { "is" } else { "is not" };
                let _ = writeln!(out, "agent {} ({}) {state} listening", agent.name, agent.id);
            }
            if review.kept == Kept::Unreadable {
                let _ = writeln!(
                    out,
                    "the review kept with this model could not be read ({}); it is left as it is, and comments made now last until the session closes",
                    review.kept_problem.as_deref().unwrap_or("no reason given")
                );
            }
            if shown.is_empty() {
                let hidden = review.threads.len();
                let _ = if hidden > 0 {
                    writeln!(out, "(no open threads; {hidden} resolved, shown by --all)")
                } else {
                    writeln!(out, "(no threads)")
                };
            }
            for thread in shown {
                describe_thread(&mut out, thread, false, saved);
            }
        }
        ResponseBody::ReviewThread { thread } => describe_thread(&mut out, thread, false, saved),
        ResponseBody::ReviewFetched { threads, .. } => {
            if threads.is_empty() {
                let _ = writeln!(out, "(nothing sent to you)");
            }
            for thread in threads {
                describe_thread(&mut out, thread, true, saved);
            }
            if !threads.is_empty() {
                let _ = writeln!(
                    out,
                    "Make the changes, then answer each thread with\n\
                     `review reply <thread> -m \"<what changed, or your question>\"`; the reply ends\n\
                     your claim on it. Resolving is the person's call. Unless you listen\n\
                     (`review listen`), run `review watch` again to wait for more."
                );
                if threads
                    .iter()
                    .flat_map(|t| &t.comments)
                    .any(|c| !c.images.is_empty())
                {
                    let _ = match saved {
                        Some(_) => writeln!(
                            out,
                            "Open each image file listed above before answering: the comment is about\n\
                             what it shows."
                        ),
                        None => writeln!(
                            out,
                            "Some comments carry images. Save them with `review list --attachments-dir\n\
                             <dir>` and open each file before answering."
                        ),
                    };
                }
            }
        }
        ResponseBody::ReviewImage { image } => {
            let _ = writeln!(out, "image {}", describe_image(image));
        }
        ResponseBody::ReviewAcked { claimed, held } => {
            if !claimed.is_empty() {
                let _ = writeln!(out, "delivered; claimed thread {}", listed(claimed));
            }
            for held in held {
                let _ = writeln!(
                    out,
                    "thread {} is claimed by {} ({}): its comments were left for them; leave it to them",
                    held.thread.0, held.holder.name, held.holder.id
                );
            }
        }
        ResponseBody::ReviewClaimed {
            threads,
            lease_seconds,
        } => {
            let _ = writeln!(
                out,
                "claimed thread {} for {} minutes; claim again to renew",
                listed(threads),
                lease_seconds / 60
            );
        }
        ResponseBody::ReviewPending { pending, .. } => {
            let _ = writeln!(
                out,
                "{pending} thread(s) waiting; `review fetch` takes them"
            );
        }
        _ => return None,
    }
    Some(out)
}

fn describe_target(target: &ReviewTarget) -> String {
    match target {
        ReviewTarget::Model => "the model".into(),
        ReviewTarget::Node { node } => format!("node {node}"),
        ReviewTarget::Param { param } => format!("param {param}"),
        ReviewTarget::Texture { texture } => format!("texture {texture}"),
        ReviewTarget::Slot { node, slot } => format!("slot {slot} on node {node}"),
    }
}

fn describe_image(image: &ReviewImage) -> String {
    let size = match image.byte_length {
        bytes if bytes < 1024 => format!("{bytes} B"),
        bytes => format!("{} KiB", bytes.div_ceil(1024)),
    };
    format!("{} ({}, {size})", image.image, image.media_type)
}

/// `fetched`: the comments this fetch delivered still read `sent`.
fn describe_thread(out: &mut String, thread: &ReviewThread, fetched: bool, saved: Option<&Saved>) {
    let status = match thread.status {
        ThreadStatus::Open => "open",
        ThreadStatus::Resolved => "resolved",
    };
    let _ = write!(
        out,
        "thread {} on {} [{status}]",
        thread.thread.0,
        describe_target(&thread.target)
    );
    if let Some(holder) = thread.holder.as_ref().filter(|_| !fetched) {
        let _ = write!(out, " ({} is working on it)", holder.name);
    }
    let _ = writeln!(out);
    if thread.target_missing {
        let _ = writeln!(out, "  target missing: the model no longer holds it");
    }
    if !thread.pose.is_empty() {
        let pose: Vec<String> = thread
            .pose
            .iter()
            .map(|p| format!("{}={}", p.param, p.value))
            .collect();
        let _ = writeln!(out, "  opened at pose: {}", pose.join(" "));
    }
    for comment in &thread.comments {
        let who = match &comment.author {
            ReviewAuthor::Human { name: None, .. } => "[human]".to_owned(),
            ReviewAuthor::Human {
                name: Some(name), ..
            } => format!("[human: {name}]"),
            ReviewAuthor::Agent { name, .. } => format!("[agent: {name}]"),
        };
        let mark = match (comment.delivery, fetched) {
            (ReviewDelivery::Sent, true) => " (new)",
            (ReviewDelivery::Sent, false) => " (sent, not yet taken)",
            (ReviewDelivery::Unsent, _) => " (not sent yet)",
            (ReviewDelivery::Delivered, _) => "",
        };
        let _ = match comment.version {
            Some(version) => writeln!(
                out,
                "  {who}{mark} at rev {}, pointing at version {version}:",
                comment.rev
            ),
            None => writeln!(out, "  {who}{mark} at rev {}:", comment.rev),
        };
        for line in comment.body.lines() {
            let _ = writeln!(out, "    {line}");
        }
        for image in &comment.images {
            let _ = match saved.and_then(|s| s.get(&image.image)) {
                Some(path) => writeln!(out, "    image: {}", path.display()),
                None => writeln!(
                    out,
                    "    image {}: fetch with --attachments-dir <dir>",
                    describe_image(image)
                ),
            };
        }
    }
    let _ = writeln!(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUMAN: ReviewAuthor = ReviewAuthor::Human {
        id: None,
        name: None,
    };

    fn claude() -> ReviewIdentity {
        ReviewIdentity {
            id: "claude:s1".into(),
            name: "claude".into(),
        }
    }

    fn comment(
        id: u64,
        author: ReviewAuthor,
        body: &str,
        delivery: ReviewDelivery,
    ) -> ReviewComment {
        ReviewComment {
            comment: id,
            author,
            body: body.into(),
            delivery,
            rev: 4,
            images: Vec::new(),
            version: None,
        }
    }

    fn thread() -> ReviewThread {
        ReviewThread {
            thread: ThreadId(3),
            target: ReviewTarget::Node {
                node: "head/part-1".parse().unwrap(),
            },
            target_missing: true,
            status: ThreadStatus::Open,
            pose: vec![ParamPose {
                param: "head_yaw".parse().unwrap(),
                value: 0.5,
            }],
            comments: vec![
                comment(1, HUMAN, "old", ReviewDelivery::Delivered),
                comment(
                    2,
                    ReviewAuthor::Agent {
                        id: "claude:s1".into(),
                        name: "claude".into(),
                    },
                    "done",
                    ReviewDelivery::Delivered,
                ),
                comment(3, HUMAN, "still torn\nat the ear", ReviewDelivery::Sent),
            ],
            holder: Some(claude()),
        }
    }

    #[test]
    fn a_fetch_marks_what_is_new_and_says_how_to_answer() {
        let text = describe(
            &ResponseBody::ReviewFetched {
                threads: vec![thread()],
                batch: Some("3".into()),
            },
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            text,
            "thread 3 on node head/part-1 [open]\n\
             \x20 target missing: the model no longer holds it\n\
             \x20 opened at pose: head_yaw=0.5\n\
             \x20 [human] at rev 4:\n\
             \x20   old\n\
             \x20 [agent: claude] at rev 4:\n\
             \x20   done\n\
             \x20 [human] (new) at rev 4:\n\
             \x20   still torn\n\
             \x20   at the ear\n\
             \n\
             Make the changes, then answer each thread with\n\
             `review reply <thread> -m \"<what changed, or your question>\"`; the reply ends\n\
             your claim on it. Resolving is the person's call. Unless you listen\n\
             (`review listen`), run `review watch` again to wait for more.\n"
        );
    }

    #[test]
    fn a_comment_names_the_version_it_points_at() {
        let mut thread = thread();
        thread.comments.truncate(1);
        thread.comments[0].version = Some(3);
        let text = describe(&ResponseBody::ReviewThread { thread }, true, None).unwrap();
        assert!(
            text.contains("at rev 4, pointing at version 3:\n"),
            "{text}"
        );
    }

    #[test]
    fn a_list_hides_resolved_threads_unless_asked() {
        let mut resolved = thread();
        resolved.status = ThreadStatus::Resolved;
        let body = ResponseBody::Review {
            review: Review {
                threads: vec![resolved],
                agents: vec![
                    ReviewAgent {
                        id: "claude:s1".into(),
                        name: "claude".into(),
                        listening: true,
                    },
                    ReviewAgent {
                        id: "codex:t1".into(),
                        name: "codex".into(),
                        listening: false,
                    },
                ],
                kept: Kept::Stored,
                kept_problem: None,
            },
        };
        let open = describe(&body, false, None).unwrap();
        assert!(
            open.contains("agent claude (claude:s1) is listening\n"),
            "{open}"
        );
        assert!(
            open.contains("agent codex (codex:t1) is not listening\n"),
            "{open}"
        );
        assert!(
            open.contains("(no open threads; 1 resolved, shown by --all)"),
            "{open}"
        );
        let all = describe(&body, true, None).unwrap();
        assert!(
            all.contains("thread 3 on node head/part-1 [resolved]"),
            "{all}"
        );
        assert!(all.contains("(sent, not yet taken)"), "{all}");
        assert!(
            all.contains("[resolved] (claude is working on it)"),
            "{all}"
        );
    }

    #[test]
    fn an_image_prints_its_saved_path_or_how_to_get_it() {
        let mut thread = thread();
        thread.comments.truncate(1);
        thread.comments[0].body.clear();
        thread.comments[0].images = vec![
            ReviewImage {
                image: 7,
                media_type: "image/png".into(),
                byte_length: 2049,
            },
            ReviewImage {
                image: 8,
                media_type: "image/webp".into(),
                byte_length: 900,
            },
        ];
        let body = ResponseBody::ReviewFetched {
            threads: vec![thread],
            batch: Some("1".into()),
        };
        let bare = describe(&body, false, None).unwrap();
        assert!(
            bare.contains(
                "\x20 [human] at rev 4:\n\
                 \x20   image 7 (image/png, 3 KiB): fetch with --attachments-dir <dir>\n\
                 \x20   image 8 (image/webp, 900 B): fetch with --attachments-dir <dir>\n"
            ),
            "{bare}"
        );
        let saved = Saved::from([(7, PathBuf::from("/shots/thread-3-comment-1-7.png"))]);
        let fetched = describe(&body, false, Some(&saved)).unwrap();
        assert!(
            fetched.contains("\x20   image: /shots/thread-3-comment-1-7.png\n"),
            "{fetched}"
        );
        assert!(fetched.contains("Open each image file"), "{fetched}");
        assert!(bare.contains("review list --attachments-dir"), "{bare}");
    }

    #[test]
    fn an_attachment_stands_in_for_the_message() {
        let parse = |args: &[&str]| {
            let mut full = vec!["cli", "--session", "1", "review"];
            full.extend_from_slice(args);
            Cli::try_parse_from(full).map(|cli| cli.cmd)
        };
        let Ok(Cmd::Review(ReviewArgs {
            action:
                ReviewCmd::Reply {
                    message: text,
                    attach,
                    ..
                },
            ..
        })) = parse(&["reply", "2", "--attach", "a.png", "--attach", "b.jpg"])
        else {
            panic!("--attach repeats");
        };
        assert_eq!(attach.len(), 2);
        assert_eq!(message(&text, &attach).unwrap(), "");
        assert!(matches!(
            parse(&["watch", "--attachments-dir", "shots"]),
            Ok(Cmd::Review(ReviewArgs {
                action: ReviewCmd::Watch {
                    attachments_dir: Some(_),
                    ..
                },
                ..
            }))
        ));
        assert!(parse(&["fetch", "--attachments-dir", "shots"]).is_ok());
        assert!(parse(&["list", "--all", "--attachments-dir", "shots"]).is_ok());
    }

    #[test]
    fn a_thread_names_exactly_one_target() {
        let parse = |args: &[&str]| {
            let mut full = vec!["cli", "--session", "1", "review", "open", "-m", "x"];
            full.extend_from_slice(args);
            Cli::try_parse_from(full)
        };
        let Ok(cli) = parse(&["--slot", "arm", "wrist"]) else {
            panic!("a slot is a node and a slot");
        };
        let Cmd::Review(ReviewArgs {
            action: ReviewCmd::Open { target, .. },
            ..
        }) = &cli.cmd
        else {
            panic!("parsed as something else");
        };
        assert_eq!(
            target.target().unwrap(),
            ReviewTarget::Slot {
                node: "arm".parse().unwrap(),
                slot: "wrist".parse().unwrap(),
            }
        );
        assert!(parse(&[]).is_err());
        assert!(parse(&["--model", "--node", "arm"]).is_err());
        assert!(parse(&["--slot", "arm"]).is_err());
    }

    #[test]
    fn an_agent_is_its_harness_session_and_shown_by_name() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let id = |name: Option<&str>, pairs| {
            let who = identity(name, &env(pairs));
            (who.id, who.name)
        };
        assert_eq!(id(None, &[]), ("agent".into(), "agent".into()));
        assert_eq!(
            id(
                None,
                &[("CLAUDE_CODE_SESSION_ID", "s1"), ("CODEX_THREAD_ID", "t1")]
            ),
            ("codex:t1".into(), "codex".into()),
            "without its messaging socket Claude cannot be notified; Codex can"
        );
        assert_eq!(
            id(
                None,
                &[
                    ("CLAUDE_CODE_SESSION_ID", "s1"),
                    ("CLAUDE_CODE_MESSAGING_SOCKET", "/tmp/cc-socks/1.sock"),
                    ("CLAUDE_CODE_MESSAGING_TOKEN", "k"),
                    ("CODEX_THREAD_ID", "t1")
                ]
            ),
            ("claude:s1".into(), "claude".into())
        );
        assert_eq!(
            id(None, &[("CLAUDE_CODE_SESSION_ID", "s1")]),
            ("claude:s1".into(), "claude".into())
        );
        assert_eq!(
            id(Some("Ada"), &[("CODEX_SESSION_ID", " t2 ")]),
            ("codex:t2".into(), "Ada".into())
        );
        assert_eq!(
            id(
                None,
                &[
                    ("ANTIGRAVITY_CONVERSATION_ID", "c1"),
                    ("CODEX_THREAD_ID", "t1")
                ]
            ),
            ("codex:t1".into(), "codex".into()),
            "an Antigravity id without its connection cannot be notified"
        );
        assert_eq!(
            id(None, &[("ANTIGRAVITY_CONVERSATION_ID", "c1")]),
            ("antigravity:c1".into(), "antigravity".into())
        );
        assert_eq!(
            id(
                None,
                &[
                    ("CATCHLIGHT_AGENT_SESSION", "b"),
                    ("CLAUDE_CODE_SESSION_ID", "s1")
                ]
            ),
            ("b".into(), "claude".into()),
            "the override names who, not what it is called"
        );
    }

    #[test]
    fn a_claim_and_a_key_parse() {
        let parse = |args: &[&str]| {
            let mut full = vec!["cli", "--session", "1", "review"];
            full.extend_from_slice(args);
            Cli::try_parse_from(full).map(|cli| cli.cmd)
        };
        assert!(matches!(
            parse(&["claim", "1", "2"]),
            Ok(Cmd::Review(ReviewArgs { action: ReviewCmd::Claim { threads }, .. })) if threads == [1, 2]
        ));
        assert!(parse(&["claim"]).is_err());
        assert!(parse(&["release"]).is_err());
        assert!(matches!(
            parse(&["reply", "1", "-m", "x", "--key", "k", "--name", "Ada"]),
            Ok(Cmd::Review(ReviewArgs {
                name: Some(name),
                action: ReviewCmd::Reply { key: KeyArg { key: Some(key) }, .. },
            })) if name == "Ada" && key == "k"
        ));
        assert!(parse(&["open", "--model", "-m", "x", "--key", "k"]).is_ok());
        assert!(
            parse(&["watch", "--agent", "x"]).is_err(),
            "--agent is gone"
        );
    }

    #[test]
    fn an_ack_and_a_claim_say_what_is_held() {
        let acked = describe(
            &ResponseBody::ReviewAcked {
                claimed: vec![ThreadId(1)],
                held: vec![ReviewHeld {
                    thread: ThreadId(2),
                    holder: claude(),
                }],
            },
            true,
            None,
        )
        .unwrap();
        assert_eq!(
            acked,
            "delivered; claimed thread 1\n\
             thread 2 is claimed by claude (claude:s1): its comments were left for them; leave it to them\n"
        );
        let claimed = describe(
            &ResponseBody::ReviewClaimed {
                threads: vec![ThreadId(1), ThreadId(4)],
                lease_seconds: 3600,
            },
            true,
            None,
        )
        .unwrap();
        assert_eq!(
            claimed,
            "claimed thread 1 4 for 60 minutes; claim again to renew\n"
        );
    }

    #[test]
    fn a_saved_image_must_be_what_the_comment_lists() {
        let dir = std::env::temp_dir().join(format!("cl-review-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        let png = b"\x89PNG\r\n\x1a\nrest";
        std::fs::write(&path, png).unwrap();
        let image = |media_type: &str, byte_length| ReviewImage {
            image: 1,
            media_type: media_type.into(),
            byte_length,
        };
        assert!(verify(&path, &image("image/png", png.len() as u32)).is_ok());
        assert!(verify(&path, &image("image/png", 3)).is_err());
        assert!(verify(&path, &image("image/jpeg", png.len() as u32)).is_err());
        assert!(verify(&dir.join("missing.png"), &image("image/png", 1)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
