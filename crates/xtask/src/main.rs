#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Workspace build-automation tasks. Invoked as `cargo xtask <cmd>`.
//!
//! Commands:
//!   cargo xtask import <model.inx|.inp> [-o <model.clp>]

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("import") => import(&args[1..]),
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
    eprintln!("  import <model.inx|.inp> [-o <model.clp>]");
    eprintln!("      One-time convert an INX/INP rig to catchlight's editable .clp");
    eprintln!("      (default output: input path with a .clp extension).");
}

fn import(args: &[String]) -> Result<()> {
    use catchlight_core::formats::{clp, InxModel};
    use catchlight_core::importer::from_inx_model_to_clp;

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
        input.ok_or_else(|| anyhow!("usage: cargo xtask import <model.inx> [-o <out.clp>]"))?;
    let output = output.unwrap_or_else(|| input.with_extension("clp"));

    let bytes = std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    let model = match input.extension().and_then(|e| e.to_str()) {
        Some("inp") => catchlight_core::formats::parse_inp(std::io::Cursor::new(&bytes))
            .context("parsing .inp")?,
        _ => InxModel::parse(std::io::Cursor::new(&bytes)).context("parsing .inx")?,
    };
    let file = from_inx_model_to_clp(&model).context("inx -> clp")?;
    let encoded = clp::encode(&file.doc, &file.textures).context("encoding .clp")?;
    std::fs::write(&output, &encoded).with_context(|| format!("writing {}", output.display()))?;

    eprintln!(
        "imported {} -> {} ({} nodes, {} textures, {:.2} MB)",
        input.display(),
        output.display(),
        file.doc.nodes.len(),
        file.textures.len(),
        encoded.len() as f64 / 1e6,
    );
    Ok(())
}
