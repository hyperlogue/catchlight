//! The harness process that owns a registration, so the worker can drop it
//! once that process exits. Ported from r3's `cli/agent-process.ts`.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A PID means something only in its boot and PID namespace; `started_at`
/// tells the agent from an unrelated process that reused its PID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcess {
    pub pid: u32,
    pub platform: String,
    pub boot_id: String,
    pub namespace: String,
    pub started_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub parent: u32,
    pub uid: u32,
    pub name: String,
    pub started_at: String,
    pub dead: bool,
}

/// `None`: unknown. `Some(None)`: no such process.
type Read = Option<Option<Entry>>;

/// Parses `/proc/<pid>/stat`. `comm` may itself hold spaces and parentheses;
/// field 22 is the start time.
pub fn parse_linux_stat(stat: &str, uid: u32) -> Option<Entry> {
    let begin = stat.find('(')?;
    let end = stat.rfind(')')?;
    if begin < 1 || end < begin {
        return None;
    }
    let fields: Vec<&str> = stat.get(end + 2..)?.split_whitespace().collect();
    if fields.len() < 20 || !fields[19].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stat[..begin]
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|p| *p > 0)?;
    Some(Entry {
        parent: fields[1].parse().ok()?,
        uid,
        name: stat[begin + 1..end].to_owned(),
        started_at: fields[19].to_owned(),
        dead: matches!(fields[0], "Z" | "X" | "x"),
    })
}

/// Names a harness's long-lived process goes by. Every harness also starts
/// short-lived helpers under its own name, so both the process name and its
/// command must match.
pub fn harness_names(harness: &str, executable: Option<&Path>) -> Vec<String> {
    let mut names: Vec<String> = match harness {
        "claude" => vec!["claude".into(), ".claude-wrapped".into()],
        "codex" => vec!["codex".into(), ".codex-wrapped".into()],
        "antigravity" => vec!["agy".into(), ".agy-wrapped".into(), "antigravity".into()],
        _ => Vec::new(),
    };
    if harness != "claude"
        && let Some(name) = executable.and_then(Path::file_name)
    {
        names.push(name.to_string_lossy().into_owned());
    }
    names
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The nearest ancestor from `first` that is the harness itself. `None`
/// when isolation hides it: then the registration is left untracked rather
/// than tied to the life of one tool call.
pub fn find_ancestor(
    names: &[String],
    read: &mut dyn FnMut(u32) -> Read,
    first: u32,
    command_name: &dyn Fn(u32) -> Option<String>,
) -> Option<u32> {
    let mut seen = Vec::new();
    let mut pid = first;
    while pid > 0 && seen.len() < 64 && !seen.contains(&pid) {
        seen.push(pid);
        let entry = read(pid).flatten().filter(|e| !e.dead)?;
        if names.iter().any(|n| n == basename(&entry.name))
            && let Some(invoked) = command_name(pid)
            && names.iter().any(|n| n == basename(&invoked))
        {
            return Some(pid);
        }
        pid = entry.parent;
    }
    None
}

/// One inspection of the process table; reads are cached for its life.
pub struct Processes {
    context: Option<(String, String, String)>,
    uid: Option<u32>,
    entries: HashMap<u32, Read>,
}

impl Processes {
    /// `uid` is this user's, from a file this process owns.
    pub fn new(uid: Option<u32>) -> Self {
        Self {
            context: context(),
            uid,
            entries: HashMap::new(),
        }
    }

    /// Drops what earlier reads saw.
    pub fn forget(&mut self) {
        self.entries.clear();
    }

    pub fn read(&mut self, pid: u32) -> Read {
        if let Some(entry) = self.entries.get(&pid) {
            return entry.clone();
        }
        let entry = match self.context.as_ref().map(|c| c.0.as_str()) {
            Some("linux") => read_linux(pid),
            Some("darwin") => read_darwin(pid),
            _ => None,
        };
        self.entries.insert(pid, entry.clone());
        entry
    }

    pub fn identity(&mut self, pid: u32) -> Option<AgentProcess> {
        let (platform, boot_id, namespace) = self.context.clone()?;
        let entry = self.read(pid).flatten()?;
        if entry.dead || Some(entry.uid) != self.uid {
            return None;
        }
        Some(AgentProcess {
            pid,
            platform,
            boot_id,
            namespace,
            started_at: entry.started_at,
        })
    }

    pub fn liveness(&mut self, owner: &AgentProcess) -> Liveness {
        let Some((platform, boot_id, namespace)) = self.context.clone() else {
            return Liveness::Unknown;
        };
        if platform != owner.platform {
            return Liveness::Unknown;
        }
        if boot_id != owner.boot_id {
            return Liveness::Dead;
        }
        if namespace != owner.namespace {
            return Liveness::Unknown;
        }
        match self.read(owner.pid) {
            None => Liveness::Unknown,
            Some(Some(e)) if !e.dead && e.started_at == owner.started_at => Liveness::Alive,
            Some(_) => Liveness::Dead,
        }
    }

    /// The harness process this CLI runs under: `CATCHLIGHT_AGENT_PID` when
    /// a custom launcher hides it, else the nearest recognisable ancestor.
    pub fn capture(
        &mut self,
        harness: &str,
        executable: Option<&Path>,
        supplied: Option<&str>,
    ) -> Result<Option<AgentProcess>, String> {
        if let Some(supplied) = supplied.map(str::trim).filter(|s| !s.is_empty()) {
            return supplied
                .parse::<u32>()
                .ok()
                .filter(|pid| *pid >= 1)
                .and_then(|pid| self.identity(pid))
                .map(Some)
                .ok_or_else(|| {
                    "CATCHLIGHT_AGENT_PID must name a live process owned by this user".to_owned()
                });
        }
        let names = harness_names(harness, executable);
        let platform = self.context.as_ref().map(|c| c.0.clone());
        let pid = find_ancestor(
            &names,
            &mut |pid| self.read(pid),
            std::os::unix::process::parent_id(),
            &|pid| command_name(platform.as_deref(), pid),
        );
        Ok(pid.and_then(|pid| self.identity(pid)))
    }
}

fn context() -> Option<(String, String, String)> {
    if cfg!(target_os = "linux") {
        // A host-mounted /proc inside a child namespace numbers PIDs
        // differently; then nothing here can establish that a process died.
        let own = std::fs::read_to_string("/proc/self/stat").ok()?;
        let pid: u32 = own.get(..own.find('(')?)?.trim().parse().ok()?;
        if pid != std::process::id() {
            return None;
        }
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let namespace = std::fs::read_link("/proc/self/ns/pid").ok()?;
        Some((
            "linux".into(),
            boot.trim().to_owned(),
            namespace.to_string_lossy().into_owned(),
        ))
    } else if cfg!(target_os = "macos") {
        let boot = command(&["sysctl", "-n", "kern.boottime"])?;
        Some(("darwin".into(), boot, "host".into()))
    } else {
        None
    }
}

fn read_linux(pid: u32) -> Read {
    use std::os::unix::fs::MetadataExt as _;
    let dir = format!("/proc/{pid}");
    match (
        std::fs::read_to_string(format!("{dir}/stat")),
        std::fs::metadata(&dir),
    ) {
        (Ok(stat), Ok(meta)) => Some(parse_linux_stat(&stat, meta.uid())),
        // A hidden entry or EPERM is no proof the process exited.
        (_, Err(err)) if err.kind() == std::io::ErrorKind::NotFound => Some(None),
        _ => None,
    }
}

fn read_darwin(pid: u32) -> Read {
    let pid_arg = pid.to_string();
    let out = command(&[
        "ps",
        "-p",
        &pid_arg,
        "-o",
        "pid=,ppid=,uid=,stat=,lstart=,comm=",
    ]);
    let Some(line) = out else {
        // `ps -p` exits 1 for a missing PID and for a failed inspection alike.
        return command(&["ps", "-p", "1", "-o", "pid="]).map(|_| None);
    };
    let fields: Vec<&str> = line.split_whitespace().collect();
    // pid ppid uid stat, then lstart's five words, then comm.
    if fields.len() < 10 {
        return None;
    }
    Some(Some(Entry {
        parent: fields[1].parse().ok()?,
        uid: fields[2].parse().ok()?,
        dead: fields[3].starts_with('Z'),
        started_at: fields[4..9].join(" "),
        name: basename(&fields[9..].join(" ")).to_owned(),
    }))
}

/// `argv[0]`: `comm` can name the original executable when `argv[0]` selects
/// another role. Only ever read for discovery, never stored.
fn command_name(platform: Option<&str>, pid: u32) -> Option<String> {
    let argv0 = match platform? {
        "linux" => std::fs::read(format!("/proc/{pid}/cmdline"))
            .ok()
            .and_then(|bytes| {
                let first = bytes.split(|b| *b == 0).next()?.to_vec();
                String::from_utf8(first).ok()
            })?,
        "darwin" => command(&["ps", "-ww", "-p", &pid.to_string(), "-o", "args="])?
            .split_whitespace()
            .next()?
            .to_owned(),
        _ => return None,
    };
    (!argv0.is_empty()).then(|| basename(&argv0).to_owned())
}

fn command(argv: &[&str]) -> Option<String> {
    let out = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(parent: u32, name: &str) -> Entry {
        Entry {
            parent,
            uid: 1000,
            name: name.into(),
            started_at: "1".into(),
            dead: false,
        }
    }

    #[test]
    fn a_stat_line_survives_parentheses_in_its_name() {
        let stat = "42 (a (b) c) S 7 42 42 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 98765 0 0";
        let parsed = parse_linux_stat(stat, 1000).unwrap();
        assert_eq!(parsed.name, "a (b) c");
        assert_eq!(parsed.parent, 7);
        assert_eq!(parsed.started_at, "98765");
        assert!(!parsed.dead);
        assert!(parse_linux_stat("42 (x) Z 7", 1000).is_none());
        let zombie = stat.replace(") S ", ") Z ");
        assert!(parse_linux_stat(&zombie, 1000).unwrap().dead);
    }

    #[test]
    fn the_owner_is_the_harness_not_a_helper_that_shares_its_name() {
        // 10 bash <- 20 claude helper (argv0 is a sandbox) <- 30 claude <- 1
        let table: HashMap<u32, Entry> = HashMap::from([
            (10, entry(20, "bash")),
            (20, entry(30, "claude")),
            (30, entry(1, ".claude-wrapped")),
            (1, entry(0, "init")),
        ]);
        let argv0 = |pid: u32| match pid {
            20 => Some("sandbox-helper".to_owned()),
            30 => Some(".claude-wrapped".to_owned()),
            _ => None,
        };
        let names = harness_names("claude", None);
        let mut read = |pid: u32| Some(table.get(&pid).cloned());
        assert_eq!(find_ancestor(&names, &mut read, 10, &argv0), Some(30));
        let mut hidden = |pid: u32| (pid != 30).then(|| table.get(&pid).cloned());
        assert_eq!(find_ancestor(&names, &mut hidden, 10, &argv0), None);
        let codex = harness_names("codex", Some(Path::new("/opt/bin/codex-cli")));
        assert!(codex.contains(&"codex-cli".to_owned()));
    }
}
