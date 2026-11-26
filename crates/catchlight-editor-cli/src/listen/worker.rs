//! The detached per-user process that keeps registered agents listening and
//! announces each new batch of sent comments in their harness sessions. It
//! never fetches or acknowledges: the agent does that when the notice
//! arrives.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow, bail};
use catchlight_editor_protocol::*;

use super::harness;
use super::process::{Liveness, Processes};
use super::store::{Registration, Store};
use crate::transport::{Endpoint, Transport};

/// Well inside the server's five-second listening window.
const TICK: Duration = Duration::from_secs(1);
/// Owner checks walk the process table; they need not run every tick.
const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// An editor unreachable this long has exited; its session ids would name
/// other models in the next one.
const GONE_AFTER: Duration = Duration::from_secs(30);
const MAX_LOG_BYTES: u64 = 1024 * 1024;

pub(crate) fn log(line: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    eprintln!("[{now}] {line}");
}

fn lock_file(store: &Store) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(store.worker_lock())?)
}

/// The running worker's pid, or `None` when none runs.
pub(crate) fn running(store: &Store) -> Result<Option<u32>> {
    let mut file = lock_file(store)?;
    match file.try_lock() {
        Ok(()) => Ok(None),
        Err(TryLockError::WouldBlock) => {
            let mut pid = String::new();
            file.read_to_string(&mut pid)?;
            Ok(Some(pid.trim().parse().unwrap_or(0)))
        }
        Err(TryLockError::Error(err)) => Err(err.into()),
    }
}

/// How many polls the worker running as `pid` has made, once it has
/// counted any.
pub(crate) fn polls(store: &Store, pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(store.worker_polls()).ok()?;
    let (owner, polls) = text.trim().split_once(' ')?;
    if owner.parse() != Ok(pid) {
        return None;
    }
    polls.parse().ok()
}

/// Replaced whole, so a reader never sees half of it.
fn write_polls(store: &Store, polls: u64) -> Result<()> {
    let path = store.worker_polls();
    let tmp = path.with_extension("polls.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    write!(file, "{} {polls}", std::process::id())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Starts the worker unless one runs. `true` when this call started it.
pub(crate) fn ensure(store: &Store) -> Result<bool> {
    if running(store)?.is_some() {
        return Ok(false);
    }
    trim_log(store);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(store.log())?;
    let exe = std::env::current_exe()?;
    let mut child = {
        use std::os::unix::process::CommandExt as _;
        std::process::Command::new(exe)
            .args(["review", "worker", "run"])
            .current_dir(&store.dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            // Out of the agent's process group, so the end of its tool call
            // or a Ctrl-C there does not take the worker along.
            .process_group(0)
            .spawn()?
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if running(store)?.is_some() {
            return Ok(true);
        }
        if child.try_wait()?.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!(
        "the review worker did not start; see {}",
        store.log().display()
    )
}

fn trim_log(store: &Store) {
    if std::fs::metadata(store.log()).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::remove_file(store.log());
    }
}

/// Asks the worker to exit and waits for it. `false` when none ran.
pub(crate) fn stop(store: &Store) -> Result<bool> {
    let Some(pid) = running(store)? else {
        return Ok(false);
    };
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(store.stop_file())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if running(store)?.is_none() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the review worker (pid {pid}) has not stopped")
}

/// What the agent reads when comments arrive. `fresh` are the comment ids
/// the last notice did not cover. Naming them keeps consecutive notices
/// distinct: Claude Code drops a message identical to the one before it.
pub(crate) fn notice(reg: &Registration, threads: u32, fresh: &[&str]) -> String {
    format!(
        "[catchlight] The person sent {} new review comment(s) ({}) on editor session {} \
         (model \"{}\"); {threads} thread(s) now wait for you.\n\
         Run `{}` to read and acknowledge them.",
        fresh.len(),
        fresh
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", "),
        reg.session,
        reg.title,
        reg.fetch
    )
}

struct Editor {
    endpoint: Endpoint,
    transport: Option<Transport>,
    down_since: Option<Instant>,
    /// Its sessions were checked against the registrations since it was
    /// last dialled.
    checked: bool,
    next_id: u64,
}

impl Editor {
    fn call(&mut self, command: Command) -> Result<Reply> {
        let transport = self
            .transport
            .as_mut()
            .ok_or_else(|| anyhow!("not connected"))?;
        self.next_id += 1;
        let reply = transport.call(
            Request {
                id: self.next_id,
                command,
            },
            serde_json::Map::new(),
        );
        if reply.is_err() {
            self.transport = None;
            self.checked = false;
            self.down_since.get_or_insert_with(Instant::now);
        }
        reply
    }
}

struct Delivered {
    reg: Registration,
    result: Result<(), String>,
}

/// `review worker run`: returns when no registration is left, or when asked
/// to stop.
pub(crate) fn run(store: &Store) -> Result<()> {
    let mut lock = lock_file(store)?;
    let deadline = Instant::now() + Duration::from_secs(1);
    // A `running` probe holds the lock for a moment; a worker holds it for
    // good.
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Error(err)) => return Err(err.into()),
        }
    }
    lock.set_len(0)?;
    lock.rewind()?;
    write!(lock, "{}", std::process::id())?;
    lock.flush()?;
    let _ = std::fs::remove_file(store.stop_file());
    log(&format!("worker {} started", std::process::id()));

    let (sent, done) = mpsc::channel::<Delivered>();
    let mut editors: BTreeMap<String, Editor> = BTreeMap::new();
    let mut in_flight: BTreeSet<String> = BTreeSet::new();
    let mut swept: Option<Instant> = None;
    let mut processes = Processes::new(Some(store.uid));
    let mut polls = 0;
    write_polls(store, polls)?;
    loop {
        if store.stop_file().exists() {
            let _ = std::fs::remove_file(store.stop_file());
            log("stopped on request");
            break;
        }
        while let Ok(Delivered { reg, result }) = done.try_recv() {
            in_flight.remove(&reg.name());
            match result {
                Ok(()) => log(&format!(
                    "notified {} ({}) of session {}",
                    reg.agent.id,
                    reg.target.harness(),
                    reg.session
                )),
                Err(why) => {
                    log(&format!(
                        "notifying {} of session {} failed: {why}",
                        reg.agent.id, reg.session
                    ));
                    store.update(&reg, |r| r.failure = Some(why))?;
                }
            }
        }
        processes.forget();
        if swept.is_none_or(|at| at.elapsed() >= SWEEP_EVERY) {
            swept = Some(Instant::now());
            for gone in store.sweep(&mut |owner| processes.liveness(owner))? {
                log(&format!(
                    "dropped {}: its harness process {} exited",
                    gone.agent.id,
                    gone.owner.map(|o| o.pid).unwrap_or_default()
                ));
            }
        }
        let active: Vec<Registration> = {
            let _lock = store.lock()?;
            store.list()?
        }
        .into_iter()
        .filter(|r| r.failure.is_none())
        .collect();
        if active.is_empty() && in_flight.is_empty() {
            log("no agent is listening; exiting");
            break;
        }

        let mut by_editor: BTreeMap<String, Vec<Registration>> = BTreeMap::new();
        for reg in active {
            by_editor
                .entry(format!("{:?}", reg.editor))
                .or_default()
                .push(reg);
        }
        editors.retain(|key, _| by_editor.contains_key(key));
        let before = polls;
        for (key, regs) in by_editor {
            let editor = editors.entry(key).or_insert_with(|| Editor {
                endpoint: regs[0].editor.clone(),
                transport: None,
                down_since: None,
                checked: false,
                next_id: 0,
            });
            serve(
                store,
                editor,
                regs,
                &mut processes,
                &mut in_flight,
                &sent,
                &mut polls,
            )?;
        }
        if polls != before {
            write_polls(store, polls)?;
        }
        std::thread::sleep(TICK);
    }
    Ok(())
}

fn drop_reg(store: &Store, reg: &Registration, why: &str) -> Result<()> {
    log(&format!(
        "dropped {} on session {}: {why}",
        reg.agent.id, reg.session
    ));
    let _lock = store.lock()?;
    if store
        .read(&reg.name())?
        .is_some_and(|r| r.nonce == reg.nonce)
    {
        store.remove(&reg.name())?;
    }
    Ok(())
}

fn serve(
    store: &Store,
    editor: &mut Editor,
    regs: Vec<Registration>,
    processes: &mut Processes,
    in_flight: &mut BTreeSet<String>,
    sent: &mpsc::Sender<Delivered>,
    polls: &mut u64,
) -> Result<()> {
    if editor.transport.is_none() {
        match editor.endpoint.connect() {
            Ok(transport) => editor.transport = Some(transport),
            Err(err) => {
                let since = *editor.down_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= GONE_AFTER {
                    for reg in &regs {
                        drop_reg(store, reg, &format!("the editor is gone ({err:#})"))?;
                    }
                }
                return Ok(());
            }
        }
    }
    if !editor.checked {
        let Ok(reply) = editor.call(Command::SessionList) else {
            return Ok(());
        };
        let sessions = match reply {
            Reply::Ok {
                body: ResponseBody::Sessions { sessions },
                ..
            } => sessions,
            other => bail!("session_list answered {other:?}"),
        };
        editor.down_since = None;
        editor.checked = true;
        for reg in &regs {
            let same = sessions
                .iter()
                .any(|s| s.session.0 == reg.session && s.title == reg.title && s.file == reg.file);
            if !same {
                drop_reg(store, reg, "the session closed or now holds another model")?;
            }
        }
        return Ok(());
    }
    for reg in regs {
        let Ok(reply) = editor.call(Command::ReviewPoll {
            session: SessionId(reg.session),
            agent: reg.agent.clone(),
        }) else {
            return Ok(());
        };
        editor.down_since = None;
        *polls += 1;
        let (threads, batch) = match reply {
            Reply::Ok {
                body: ResponseBody::ReviewPending { pending, batch },
                ..
            } => (pending, batch),
            Reply::Err {
                code: ErrorCode::NoSession,
                ..
            } => {
                drop_reg(store, &reg, "the session closed")?;
                continue;
            }
            other => {
                log(&format!("polling session {}: {other:?}", reg.session));
                continue;
            }
        };
        let Some(batch) = batch.filter(|b| reg.is_new(b)) else {
            continue;
        };
        if in_flight.contains(&reg.name()) {
            continue;
        }
        if let Some(owner) = &reg.owner
            && processes.liveness(owner) == Liveness::Dead
        {
            drop_reg(store, &reg, "its harness process exited")?;
            continue;
        }
        let known: Vec<&str> = reg
            .notified
            .as_deref()
            .map(|b| b.split('.').collect())
            .unwrap_or_default();
        let fresh: Vec<&str> = batch.split('.').filter(|id| !known.contains(id)).collect();
        let text = notice(&reg, threads, &fresh);
        // Recorded before delivery: one notice per batch even if this
        // worker dies mid-delivery.
        let mut told = None;
        store.update(&reg, |r| {
            r.notified = Some(batch.clone());
            told = Some(r.clone());
        })?;
        let Some(reg) = told else {
            continue;
        };
        in_flight.insert(reg.name());
        let sent = sent.clone();
        let cwd = store.dir.clone();
        std::thread::spawn(move || {
            let result = harness::deliver(&reg.target, &text, &cwd);
            let _ = sent.send(Delivered { reg, result });
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_session_the_model_and_the_command() {
        let reg = Registration {
            nonce: "n".into(),
            editor: Endpoint::Socket("/run/e.sock".into()),
            session: 3,
            title: "lan".into(),
            file: Some("lan.clm".into()),
            agent: ReviewIdentity {
                id: "claude:s".into(),
                name: "claude".into(),
            },
            target: harness::Target::Codex {
                thread: "t".into(),
                executable: None,
                home: None,
            },
            owner: None,
            fetch: "catchlight-editor --session 3 review fetch --attachments-dir /tmp/x".into(),
            notified: None,
            failure: None,
            registered: 0,
        };
        assert_eq!(
            notice(&reg, 2, &["4", "7"]),
            "[catchlight] The person sent 2 new review comment(s) (#4, #7) on editor session 3 \
             (model \"lan\"); 2 thread(s) now wait for you.\n\
             Run `catchlight-editor --session 3 review fetch --attachments-dir /tmp/x` to read and \
             acknowledge them."
        );
    }
}
