//! `review`: the thread a person and an agent keep about a session's model.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde_json::Value;

use super::*;

/// `review watch` ran out its `--timeout` with nothing sent. Apart from 1
/// (a refused command) and 2 (clap's usage error).
pub(super) const TIMED_OUT: i32 = 3;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(clap::Args)]
pub(super) struct ReviewArgs {
    /// The name comments and work are recorded under.
    #[arg(long, global = true, env = "CATCHLIGHT_AGENT", default_value = "agent")]
    agent: String,
    #[command(subcommand)]
    action: ReviewCmd,
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
    /// Wait until the person sends comments, then take and print them.
    /// Exits 3 if `--timeout` passes first.
    Watch {
        /// Give up after this many seconds; without it, wait indefinitely.
        #[arg(long)]
        timeout: Option<u64>,
        /// Save the threads' images here and print each one's path.
        #[arg(long, value_name = "DIR")]
        attachments_dir: Option<PathBuf>,
    },
    /// Take and print the comments sent so far, without waiting.
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
                    saved.insert(image.image, path);
                }
            }
        }
        Ok(saved)
    }
}

/// Where each downloaded image landed.
type Saved = BTreeMap<u64, PathBuf>;

fn author(args: &ReviewArgs, human: bool) -> ReviewAuthor {
    if human {
        ReviewAuthor::Human
    } else {
        ReviewAuthor::Agent {
            name: args.agent.clone(),
        }
    }
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

/// The process exit code.
pub(super) fn run(cli: &Cli, args: &ReviewArgs, stream: &mut transport::Transport) -> Result<i32> {
    let session = resolve_session(cli)?;
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
            save_to = attachments_dir.as_deref();
            let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(s));
            loop {
                let reply = client.send(Command::ReviewPoll {
                    session,
                    agent: args.agent.clone(),
                })?;
                match reply {
                    Reply::Ok {
                        body: ResponseBody::ReviewPending { pending },
                        ..
                    } => {
                        if pending > 0 {
                            break;
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
            Command::ReviewFetch {
                session,
                agent: args.agent.clone(),
            }
        }
        ReviewCmd::Fetch { attachments_dir } => {
            save_to = attachments_dir.as_deref();
            Command::ReviewFetch {
                session,
                agent: args.agent.clone(),
            }
        }
        ReviewCmd::Reply {
            thread,
            message: text,
            attach,
            human,
        } => {
            let body = message(text, attach)?;
            let images = match client.upload(session, attach)? {
                Ok(images) => images,
                Err(refused) => return Ok(finish(cli, &refused, all, None)),
            };
            Command::ReviewCommentAdd {
                session,
                thread: ThreadId(*thread),
                author: author(args, *human),
                body,
                images,
            }
        }
        ReviewCmd::Open {
            target,
            message: text,
            attach,
            human,
        } => {
            let target = target.target()?;
            let body = message(text, attach)?;
            let images = match client.upload(session, attach)? {
                Ok(images) => images,
                Err(refused) => return Ok(finish(cli, &refused, all, None)),
            };
            Command::ReviewThreadAdd {
                session,
                target,
                author: author(args, *human),
                body,
                pose: Vec::new(),
                images,
            }
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

/// The threads [`describe`] prints for `body`.
fn shown(body: &ResponseBody, all: bool) -> Vec<&ReviewThread> {
    match body {
        ResponseBody::Review { review } => review
            .threads
            .iter()
            .filter(|t| all || t.status == ThreadStatus::Open)
            .collect(),
        ResponseBody::ReviewThread { thread } => vec![thread],
        ResponseBody::ReviewFetched { threads } => threads.iter().collect(),
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
            match &review.agent {
                Some(agent) if agent.listening => {
                    let _ = writeln!(out, "agent {} is listening", agent.name);
                }
                Some(agent) => {
                    let _ = writeln!(out, "agent {} is not listening", agent.name);
                }
                None => {}
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
        ResponseBody::ReviewFetched { threads } => {
            if threads.is_empty() {
                let _ = writeln!(out, "(nothing sent to you)");
            }
            for thread in threads {
                describe_thread(&mut out, thread, true, saved);
            }
            if !threads.is_empty() {
                let _ = writeln!(
                    out,
                    "These threads are now yours. Make the changes, then answer each one with\n\
                     `review reply <thread> -m \"<what changed, or your question>\"`. Resolving is\n\
                     the person's call. Run `review watch` again to wait for more."
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
        ResponseBody::ReviewPending { pending } => {
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
    if let Some(agent) = thread.working.as_ref().filter(|_| !fetched) {
        let _ = write!(out, " ({agent} is working on it)");
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
            ReviewAuthor::Human => "[human]".to_owned(),
            ReviewAuthor::Agent { name } => format!("[agent: {name}]"),
        };
        let mark = match (comment.delivery, fetched) {
            (ReviewDelivery::Sent, true) => " (new)",
            (ReviewDelivery::Sent, false) => " (sent, not yet taken)",
            (ReviewDelivery::Unsent, _) => " (not sent yet)",
            (ReviewDelivery::Delivered, _) => "",
        };
        let _ = writeln!(out, "  {who}{mark} at rev {}:", comment.rev);
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
                comment(1, ReviewAuthor::Human, "old", ReviewDelivery::Delivered),
                comment(
                    2,
                    ReviewAuthor::Agent {
                        name: "claude".into(),
                    },
                    "done",
                    ReviewDelivery::Delivered,
                ),
                comment(
                    3,
                    ReviewAuthor::Human,
                    "still torn\nat the ear",
                    ReviewDelivery::Sent,
                ),
            ],
            working: Some("claude".into()),
        }
    }

    #[test]
    fn a_fetch_marks_what_is_new_and_says_how_to_answer() {
        let text = describe(
            &ResponseBody::ReviewFetched {
                threads: vec![thread()],
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
             These threads are now yours. Make the changes, then answer each one with\n\
             `review reply <thread> -m \"<what changed, or your question>\"`. Resolving is\n\
             the person's call. Run `review watch` again to wait for more.\n"
        );
    }

    #[test]
    fn a_list_hides_resolved_threads_unless_asked() {
        let mut resolved = thread();
        resolved.status = ThreadStatus::Resolved;
        let body = ResponseBody::Review {
            review: Review {
                threads: vec![resolved],
                agent: Some(ReviewAgent {
                    name: "claude".into(),
                    listening: true,
                }),
            },
        };
        let open = describe(&body, false, None).unwrap();
        assert!(open.contains("agent claude is listening"), "{open}");
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
            agent,
            action: ReviewCmd::Open { target, .. },
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
        assert!(!agent.is_empty());
        assert!(parse(&[]).is_err());
        assert!(parse(&["--model", "--node", "arm"]).is_err());
        assert!(parse(&["--slot", "arm"]).is_err());
    }
}
