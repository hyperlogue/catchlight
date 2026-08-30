//! `catchlight-clm` — the command line over [`catchlight_clm`].
//!
//! Argument parsing, printing and exit statuses only; every operation lives in
//! the library beside this file, which is what the integration tests drive.
//! See the crate's module doc for why these commands are their own binary
//! rather than subcommands of `catchlight-editor-cli`.

use std::path::PathBuf;

use catchlight_clm::{diff, patch, texture, Error, EXIT_DIFFERS, EXIT_ERROR};
use catchlight_core::formats::clm::TextureAlpha;
use clap::{Parser, Subcommand, ValueEnum};

/// `--alpha`: what the replacement bytes mean by their alpha channel. The
/// bytes cannot say, so only the person doing the swap can.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AlphaArg {
    Premultiplied,
    Straight,
}

impl AlphaArg {
    fn alpha(self) -> TextureAlpha {
        match self {
            Self::Premultiplied => TextureAlpha::PremultipliedSrgb,
            Self::Straight => TextureAlpha::Straight,
        }
    }
}

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
    /// Swap a texture's source bytes for another image's.
    ReplaceTexture {
        /// The .clm to edit.
        file: PathBuf,
        /// The texture id whose bytes to replace.
        tex_id: String,
        /// The image file. Its encoding comes from its signature, or its
        /// extension when the bytes carry none.
        image: PathBuf,
        /// The alpha convention of the new bytes. Defaults to the slot's.
        #[arg(long, value_enum)]
        alpha: Option<AlphaArg>,
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
        Cmd::ReplaceTexture {
            file,
            tex_id,
            image,
            alpha,
            out,
        } => {
            let replaced = texture::run(
                &file,
                &tex_id,
                &image,
                alpha.map(AlphaArg::alpha),
                out.as_deref(),
            )?;
            println!("{replaced}");
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
