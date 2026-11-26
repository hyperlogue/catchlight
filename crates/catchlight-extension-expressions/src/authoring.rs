//! Checked edits a tool makes on a person's behalf. [`Expressions::insert`]
//! and [`Expressions::remove`] take anything; these refuse a member with no
//! settings, a setting naming a param the model lacks, a control outside the
//! standard list, or a value its target cannot take.

use catchlight_core::extension::ExtensionError;
use catchlight_core::{Model, ParamId};
use catchlight_extension_humanoid::authoring::{
    AuthoringError as ControlError, ControlNames, standard_control,
};

use crate::{Expression, Expressions, Group, Setting, Target, standard_visemes};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AuthoringError {
    #[error("{} name must not be empty", article(*.0))]
    EmptyName(Group),
    #[error("{} {name:?} needs at least one setting", .group.noun())]
    NoSettings { group: Group, name: String },
    #[error(transparent)]
    Control(ControlError),
    #[error("control {control:?} takes a finite value in {min}..{max}, not {value}")]
    ControlValue {
        control: String,
        value: f32,
        min: f32,
        max: f32,
    },
    #[error("param {param:?} takes a finite value, not {value}")]
    ParamValue { param: ParamId, value: f32 },
    #[error("the model has no param {0:?}")]
    NoParam(ParamId),
    #[error("there is no {} {name:?}", .group.noun())]
    NoMember { group: Group, name: String },
    /// The data under the key cannot be decoded as this build's type, so it
    /// is kept rather than overwritten.
    #[error("the expressions data cannot be edited: {0}")]
    Unreadable(ExtensionError),
}

fn article(group: Group) -> &'static str {
    match group {
        Group::Expression => "an expression",
        Group::Viseme => "a viseme",
    }
}

/// What [`add_standard_visemes`] does with a viseme the model already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExisting {
    Keep,
    Replace,
}

/// What [`add_standard_visemes`] did, by viseme name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandardAdded {
    pub added: Vec<String>,
    /// Existing visemes replaced with the defaults.
    pub replaced: Vec<String>,
    /// Existing visemes left as they were.
    pub skipped: Vec<String>,
}

/// Checks `settings` in order against `model`: a control value must lie in
/// the control's range (a standard control's own, -1..1 for a name `names`
/// allows outside it), and a param must exist and take a finite value.
pub fn check_settings(
    model: &Model,
    settings: &[Setting],
    names: ControlNames,
) -> Result<(), AuthoringError> {
    for setting in settings {
        let value = setting.value;
        match &setting.target {
            Target::Control(control) => {
                let standard = standard_control(control, names).map_err(AuthoringError::Control)?;
                let (min, max) = standard.map_or((-1.0, 1.0), |c| (c.min(), c.max()));
                if !value.is_finite() || !(min..=max).contains(&value) {
                    return Err(AuthoringError::ControlValue {
                        control: control.clone(),
                        value,
                        min,
                        max,
                    });
                }
            }
            Target::Param(param) => {
                if !value.is_finite() {
                    return Err(AuthoringError::ParamValue {
                        param: param.clone(),
                        value,
                    });
                }
                if model.param(param).is_none() {
                    return Err(AuthoringError::NoParam(param.clone()));
                }
            }
        }
    }
    Ok(())
}

/// Creates or replaces the member `name` of `group`, its settings kept in
/// the order given, and returns the member it replaced.
pub fn set(
    model: &mut Model,
    group: Group,
    name: &str,
    settings: Vec<Setting>,
    names: ControlNames,
) -> Result<Option<Expression>, AuthoringError> {
    if name.is_empty() {
        return Err(AuthoringError::EmptyName(group));
    }
    if settings.is_empty() {
        return Err(AuthoringError::NoSettings {
            group,
            name: name.to_string(),
        });
    }
    check_settings(model, &settings, names)?;
    let expressions = model
        .extension_or_default::<Expressions>()
        .map_err(AuthoringError::Unreadable)?;
    Ok(expressions.insert(group, Expression::new(name, settings)))
}

/// Removes the member `name` of `group` and returns it. Removing the last
/// expression or viseme removes the extension from the model.
pub fn remove(model: &mut Model, group: Group, name: &str) -> Result<Expression, AuthoringError> {
    let no_member = || AuthoringError::NoMember {
        group,
        name: name.to_string(),
    };
    let expressions = match model.extension_mut::<Expressions>() {
        Ok(expressions) => expressions,
        Err(ExtensionError::Missing) => return Err(no_member()),
        Err(e) => return Err(AuthoringError::Unreadable(e)),
    };
    let removed = expressions.remove(group, name).ok_or_else(no_member)?;
    if expressions.is_empty() {
        model.remove_extension::<Expressions>();
    }
    Ok(removed)
}

/// Adds the standard visemes with their default settings after the existing
/// visemes, in the standard order. An existing viseme of a standard name is
/// replaced in place or skipped, as `existing` says.
pub fn add_standard_visemes(
    model: &mut Model,
    existing: OnExisting,
) -> Result<StandardAdded, AuthoringError> {
    let expressions = model
        .extension_or_default::<Expressions>()
        .map_err(AuthoringError::Unreadable)?;
    let mut report = StandardAdded::default();
    for viseme in standard_visemes() {
        let name = viseme.name.clone();
        if !expressions.contains(Group::Viseme, &name) {
            report.added.push(name);
        } else if existing == OnExisting::Replace {
            report.replaced.push(name);
        } else {
            report.skipped.push(name);
            continue;
        }
        expressions.insert(Group::Viseme, viseme);
    }
    Ok(report)
}
