//! `catchlight-clm` — the command line over [`catchlight_clm`].
//!
//! Argument parsing, printing and exit statuses only; every operation lives in
//! the library beside this file, which is what the integration tests drive.
//! See the crate's module doc for why these commands are their own binary
//! rather than subcommands of `catchlight-editor-cli`.

use std::path::PathBuf;

use catchlight_clm::{diff, patch, Error, EXIT_DIFFERS, EXIT_ERROR};
use clap::{Parser, Subcommand};

const AFTER_HELP: &str = "\
Ids:
  A node, param or texture id is a non-empty string of [A-Za-z0-9_./-] that
  starts with neither `.` nor `/`. It may start with `-`, so every command
  takes `--` to end its options:

      catchlight-clm patch model.clm -- -odd-id z_order 1

  Ids are case-sensitive, and the `/` a generated id carries (`head/part-3f9a`)
  is a reading aid, not a path.

Exit status:
  0  the command succeeded; for `diff`, the two files are identical
  1  `diff` only: the two files differ
  2  the command failed";

#[derive(Parser)]
#[command(
    name = "catchlight-clm",
    about = "File-level operations on a .clm model file",
    long_about = "File-level operations on a .clm model file.\n\nEvery command edits the file's \
                  structure section directly. Texture bytes are carried through as they are and \
                  no image is ever decoded, so these are cheap on a model of any size.",
    after_help = AFTER_HELP,
    after_long_help = AFTER_HELP
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Set one scalar field on one node or param.
    Patch {
        /// The .clm to edit.
        file: PathBuf,
        /// The node or param id.
        id: String,
        /// The field, e.g. z_order, enabled, name, translation.x, opacity, min.
        field: String,
        /// The new value.
        value: String,
        /// Resolve the id as a node (only needed when a param shares it).
        #[arg(long, conflicts_with = "param")]
        node: bool,
        /// Resolve the id as a param (only needed when a node shares it).
        #[arg(long)]
        param: bool,
        /// Write here instead of over the input.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Compare two model files by id.
    Diff { a: PathBuf, b: PathBuf },
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("catchlight-clm: {error}");
            EXIT_ERROR
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32, Error> {
    match Cli::parse().cmd {
        Cmd::Patch {
            file,
            id,
            field,
            value,
            node,
            param,
            out,
        } => {
            let want = match (node, param) {
                (true, false) => Some(patch::Kind::Node),
                (false, true) => Some(patch::Kind::Param),
                _ => None,
            };
            let change = patch::run(&file, &id, &field, &value, want, out.as_deref())?;
            if change.changed() {
                println!("{change}");
            } else {
                println!("{change} (unchanged)");
            }
        }
        Cmd::Diff { a, b } => {
            let lines = diff::run(&a, &b)?;
            for line in &lines {
                println!("{line}");
            }
            if !lines.is_empty() {
                return Ok(EXIT_DIFFERS);
            }
        }
    }
    Ok(0)
}
