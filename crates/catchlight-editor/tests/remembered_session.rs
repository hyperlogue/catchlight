#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]
#![cfg(unix)]

//! The session a command remembers belongs to the editor that numbered it:
//! every editor numbers from 1, so another one's session 1 is not it.

use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{Duration, Instant};

use catchlight_editor_protocol::{Command as Wire, Reply, Request};
use catchlight_editor_server::{Editor, serve_unix};

/// A new editor at `name`, holding its session 1.
fn serve(dir: &Path, name: &str) {
    let editor = Arc::new(Editor::new());
    let opened = editor.handle(Request {
        id: 1,
        command: Wire::SessionNew {
            name: None,
            source: None,
        },
    });
    assert!(matches!(opened, Reply::Ok { .. }), "{opened:?}");
    let socket = dir.join(name);
    // A listener left behind keeps its thread, unreachable once its path is gone.
    let _ = std::fs::remove_file(&socket);
    let listening = socket.clone();
    std::thread::spawn(move || serve_unix(editor, &listening));
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(Instant::now() < deadline, "no editor at {name}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn cli(dir: &Path, socket: &str, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_catchlight-editor"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CATCHLIGHT_") {
            command.env_remove(&name);
        }
    }
    command
        .current_dir(dir)
        .env("XDG_CACHE_HOME", dir.join("cache"))
        // Relative: an absolute temp path can outgrow `sun_path`.
        .args(["--socket", socket])
        .args(args)
        .output()
        .unwrap()
}

fn select(dir: &Path, socket: &str) {
    let out = cli(dir, socket, &["session", "use", "1"]);
    assert!(out.status.success(), "{out:?}");
}

/// Whether a command naming no session finds one.
fn selected(dir: &Path, socket: &str) -> bool {
    let out = cli(dir, socket, &["node", "tree"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() || err.contains("no session selected"),
        "{err}"
    );
    out.status.success()
}

#[test]
fn a_remembered_session_is_used_only_on_the_editor_that_numbered_it() {
    let dir = std::env::temp_dir().join(format!("cl-remembered-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    serve(&dir, "a.sock");
    serve(&dir, "b.sock");

    select(&dir, "a.sock");
    assert!(selected(&dir, "a.sock"));
    assert!(!selected(&dir, "b.sock"), "b's session 1 is not a's");

    select(&dir, "b.sock");
    assert!(selected(&dir, "b.sock"));
    serve(&dir, "b.sock");
    assert!(
        !selected(&dir, "b.sock"),
        "a new editor at the same socket numbered its session 1 afresh"
    );
    std::fs::remove_dir_all(&dir).ok();
}
