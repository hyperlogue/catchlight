//! Builds the editor page and copies it into `catchlight-editor` for embedding.
//!
//! The copy is git-ignored but packaged: `cargo install` cannot run
//! wasm-bindgen or Vite, so a published crate must ship the built page.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

const APP: &str = "apps/local-editor";
const PACKAGE: &str = "catchlight-local-editor";
const OUT_DIR: &str = "crates/catchlight-editor/ui";

pub fn run(args: &[String]) -> Result<()> {
    let mut wasm_args = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--debug" | "--release" => wasm_args.push(arg.clone()),
            other => bail!("unexpected argument: {other}"),
        }
    }
    let root = crate::workspace_root()?;
    crate::wasm::run(&wasm_args)?;

    let mut install = Command::new("bun");
    install
        .current_dir(&root)
        .args(["install", "--frozen-lockfile"]);
    crate::wasm::run_to_completion(&mut install, "bun install")?;
    let mut build = Command::new("bun");
    build
        .current_dir(&root)
        .args(["run", "--filter", PACKAGE, "build"]);
    crate::wasm::run_to_completion(&mut build, "vite build")?;

    let dist = root.join(APP).join("dist");
    if !dist.join("index.html").exists() {
        bail!(
            "expected {} after the build",
            dist.join("index.html").display()
        );
    }
    let out = root.join(OUT_DIR);
    if out.exists() {
        std::fs::remove_dir_all(&out).with_context(|| format!("clearing {}", out.display()))?;
    }
    let (files, bytes) = copy_tree(&dist, &out)?;
    eprintln!(
        "wrote {} ({files} files, {:.1} MiB)",
        out.display(),
        bytes as f64 / 1048576.0
    );
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(usize, u64)> {
    std::fs::create_dir_all(to)?;
    let (mut files, mut bytes) = (0, 0);
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            let (f, b) = copy_tree(&entry.path(), &target)?;
            files += f;
            bytes += b;
        } else {
            bytes += std::fs::copy(entry.path(), &target)
                .with_context(|| format!("copying {}", entry.path().display()))?;
            files += 1;
        }
    }
    Ok((files, bytes))
}
