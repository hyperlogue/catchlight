//! `expression`: the named expressions of the official
//! `catchlight.expressions` extension, edited in the file's structure
//! section. No image is decoded.

use std::path::Path;

use catchlight_core::extension::Extension;
use catchlight_core::formats::clm::ClmFile;
use catchlight_extension_expressions::{
    standard_visemes, Expression, Expressions, Setting, Target, EMOTIONS, VISEMES,
};
use serde::Serialize;

use crate::humanoid::{find_param, standard_control, warning_lines, Edited, WarningLine};
use crate::{extension, file, Error};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StandardNames {
    pub emotions: &'static [&'static str],
    pub visemes: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    pub value: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub expressions: Vec<ExpressionLine>,
    pub warnings: Vec<WarningLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Set {
    pub expression: ExpressionLine,
    /// The expression this one replaced.
    pub replaced: Option<ExpressionLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisemesAdded {
    pub added: Vec<String>,
    /// Existing expressions replaced with the defaults, under `--replace`.
    pub replaced: Vec<String>,
    /// Existing expressions left as they were.
    pub skipped: Vec<String>,
}

/// One `--control` or `--param` as given.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingArg {
    Control(String, f32),
    Param(String, f32),
}

pub fn names() -> StandardNames {
    StandardNames {
        emotions: &EMOTIONS,
        visemes: &VISEMES,
    }
}

pub fn list(path: &Path) -> Result<Listing, Error> {
    let clm = file::read(path)?;
    let expressions: Expressions = extension::for_edit(&clm, path)?;
    Ok(Listing {
        expressions: expressions
            .expressions()
            .into_iter()
            .map(ExpressionLine::from)
            .collect(),
        warnings: warning_lines(extension::warnings(&clm, path, Expressions::KEY)?),
    })
}

/// A control value must lie in the control's range: a standard control's
/// own, -1..1 for a name kept with `nonstandard`.
pub fn set(
    path: &Path,
    name: &str,
    settings: &[SettingArg],
    nonstandard: bool,
) -> Result<Edited<Set>, Error> {
    if name.is_empty() {
        return Err(Error::BadValue {
            field: "the expression name".into(),
            expected: "a non-empty name".into(),
            value: String::new(),
        });
    }
    if settings.is_empty() {
        return Err(Error::BadValue {
            field: format!("expression {name:?}"),
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
    let expression = Expression::new(name, out);
    let replaced = expressions
        .insert(expression.clone())
        .map(ExpressionLine::from);
    let warnings = save(&mut clm, path, &expressions)?;
    Ok(Edited {
        result: Set {
            expression: expression.into(),
            replaced,
        },
        warnings,
    })
}

/// Removing the last expression drops the extension from the file.
pub fn remove(path: &Path, name: &str) -> Result<Edited<ExpressionLine>, Error> {
    let mut clm = file::read(path)?;
    file::load(&clm, path)?;
    let mut expressions: Expressions = extension::for_edit(&clm, path)?;
    let removed = expressions
        .remove(name)
        .ok_or_else(|| Error::NoSuchExpression {
            path: path.to_path_buf(),
            name: name.to_string(),
        })?;
    let warnings = save(&mut clm, path, &expressions)?;
    Ok(Edited {
        result: removed.into(),
        warnings,
    })
}

/// New visemes go after the existing expressions, in the standard order.
pub fn add_visemes(path: &Path, replace: bool) -> Result<Edited<VisemesAdded>, Error> {
    let mut clm = file::read(path)?;
    file::load(&clm, path)?;
    let mut expressions: Expressions = extension::for_edit(&clm, path)?;
    let mut report = VisemesAdded {
        added: Vec::new(),
        replaced: Vec::new(),
        skipped: Vec::new(),
    };
    for viseme in standard_visemes() {
        let name = viseme.name.clone();
        if !expressions.contains(&name) {
            report.added.push(name);
        } else if replace {
            report.replaced.push(name);
        } else {
            report.skipped.push(name);
            continue;
        }
        expressions.insert(viseme);
    }
    let warnings = save(&mut clm, path, &expressions)?;
    Ok(Edited {
        result: report,
        warnings,
    })
}

fn save(
    clm: &mut ClmFile,
    path: &Path,
    expressions: &Expressions,
) -> Result<Vec<WarningLine>, Error> {
    extension::store(clm, expressions, expressions.is_empty())?;
    let bytes = file::encode(clm, path)?;
    let warnings = extension::warnings(clm, path, Expressions::KEY)?;
    file::write(path, &bytes)?;
    Ok(warning_lines(warnings))
}
