//! Builds the generated, uncommitted `@catchlight/wasm` package.
//!
//! The `wasm-bindgen` CLI must match the crate version exactly; a mismatch
//! otherwise surfaces only in the browser as an unresolved import.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

const OUT_DIR: &str = "packages/wasm";
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

    let root = crate::workspace_root()?;
    check_versions(&root)?;

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut build = Command::new(&cargo);
    build
        .current_dir(&root)
        .args(["build", "-p", CRATE, "--target", TARGET]);
    if profile == "release" {
        build.arg("--release");
    }
    run_to_completion(&mut build, "cargo build")?;

    let wasm = target_directory(&cargo, &root)?
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

/// Where cargo puts build output, honouring `CARGO_TARGET_DIR` and
/// `build.target-dir`.
fn target_directory(cargo: &OsString, root: &Path) -> Result<PathBuf> {
    let out = Command::new(cargo)
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .context("running `cargo metadata`")?;
    if !out.status.success() {
        bail!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    target_directory_of(&out.stdout)
}

fn target_directory_of(metadata: &[u8]) -> Result<PathBuf> {
    let metadata: serde_json::Value =
        serde_json::from_slice(metadata).context("parsing `cargo metadata` output")?;
    metadata["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .context("`cargo metadata` names no target_directory")
}

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

pub(crate) fn run_to_completion(cmd: &mut Command, what: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("spawning {what}"))?;
    if !status.success() {
        bail!("{what} failed with {status}");
    }
    Ok(())
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
    fn the_target_directory_comes_from_cargo_metadata() {
        let metadata = br#"{"packages":[],"target_directory":"/elsewhere/target","version":1}"#;
        assert_eq!(
            target_directory_of(metadata).unwrap(),
            Path::new("/elsewhere/target")
        );
        assert!(target_directory_of(b"{}").is_err());
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
