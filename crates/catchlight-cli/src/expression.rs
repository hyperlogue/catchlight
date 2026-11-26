//! `expression` and `viseme`: the two groups of the official
//! `catchlight.expressions` extension, edited in the file's structure
//! section. Each command acts on one group and reports only its warnings. No
//! image is decoded.

use std::path::Path;

use catchlight_core::Model;
use catchlight_core::extension::{Extension, Warning};
use catchlight_core::formats::clm::ClmFile;
use catchlight_extension_expressions::authoring::{self, AuthoringError};
use catchlight_extension_expressions::{
    EMOTIONS, Expression, Expressions, Group, Setting, Target, VISEMES,
};
use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::humanoid::{self, Edited, WarningLine, parse_param, warning_lines};
use crate::{Error, extension, file};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SettingLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    pub value: f32,
}

/// One expression or viseme.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ExpressionLine {
    pub name: String,
    pub settings: Vec<SettingLine>,
}

impl From<Expression> for ExpressionLine {
    fn from(expression: Expression) -> Self {
        Self {
            name: expression.name,
            settings: expression
                .settings
                .into_iter()
                .map(|s| match s.target {
                    Target::Control(name) => SettingLine {
                        control: Some(name),
                        param: None,
                        value: s.value,
                    },
                    Target::Param(id) => SettingLine {
                        control: None,
                        param: Some(id.to_string()),
                        value: s.value,
                    },
                })
                .collect(),
        }
    }
}

/// Prints as `{"expressions": [...], "warnings": [...]}`, or `visemes`.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub group: Group,
    pub members: Vec<ExpressionLine>,
    pub warnings: Vec<WarningLine>,
}

impl Serialize for Listing {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry(&format!("{}s", self.group.noun()), &self.members)?;
        map.serialize_entry("warnings", &self.warnings)?;
        map.end()
    }
}

/// Prints as `{"expression": ..., "replaced": ...}`, or `viseme`.
#[derive(Debug, Clone, PartialEq)]
pub struct Set {
    pub group: Group,
    pub member: ExpressionLine,
    /// The member of the same name this one replaced.
    pub replaced: Option<ExpressionLine>,
}

impl Serialize for Set {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry(self.group.noun(), &self.member)?;
        map.serialize_entry("replaced", &self.replaced)?;
        map.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StandardAdded {
    pub added: Vec<String>,
    /// Existing visemes replaced with the defaults, under `--replace`.
    pub replaced: Vec<String>,
    /// Existing visemes left as they were.
    pub skipped: Vec<String>,
}

/// One `--control` or `--param` as given.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingArg {
    Control(String, f32),
    Param(String, f32),
}

/// The group's standard names: the emotions, or the visemes.
pub fn names(group: Group) -> &'static [&'static str] {
    match group {
        Group::Expression => &EMOTIONS,
        Group::Viseme => &VISEMES,
    }
}

pub fn list(path: &Path, group: Group) -> Result<Listing, Error> {
    let clm = file::read(path)?;
    let expressions: Expressions = extension::for_edit(&clm, path)?;
    let (model, _) = file::load(&clm, path)?;
    Ok(Listing {
        group,
        members: expressions
            .all(group)
            .into_iter()
            .map(ExpressionLine::from)
            .collect(),
        warnings: warnings(&model, &expressions, group),
    })
}

/// A control value must lie in the control's range: a standard control's
/// own, -1..1 for a name kept with `nonstandard`.
pub fn set(
    path: &Path,
    group: Group,
    name: &str,
    settings: &[SettingArg],
    nonstandard: bool,
) -> Result<Edited<Set>, Error> {
    let settings = settings
        .iter()
        .map(|setting| {
            Ok(match setting {
                SettingArg::Control(control, value) => Setting::control(control.clone(), *value),
                SettingArg::Param(id, value) => Setting::param(parse_param(id)?, *value),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let mut clm = file::read(path)?;
    let (mut model, _) = file::load(&clm, path)?;
    let member = Expression::new(name, settings.clone());
    let replaced = authoring::set(&mut model, group, name, settings, nonstandard)
        .map_err(|e| authoring_error(e, path))?
        .map(ExpressionLine::from);
    let warnings = save(&mut clm, path, &model, group)?;
    Ok(Edited {
        result: Set {
            group,
            member: member.into(),
            replaced,
        },
        warnings,
    })
}

/// Removing the last expression or viseme drops the extension.
pub fn remove(path: &Path, group: Group, name: &str) -> Result<Edited<ExpressionLine>, Error> {
    let mut clm = file::read(path)?;
    let (mut model, _) = file::load(&clm, path)?;
    let removed =
        authoring::remove(&mut model, group, name).map_err(|e| authoring_error(e, path))?;
    let warnings = save(&mut clm, path, &model, group)?;
    Ok(Edited {
        result: removed.into(),
        warnings,
    })
}

/// New visemes go after the existing ones, in the standard order.
pub fn add_standard_visemes(path: &Path, replace: bool) -> Result<Edited<StandardAdded>, Error> {
    let mut clm = file::read(path)?;
    let (mut model, _) = file::load(&clm, path)?;
    let report = authoring::add_standard_visemes(&mut model, replace)
        .map_err(|e| authoring_error(e, path))?;
    let warnings = save(&mut clm, path, &model, Group::Viseme)?;
    Ok(Edited {
        result: StandardAdded {
            added: report.added,
            replaced: report.replaced,
            skipped: report.skipped,
        },
        warnings,
    })
}

fn save(
    clm: &mut ClmFile,
    path: &Path,
    model: &Model,
    group: Group,
) -> Result<Vec<WarningLine>, Error> {
    extension::store::<Expressions>(clm, model)?;
    let bytes = file::encode(clm, path)?;
    file::write(path, &bytes)?;
    let empty = Expressions::default();
    let expressions = model.extension::<Expressions>().unwrap_or(&empty);
    Ok(warnings(model, expressions, group))
}

fn authoring_error(error: AuthoringError, path: &Path) -> Error {
    match error {
        AuthoringError::EmptyName(group) => Error::BadValue {
            field: format!("the {} name", group.noun()),
            expected: "a non-empty name".into(),
            value: String::new(),
        },
        AuthoringError::NoSettings { group, name } => Error::BadValue {
            field: format!("{} {name:?}", group.noun()),
            expected: "at least one --control name=value or --param id=value".into(),
            value: String::new(),
        },
        AuthoringError::Control(e) => humanoid::authoring_error(e, path),
        AuthoringError::ControlValue {
            control,
            value,
            min,
            max,
        } => Error::BadValue {
            field: format!("control {control:?}"),
            expected: format!("a finite value in {min}..{max}"),
            value: value.to_string(),
        },
        AuthoringError::ParamValue { param, value } => Error::BadValue {
            field: format!("param {:?}", param.as_str()),
            expected: "a finite value".into(),
            value: value.to_string(),
        },
        AuthoringError::NoParam(id) => Error::NoSuchId {
            path: path.to_path_buf(),
            kind: "param",
            id: id.to_string(),
        },
        AuthoringError::NoMember { group, name } => Error::NoSuchExpression {
            path: path.to_path_buf(),
            noun: group.noun(),
            name,
        },
        AuthoringError::Unreadable(e) => extension::unreadable::<Expressions>(e, path),
    }
}

/// The extension's warnings that concern `group`: the core's about a
/// reference the group uses or about the data as a whole, then the
/// extension's own check of the group.
fn warnings(model: &Model, expressions: &Expressions, group: Group) -> Vec<WarningLine> {
    let own = format!("{}/", Expressions::KEY);
    let mut out: Vec<Warning> = extension::model_warnings(model, Expressions::KEY)
        .into_iter()
        .filter(|w| !w.code.starts_with(&own))
        .filter(|w| w.subjects.is_empty() || w.subjects.iter().any(|r| expressions.uses(group, r)))
        .collect();
    out.extend(expressions.check_group(group, model));
    warning_lines(out)
}
