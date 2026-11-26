//! `humanoid`: the control mappings of the official `catchlight.humanoid`
//! extension, edited in the file's structure section. No image is decoded.

use std::path::Path;

use catchlight_core::extension::{Extension, Warning};
use catchlight_core::formats::clm::ClmFile;
use catchlight_core::{Model, ParamId};
use catchlight_extension_humanoid::authoring::{self, AuthoringError, ControlNames};
use catchlight_extension_humanoid::{CONTROLS, Humanoid};
use serde::Serialize;

use crate::{Error, extension, file};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StandardControl {
    pub name: &'static str,
    pub one_sided: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mapping {
    pub control: String,
    pub param: String,
    pub stops: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WarningLine {
    pub code: String,
    pub message: String,
}

impl From<Warning> for WarningLine {
    fn from(warning: Warning) -> Self {
        Self {
            code: warning.code.into_owned(),
            message: warning.message,
        }
    }
}

impl std::fmt::Display for WarningLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "warning: {}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub mappings: Vec<Mapping>,
    /// Standard controls with no mapping, in the standard order.
    pub unmapped: Vec<&'static str>,
    pub warnings: Vec<WarningLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mapped {
    pub control: String,
    pub param: String,
    pub stops: [f32; 3],
    /// The mapping this one replaced.
    pub replaced: Option<Mapping>,
}

/// An edit's result, and the edited extension's warnings for the model it
/// leaves, which never fail the edit.
#[derive(Debug)]
pub struct Edited<T> {
    pub result: T,
    pub warnings: Vec<WarningLine>,
}

#[must_use]
pub fn controls() -> Vec<StandardControl> {
    CONTROLS
        .iter()
        .map(|c| StandardControl {
            name: c.name,
            one_sided: c.one_sided,
        })
        .collect()
}

pub fn list(path: &Path) -> Result<Listing, Error> {
    let clm = file::read(path)?;
    let humanoid: Humanoid = extension::for_edit(&clm, path)?;
    let mappings: Vec<Mapping> = humanoid
        .mappings()
        .map(|(name, m)| Mapping {
            control: name.to_string(),
            param: m.param.to_string(),
            stops: m.stops,
        })
        .collect();
    let unmapped = CONTROLS
        .iter()
        .filter(|c| humanoid.mapping(c.name).is_none())
        .map(|c| c.name)
        .collect();
    Ok(Listing {
        mappings,
        unmapped,
        warnings: warning_lines(extension::warnings(&clm, path, Humanoid::KEY)?),
    })
}

/// Without `stops`, a two-sided control spans the param's min, default and
/// max, and a one-sided control rests at the default and reaches the end of
/// the range farther from it.
pub fn map(
    path: &Path,
    control_name: &str,
    param: &str,
    stops: Option<[f32; 3]>,
    nonstandard: bool,
) -> Result<Edited<Mapped>, Error> {
    let names = if nonstandard {
        ControlNames::AllowNonstandard
    } else {
        ControlNames::StandardOnly
    };
    authoring::standard_control(control_name, names).map_err(|e| authoring_error(e, path))?;
    let mut clm = file::read(path)?;
    let (mut model, _) = file::load(&clm, path)?;
    let param = parse_param(param)?;
    let mapped = authoring::map(&mut model, control_name, &param, stops, names)
        .map_err(|e| authoring_error(e, path))?;
    let warnings = save(&mut clm, path, &model)?;
    Ok(Edited {
        result: Mapped {
            control: control_name.to_string(),
            param: param.to_string(),
            stops: mapped.mapping.stops,
            replaced: mapped.replaced.map(|m| Mapping {
                control: control_name.to_string(),
                param: m.param.to_string(),
                stops: m.stops,
            }),
        },
        warnings,
    })
}

/// Removing the last mapping drops the extension from the file.
pub fn unmap(path: &Path, control_name: &str) -> Result<Edited<Mapping>, Error> {
    let mut clm = file::read(path)?;
    let (mut model, _) = file::load(&clm, path)?;
    let removed =
        authoring::unmap(&mut model, control_name).map_err(|e| authoring_error(e, path))?;
    let warnings = save(&mut clm, path, &model)?;
    Ok(Edited {
        result: Mapping {
            control: control_name.to_string(),
            param: removed.param.to_string(),
            stops: removed.stops,
        },
        warnings,
    })
}

fn save(clm: &mut ClmFile, path: &Path, model: &Model) -> Result<Vec<WarningLine>, Error> {
    extension::store::<Humanoid>(clm, model)?;
    let bytes = file::encode(clm, path)?;
    file::write(path, &bytes)?;
    Ok(warning_lines(extension::model_warnings(
        model,
        Humanoid::KEY,
    )))
}

pub fn warning_lines(warnings: Vec<Warning>) -> Vec<WarningLine> {
    warnings.into_iter().map(WarningLine::from).collect()
}

pub(crate) fn parse_param(id: &str) -> Result<ParamId, Error> {
    ParamId::new(id).map_err(|source| Error::BadId {
        value: id.to_string(),
        source,
    })
}

pub(crate) fn authoring_error(error: AuthoringError, path: &Path) -> Error {
    match error {
        AuthoringError::EmptyControlName => Error::BadValue {
            field: "a control name".into(),
            expected: "a non-empty name".into(),
            value: String::new(),
        },
        AuthoringError::NonstandardControl { name, nearest } => {
            Error::NonstandardControl { name, nearest }
        }
        AuthoringError::NoParam(id) => Error::NoSuchId {
            path: path.to_path_buf(),
            kind: "param",
            id: id.to_string(),
        },
        AuthoringError::NonFiniteStop { stops, .. } => Error::BadValue {
            field: "--stops".into(),
            expected: "three finite numbers low,mid,high".into(),
            value: format!("{},{},{}", stops[0], stops[1], stops[2]),
        },
        AuthoringError::NotMapped(control) => Error::NotMapped {
            path: path.to_path_buf(),
            control,
        },
        AuthoringError::Unreadable(e) => extension::unreadable::<Humanoid>(e, path),
    }
}

/// `low,mid,high`, each finite.
pub fn parse_stops(text: &str) -> Result<[f32; 3], Error> {
    let bad = || Error::BadValue {
        field: "--stops".into(),
        expected: "three finite numbers low,mid,high".into(),
        value: text.to_string(),
    };
    let stops: Vec<f32> = text
        .split(',')
        .map(|s| s.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| bad())?;
    let stops: [f32; 3] = stops.try_into().map_err(|_| bad())?;
    if !stops.iter().all(|s| s.is_finite()) {
        return Err(bad());
    }
    Ok(stops)
}

/// `name=value` with a finite value, as `--control` and `--param` take it.
pub fn parse_assignment(flag: &str, text: &str) -> Result<(String, f32), Error> {
    let bad = |expected: &str| Error::BadValue {
        field: flag.to_string(),
        expected: expected.to_string(),
        value: text.to_string(),
    };
    let (name, value) = text.rsplit_once('=').ok_or_else(|| bad("name=value"))?;
    let value: f32 = value
        .trim()
        .parse()
        .map_err(|_| bad("name=value with a numeric value"))?;
    if !value.is_finite() {
        return Err(bad("name=value with a finite value"));
    }
    Ok((name.to_string(), value))
}
