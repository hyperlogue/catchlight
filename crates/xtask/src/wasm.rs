//! Builds `@catchlight/wasm` — the generated half of the web editor's bottom
//! layer.
//!
//! The output package is **generated, never edited, and not committed**: it is
//! `catchlight-editor-wasm` compiled to wasm32 and run through `wasm-bindgen`,
//! plus the one `package.json` that makes the result resolvable from the bun
//! workspace. Anything hand-written belongs in the Rust crate or in
//! `@catchlight/core` above it.
//!
//! `wasm-bindgen` the CLI and `wasm-bindgen` the crate must be the same
//! version — the glue the CLI emits only loads against the runtime the crate
//! compiled in. The workspace pins the crate exactly and the dev shell pins the
//! CLI; this checks them against each other first, because the failure
//! otherwise surfaces in the browser as an unresolved import rather than here.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Where the generated package lands, relative to the workspace root.
const OUT_DIR: &str = "packages/wasm";
/// The base name of the emitted `.js` / `.wasm` / `.d.ts` set.
const OUT_NAME: &str = "catchlight";
const CRATE: &str = "catchlight-editor-wasm";
const TARGET: &str = "wasm32-unknown-unknown";

pub fn run(args: &[String]) -> Result<()> {
    let mut profile = "release";
    for arg in args {
        match arg.as_str() {
            "--debug" => profile = "debug",
            "--release" => profile = "release",
            other => bail!("unexpected argument: {other}"),
        }
    }

    let root = workspace_root()?;
    check_versions(&root)?;

    let mut build = Command::new("cargo");
    build
        .current_dir(&root)
        .args(["build", "-p", CRATE, "--target", TARGET]);
    if profile == "release" {
        build.arg("--release");
    }
    run_to_completion(&mut build, "cargo build")?;

    let wasm = root
        .join("target")
        .join(TARGET)
        .join(profile)
        .join(CRATE.replace('-', "_"))
        .with_extension("wasm");
    if !wasm.exists() {
        bail!("expected {} to exist after the build", wasm.display());
    }

    let out = root.join(OUT_DIR);
    std::fs::create_dir_all(&out)?;
    // `--target bundler` is what Vite consumes: ESM with a `.wasm` import the
    // bundler resolves, rather than a fetch the page has to sequence itself.
    run_to_completion(
        Command::new("wasm-bindgen")
            .current_dir(&root)
            .arg("--target")
            .arg("bundler")
            .arg("--out-dir")
            .arg(&out)
            .arg("--out-name")
            .arg(OUT_NAME)
            .arg(&wasm),
        "wasm-bindgen",
    )?;

    std::fs::write(out.join("package.json"), package_json())?;

    let bytes = std::fs::metadata(out.join(format!("{OUT_NAME}_bg.wasm")))?.len();
    eprintln!(
        "wrote {OUT_DIR}/ ({profile}, {:.1} MiB wasm)",
        bytes as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}

/// The `wasm-bindgen` crate version cargo actually resolved, against the CLI on
/// PATH. A mismatch is the single most common way this build produces a broken
/// package, and it is silent until the browser tries to load it.
fn check_versions(root: &Path) -> Result<()> {
    let lock = std::fs::read_to_string(root.join("Cargo.lock"))
        .context("reading Cargo.lock to check the wasm-bindgen version")?;
    let Some(locked) = locked_version(&lock, "wasm-bindgen") else {
        bail!("Cargo.lock names no wasm-bindgen; is {CRATE} still in the workspace?");
    };

    let out = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .context("running `wasm-bindgen --version` — is it on PATH? (nix develop)")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let cli = stdout.split_whitespace().nth(1).unwrap_or("").trim();

    if cli != locked {
        bail!(
            "wasm-bindgen CLI is {cli}, but Cargo.lock pins the crate at {locked}.\n\
             The generated glue only loads against its own runtime. Move the \
             `wasm-bindgen` pin in Cargo.toml to {cli}, or update the dev \
             shell's nixpkgs so the CLI is {locked}."
        );
    }
    Ok(())
}

/// The `version` of the `[[package]]` block named `name`.
fn locked_version(lock: &str, name: &str) -> Option<String> {
    let mut in_block = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_block = false;
        } else if line == format!("name = \"{name}\"") {
            in_block = true;
        } else if in_block {
            if let Some(rest) = line.strip_prefix("version = ") {
                return Some(rest.trim_matches('"').to_string());
            }
        }
    }
    None
}

fn package_json() -> String {
    // Generated: the workspace resolves `@catchlight/wasm` to this directory,
    // and nothing else reads these fields.
    format!(
        r#"{{
  "name": "@catchlight/wasm",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "main": "{OUT_NAME}.js",
  "types": "{OUT_NAME}.d.ts",
  "sideEffects": [
    "./{OUT_NAME}.js",
    "./snippets/*"
  ]
}}
"#
    )
}

fn run_to_completion(cmd: &mut Command, what: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("spawning {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
}

/// The workspace root, from this crate's manifest directory.
fn workspace_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .context("locating the workspace root from the xtask manifest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_version_reads_the_block_it_names() {
        let lock = r#"
[[package]]
name = "serde"
version = "1.0.228"

[[package]]
name = "wasm-bindgen"
version = "0.2.121"
dependencies = []
"#;
        assert_eq!(
            locked_version(lock, "wasm-bindgen").as_deref(),
            Some("0.2.121")
        );
        assert_eq!(locked_version(lock, "serde").as_deref(), Some("1.0.228"));
        assert_eq!(locked_version(lock, "nope"), None);
    }

    #[test]
    fn locked_version_does_not_leak_across_blocks() {
        // A dependency list mentioning the name must not be read as its block.
        let lock = r#"
[[package]]
name = "wasm-bindgen-macro"
version = "9.9.9"

[[package]]
name = "wasm-bindgen"
version = "0.2.121"
"#;
        assert_eq!(
            locked_version(lock, "wasm-bindgen").as_deref(),
            Some("0.2.121")
        );
    }
}
