//! `expression` and `viseme`: the two groups of the official
//! `catchlight.expressions` extension, edited in the file's structure
//! section. Each command acts on one group and reports only its warnings. No
//! image is decoded.

use std::path::Path;

use catchlight_core::extension::{Extension, Warning};
use catchlight_core::formats::clm::ClmFile;
use catchlight_extension_expressions::{
    standard_visemes, Expression, Expressions, Group, Setting, Target, EMOTIONS, VISEMES,
};
use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::humanoid::{find_param, standard_control, warning_lines, Edited, WarningLine};
use crate::{extension, file, Error};

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
    Ok(Listing {
        group,
        members: expressions
            .all(group)
            .into_iter()
            .map(ExpressionLine::from)
            .collect(),
        warnings: warnings(&clm, path, &expressions, group)?,
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
    let noun = group.noun();
    if name.is_empty() {
        return Err(Error::BadValue {
            field: format!("the {noun} name"),
            expected: "a non-empty name".into(),
            value: String::new(),
        });
    }
    if settings.is_empty() {
        return Err(Error::BadValue {
            field: format!("{noun} {name:?}"),
            expected: "at least one --control name=value or --param id=value".into(),
            value: String::new(),
        });
    }
    let mut clm = file::read(path)?;
    file::load(&clm, path)?;
    let mut out = Vec::with_capacity(settings.len());
    for setting in settings {
        out.push(match setting {
            SettingArg::Control(control, value) => {
                let standard = standard_control(control, nonstandard)?;
                let (min, max) = standard.map_or((-1.0, 1.0), |c| (c.min(), c.max()));
                if !value.is_finite() || !(min..=max).contains(value) {
                    return Err(Error::BadValue {
                        field: format!("control {control:?}"),
                        expected: format!("a finite value in {min}..{max}"),
                        value: value.to_string(),
                    });
                }
                Setting::control(control.clone(), *value)
            }
            SettingArg::Param(id, value) => {
                if !value.is_finite() {
                    return Err(Error::BadValue {
                        field: format!("param {id:?}"),
                        expected: "a finite value".into(),
                        value: value.to_string(),
                    });
                }
                Setting::param(find_param(&clm, path, id)?.id.clone(), *value)
            }
        });
    }
    let mut expressions: Expressions = extension::for_edit(&clm, path)?;
    let member = Expression::new(name, out);
    let replaced = expressions
        .insert(group, member.clone())
        .map(ExpressionLine::from);
    let warnings = save(&mut clm, path, &expressions, group)?;
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
    file::load(&clm, path)?;
    let mut expressions: Expressions = extension::for_edit(&clm, path)?;
    let removed = expressions
        .remove(group, name)
        .ok_or_else(|| Error::NoSuchExpression {
            path: path.to_path_buf(),
            noun: group.noun(),
            name: name.to_string(),
        })?;
    let warnings = save(&mut clm, path, &expressions, group)?;
    Ok(Edited {
        result: removed.into(),
        warnings,
    })
}

/// New visemes go after the existing ones, in the standard order.
pub fn add_standard_visemes(path: &Path, replace: bool) -> Result<Edited<StandardAdded>, Error> {
    let mut clm = file::read(path)?;
    file::load(&clm, path)?;
    let mut expressions: Expressions = extension::for_edit(&clm, path)?;
    let mut report = StandardAdded {
        added: Vec::new(),
        replaced: Vec::new(),
        skipped: Vec::new(),
    };
    for viseme in standard_visemes() {
        let name = viseme.name.clone();
        if !expressions.contains(Group::Viseme, &name) {
            report.added.push(name);
        } else if replace {
            report.replaced.push(name);
        } else {
            report.skipped.push(name);
            continue;
        }
        expressions.insert(Group::Viseme, viseme);
    }
    let warnings = save(&mut clm, path, &expressions, Group::Viseme)?;
    Ok(Edited {
        result: report,
        warnings,
    })
}

fn save(
    clm: &mut ClmFile,
    path: &Path,
    expressions: &Expressions,
    group: Group,
) -> Result<Vec<WarningLine>, Error> {
    extension::store(clm, expressions, expressions.is_empty())?;
    let bytes = file::encode(clm, path)?;
    let warnings = warnings(clm, path, expressions, group)?;
    file::write(path, &bytes)?;
    Ok(warnings)
}

/// The extension's warnings that concern `group`: the core's about a
/// reference the group uses or about the data as a whole, then the
/// extension's own check of the group.
fn warnings(
    clm: &ClmFile,
    path: &Path,
    expressions: &Expressions,
    group: Group,
) -> Result<Vec<WarningLine>, Error> {
    let own = format!("{}/", Expressions::KEY);
    let (model, _) = file::load(clm, path)?;
    let mut out: Vec<Warning> = model
        .extension_warnings()
        .into_iter()
        .filter(|w| w.key.as_str() == Expressions::KEY)
        .map(|w| w.warning)
        .filter(|w| !w.code.starts_with(&own))
        .filter(|w| w.subjects.is_empty() || w.subjects.iter().any(|r| expressions.uses(group, r)))
        .collect();
    out.extend(expressions.check_group(group, &model));
    Ok(warning_lines(out))
}
