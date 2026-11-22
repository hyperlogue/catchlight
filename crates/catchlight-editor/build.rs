//! Compiles the editor page into the binary: every file under `ui/` (or the
//! directory `CATCHLIGHT_EDITOR_UI` names) becomes one `include_bytes!` in a
//! table the server serves from.
//!
//! A checkout that never ran `cargo xtask bundle-ui` still builds, with an
//! empty table, so workspace builds and lints need no web toolchain. A
//! release sets `CATCHLIGHT_EDITOR_REQUIRE_UI=1`, which turns a missing page
//! into a build failure rather than a binary that serves nothing.

// A build script reports failure by panicking; there is no other channel.
#![allow(clippy::panic)]

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-env-changed=CATCHLIGHT_EDITOR_UI");
    println!("cargo::rerun-if-env-changed=CATCHLIGHT_EDITOR_REQUIRE_UI");
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let dir = std::env::var_os("CATCHLIGHT_EDITOR_UI")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("ui"));
    println!("cargo::rerun-if-changed={}", dir.display());

    let mut files = Vec::new();
    if dir.join("index.html").is_file() {
        collect(&dir, &dir, &mut files);
    } else if std::env::var_os("CATCHLIGHT_EDITOR_REQUIRE_UI").is_some_and(|v| v != "0") {
        panic!(
            "{} has no index.html; run `cargo xtask bundle-ui` first",
            dir.display()
        );
    }
    files.sort();

    let mut table = String::from("pub static UI: &[(&str, &[u8])] = &[\n");
    for (relative, absolute) in &files {
        table.push_str(&format!(
            "    ({relative:?}, include_bytes!({:?})),\n",
            absolute.display().to_string()
        ));
    }
    table.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("ui.rs");
    if let Err(err) = std::fs::write(&out, table) {
        panic!("writing {}: {err}", out.display());
    }
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => panic!("reading {}: {err}", dir.display()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            let relative = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
}
