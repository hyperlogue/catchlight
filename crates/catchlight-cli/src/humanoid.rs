//! `humanoid`: the control mappings of the official `catchlight.humanoid`
//! extension, edited in the file's structure section. No image is decoded.

use std::path::Path;

use catchlight_core::extension::{Extension, Warning};
use catchlight_core::formats::clm::{ClmFile, ClmParam};
use catchlight_core::ParamId;
use catchlight_extension_humanoid::{control, ControlMapping, Humanoid, CONTROLS};
use serde::Serialize;

use crate::{extension, file, Error};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StandardControl {
    pub name: &'static str,
    pub one_sided: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mapping {
    pub control: String,
    /// `None` for a mapping whose param was removed and not yet reconciled.
    pub param: Option<String>,
    pub stops: Option<[f32; 3]>,
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
pub struct Edited<T> {
    pub result: T,
    pub warnings: Vec<WarningLine>,
}

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
            param: Some(m.param.to_string()),
            stops: Some(m.stops),
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
/// max, and a one-sided control rests at the default and reaches the max.
pub fn map(
    path: &Path,
    control_name: &str,
    param: &str,
    stops: Option<[f32; 3]>,
    nonstandard: bool,
) -> Result<Edited<Mapped>, Error> {
    let standard = standard_control(control_name, nonstandard)?;
    let mut clm = file::read(path)?;
    file::load(&clm, path)?;
    let param = find_param(&clm, path, param)?;
    let stops = match stops {
        Some(stops) => stops,
        None if standard.is_some_and(|c| c.one_sided) => [param.default, param.default, param.max],
        None => [param.min, param.default, param.max],
    };
    let param = param.id.clone();
    let mut humanoid: Humanoid = extension::for_edit(&clm, path)?;
    let replaced = humanoid
        .set(
            control_name,
            ControlMapping {
                param: param.clone(),
                stops,
            },
        )
        .map(|m| Mapping {
            control: control_name.to_string(),
            param: Some(m.param.to_string()),
            stops: Some(m.stops),
        });
    let warnings = save(&mut clm, path, &humanoid)?;
    Ok(Edited {
        result: Mapped {
            control: control_name.to_string(),
            param: param.to_string(),
            stops,
            replaced,
        },
        warnings,
    })
}

/// Removing the last mapping drops the extension from the file.
pub fn unmap(path: &Path, control_name: &str) -> Result<Edited<Mapping>, Error> {
    let mut clm = file::read(path)?;
    file::load(&clm, path)?;
    let mut humanoid: Humanoid = extension::for_edit(&clm, path)?;
    let before = humanoid.len();
    let removed = humanoid.remove(control_name);
    if humanoid.len() == before {
        return Err(Error::NotMapped {
            path: path.to_path_buf(),
            control: control_name.to_string(),
        });
    }
    let warnings = save(&mut clm, path, &humanoid)?;
    Ok(Edited {
        result: Mapping {
            control: control_name.to_string(),
            param: removed.as_ref().map(|m| m.param.to_string()),
            stops: removed.map(|m| m.stops),
        },
        warnings,
    })
}

fn save(clm: &mut ClmFile, path: &Path, humanoid: &Humanoid) -> Result<Vec<WarningLine>, Error> {
    extension::store(clm, humanoid, humanoid.is_empty())?;
    let bytes = file::encode(clm, path)?;
    let warnings = extension::warnings(clm, path, Humanoid::KEY)?;
    file::write(path, &bytes)?;
    Ok(warning_lines(warnings))
}

pub(crate) fn warning_lines(warnings: Vec<Warning>) -> Vec<WarningLine> {
    warnings.into_iter().map(WarningLine::from).collect()
}

pub(crate) fn find_param<'a>(
    clm: &'a ClmFile,
    path: &Path,
    id: &str,
) -> Result<&'a ClmParam, Error> {
    let parsed = ParamId::new(id).map_err(|source| Error::BadId {
        value: id.to_string(),
        source,
    })?;
    clm.doc
        .params
        .iter()
        .find(|p| p.id == parsed)
        .ok_or_else(|| Error::NoSuchId {
            path: path.to_path_buf(),
            kind: "param",
            id: id.to_string(),
        })
}

/// The standard control named `name`. A name outside the list is refused
/// unless `nonstandard`, which yields `None`.
pub(crate) fn standard_control(
    name: &str,
    nonstandard: bool,
) -> Result<Option<&'static catchlight_extension_humanoid::Control>, Error> {
    if let Some(c) = control(name) {
        return Ok(Some(c));
    }
    if name.is_empty() {
        return Err(Error::BadValue {
            field: "a control name".into(),
            expected: "a non-empty name".into(),
            value: String::new(),
        });
    }
    if nonstandard {
        return Ok(None);
    }
    Err(Error::NonstandardControl {
        name: name.to_string(),
        nearest: nearest(name, CONTROLS.iter().map(|c| c.name)),
    })
}

/// The candidate within a few edits of `name`, for a "did you mean".
pub(crate) fn nearest(
    name: &str,
    candidates: impl Iterator<Item = &'static str>,
) -> Option<&'static str> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_near_miss_suggests_the_standard_name() {
        let near = |n| nearest(n, CONTROLS.iter().map(|c| c.name));
        assert_eq!(near("mouth_opn"), Some("mouth_open"));
        assert_eq!(near("head_yaww"), Some("head_yaw"));
        assert_eq!(near("tail_wag_speed"), None);
    }
}
