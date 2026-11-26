//! The official extensions' typed commands: edits through the extension
//! crates' checked authoring functions, and reads of the model alone that a
//! replica answers too.

use catchlight_core::Model;
use catchlight_core::extension::ExtensionError;
use catchlight_editor_protocol::*;
use catchlight_extension_animations::{Animations, LaneTarget};
use catchlight_extension_expressions::authoring as expressions;
use catchlight_extension_expressions::{
    EMOTIONS, Expression, Expressions, Group, Setting, Target, VISEMES,
};
use catchlight_extension_humanoid::authoring as humanoid;
use catchlight_extension_humanoid::{CONTROLS, ControlMapping, Humanoid};

use crate::EditorError;

impl From<humanoid::AuthoringError> for EditorError {
    fn from(e: humanoid::AuthoringError) -> Self {
        Self::Humanoid(e)
    }
}

impl From<expressions::AuthoringError> for EditorError {
    fn from(e: expressions::AuthoringError) -> Self {
        Self::Expressions(e)
    }
}

pub(crate) const fn humanoid_code(e: &humanoid::AuthoringError) -> ErrorCode {
    use humanoid::AuthoringError as E;
    match e {
        E::EmptyControlName | E::NonFiniteStop { .. } => ErrorCode::BadRequest,
        E::NonstandardControl { .. } => ErrorCode::NonstandardControl,
        E::NoParam(_) => ErrorCode::NoParam,
        E::NotMapped(_) => ErrorCode::Edit,
        E::Unreadable(_) => ErrorCode::ExtensionUnread,
    }
}

pub(crate) const fn expressions_code(e: &expressions::AuthoringError) -> ErrorCode {
    use expressions::AuthoringError as E;
    match e {
        E::EmptyName(_) | E::NoSettings { .. } | E::ControlValue { .. } | E::ParamValue { .. } => {
            ErrorCode::BadRequest
        }
        E::Control(e) => humanoid_code(e),
        E::NoParam(_) => ErrorCode::NoParam,
        E::NoMember { .. } => ErrorCode::Edit,
        E::Unreadable(_) => ErrorCode::ExtensionUnread,
    }
}

const fn group(group: ExpressionGroup) -> Group {
    match group {
        ExpressionGroup::Expression => Group::Expression,
        ExpressionGroup::Viseme => Group::Viseme,
    }
}

fn mapping_info(control: &str, mapping: ControlMapping) -> ControlMappingInfo {
    ControlMappingInfo {
        control: control.to_string(),
        param: mapping.param,
        stops: mapping.stops,
    }
}

fn expression_info(expression: Expression) -> ExpressionInfo {
    ExpressionInfo {
        name: expression.name,
        settings: expression
            .settings
            .into_iter()
            .map(|s| match s.target {
                Target::Control(control) => ExpressionSetting {
                    control: Some(control),
                    param: None,
                    value: s.value,
                },
                Target::Param(param) => ExpressionSetting {
                    control: None,
                    param: Some(param),
                    value: s.value,
                },
            })
            .collect(),
    }
}

fn setting(setting: ExpressionSetting) -> Result<Setting, EditorError> {
    match (setting.control, setting.param) {
        (Some(control), None) => Ok(Setting::control(control, setting.value)),
        (None, Some(param)) => Ok(Setting::param(param, setting.value)),
        _ => Err(EditorError::BadRequest(
            "a setting names exactly one of `control` and `param`".into(),
        )),
    }
}

const fn control_names(nonstandard: bool) -> humanoid::ControlNames {
    if nonstandard {
        humanoid::ControlNames::AllowNonstandard
    } else {
        humanoid::ControlNames::StandardOnly
    }
}

pub(crate) fn humanoid_map(
    model: &mut Model,
    control: &str,
    param: &ParamId,
    stops: Option<[f32; 3]>,
    nonstandard: bool,
) -> Result<ResponseBody, EditorError> {
    let mapped = humanoid::map(model, control, param, stops, control_names(nonstandard))?;
    Ok(ResponseBody::HumanoidMapped {
        mapping: mapping_info(control, mapped.mapping),
        replaced: mapped.replaced.map(|m| mapping_info(control, m)),
    })
}

pub(crate) fn humanoid_unmap(
    model: &mut Model,
    control: &str,
) -> Result<ResponseBody, EditorError> {
    let removed = humanoid::unmap(model, control)?;
    Ok(ResponseBody::HumanoidUnmapped {
        mapping: mapping_info(control, removed),
    })
}

pub(crate) fn expression_set(
    model: &mut Model,
    which: ExpressionGroup,
    name: &str,
    settings: Vec<ExpressionSetting>,
    nonstandard: bool,
) -> Result<ResponseBody, EditorError> {
    let settings = settings
        .into_iter()
        .map(setting)
        .collect::<Result<Vec<_>, _>>()?;
    let stored = Expression::new(name, settings.clone());
    let replaced = expressions::set(
        model,
        group(which),
        name,
        settings,
        control_names(nonstandard),
    )?;
    Ok(ResponseBody::Expression {
        group: which,
        expression: expression_info(stored),
        replaced: replaced.map(expression_info),
    })
}

pub(crate) fn expression_delete(
    model: &mut Model,
    which: ExpressionGroup,
    name: &str,
) -> Result<ResponseBody, EditorError> {
    let removed = expressions::remove(model, group(which), name)?;
    Ok(ResponseBody::ExpressionDeleted {
        group: which,
        expression: expression_info(removed),
    })
}

pub(crate) fn viseme_add_standard(
    model: &mut Model,
    replace: bool,
) -> Result<ResponseBody, EditorError> {
    let existing = if replace {
        expressions::OnExisting::Replace
    } else {
        expressions::OnExisting::Keep
    };
    let report = expressions::add_standard_visemes(model, existing)?;
    Ok(ResponseBody::VisemesAdded {
        added: report.added,
        replaced: report.replaced,
        skipped: report.skipped,
    })
}

/// `None` when the model carries none; data this editor cannot read is
/// refused rather than shown as empty.
fn read<T: catchlight_core::extension::Extension>(
    model: &Model,
) -> Result<Option<&T>, EditorError> {
    match model.extension::<T>() {
        Ok(data) => Ok(Some(data)),
        Err(ExtensionError::Missing) => Ok(None),
        Err(e) => Err(EditorError::ExtensionUnread {
            key: T::KEY,
            why: e.to_string(),
        }),
    }
}

pub(crate) fn humanoid_get(model: &Model) -> Result<ResponseBody, EditorError> {
    let mappings = read::<Humanoid>(model)?
        .map(|h| {
            h.mappings()
                .map(|(control, m)| mapping_info(control, m))
                .collect()
        })
        .unwrap_or_default();
    Ok(ResponseBody::Humanoid {
        mappings,
        controls: CONTROLS
            .iter()
            .map(|c| StandardControlInfo {
                name: c.name.to_string(),
                one_sided: c.one_sided,
            })
            .collect(),
    })
}

pub(crate) fn expression_list(model: &Model) -> Result<ResponseBody, EditorError> {
    let data = read::<Expressions>(model)?;
    let members = |g: Group| -> Vec<ExpressionInfo> {
        data.map(|d| d.all(g).map(expression_info).collect())
            .unwrap_or_default()
    };
    Ok(ResponseBody::Expressions {
        expressions: members(Group::Expression),
        visemes: members(Group::Viseme),
        emotion_names: EMOTIONS.iter().map(ToString::to_string).collect(),
        viseme_names: VISEMES.iter().map(ToString::to_string).collect(),
    })
}

/// A lane whose param is gone is left out, as the extension reads it.
pub(crate) fn clip_list(model: &Model) -> Result<ResponseBody, EditorError> {
    let clips = read::<Animations>(model)?
        .map(|animations| animations.clips().collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(ResponseBody::Clips {
        clips: clips
            .into_iter()
            .map(|clip| ClipInfo {
                lanes: clip
                    .lanes
                    .iter()
                    .map(|lane| LaneInfo {
                        target: match &lane.target {
                            LaneTarget::Param(param) => LaneTargetInfo::Param {
                                param: param.clone(),
                            },
                            LaneTarget::Control(name) => {
                                LaneTargetInfo::Control { name: name.clone() }
                            }
                            LaneTarget::Expression(name) => {
                                LaneTargetInfo::Expression { name: name.clone() }
                            }
                            LaneTarget::Viseme(name) => {
                                LaneTargetInfo::Viseme { name: name.clone() }
                            }
                        },
                        keyframes: lane.keyframes().len() as u32,
                    })
                    .collect(),
                name: clip.name,
                timestep: clip.timestep,
                length: clip.length,
                lead_in: clip.lead_in,
                lead_out: clip.lead_out,
            })
            .collect(),
    })
}

pub(crate) fn extension_warnings(model: &Model) -> ResponseBody {
    ResponseBody::ExtensionWarnings {
        warnings: model
            .extension_warnings()
            .into_iter()
            .map(|w| ExtensionWarningInfo {
                key: w.key,
                code: w.warning.code.into_owned(),
                message: w.warning.message,
                subjects: w.warning.subjects.iter().map(ExtensionRef::from).collect(),
            })
            .collect(),
    }
}
