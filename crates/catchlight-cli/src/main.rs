//! `catchlight-cli` — the command line over [`catchlight_cli`].
//!
//! Argument parsing, printing and exit statuses only; every operation lives in
//! the library beside this file, which is what the integration tests drive.
//! See the crate's module doc for the line this crate holds: nothing here
//! reaches the editor server, its protocol, or a client of either.

use std::path::PathBuf;

use catchlight_cli::{
    diff, extension, fragment, isolate, patch, poses, render, texture, Error, EXIT_DIFFERS,
    EXIT_ERROR,
};
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
  starts with none of `.`, `/` or `-` — so an id is always safe to pass as a
  bare argument.

  Ids are case-sensitive, and the `/` a generated id carries (`head/part-3f9a`)
  is a reading aid, not a path.

Exit status:
  0  the command succeeded; for `diff`, the two files are identical
  1  `diff` only: the two files differ
  2  the command failed";

#[derive(Parser)]
#[command(
    name = "catchlight-cli",
    about = "The command line over a .clm model file",
    long_about = "The command line over a .clm model file.\n\nThe file operations edit the \
                  file's structure section directly. Texture bytes are carried through as they \
                  are and no image is ever decoded, so these are cheap on a model of any size. \
                  `render` is the exception and the one command that needs a GPU: it draws the \
                  model and prints the render list it drew.",
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
    /// Cut subtrees out as an addon.
    Extract {
        /// The .clm to cut from.
        file: PathBuf,
        /// The node ids to cut, with their subtrees.
        #[arg(required = true)]
        ids: Vec<String>,
        /// Where to write the addon.
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Install an addon into a base model.
    Merge {
        /// The base model.
        base: PathBuf,
        /// The addon, which must be a fragment.
        addon: PathBuf,
        /// Where to write the merged model.
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Print what an addon needs from a base model, one requirement per line
    /// as kind, id, slot, field, owner, separated by tabs.
    Requirements {
        /// The addon .clm.
        addon: PathBuf,
        /// Print a JSON array instead.
        #[arg(long)]
        json: bool,
    },
    /// Compare two model files by id.
    Diff { a: PathBuf, b: PathBuf },
    /// Read or write the vendor annotations a file carries.
    #[command(subcommand)]
    Extension(ExtensionCmd),
    /// Dump every key pose of the rig to a CBOR file: the rest pose in full
    /// and every other as a sparse diff against it.
    Poses {
        /// The .clm to read.
        file: PathBuf,
        /// Where to write the CBOR.
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Draw only the parts named by --keep, over transparency, into a
    /// straight-alpha PNG framed to a world rect.
    Isolate {
        /// The .clm to read.
        file: PathBuf,
        /// The node ids of the parts to draw. Everything else is hidden.
        #[arg(long, required = true, value_delimiter = ',')]
        keep: Vec<String>,
        /// Node ids whose masks come off the kept parts, so a clipped part
        /// renders as its full art. Naming a node that masks nothing is fine.
        #[arg(long, value_delimiter = ',')]
        strip_masks: Vec<String>,
        /// The world-space window, y-up, x0,y0 its minimum corner. A
        /// minimum corner is usually negative, so hyphens are values here.
        #[arg(long, value_name = "x0,y0,w,h", allow_hyphen_values = true)]
        rect: String,
        /// Pixels per world unit.
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Pose one param, repeatable. Every other param sits at its default.
        /// A negative value is a value, not a flag.
        #[arg(long, value_name = "param=value", allow_hyphen_values = true)]
        set: Vec<String>,
        /// Where to write the PNG.
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Render the model at rest to a PNG and print the render list it drew.
    Render {
        /// The .clm to render.
        file: PathBuf,
        /// Where to write the PNG.
        out: PathBuf,
        /// Width in pixels.
        #[arg(default_value_t = render::DEFAULT_WIDTH)]
        width: u32,
        /// Height in pixels.
        #[arg(default_value_t = render::DEFAULT_HEIGHT)]
        height: u32,
        /// How many world units tall the camera frames, centred on the origin.
        #[arg(default_value_t = render::DEFAULT_CAMERA_HEIGHT)]
        camera_height: f32,
    },
}

/// A key is `vendor.name`: the id charset with a required dot. `catchlight.`
/// is the format's own prefix and is refused here.
#[derive(Subcommand)]
enum ExtensionCmd {
    /// List every extension as key, kind, and size for a byte value.
    List {
        /// The .clm to read.
        file: PathBuf,
    },
    /// Print one extension's JSON, or write a byte value to --out.
    Get {
        /// The .clm to read.
        file: PathBuf,
        /// The extension key.
        key: String,
        /// Write the value here instead of printing it. Required for bytes.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
    /// Set one extension, replacing whatever was under that key.
    Set {
        /// The .clm to edit, in place.
        file: PathBuf,
        /// The extension key.
        key: String,
        /// A JSON value, as text.
        #[arg(long, conflicts_with = "bytes", required_unless_present = "bytes")]
        json: Option<String>,
        /// A file whose contents become the value.
        #[arg(long)]
        bytes: Option<PathBuf>,
    },
    /// Drop one extension.
    Delete {
        /// The .clm to edit, in place.
        file: PathBuf,
        /// The extension key.
        key: String,
    },
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("catchlight-cli: {error}");
            EXIT_ERROR
        }
    };
    std::process::exit(code);
}

fn run_extension(cmd: ExtensionCmd) -> Result<i32, Error> {
    match cmd {
        ExtensionCmd::List { file } => {
            for listed in extension::list(&file)? {
                println!("{listed}");
            }
        }
        ExtensionCmd::Get { file, key, out } => {
            match extension::get(&file, &key, out.as_deref())? {
                // A byte value is written, never printed; so is a JSON value once
                // `--out` names somewhere to put it.
                extension::Got::Json(text) if !text.is_empty() => println!("{text}"),
                extension::Got::Json(_) | extension::Got::Bytes(_) => {}
            }
        }
        ExtensionCmd::Set {
            file,
            key,
            json,
            bytes,
        } => {
            let changed = match (json, bytes) {
                (Some(text), None) => extension::set_json(&file, &key, &text)?,
                (None, Some(source)) => extension::set_bytes(&file, &key, &source)?,
                // clap's `required_unless_present` and `conflicts_with` leave
                // only these two, so this arm is unreachable in practice.
                _ => {
                    return Err(Error::BadValue {
                        field: "--json/--bytes".into(),
                        expected: "exactly one of them".into(),
                        value: String::new(),
                    })
                }
            };
            println!("{changed}");
        }
        ExtensionCmd::Delete { file, key } => {
            println!("{}", extension::delete(&file, &key)?);
        }
    }
    Ok(0)
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
        Cmd::Extract { file, ids, out } => {
            let extracted = fragment::extract(&file, &ids, &out)?;
            println!("{extracted}");
        }
        Cmd::Merge { base, addon, out } => {
            let merged = fragment::merge(&base, &addon, &out)?;
            println!("{merged}");
        }
        Cmd::Requirements { addon, json } => {
            let requirements = fragment::requirements(&addon)?;
            if json {
                println!("{}", fragment::render_json(&requirements));
            } else {
                for requirement in &requirements {
                    println!("{}", fragment::render_line(requirement));
                }
            }
        }
        Cmd::Poses { file, out } => {
            let written = poses::run(&file, &out)?;
            println!("{written}");
        }
        Cmd::Isolate {
            file,
            keep,
            strip_masks,
            rect,
            scale,
            set,
            out,
        } => {
            let request = isolate::Request {
                keep,
                strip_masks,
                rect: isolate::Rect::parse(&rect)?,
                scale,
                set,
            };
            let isolated = isolate::run(&file, &out, &request)?;
            println!("{isolated}");
        }
        Cmd::Render {
            file,
            out,
            width,
            height,
            camera_height,
        } => {
            let rendered = render::run(&file, &out, width, height, camera_height)?;
            println!("{rendered}");
        }
        Cmd::Extension(cmd) => return run_extension(cmd),
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
