//! `catchlight-cli` — argument parsing, printing and exit statuses over [`catchlight_cli`].

use std::path::PathBuf;

use catchlight_cli::{
    diff, expression, extension, fragment, humanoid, patch, poses, render, storage, texture, Error,
    EXIT_DIFFERS, EXIT_ERROR,
};
use catchlight_core::formats::clm::TextureAlpha;
use catchlight_extension_animations::Animations;
use catchlight_extension_expressions::{Expressions, Group};
use catchlight_extension_humanoid::Humanoid;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

/// The bytes cannot say what their alpha means, so the user must.
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
    /// Report encoded storage by section, texture, node and binding as JSON.
    /// No images are decoded. Element sizes use canonical serialization;
    /// canonical_matches_input says whether those bytes match the input exactly.
    Storage { file: PathBuf },
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
    /// Map the standard humanoid controls onto the model's params.
    #[command(subcommand)]
    Humanoid(HumanoidCmd),
    /// Name expressions, such as emotions, by the controls and params they
    /// set.
    #[command(subcommand)]
    Expression(ExpressionCmd),
    /// Name visemes, the mouth shapes of speech, by the controls and params
    /// they set. Visemes play over expressions.
    #[command(subcommand)]
    Viseme(VisemeCmd),
    /// Render to a PNG, inspect a render plan, or discover its JSON schema.
    Render(render::args::RenderArgs),
    /// Inspect overall and per-Part evaluated world bounds without a GPU.
    Bounds(render::args::BoundsArgs),
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
    /// Set one extension, replacing whatever was under that key. With
    /// --version it is maintained: --json or --bytes is its payload, and the
    /// core keeps its --refs current as the model's Ids change.
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
        /// Store a maintained value at this version of its format.
        #[arg(long)]
        version: Option<u32>,
        /// The maintained value's reference table, as a JSON array:
        /// {"param": "id"}, {"node": "id"}, {"texture": "id"} or
        /// {"slot": ["node", "slot"]}. A reference the model lacks is kept
        /// and warned about.
        #[arg(long, requires = "version")]
        refs: Option<String>,
    },
    /// Drop one extension.
    Delete {
        /// The .clm to edit, in place.
        file: PathBuf,
        /// The extension key.
        key: String,
    },
}

/// The control mappings of `catchlight.humanoid`. Each edit prints its
/// result as JSON, then the extension's warnings for the edited model on
/// stderr; warnings never fail the command.
#[derive(Subcommand)]
enum HumanoidCmd {
    /// Print the standard controls as JSON: name, and whether one-sided
    /// (0..1 rather than -1..1).
    Controls,
    /// Print the model's mappings, the standard controls it leaves unmapped,
    /// and the extension's warnings for the model, as JSON.
    List {
        /// The .clm to read.
        file: PathBuf,
    },
    /// Map one control onto one param, replacing its mapping if it has one.
    Map {
        /// The .clm to edit, in place.
        file: PathBuf,
        /// The control name, from `humanoid controls`.
        control: String,
        /// The param id.
        param: String,
        /// The param values the control's -1, 0 and +1 land on. Default: the
        /// param's min,default,max for a two-sided control, and
        /// default,default,max for a one-sided one.
        #[arg(long, allow_hyphen_values = true, value_name = "low,mid,high")]
        stops: Option<String>,
        /// Keep a control name outside the standard list.
        #[arg(long)]
        nonstandard: bool,
    },
    /// Remove one control's mapping. Removing the last drops the extension.
    Unmap {
        /// The .clm to edit, in place.
        file: PathBuf,
        /// The control name.
        control: String,
    },
}

/// The named expressions of `catchlight.expressions`. Each edit prints its
/// result as JSON, then the warnings about expressions on stderr; warnings
/// never fail the command.
#[derive(Subcommand)]
enum ExpressionCmd {
    /// Print the standard expression names, the emotions, as JSON.
    Names,
    /// Print every expression with its settings, and the warnings about
    /// expressions, as JSON.
    List {
        /// The .clm to read.
        file: PathBuf,
    },
    /// Create or replace one expression from its settings, kept in the order
    /// given.
    Set(SetArgs),
    /// Remove one expression. Removing the last expression or viseme drops
    /// the extension.
    Remove(RemoveArgs),
}

/// The visemes of `catchlight.expressions`, kept beside its expressions under
/// their own names. Each edit prints its result as JSON, then the warnings
/// about visemes on stderr; warnings never fail the command.
#[derive(Subcommand)]
enum VisemeCmd {
    /// Print the standard viseme names as JSON.
    Names,
    /// Print every viseme with its settings, and the warnings about visemes,
    /// as JSON.
    List {
        /// The .clm to read.
        file: PathBuf,
    },
    /// Create or replace one viseme from its settings, kept in the order
    /// given.
    Set(SetArgs),
    /// Remove one viseme. Removing the last expression or viseme drops the
    /// extension.
    Remove(RemoveArgs),
    /// Add the standard visemes with their default control settings, after
    /// the existing visemes, and report which were added and skipped.
    AddStandard {
        /// The .clm to edit, in place.
        file: PathBuf,
        /// Replace an existing viseme of a standard name rather than skip it.
        #[arg(long)]
        replace: bool,
    },
}

#[derive(clap::Args)]
struct SetArgs {
    /// The .clm to edit, in place.
    file: PathBuf,
    /// The name, standard or not.
    name: String,
    /// A humanoid control and its value, within the control's range.
    #[arg(long, allow_hyphen_values = true, value_name = "name=value")]
    control: Vec<String>,
    /// A param id and its raw value.
    #[arg(long, allow_hyphen_values = true, value_name = "id=value")]
    param: Vec<String>,
    /// Keep a control name outside the standard list, valued in -1..1.
    #[arg(long)]
    nonstandard: bool,
}

#[derive(clap::Args)]
struct RemoveArgs {
    /// The .clm to edit, in place.
    file: PathBuf,
    /// The name.
    name: String,
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
                extension::Got::Json(text) if !text.is_empty() => println!("{text}"),
                extension::Got::Json(_) | extension::Got::Bytes(_) => {}
            }
        }
        ExtensionCmd::Set {
            file,
            key,
            json,
            bytes,
            version,
            refs,
        } => {
            let source = match (&json, &bytes) {
                (Some(text), None) => extension::Source::Json(text),
                (None, Some(source)) => extension::Source::Bytes(source),
                // Unreachable: clap requires exactly one of the two.
                _ => {
                    return Err(Error::BadValue {
                        field: "--json/--bytes".into(),
                        expected: "exactly one of them".into(),
                        value: String::new(),
                    })
                }
            };
            let changed = match (version, source) {
                (Some(version), source) => {
                    let refs = refs.as_deref().unwrap_or("[]");
                    let (changed, warnings) =
                        extension::set_maintained(&file, &key, version, refs, source)?;
                    for warning in humanoid::warning_lines(warnings) {
                        eprintln!("catchlight-cli: {warning}");
                    }
                    changed
                }
                (None, extension::Source::Json(text)) => extension::set_json(&file, &key, text)?,
                (None, extension::Source::Bytes(source)) => {
                    extension::set_bytes(&file, &key, source)?
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

fn print_json(value: &impl serde::Serialize) -> Result<(), Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_edit<T: serde::Serialize>(edited: humanoid::Edited<T>) -> Result<(), Error> {
    print_json(&edited.result)?;
    for warning in &edited.warnings {
        eprintln!("catchlight-cli: {warning}");
    }
    Ok(())
}

fn run_humanoid(cmd: HumanoidCmd) -> Result<(), Error> {
    match cmd {
        HumanoidCmd::Controls => print_json(&humanoid::controls()),
        HumanoidCmd::List { file } => print_json(&humanoid::list(&file)?),
        HumanoidCmd::Map {
            file,
            control,
            param,
            stops,
            nonstandard,
        } => {
            let stops = stops.as_deref().map(humanoid::parse_stops).transpose()?;
            print_edit(humanoid::map(&file, &control, &param, stops, nonstandard)?)
        }
        HumanoidCmd::Unmap { file, control } => print_edit(humanoid::unmap(&file, &control)?),
    }
}

fn run_expression(cmd: ExpressionCmd, matches: Option<&ArgMatches>) -> Result<(), Error> {
    let group = Group::Expression;
    match cmd {
        ExpressionCmd::Names => print_json(&expression::names(group)),
        ExpressionCmd::List { file } => print_json(&expression::list(&file, group)?),
        ExpressionCmd::Set(args) => set_member(group, args, matches),
        ExpressionCmd::Remove(RemoveArgs { file, name }) => {
            print_edit(expression::remove(&file, group, &name)?)
        }
    }
}

fn run_viseme(cmd: VisemeCmd, matches: Option<&ArgMatches>) -> Result<(), Error> {
    let group = Group::Viseme;
    match cmd {
        VisemeCmd::Names => print_json(&expression::names(group)),
        VisemeCmd::List { file } => print_json(&expression::list(&file, group)?),
        VisemeCmd::Set(args) => set_member(group, args, matches),
        VisemeCmd::Remove(RemoveArgs { file, name }) => {
            print_edit(expression::remove(&file, group, &name)?)
        }
        VisemeCmd::AddStandard { file, replace } => {
            print_edit(expression::add_standard_visemes(&file, replace)?)
        }
    }
}

/// `matches` are the `set` arguments, which say where each `--control` and
/// `--param` stood so the settings keep their given order.
fn set_member(group: Group, args: SetArgs, matches: Option<&ArgMatches>) -> Result<(), Error> {
    let SetArgs {
        file,
        name,
        control,
        param,
        nonstandard,
    } = args;
    let at = |flag: &str| -> Vec<usize> {
        matches
            .and_then(|m| m.indices_of(flag))
            .map(Iterator::collect)
            .unwrap_or_default()
    };
    let (controls, params) = (at("control"), at("param"));
    let mut settings = Vec::with_capacity(control.len() + param.len());
    for (i, text) in control.iter().enumerate() {
        let (name, value) = humanoid::parse_assignment("--control", text)?;
        let index = controls.get(i).copied().unwrap_or(usize::MAX);
        settings.push((index, expression::SettingArg::Control(name, value)));
    }
    for (i, text) in param.iter().enumerate() {
        let (id, value) = humanoid::parse_assignment("--param", text)?;
        let index = params.get(i).copied().unwrap_or(usize::MAX);
        settings.push((index, expression::SettingArg::Param(id, value)));
    }
    settings.sort_by_key(|(index, _)| *index);
    let settings: Vec<_> = settings.into_iter().map(|(_, s)| s).collect();
    print_edit(expression::set(
        &file,
        group,
        &name,
        &settings,
        nonstandard,
    )?)
}

fn run() -> Result<i32, Error> {
    catchlight_core::extension::register::<Animations>()?;
    catchlight_core::extension::register::<Humanoid>()?;
    catchlight_core::extension::register::<Expressions>()?;
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    match cli.cmd {
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
        Cmd::Render(args) => args.run()?,
        Cmd::Bounds(args) => args.run()?,
        Cmd::Extension(cmd) => return run_extension(cmd),
        Cmd::Humanoid(cmd) => run_humanoid(cmd)?,
        Cmd::Expression(cmd) => {
            let set = matches
                .subcommand_matches("expression")
                .and_then(|m| m.subcommand_matches("set"));
            run_expression(cmd, set)?
        }
        Cmd::Viseme(cmd) => {
            let set = matches
                .subcommand_matches("viseme")
                .and_then(|m| m.subcommand_matches("set"));
            run_viseme(cmd, set)?
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
        Cmd::Storage { file } => {
            let report = storage::run(&file)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(0)
}
