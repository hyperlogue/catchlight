#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

//! `review listen` end to end: a real editor, the real binary and its
//! detached worker, and the test-only harness that appends each notice to a
//! file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{Duration, Instant};

use catchlight_editor_protocol::{Command as Wire, Reply, Request, ResponseBody};
use catchlight_editor_server::{Editor, FileStorage, serve_unix};

/// Removes the directory and stops a worker the test left behind.
struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = cli(&self.0, &["review", "worker", "stop"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn agent(dir: &Path, program: &str) -> Command {
    let mut command = Command::new(program);
    // Nothing of the harness this test itself runs under may leak in.
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy().into_owned();
        if ["CLAUDE_", "CODEX_", "ANTIGRAVITY_", "CATCHLIGHT_"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            command.env_remove(&name);
        }
    }
    command
        .current_dir(dir)
        .env("CATCHLIGHT_STATE_DIR", dir.join("state"))
        .env("CATCHLIGHT_FAKE_HARNESS", dir.join("notices"))
        .env("CATCHLIGHT_AGENT_SESSION", "fake:agent")
        .env("CATCHLIGHT_AGENT_PID", std::process::id().to_string())
        .env("XDG_CACHE_HOME", dir.join("cache"));
    command
}

fn cli(dir: &Path, args: &[&str]) -> Output {
    agent(dir, env!("CARGO_BIN_EXE_catchlight-editor"))
        // Relative: an absolute temp path can outgrow `sun_path`.
        .args(["--socket", "e.sock", "--session", "1"])
        .args(args)
        .output()
        .unwrap()
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let out = cli(dir, args);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{args:?}: {:?}\n{text}{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

fn notices(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("notices"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn each_send_is_announced_once_in_the_listening_session() {
    let dir = std::env::temp_dir().join(format!("cl-listen-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = Dir(dir);

    let editor = Arc::new(Editor::with_storage(Arc::new(FileStorage::new(&dir.0))));
    let opened = editor.handle(Request {
        id: 1,
        command: Wire::SessionNew {
            name: Some("lan".into()),
            source: None,
        },
    });
    assert!(
        matches!(&opened, Reply::Ok { body: ResponseBody::Session { session }, .. } if session.0 == 1),
        "{opened:?}"
    );
    let socket = dir.0.join("e.sock");
    std::thread::spawn(move || {
        let _ = serve_unix(editor, &socket);
    });
    wait_for("the editor socket", || dir.0.join("e.sock").exists());

    let listening = ok(&dir.0, &["review", "listen"]);
    assert!(listening.contains("listening on session 1"), "{listening}");
    let status = ok(&dir.0, &["review", "worker", "status"]);
    assert!(status.contains("review worker: running"), "{status}");
    assert!(status.contains("fake:agent"), "{status}");
    let list = ok(&dir.0, &["review", "list"]);
    assert!(list.contains("(fake:agent) is listening"), "{list}");

    ok(
        &dir.0,
        &[
            "review",
            "open",
            "--model",
            "--human",
            "-m",
            "the ear tears",
        ],
    );
    ok(&dir.0, &["review", "send"]);
    wait_for("the first notice", || notices(&dir.0).len() == 1);
    let notice = &notices(&dir.0)[0];
    assert!(notice.contains("editor session 1"), "{notice}");
    assert!(notice.contains("\"lan\""), "{notice}");
    assert!(
        notice.contains("--socket") && notice.contains("review fetch --attachments-dir"),
        "{notice}"
    );
    let images = dir.0.join("state/listen/images");
    assert!(notice.contains(&*images.to_string_lossy()), "{notice}");

    // The same pending batch is never announced again.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(notices(&dir.0).len(), 1);

    ok(
        &dir.0,
        &["review", "reply", "1", "--human", "-m", "and the other one"],
    );
    ok(&dir.0, &["review", "send"]);
    wait_for("the second notice", || notices(&dir.0).len() == 2);
    let [first, second] = &notices(&dir.0)[..] else {
        panic!("two notices");
    };
    assert!(first.contains("1 new review comment(s) (#1)"), "{first}");
    assert!(second.contains("1 new review comment(s) (#2)"), "{second}");

    // Run as printed, with this build first on PATH.
    let printed = notices(&dir.0)[1].split('`').nth(1).unwrap().to_owned();
    let bin = Path::new(env!("CARGO_BIN_EXE_catchlight-editor"))
        .parent()
        .unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = agent(&dir.0, "sh")
        .env("PATH", path)
        .args(["-c", &printed])
        .output()
        .unwrap();
    let fetched = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{printed}: {fetched}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fetched.contains("and the other one"), "{fetched}");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(notices(&dir.0).len(), 2, "taking them announces nothing");

    ok(&dir.0, &["review", "unlisten"]);
    wait_for("the worker to exit", || {
        ok(&dir.0, &["review", "worker", "status"]).contains("review worker: not running")
    });
}
