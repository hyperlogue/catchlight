#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]

//! A stopped launcher takes its connection file with it and exits as a shell
//! reports the signal that stopped it.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn stopped_by(signal: &str) -> Option<i32> {
    let dir =
        std::env::temp_dir().join(format!("cl-editor-signal-{}-{signal}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let connection = dir.join("connection.json");
    let mut launcher = Command::new(env!("CARGO_BIN_EXE_catchlight-editor"))
        .args(["--no-open", "--port", "0"])
        .current_dir(&dir)
        .env("CATCHLIGHT_EDITOR_CONNECTION", &connection)
        .env("CATCHLIGHT_KEEP", dir.join("kept"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&connection);
    let sent = Command::new("kill")
        .args([format!("-{signal}"), launcher.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success(), "kill -{signal}");
    let code = launcher.wait().unwrap().code();
    assert!(!connection.exists(), "SIG{signal} left the connection file");
    std::fs::remove_dir_all(&dir).ok();
    code
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(Instant::now() < deadline, "no {}", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn each_signal_exits_with_its_own_code() {
    assert_eq!(stopped_by("INT"), Some(130));
    assert_eq!(stopped_by("TERM"), Some(143));
    assert_eq!(stopped_by("HUP"), Some(129));
}
