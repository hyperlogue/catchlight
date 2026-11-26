//! `review listen`: the comments a person sends are announced in the agent's
//! harness session by a detached worker, so no command blocks the agent's
//! turn. Every agent listening on a session is notified; claims decide who
//! works a thread.
//!
//! The mechanics are r3's (`r3 listen`); see the submodules for which part
//! of it each follows.

mod harness;
mod process;
mod store;
mod worker;

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use catchlight_editor_protocol::*;
use serde_json::json;

use crate::transport::{Endpoint, Transport};
use store::{Registration, Store};

/// `listen` cannot work here: no adapter for this harness, or none on this
/// platform. `review watch` still does.
pub(crate) const UNSUPPORTED: i32 = 5;

/// The agent identity that goes with the harness `listen` would notify.
pub(crate) fn detected_session(env: harness::Env) -> Option<String> {
    harness::detect(env).ok()?.session
}

fn store() -> Result<Store> {
    let state = store::state_dir(&|name| std::env::var_os(name)).ok_or_else(|| {
        anyhow!("no per-user state directory: set CATCHLIGHT_STATE_DIR to an absolute path")
    })?;
    Store::open(&state)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn call(stream: &mut Transport, command: Command) -> Result<Reply> {
    stream.call(Request { id: 1, command }, serde_json::Map::new())
}

/// Quoted for a POSIX shell when it needs to be.
fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The command an agent runs on a notice: the same editor, session and
/// images directory every time.
pub(crate) fn fetch_command(
    program: &str,
    reach: &[String],
    session: SessionId,
    images: &std::path::Path,
) -> String {
    let mut words = vec![program.to_owned()];
    words.extend(reach.iter().map(|w| shell_word(w)));
    words.extend([
        "--session".into(),
        session.0.to_string(),
        "review".into(),
        "fetch".into(),
        "--attachments-dir".into(),
        shell_word(&images.to_string_lossy()),
    ]);
    words.join(" ")
}

pub(crate) struct Listening<'a> {
    pub program: &'a str,
    /// The flags that name this editor: `--socket` or `--server`.
    pub reach: Vec<String>,
    pub endpoint: &'a Endpoint,
    pub session: SessionId,
    pub who: &'a ReviewIdentity,
    pub json: bool,
}

pub(crate) fn listen(how: &Listening, stream: &mut Transport) -> Result<i32> {
    let env = |name: &str| env(name);
    let target = match harness::detect(&env)
        .map_err(|why| why.to_string())
        .and_then(|found| harness::prepare(found.target, &env))
    {
        Ok(target) => target,
        Err(why) => {
            eprintln!("cannot listen: {why}; run `review watch` in the background instead");
            return Ok(UNSUPPORTED);
        }
    };
    let store = store()?;
    let mut processes = process::Processes::new(Some(store.uid));
    let owner = match processes.capture(
        target.harness(),
        target.executable(),
        env("CATCHLIGHT_AGENT_PID").as_deref(),
    ) {
        Ok(owner) => owner,
        Err(why) => {
            eprintln!("cannot listen: {why}");
            return Ok(UNSUPPORTED);
        }
    };

    let sessions = match call(stream, Command::SessionList)? {
        Reply::Ok {
            body: ResponseBody::Sessions { sessions, .. },
            ..
        } => sessions,
        other => return Ok(crate::review::refused(&other)),
    };
    let Some(info) = sessions.into_iter().find(|s| s.session == how.session) else {
        eprintln!("no session {} is open in this editor", how.session.0);
        return Ok(1);
    };
    // Also this agent's first heartbeat: the tab shows it listening at once.
    let (pending, batch) = match call(
        stream,
        Command::ReviewPoll {
            session: how.session,
            agent: how.who.clone(),
        },
    )? {
        Reply::Ok {
            body: ResponseBody::ReviewPending { pending, batch },
            ..
        } => (pending, batch),
        other => return Ok(crate::review::refused(&other)),
    };

    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute());
    let images = store.images(how.endpoint, how.session.0, &how.who.id, runtime.as_deref())?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|e| anyhow!("no system randomness: {e}"))?;
    let reg = Registration {
        nonce: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        editor: how.endpoint.clone(),
        session: how.session.0,
        title: info.title.clone(),
        file: info.file.clone(),
        agent: how.who.clone(),
        target,
        owner,
        fetch: fetch_command(how.program, &how.reach, how.session, &images),
        // What waits now is this command's to report, not the worker's.
        notified: batch,
        failure: None,
        registered: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default(),
    };
    store.sweep(&mut |owner| processes.liveness(owner))?;
    {
        let _lock = store.lock()?;
        store.write(&reg)?;
    }
    if worker::ensure(&store)?
        && let Some(warning) = sandbox_warning(&env)
    {
        eprintln!("{warning}");
    }

    if how.json {
        println!(
            "{}",
            json!({
                "listening": true,
                "session": how.session.0,
                "model": info.title,
                "harness": reg.target.harness(),
                "agent": how.who,
                "tracked": reg.owner.is_some(),
                "pending": pending,
                "fetch": reg.fetch,
            })
        );
        return Ok(0);
    }
    println!(
        "listening on session {} (\"{}\"): when the person sends comments, a notice arrives in \
         this {} session; until then there is nothing to wait for",
        how.session.0,
        info.title,
        reg.target.harness()
    );
    if reg.owner.is_none() {
        println!(
            "the {} process was not found among this command's ancestors, so the registration \
             outlives it: run `review unlisten` when done",
            reg.target.harness()
        );
    }
    if pending > 0 {
        println!(
            "{pending} thread(s) already wait for you: run `{}` now",
            reg.fetch
        );
    }
    Ok(0)
}

/// Needs no running editor: an agent may let go after its editor exited.
/// `editor` narrows it to the one `--socket` or `--server` names.
pub(crate) fn unlisten(
    editor: Option<&Endpoint>,
    session: SessionId,
    who: &ReviewIdentity,
    json: bool,
) -> Result<i32> {
    let store = store()?;
    let same = |reg: &Registration| match (editor, &reg.editor) {
        (None, _) => true,
        (Some(Endpoint::Http { url, .. }), Endpoint::Http { url: theirs, .. }) => {
            url.trim_end_matches('/') == theirs.trim_end_matches('/')
        }
        (Some(mine), theirs) => mine == theirs,
    };
    let was = {
        let _lock = store.lock()?;
        let mut was = false;
        for reg in store.list()? {
            if reg.session == session.0 && reg.agent.id == who.id && same(&reg) {
                store.remove(&reg.name())?;
                was = true;
            }
        }
        was
    };
    if json {
        println!("{}", json!({ "listening": false, "was": was }));
    } else if was {
        println!("stopped listening on session {}", session.0);
    } else {
        println!("you were not listening on session {}", session.0);
    }
    Ok(0)
}

#[derive(clap::Subcommand)]
pub(crate) enum WorkerCmd {
    /// Show whether the worker runs and how many polls it has made, every
    /// registration, and the last delivery failure of each.
    Status,
    /// Start the worker for the registrations kept on disk.
    Start,
    /// Stop the worker. Registrations are kept: `start` or the next `listen`
    /// resumes them; `review unlisten` ends one.
    Stop,
    #[command(hide = true)]
    Run,
}

pub(crate) fn worker_command(action: &WorkerCmd, json: bool) -> Result<i32> {
    let store = store()?;
    match action {
        WorkerCmd::Run => {
            worker::run(&store)?;
            Ok(0)
        }
        WorkerCmd::Start => {
            if store.list()?.is_empty() {
                println!("no agent is listening; nothing to start");
            } else if worker::ensure(&store)? {
                println!("review worker started");
            } else {
                println!("review worker is already running");
            }
            Ok(0)
        }
        WorkerCmd::Stop => {
            if worker::stop(&store)? {
                println!("review worker stopped; registrations are kept until `review unlisten`");
            } else {
                println!("review worker is not running");
            }
            Ok(0)
        }
        WorkerCmd::Status => {
            let mut processes = process::Processes::new(Some(store.uid));
            let dropped = store.sweep(&mut |owner| processes.liveness(owner))?;
            let pid = worker::running(&store)?;
            let polls = pid.and_then(|pid| worker::polls(&store, pid));
            let regs = {
                let _lock = store.lock()?;
                store.list()?
            };
            if json {
                let agents: Vec<_> = regs
                    .iter()
                    .map(|r| {
                        json!({
                            "agent": r.agent,
                            "harness": r.target.harness(),
                            "editor": r.editor.describe(),
                            "session": r.session,
                            "model": r.title,
                            "owner": r.owner.as_ref().map(|o| o.pid),
                            "notified": r.notified,
                            "failure": r.failure,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    json!({
                        "worker": pid,
                        "polls": polls,
                        "log": store.log(),
                        "agents": agents,
                        "dropped": dropped.len(),
                    })
                );
                return Ok(0);
            }
            match (pid, polls) {
                (Some(pid), Some(polls)) => {
                    println!("review worker: running (pid {pid}, {polls} polls)");
                }
                (Some(pid), None) => println!("review worker: running (pid {pid})"),
                (None, _) => println!("review worker: not running"),
            }
            println!("log: {}", store.log().display());
            if regs.is_empty() {
                println!("no agent is listening");
            }
            for r in &regs {
                let owner = match &r.owner {
                    Some(o) => format!("harness pid {}", o.pid),
                    None => "harness process untracked".into(),
                };
                println!(
                    "{} ({}) via {} on session {} (\"{}\") at {}; {owner}",
                    r.agent.name,
                    r.agent.id,
                    r.target.harness(),
                    r.session,
                    r.title,
                    r.editor.describe()
                );
                if let Some(failure) = &r.failure {
                    println!(
                        "  the last notice failed: {failure}; it is no longer polled, run \
                         `review listen` again from that session"
                    );
                }
            }
            Ok(0)
        }
    }
}

/// Environment markers are advisory: a sandbox may set none, and a marker
/// may outlive an approved escape.
fn sandbox_warning(env: harness::Env) -> Option<&'static str> {
    let profile = env("CODEX_PERMISSION_PROFILE");
    let sandboxed = env("CODEX_SANDBOX").is_some_and(|v| !v.trim().is_empty())
        || env("CODEX_SANDBOX_NETWORK_DISABLED").as_deref() == Some("1")
        || matches!(profile.as_deref(), Some("read-only" | "workspace-write"));
    sandboxed.then_some(
        "warning: this command runs in a sandbox, and the review worker it started inherits it: \
         notices may fail even though `listen` succeeded. If they do, run `review worker stop` \
         and then `review worker start` from a terminal outside the sandbox.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fetch_command_reaches_the_same_editor() {
        let images = std::path::Path::new("/tmp/catchlight review/3");
        assert_eq!(
            fetch_command(
                "catchlight-editor",
                &["--socket".into(), "/run/e.sock".into()],
                SessionId(3),
                images
            ),
            "catchlight-editor --socket /run/e.sock --session 3 review fetch --attachments-dir \
             '/tmp/catchlight review/3'"
        );
        assert_eq!(shell_word("it's"), r"'it'\''s'");
    }

    #[test]
    fn a_sandbox_marker_warns() {
        let none = |_: &str| None;
        assert!(sandbox_warning(&none).is_none());
        let codex = |name: &str| (name == "CODEX_PERMISSION_PROFILE").then(|| "read-only".into());
        assert!(sandbox_warning(&codex).is_some());
    }
}
