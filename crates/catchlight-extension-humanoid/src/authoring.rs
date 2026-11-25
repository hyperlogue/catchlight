//! Checked edits a tool makes on a person's behalf. [`Humanoid::set`] and
//! [`Humanoid::remove`] take anything; these refuse a request that names a
//! param the model lacks or a control outside the standard list.

use catchlight_core::extension::ExtensionError;
use catchlight_core::{Model, ParamId};

use crate::{control, Control, ControlMapping, Humanoid, CONTROLS};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AuthoringError {
    #[error("a control name must not be empty")]
    EmptyControlName,
    #[error(
        "{name:?} is not a standard humanoid control{}",
        .nearest.map(|n| format!(" (did you mean {n:?}?)")).unwrap_or_default()
    )]
    NonstandardControl {
        name: String,
        nearest: Option<&'static str>,
    },
    #[error("the model has no param {0:?}")]
    NoParam(ParamId),
    #[error("control {control:?} has a stop that is not finite: {stops:?}")]
    NonFiniteStop { control: String, stops: [f32; 3] },
    #[error("no control {0:?} is mapped")]
    NotMapped(String),
    /// The data under the key cannot be decoded as this build's type, so it
    /// is kept rather than overwritten.
    #[error("the humanoid data cannot be edited: {0}")]
    Unreadable(ExtensionError),
}

/// What [`map`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub mapping: ControlMapping,
    /// The mapping this one replaced.
    pub replaced: Option<ControlMapping>,
}

/// The standard control named `name`. A name outside the list is refused
/// unless `nonstandard`, which yields `None`.
pub fn standard_control(
    name: &str,
    nonstandard: bool,
) -> Result<Option<&'static Control>, AuthoringError> {
    if let Some(c) = control(name) {
        return Ok(Some(c));
    }
    if name.is_empty() {
        return Err(AuthoringError::EmptyControlName);
    }
    if nonstandard {
        return Ok(None);
    }
    Err(AuthoringError::NonstandardControl {
        name: name.to_string(),
        nearest: nearest(name, CONTROLS.iter().map(|c| c.name)),
    })
}

/// The candidate within a few edits of `name`, for a "did you mean".
pub fn nearest(name: &str, candidates: impl Iterator<Item = &'static str>) -> Option<&'static str> {
    candidates
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, _)| *d <= 3)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// Maps `control_name` onto `param`, replacing its mapping. Without `stops`,
/// a two-sided control spans the param's min, default and max, and a
/// one-sided control rests at the default and reaches the end of the range
/// farther from it.
pub fn map(
    model: &mut Model,
    control_name: &str,
    param: &ParamId,
    stops: Option<[f32; 3]>,
    nonstandard: bool,
) -> Result<Mapped, AuthoringError> {
    let standard = standard_control(control_name, nonstandard)?;
    let limits = model
        .param(param)
        .ok_or_else(|| AuthoringError::NoParam(param.clone()))?;
    let stops = match stops {
        Some(stops) => stops,
        None if standard.is_some_and(|c| c.one_sided) => {
            let far = if limits.default - limits.min > limits.max - limits.default {
                limits.min
            } else {
                limits.max
            };
            [limits.default, limits.default, far]
        }
        None => [limits.min, limits.default, limits.max],
    };
    if !stops.iter().all(|s| s.is_finite()) {
        return Err(AuthoringError::NonFiniteStop {
            control: control_name.to_string(),
            stops,
        });
    }
    let mapping = ControlMapping {
        param: param.clone(),
        stops,
    };
    let humanoid = model
        .extension_or_default::<Humanoid>()
        .map_err(AuthoringError::Unreadable)?;
    let replaced = humanoid.set(control_name, mapping.clone());
    Ok(Mapped { mapping, replaced })
}

/// Removes `control_name`'s mapping and returns it. Removing the last
/// mapping removes the extension from the model.
pub fn unmap(model: &mut Model, control_name: &str) -> Result<ControlMapping, AuthoringError> {
    let not_mapped = || AuthoringError::NotMapped(control_name.to_string());
    let humanoid = match model.extension_mut::<Humanoid>() {
        Ok(humanoid) => humanoid,
        Err(ExtensionError::Missing) => return Err(not_mapped()),
        Err(e) => return Err(AuthoringError::Unreadable(e)),
    };
    // A first write reconciles, so every mapping left resolves.
    let removed = humanoid.remove(control_name).ok_or_else(not_mapped)?;
    if humanoid.is_empty() {
        model.remove_extension::<Humanoid>();
    }
    Ok(removed)
}
