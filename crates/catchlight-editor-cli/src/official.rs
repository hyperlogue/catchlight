//! `humanoid`, `expression`, `viseme` and `clip`: the official extensions'
//! typed commands, named and printed as the file CLI's are. Each edit prints its
//! result as JSON, then the edited extension's warnings on stderr; warnings
//! never fail the command.

use clap::ArgMatches;
use serde::ser::{Serialize, SerializeMap, Serializer};

use super::*;

const HUMANOID: &str = "catchlight.humanoid";
const EXPRESSIONS: &str = "catchlight.expressions";
const ANIMATIONS: &str = "catchlight.animations";

#[derive(Subcommand)]
pub enum HumanoidCmd {
    /// Print the standard controls, and whether each is one-sided, as JSON.
    Controls,
    /// Print the control mappings, the standard controls left unmapped, and
    /// the extension's warnings, as JSON.
    List,
    /// Map a control onto a param, replacing its mapping.
    Map {
        /// The control name, from `humanoid controls`.
        control: String,
        /// The param Id.
        param: ParamId,
        /// The param values at control -1, 0 and +1, as `low,mid,high`.
        /// Without it a two-sided control spans the param's min, default and
        /// max, and a one-sided control rests at the default and reaches the
        /// end of the range farther from it.
        #[arg(long, value_parser = parse_floats::<3>)]
        stops: Option<[f32; 3]>,
        /// Keep a control name outside the standard list.
        #[arg(long)]
        nonstandard: bool,
    },
    /// Remove one control's mapping. Removing the last drops the extension.
    Unmap { control: String },
}

#[derive(clap::Args)]
pub struct SetArgs {
    /// The name, such as a standard one from `names`.
    name: String,
    /// A humanoid control and its value, within the control's range, as
    /// `name=value` (repeatable).
    #[arg(long)]
    control: Vec<String>,
    /// One of the model's params and its raw value, as `id=value`
    /// (repeatable).
    #[arg(long)]
    param: Vec<String>,
    /// Keep a control name outside the standard list.
    #[arg(long)]
    nonstandard: bool,
}

#[derive(Subcommand)]
pub enum ExpressionCmd {
    /// Print the standard expression names, the emotions, as JSON.
    Names,
    /// Print every expression with its settings, and the extension's
    /// warnings, as JSON.
    List,
    /// Create or replace one expression from its settings, kept in the order
    /// given.
    Set(SetArgs),
    /// Remove one expression. Removing the last expression or viseme drops
    /// the extension.
    Remove { name: String },
}

#[derive(Subcommand)]
pub enum VisemeCmd {
    /// Print the standard viseme names as JSON.
    Names,
    /// Print every viseme with its settings, and the extension's warnings,
    /// as JSON.
    List,
    /// Create or replace one viseme from its settings, kept in the order
    /// given.
    Set(SetArgs),
    /// Remove one viseme. Removing the last expression or viseme drops the
    /// extension.
    Remove { name: String },
    /// Add the standard visemes with their default settings after the
    /// existing ones, and report which were added, replaced and skipped.
    AddStandard {
        /// Replace an existing viseme of a standard name rather than skip it.
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Subcommand)]
pub enum ClipCmd {
    /// Print every clip with its timing and what each lane drives, and the
    /// extension's warnings, as JSON.
    List,
}

/// A read's reply as JSON text, given the warnings it asked for; `None` for
/// a reply of another kind.
type Show = fn(&ResponseBody, &[ExtensionWarningInfo]) -> Option<String>;

/// Pretty JSON through each type's own `Serialize`, so an `f32` prints as
/// itself and not as the `f64` a `serde_json::Value` would widen it to.
fn pretty(value: &impl Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Two entries under keys only known at run time, such as `expressions` or
/// `visemes`.
struct Two<'a, A, B>((&'a str, &'a A), (&'a str, &'a B));

impl<A: Serialize, B: Serialize> Serialize for Two<'_, A, B> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry(self.0.0, self.0.1)?;
        map.serialize_entry(self.1.0, self.1.1)?;
        map.end()
    }
}

#[derive(serde::Serialize)]
struct WarningLine<'a> {
    code: &'a str,
    message: &'a str,
}

#[derive(serde::Serialize)]
struct HumanoidListing<'a> {
    mappings: &'a [ControlMappingInfo],
    unmapped: Vec<&'a str>,
    warnings: Vec<WarningLine<'a>>,
}

#[derive(serde::Serialize)]
struct Mapped<'a> {
    control: &'a str,
    param: &'a ParamId,
    stops: &'a [f32; 3],
    replaced: &'a Option<ControlMappingInfo>,
}

#[derive(serde::Serialize)]
struct Added<'a> {
    added: &'a [String],
    replaced: &'a [String],
    skipped: &'a [String],
}

/// What one typed command sends, and how its reply prints.
enum Plan {
    /// An edit: its reply's JSON, then the warnings under `key`.
    Edit(Command, &'static str),
    /// A read whose reply prints through `show`, which may also want the
    /// warnings under `key`.
    Read(Command, Show, Option<&'static str>),
}

pub fn humanoid(cli: &Cli, action: &HumanoidCmd, stream: &mut transport::Transport) -> Result<i32> {
    let session = resolve_session(cli)?;
    let plan = match action {
        HumanoidCmd::Controls => Plan::Read(
            Command::HumanoidGet { session },
            |body, _| match body {
                ResponseBody::Humanoid { controls, .. } => Some(pretty(controls)),
                _ => None,
            },
            None,
        ),
        HumanoidCmd::List => Plan::Read(
            Command::HumanoidGet { session },
            |body, warnings| match body {
                ResponseBody::Humanoid { mappings, controls } => Some(pretty(&HumanoidListing {
                    mappings,
                    unmapped: controls
                        .iter()
                        .filter(|c| !mappings.iter().any(|m| m.control == c.name))
                        .map(|c| c.name.as_str())
                        .collect(),
                    warnings: warning_lines(warnings),
                })),
                _ => None,
            },
            Some(HUMANOID),
        ),
        HumanoidCmd::Map {
            control,
            param,
            stops,
            nonstandard,
        } => Plan::Edit(
            Command::HumanoidMap {
                session,
                control: control.clone(),
                param: param.clone(),
                stops: *stops,
                nonstandard: *nonstandard,
            },
            HUMANOID,
        ),
        HumanoidCmd::Unmap { control } => Plan::Edit(
            Command::HumanoidUnmap {
                session,
                control: control.clone(),
            },
            HUMANOID,
        ),
    };
    execute(cli, session, plan, stream)
}

pub fn expression(
    cli: &Cli,
    group: ExpressionGroup,
    action: &ExpressionCmd,
    matches: Option<&ArgMatches>,
    stream: &mut transport::Transport,
) -> Result<i32> {
    let session = resolve_session(cli)?;
    let plan = match action {
        ExpressionCmd::Names => names(session, group),
        ExpressionCmd::List => list(session, group),
        ExpressionCmd::Set(args) => set(session, group, args, matches)?,
        ExpressionCmd::Remove { name } => remove(session, group, name),
    };
    execute(cli, session, plan, stream)
}

pub fn viseme(
    cli: &Cli,
    action: &VisemeCmd,
    matches: Option<&ArgMatches>,
    stream: &mut transport::Transport,
) -> Result<i32> {
    let session = resolve_session(cli)?;
    let group = ExpressionGroup::Viseme;
    let plan = match action {
        VisemeCmd::Names => names(session, group),
        VisemeCmd::List => list(session, group),
        VisemeCmd::Set(args) => set(session, group, args, matches)?,
        VisemeCmd::Remove { name } => remove(session, group, name),
        VisemeCmd::AddStandard { replace } => Plan::Edit(
            Command::VisemeAddStandard {
                session,
                replace: *replace,
            },
            EXPRESSIONS,
        ),
    };
    execute(cli, session, plan, stream)
}

pub fn clip(cli: &Cli, action: &ClipCmd, stream: &mut transport::Transport) -> Result<i32> {
    let session = resolve_session(cli)?;
    let plan = match action {
        ClipCmd::List => Plan::Read(
            Command::ClipList { session },
            |body, warnings| match body {
                ResponseBody::Clips { clips } => Some(pretty(&Two(
                    ("clips", clips),
                    ("warnings", &warning_lines(warnings)),
                ))),
                _ => None,
            },
            Some(ANIMATIONS),
        ),
    };
    execute(cli, session, plan, stream)
}

fn names(session: SessionId, group: ExpressionGroup) -> Plan {
    let show: Show = match group {
        ExpressionGroup::Expression => |body, _| match body {
            ResponseBody::Expressions { emotion_names, .. } => Some(pretty(emotion_names)),
            _ => None,
        },
        ExpressionGroup::Viseme => |body, _| match body {
            ResponseBody::Expressions { viseme_names, .. } => Some(pretty(viseme_names)),
            _ => None,
        },
    };
    Plan::Read(Command::ExpressionList { session }, show, None)
}

/// Prints as `{"expressions": [...], "warnings": [...]}`, or `visemes`, with
/// every warning of the extension: the editor does not sort them by group.
fn list(session: SessionId, group: ExpressionGroup) -> Plan {
    let show: Show = match group {
        ExpressionGroup::Expression => |body, warnings| match body {
            ResponseBody::Expressions { expressions, .. } => Some(pretty(&Two(
                ("expressions", expressions),
                ("warnings", &warning_lines(warnings)),
            ))),
            _ => None,
        },
        ExpressionGroup::Viseme => |body, warnings| match body {
            ResponseBody::Expressions { visemes, .. } => Some(pretty(&Two(
                ("visemes", visemes),
                ("warnings", &warning_lines(warnings)),
            ))),
            _ => None,
        },
    };
    Plan::Read(Command::ExpressionList { session }, show, Some(EXPRESSIONS))
}

/// `matches` are the `set` arguments, which say where each `--control` and
/// `--param` stood so the settings keep their given order.
fn set(
    session: SessionId,
    group: ExpressionGroup,
    args: &SetArgs,
    matches: Option<&ArgMatches>,
) -> Result<Plan> {
    let at = |flag: &str| -> Vec<usize> {
        matches
            .and_then(|m| m.indices_of(flag))
            .map(Iterator::collect)
            .unwrap_or_default()
    };
    let (controls, params) = (at("control"), at("param"));
    let mut settings = Vec::with_capacity(args.control.len() + args.param.len());
    for (i, text) in args.control.iter().enumerate() {
        let (name, value) = assignment("--control", text)?;
        let index = controls.get(i).copied().unwrap_or(usize::MAX);
        settings.push((
            index,
            ExpressionSetting {
                control: Some(name.to_string()),
                param: None,
                value,
            },
        ));
    }
    for (i, text) in args.param.iter().enumerate() {
        let (id, value) = assignment("--param", text)?;
        let index = params.get(i).copied().unwrap_or(usize::MAX);
        settings.push((
            index,
            ExpressionSetting {
                control: None,
                param: Some(ParamId::new(id)?),
                value,
            },
        ));
    }
    settings.sort_by_key(|(index, _)| *index);
    Ok(Plan::Edit(
        Command::ExpressionSet {
            session,
            group,
            name: args.name.clone(),
            settings: settings.into_iter().map(|(_, s)| s).collect(),
            nonstandard: args.nonstandard,
        },
        EXPRESSIONS,
    ))
}

fn remove(session: SessionId, group: ExpressionGroup, name: &str) -> Plan {
    Plan::Edit(
        Command::ExpressionDelete {
            session,
            group,
            name: name.to_string(),
        },
        EXPRESSIONS,
    )
}

/// `name=value` with a finite value.
fn assignment<'a>(flag: &str, text: &'a str) -> Result<(&'a str, f32)> {
    let (name, value) = text
        .rsplit_once('=')
        .ok_or_else(|| anyhow!("{flag} takes name=value, not {text:?}"))?;
    let value: f32 = value
        .trim()
        .parse()
        .map_err(|_| anyhow!("{flag} takes name=value with a numeric value, not {text:?}"))?;
    if !value.is_finite() {
        bail!("{flag} takes name=value with a finite value, not {text:?}");
    }
    Ok((name, value))
}

fn warning_lines(warnings: &[ExtensionWarningInfo]) -> Vec<WarningLine<'_>> {
    warnings
        .iter()
        .map(|w| WarningLine {
            code: &w.code,
            message: &w.message,
        })
        .collect()
}

fn execute(
    cli: &Cli,
    session: SessionId,
    plan: Plan,
    stream: &mut transport::Transport,
) -> Result<i32> {
    let (command, key) = match &plan {
        Plan::Edit(command, key) => (command.clone(), Some(*key)),
        Plan::Read(command, _, key) => (command.clone(), *key),
    };
    let reply = stream.call(Request { id: 1, command }, serde_json::Map::new())?;
    let Reply::Ok { body, .. } = &reply else {
        print_reply(cli, &reply);
        return Ok(1);
    };
    if cli.json {
        print_reply(cli, &reply);
        return Ok(0);
    }
    let warnings = match key {
        Some(key) => warnings(stream, session, key)?,
        None => Vec::new(),
    };
    match plan {
        Plan::Edit(..) => {
            println!("{}", edit_json(body));
            for w in &warnings {
                eprintln!("warning: {}: {}", w.code, w.message);
            }
        }
        Plan::Read(_, show, _) => match show(body, &warnings) {
            Some(text) => println!("{text}"),
            None => print_reply(cli, &reply),
        },
    }
    Ok(0)
}

/// An edit's reply as the file CLI prints the same edit's result.
fn edit_json(body: &ResponseBody) -> String {
    match body {
        ResponseBody::HumanoidMapped { mapping, replaced } => pretty(&Mapped {
            control: &mapping.control,
            param: &mapping.param,
            stops: &mapping.stops,
            replaced,
        }),
        ResponseBody::HumanoidUnmapped { mapping } => pretty(mapping),
        ResponseBody::Expression {
            group,
            expression,
            replaced,
        } => pretty(&Two((noun(*group), expression), ("replaced", replaced))),
        ResponseBody::ExpressionDeleted { expression, .. } => pretty(expression),
        ResponseBody::VisemesAdded {
            added,
            replaced,
            skipped,
        } => pretty(&Added {
            added,
            replaced,
            skipped,
        }),
        other => pretty(other),
    }
}

const fn noun(group: ExpressionGroup) -> &'static str {
    match group {
        ExpressionGroup::Expression => "expression",
        ExpressionGroup::Viseme => "viseme",
    }
}

/// The model's warnings under `key`, after the command that just ran.
fn warnings(
    stream: &mut transport::Transport,
    session: SessionId,
    key: &str,
) -> Result<Vec<ExtensionWarningInfo>> {
    match stream.call(
        Request {
            id: 2,
            command: Command::ExtensionWarnings { session },
        },
        serde_json::Map::new(),
    )? {
        Reply::Ok {
            body: ResponseBody::ExtensionWarnings { warnings },
            ..
        } => Ok(warnings
            .into_iter()
            .filter(|w| w.key.as_str() == key)
            .collect()),
        other => bail!("extension_warnings answered {other:?}"),
    }
}

/// `extension warnings`: one line per finding, the key first.
pub fn print_warnings(warnings: &[ExtensionWarningInfo]) {
    if warnings.is_empty() {
        println!("no extension warnings");
    }
    for w in warnings {
        let subjects = if w.subjects.is_empty() {
            String::new()
        } else {
            format!(
                "  {}",
                serde_json::to_string(&w.subjects).unwrap_or_default()
            )
        };
        println!("warning: {}: {}: {}{subjects}", w.key, w.code, w.message);
    }
}
