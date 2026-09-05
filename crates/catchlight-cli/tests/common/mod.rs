//! Shared scaffolding for the file-level operation tests.
//!
//! Every test works on a *copy* of a committed fixture, or on a file built in
//! the test out of one: `tests/models/*.clm` are Git LFS objects and nothing
//! here may write to them. `guard_the_fixtures` in `diff.rs` is the check that
//! says so.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use catchlight_core::formats::clm::{self, ClmFile};

/// `tests/models/` at the workspace root.
pub fn models_dir() -> PathBuf {
    // crates/catchlight-cli/ -> crates/ -> workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("tests/models")
}

pub fn fixture(name: &str) -> PathBuf {
    models_dir().join(format!("{name}.clm"))
}

/// Every committed `.clm`, sorted.
pub fn fixtures() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(models_dir())
        .expect("tests/models")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "clm"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no .clm fixtures found");
    paths
}

/// A fresh, empty directory for one test.
pub fn tmp(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A writable copy of a committed fixture.
pub fn copy_fixture(name: &str, dir: &Path) -> PathBuf {
    let to = dir.join(format!("{name}.clm"));
    std::fs::copy(fixture(name), &to).expect("copy fixture");
    to
}

pub fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read")
}

pub fn decode(path: &Path) -> ClmFile {
    clm::decode(&read(path)).expect("decode")
}

/// Write an edited structure out as `<name>.clm` in `dir`.
pub fn write_clm(dir: &Path, name: &str, file: &ClmFile) -> PathBuf {
    let path = dir.join(format!("{name}.clm"));
    let bytes = clm::encode(&file.doc, &file.textures, &file.extensions).expect("encode");
    std::fs::write(&path, bytes).expect("write");
    path
}

/// The built `catchlight-cli` binary, for the tests that are about argument
/// parsing and exit statuses rather than about the operations.
pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_catchlight-cli")
}

/// Run the binary and return `(exit code, stdout, stderr)`.
pub fn run(args: &[&str]) -> (i32, String, String) {
    let output = std::process::Command::new(bin())
        .args(args)
        .output()
        .expect("run catchlight-cli");
    (
        output.status.code().expect("exit code"),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}
