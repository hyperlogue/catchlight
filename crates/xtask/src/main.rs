#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Workspace build-automation tasks. Invoked as `cargo xtask <cmd>`.
//!
//! Commands:
//!   cargo xtask gen-fixture <name>
//!   cargo xtask build-wasm [--debug]
//!   cargo xtask generate [typescript|python]

mod fixtures;
mod generate;
mod wasm;

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("gen-fixture") => gen_fixture(&args[1..]),
        Some("build-wasm") => wasm::run(&args[1..]),
        Some("generate") => generate::run(&args[1..]),
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
    eprintln!("  gen-fixture <name>");
    eprintln!("      Rebuild a hand-authored test model into tests/models/<name>.clm.");
    eprintln!(
        "      names: {}",
        fixtures::names().collect::<Vec<_>>().join(", ")
    );
    eprintln!("  build-wasm [--debug]");
    eprintln!("      Build @catchlight/wasm into packages/wasm/ (generated, not committed).");
    eprintln!("  generate [typescript|python]");
    eprintln!("      Write the editor protocol out in the languages that speak it:");
    eprintln!("      packages/core/src/protocol.gen.ts and");
    eprintln!("      python/catchlight/protocol_gen.py. Both are committed, and");
    eprintln!("      `cargo test -p xtask` fails if either is stale. No target");
    eprintln!("      writes both.");
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
