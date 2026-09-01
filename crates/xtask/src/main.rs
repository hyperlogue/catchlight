#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Workspace build-automation tasks. Invoked as `cargo xtask <cmd>`.
//!
//! Commands:
//!   cargo xtask import <model.inx|.inp> [-o <model.clm>]
//!   cargo xtask gen-fixture <name>
//!   cargo xtask wasm [--debug]
//!   cargo xtask ts [--check]

mod fixtures;
mod ts;
mod wasm;

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("import") => import(&args[1..]),
        Some("gen-fixture") => gen_fixture(&args[1..]),
        Some("wasm") => wasm::run(&args[1..]),
        Some("ts") => ts::run(&args[1..]),
        _ => {
            print_usage();
            bail!("unknown command");
        }
    }
}

fn print_usage() {
    eprintln!("usage: cargo xtask <command>");
    eprintln!();
    eprintln!("commands:");
    eprintln!("  import <model.inx|.inp> [-o <model.clm>]");
    eprintln!("      One-time convert an INX/INP model to catchlight's editable .clm");
    eprintln!("      (default output: input path with a .clm extension).");
    eprintln!("  gen-fixture <name>");
    eprintln!("      Rebuild a hand-authored test model into tests/models/<name>.clm.");
    eprintln!(
        "      names: {}",
        fixtures::names().collect::<Vec<_>>().join(", ")
    );
    eprintln!("  wasm [--debug]");
    eprintln!("      Build @catchlight/wasm into packages/wasm/ (generated, not committed).");
    eprintln!("  ts [--check]");
    eprintln!("      Generate packages/core/src/protocol.gen.ts from the wire types");
    eprintln!("      (committed). --check fails instead of writing, which is what CI runs.");
}

/// The workspace root, from this crate's manifest directory. Every task writes
/// relative to it, so `cargo xtask` behaves the same from any subdirectory.
fn workspace_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .nth(2)
        .map(std::path::Path::to_path_buf)
        .context("locating the workspace root from the xtask manifest")
}

fn gen_fixture(args: &[String]) -> Result<()> {
    let [name] = args else {
        bail!("usage: cargo xtask gen-fixture <name>");
    };
    let written = fixtures::generate(name)?;
    eprintln!("wrote {}", written.display());
    Ok(())
}

fn import(args: &[String]) -> Result<()> {
    use catchlight_import_inochi2d::import_inx_bytes;

    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                output = Some(PathBuf::from(
                    args.get(i + 1).ok_or_else(|| anyhow!("-o needs a path"))?,
                ));
                i += 2;
            }
            other => {
                if input.is_some() {
                    bail!("unexpected argument: {other}");
                }
                input = Some(PathBuf::from(other));
                i += 1;
            }
        }
    }
    let input =
        input.ok_or_else(|| anyhow!("usage: cargo xtask import <model.inx> [-o <out.clm>]"))?;
    let output = output.unwrap_or_else(|| input.with_extension("clm"));

    let bytes = std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    // `.inx` and `.inp` share one container; the extension is only a label.
    let imported =
        import_inx_bytes(&bytes).with_context(|| format!("importing {}", input.display()))?;
    let encoded = imported.to_clm_bytes().context("writing .clm")?;
    std::fs::write(&output, &encoded).with_context(|| format!("writing {}", output.display()))?;

    eprintln!(
        "imported {} -> {} ({} nodes, {} textures, {:.2} MB)",
        input.display(),
        output.display(),
        imported.node_count(),
        imported.texture_ids().len(),
        encoded.len() as f64 / 1e6,
    );
    Ok(())
}
