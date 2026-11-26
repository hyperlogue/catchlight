//! Which harness an agent runs in, and how a notice reaches its session.
//! Ported from r3: detection from `cli/listener.ts`, validation and delivery
//! from `server/listener.ts` and `server/inbox.ts`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A harness slower than this is wedged, not busy.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Where a notice goes. Holds the harness's own credentials, so it lives
/// only in owner-only files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "harness", rename_all = "snake_case")]
pub(crate) enum Target {
    /// Claude Code's per-session messaging socket.
    Claude { socket: PathBuf, token: String },
    /// `codex queue`, which wakes a persisted thread when it is idle.
    Codex {
        thread: String,
        executable: Option<PathBuf>,
        home: Option<PathBuf>,
    },
    /// `agy agentapi send-message` through Antigravity's running backend.
    Antigravity {
        conversation: String,
        address: String,
        token: String,
        executable: Option<PathBuf>,
        source_metadata: Option<String>,
    },
    /// Appends each notice to a file, for this crate's integration test.
    #[cfg(feature = "fake-harness")]
    Fake { file: PathBuf },
}

impl Target {
    pub const fn harness(&self) -> &'static str {
        match self {
            Self::Claude { .. } => "claude",
            Self::Codex { .. } => "codex",
            Self::Antigravity { .. } => "antigravity",
            #[cfg(feature = "fake-harness")]
            Self::Fake { .. } => "fake",
        }
    }

    pub fn executable(&self) -> Option<&Path> {
        match self {
            Self::Codex { executable, .. } | Self::Antigravity { executable, .. } => {
                executable.as_deref()
            }
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Detected {
    pub target: Target,
    /// The agent identity that goes with this target, as `review` reads it.
    pub session: Option<String>,
}

/// Why `listen` cannot register: each is exit 5.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unsupported {
    MissingClaudeToken,
    MissingAntigravityConnection,
    NoAdapter,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingClaudeToken => {
                "Claude Code's messaging token (CLAUDE_CODE_MESSAGING_TOKEN) is unavailable"
            }
            Self::MissingAntigravityConnection => {
                "Antigravity's messaging connection is unavailable; run this from its agent \
                 command environment"
            }
            Self::NoAdapter => "this harness has no notification adapter",
        })
    }
}

fn value(env: Env, name: &str) -> Option<String> {
    env(name)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// The target and its identity are chosen together: a partly inherited
/// Claude environment can sit beside a valid Codex one, and a Codex target
/// must not wear Claude's session id.
pub(crate) fn detect(env: Env) -> Result<Detected, Unsupported> {
    #[cfg(feature = "fake-harness")]
    if let Some(file) = value(env, "CATCHLIGHT_FAKE_HARNESS") {
        return Ok(Detected {
            target: Target::Fake { file: file.into() },
            session: None,
        });
    }
    let socket = value(env, "CLAUDE_CODE_MESSAGING_SOCKET");
    let token = value(env, "CLAUDE_CODE_MESSAGING_TOKEN");
    if let (Some(socket), Some(token)) = (&socket, token) {
        return Ok(Detected {
            target: Target::Claude {
                socket: socket.into(),
                token,
            },
            session: value(env, "CLAUDE_CODE_SESSION_ID").map(|id| format!("claude:{id}")),
        });
    }
    let conversation = value(env, "ANTIGRAVITY_CONVERSATION_ID");
    let address = value(env, "ANTIGRAVITY_LS_ADDRESS");
    let csrf = value(env, "ANTIGRAVITY_CSRF_TOKEN");
    if let (Some(conversation), Some(address), Some(token)) = (&conversation, &address, &csrf) {
        return Ok(Detected {
            target: Target::Antigravity {
                conversation: conversation.clone(),
                address: address.clone(),
                token: token.clone(),
                executable: value(env, "ANTIGRAVITY_AGENTAPI_EXE").map(PathBuf::from),
                source_metadata: value(env, "ANTIGRAVITY_SOURCE_METADATA"),
            },
            session: Some(format!("antigravity:{conversation}")),
        });
    }
    if let Some(thread) = value(env, "CODEX_THREAD_ID").or_else(|| value(env, "CODEX_SESSION_ID")) {
        return Ok(Detected {
            session: Some(format!("codex:{thread}")),
            target: Target::Codex {
                thread,
                executable: None,
                home: None,
            },
        });
    }
    Err(if socket.is_some() {
        Unsupported::MissingClaudeToken
    } else if conversation.is_some() || address.is_some() || csrf.is_some() {
        Unsupported::MissingAntigravityConnection
    } else {
        Unsupported::NoAdapter
    })
}

/// Resolves the executables delivery will run and checks that they can,
/// before anything is registered. The error is `listen`'s exit-5 message.
pub(crate) fn prepare(target: Target, env: Env) -> Result<Target, String> {
    let target = match target {
        Target::Codex { thread, .. } => {
            let executable =
                which("codex", env).ok_or("cannot find `codex` on PATH to run `codex queue`")?;
            let home = match value(env, "CODEX_HOME") {
                Some(home) => std::path::absolute(home).ok(),
                None => value(env, "HOME").map(|h| Path::new(&h).join(".codex")),
            };
            Target::Codex {
                thread,
                executable: Some(executable),
                home,
            }
        }
        Target::Antigravity {
            conversation,
            address,
            token,
            executable,
            source_metadata,
        } => {
            let executable = match executable {
                Some(path) => path,
                None => which("agy", env).ok_or("cannot find `agy` on PATH to run agentapi")?,
            };
            Target::Antigravity {
                conversation,
                address,
                token,
                executable: Some(executable),
                source_metadata,
            }
        }
        other => other,
    };
    validate(&target)?;
    let probe = match &target {
        Target::Codex { executable, .. } => Some((executable, &["queue", "--help"][..])),
        Target::Antigravity { executable, .. } => {
            Some((executable, &["agentapi", "send-message", "--help"][..]))
        }
        _ => None,
    };
    if let Some((Some(executable), args)) = probe {
        let ran = run(Command::new(executable).args(args));
        if ran != Ok(true) {
            return Err(format!(
                "`{} {}` does not run here",
                executable.display(),
                args.join(" ")
            ));
        }
    }
    Ok(target)
}

/// Checks what a target will be used for before a socket is dialled or a
/// program run with it.
pub(crate) fn validate(target: &Target) -> Result<(), String> {
    match target {
        Target::Claude { socket, token } => {
            claude_socket(socket)?;
            if token.len() > 4096 {
                return Err("Claude Code's messaging token is too long".into());
            }
        }
        Target::Codex {
            thread,
            executable,
            home,
        } => {
            if thread.contains('\0') || thread.chars().count() > 200 {
                return Err("invalid Codex thread id".into());
            }
            for path in [executable, home].into_iter().flatten() {
                if !path.is_absolute() {
                    return Err(format!("{}: not an absolute path", path.display()));
                }
            }
        }
        Target::Antigravity {
            conversation,
            address,
            token,
            executable,
            source_metadata,
        } => {
            if conversation.contains('\0') || conversation.chars().count() > 200 {
                return Err("invalid Antigravity conversation id".into());
            }
            // agentapi speaks plaintext gRPC: its credentials stay on this
            // machine.
            if !loopback(address) {
                return Err("Antigravity requires a loopback host:port address".into());
            }
            if token.len() > 4096 || token.contains(['\0', '\r', '\n']) {
                return Err("invalid Antigravity messaging token".into());
            }
            if executable.as_ref().is_some_and(|p| !p.is_absolute()) {
                return Err("invalid Antigravity executable".into());
            }
            if let Some(metadata) = source_metadata {
                let object = metadata.len() <= 8192
                    && serde_json::from_str::<serde_json::Value>(metadata)
                        .is_ok_and(|v| v.is_object());
                if !object {
                    return Err("invalid Antigravity source metadata".into());
                }
            }
        }
        #[cfg(feature = "fake-harness")]
        Target::Fake { .. } => {}
    }
    Ok(())
}

fn loopback(address: &str) -> bool {
    let port = ["localhost:", "127.0.0.1:", "[::1]:"]
        .iter()
        .find_map(|host| address.strip_prefix(host));
    port.is_some_and(|p| {
        !p.is_empty()
            && !p.starts_with('0')
            && p.len() <= 5
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| n <= 65535)
    })
}

/// The per-user directories Claude Code keeps its sockets in, matched on
/// the path as written: what it resolves to must then be a socket of ours.
fn claude_socket_dir(dir: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let cc_socks = |s: &str| s == "cc-socks" || s.strip_prefix("cc-socks-").is_some_and(digits);
    if let Some(rest) = dir.strip_prefix("/run/user/") {
        return rest
            .split_once('/')
            .is_some_and(|(uid, name)| digits(uid) && name == "cc-socks");
    }
    [
        "/tmp/",
        "/private/tmp/",
        "/data/data/com.termux/files/usr/tmp/",
    ]
    .iter()
    .any(|base| dir.strip_prefix(base).is_some_and(cc_socks))
}

fn claude_socket(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::FileTypeExt as _;
    let text = path.to_str().ok_or("the socket path is not UTF-8")?;
    if !path.is_absolute() || text.contains('\0') || !text.ends_with(".sock") {
        return Err("the socket must be an absolute path to a .sock".into());
    }
    let parent = path.parent().and_then(Path::to_str).unwrap_or_default();
    if !claude_socket_dir(parent) {
        return Err("the socket is outside Claude Code's socket directory".into());
    }
    let meta = std::fs::metadata(path).map_err(|_| "the socket does not exist".to_owned())?;
    if !meta.file_type().is_socket() {
        return Err("the path is not a socket".into());
    }
    Ok(())
}

fn which(name: &str, env: Env) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    env("PATH")?
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(name))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .and_then(|path| std::path::absolute(path).ok())
}

/// Runs to completion or kills it at the timeout. Output is discarded: it
/// can hold the harness's connection details.
fn run(command: &mut Command) -> Result<bool, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start it: {e}"))?;
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(status.success());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer in {}s", COMMAND_TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Hands `text` to the harness. `Ok` means written or queued, not read:
/// Claude Code's socket answers nothing.
pub(crate) fn deliver(target: &Target, text: &str, cwd: &Path) -> Result<(), String> {
    validate(target)?;
    match target {
        Target::Claude { socket, token } => {
            let mut stream = std::os::unix::net::UnixStream::connect(socket)
                .map_err(|e| format!("Claude Code's socket refused the notice: {e}"))?;
            stream
                .set_write_timeout(Some(SOCKET_TIMEOUT))
                .map_err(|e| e.to_string())?;
            // Without the token the session holds the message for manual
            // approval instead of reading it.
            let lines = format!(
                "{}\n{}\n",
                serde_json::json!({ "type": "auth", "token": token }),
                serde_json::json!({
                    "type": "user",
                    "message": { "role": "user", "content": text },
                }),
            );
            stream
                .write_all(lines.as_bytes())
                .and_then(|()| stream.flush())
                .map_err(|e| format!("Claude Code's socket took no notice: {e}"))?;
            let _ = stream.shutdown(std::net::Shutdown::Write);
            Ok(())
        }
        Target::Codex {
            thread,
            executable,
            home,
        } => {
            let program = executable.as_deref().unwrap_or_else(|| Path::new("codex"));
            let mut command = Command::new(program);
            // Direct argv, never a shell: the thread id and the text are
            // untrusted strings.
            command
                .args(["queue", "--thread", thread, "--message", text])
                .current_dir(cwd);
            if let Some(home) = home {
                command.env("CODEX_HOME", home);
            }
            match run(&mut command) {
                Ok(true) => Ok(()),
                Ok(false) => Err("`codex queue` failed; check that Codex is available".into()),
                Err(e) => Err(format!("`codex queue`: {e}")),
            }
        }
        Target::Antigravity {
            conversation,
            address,
            token,
            executable,
            source_metadata,
        } => {
            let program = executable.as_deref().unwrap_or_else(|| Path::new("agy"));
            let mut command = Command::new(program);
            command
                .args([
                    "agentapi",
                    "send-message",
                    "--title=catchlight",
                    "--",
                    conversation,
                    text,
                ])
                .current_dir(cwd)
                .env("ANTIGRAVITY_LS_ADDRESS", address)
                .env("ANTIGRAVITY_CSRF_TOKEN", token)
                .env("ANTIGRAVITY_CONVERSATION_ID", conversation);
            // One worker serves many sessions: never pass on another
            // caller's attribution.
            match source_metadata {
                Some(metadata) => command.env("ANTIGRAVITY_SOURCE_METADATA", metadata),
                None => command.env_remove("ANTIGRAVITY_SOURCE_METADATA"),
            };
            match run(&mut command) {
                Ok(true) => Ok(()),
                Ok(false) => Err(
                    "`agy agentapi send-message` failed; keep the Antigravity session running \
                     and listen again"
                        .into(),
                ),
                Err(e) => Err(format!("`agy agentapi send-message`: {e}")),
            }
        }
        #[cfg(feature = "fake-harness")]
        Target::Fake { file } => {
            let line = format!("{}\n", serde_json::Value::String(text.into()));
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .and_then(|mut f| f.write_all(line.as_bytes()))
                .map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn antigravity_wins_over_an_inherited_codex_thread() {
        let found = detect(&env(&[
            ("ANTIGRAVITY_CONVERSATION_ID", "c1"),
            ("ANTIGRAVITY_LS_ADDRESS", "127.0.0.1:5000"),
            ("ANTIGRAVITY_CSRF_TOKEN", "t"),
            ("ANTIGRAVITY_SOURCE_METADATA", "{\"a\":1}"),
            ("CODEX_THREAD_ID", "th"),
        ]))
        .unwrap();
        assert_eq!(found.session.as_deref(), Some("antigravity:c1"));
        assert!(matches!(
            &found.target,
            Target::Antigravity { conversation, source_metadata: Some(_), .. } if conversation == "c1"
        ));
    }

    #[test]
    fn claude_needs_its_socket_and_token_together() {
        let found = detect(&env(&[
            ("CLAUDE_CODE_MESSAGING_SOCKET", "/tmp/cc-socks/1.sock"),
            ("CLAUDE_CODE_MESSAGING_TOKEN", "k"),
            ("CLAUDE_CODE_SESSION_ID", "s1"),
            ("CODEX_THREAD_ID", "th"),
        ]))
        .unwrap();
        assert_eq!(found.session.as_deref(), Some("claude:s1"));
        assert_eq!(found.target.harness(), "claude");

        let codex = detect(&env(&[
            ("CLAUDE_CODE_SESSION_ID", "s1"),
            ("CODEX_SESSION_ID", " th "),
        ]))
        .unwrap();
        assert_eq!(
            codex.session.as_deref(),
            Some("codex:th"),
            "an incomplete Claude environment falls back whole"
        );
        assert_eq!(
            detect(&env(&[(
                "CLAUDE_CODE_MESSAGING_SOCKET",
                "/tmp/cc-socks/1.sock"
            )])),
            Err(Unsupported::MissingClaudeToken)
        );
        assert_eq!(
            detect(&env(&[("ANTIGRAVITY_CONVERSATION_ID", "c1")])),
            Err(Unsupported::MissingAntigravityConnection)
        );
        assert_eq!(
            detect(&env(&[("CLAUDE_CODE_SESSION_ID", "s1")])),
            Err(Unsupported::NoAdapter)
        );
    }

    #[test]
    fn targets_are_checked_before_use() {
        assert!(claude_socket_dir("/run/user/1000/cc-socks"));
        assert!(claude_socket_dir("/tmp/cc-socks-1000"));
        assert!(claude_socket_dir("/private/tmp/cc-socks"));
        assert!(!claude_socket_dir("/tmp/cc-socks/nested"));
        assert!(!claude_socket_dir("/tmp/elsewhere"));
        assert!(!claude_socket_dir("/run/user/x/cc-socks"));
        let claude = |socket: &str| Target::Claude {
            socket: socket.into(),
            token: "k".into(),
        };
        assert!(validate(&claude("/tmp/elsewhere/1.sock")).is_err());
        assert!(validate(&claude("relative/cc-socks/1.sock")).is_err());
        assert!(validate(&claude("/tmp/cc-socks/missing-for-this-test.sock")).is_err());

        for (address, ok) in [
            ("localhost:5000", true),
            ("127.0.0.1:65535", true),
            ("[::1]:1", true),
            ("127.0.0.1:65536", false),
            ("127.0.0.1:0", false),
            ("example.com:5000", false),
            ("dns:///127.0.0.1:5", false),
        ] {
            assert_eq!(loopback(address), ok, "{address}");
        }
        let agy = |metadata: &str| Target::Antigravity {
            conversation: "c".into(),
            address: "localhost:1".into(),
            token: "t".into(),
            executable: Some("/bin/agy".into()),
            source_metadata: Some(metadata.into()),
        };
        assert!(validate(&agy("{}")).is_ok());
        assert!(validate(&agy("[1]")).is_err());
    }
}
