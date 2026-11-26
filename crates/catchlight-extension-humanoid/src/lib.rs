//! The official `catchlight.humanoid` extension: the standard humanoid
//! controls, and the mappings that link each one to a model's param, so one
//! source of face and body tracking can pose any model that maps them.
//!
//! ```no_run
//! # use catchlight_core::{Model, Puppet};
//! # use catchlight_extension_humanoid::{controls_from_arkit, Humanoid};
//! # fn frame(model: &Model, puppet: &mut Puppet, arkit: impl Fn(&str) -> f32, head_yaw: f32) {
//! let mut controls = controls_from_arkit(arkit);
//! controls.set("head_yaw", head_yaw);
//! if let Ok(humanoid) = model.extension::<Humanoid>() {
//!     humanoid.pose(&controls, puppet);
//! }
//! puppet.tick(model, 1.0 / 60.0);
//! # }
//! ```

mod arkit;
pub mod authoring;
mod controls;

pub use arkit::{controls_from_arkit, ARKIT_BLEND_SHAPES};
pub use controls::{clamp_control, control, Control, ControlValues, CONTROLS};

use std::collections::{BTreeMap, HashMap};

use catchlight_core::extension::{
    DecodeError, Extension, IdRef, Payload, RawExtension, RemovedRef, Warning,
};
use catchlight_core::{Model, ParamId, Puppet};

/// The warning codes [`Humanoid`] raises from its check.
pub mod codes {
    pub const SHARED_PARAM: &str = "catchlight.humanoid/shared-param";
    pub const NONSTANDARD_CONTROL: &str = "catchlight.humanoid/nonstandard-control";
    pub const DRIVEN_PARAM: &str = "catchlight.humanoid/driven-param";
    pub const NON_FINITE_STOP: &str = "catchlight.humanoid/non-finite-stop";
}

/// Links one control to one param: `stops` are the param values the control's
/// -1, 0 and +1 land on, linear between neighbours. A stop may lie outside
/// the param's range, and the stops may descend.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlMapping {
    pub param: ParamId,
    pub stops: [f32; 3],
}

impl ControlMapping {
    /// The param value for a control value, clamped to -1..1 first.
    pub fn value_at(&self, control: f32) -> f32 {
        value_at(self.stops, control)
    }
}

fn value_at([low, mid, high]: [f32; 3], control: f32) -> f32 {
    let t = control.clamp(-1.0, 1.0);
    if t < 0.0 {
        mid + (low - mid) * -t
    } else {
        mid + (high - mid) * t
    }
}

/// A model's control mappings, at most one per control. Control names are
/// plain strings, so a control from a newer standard survives a round trip.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Humanoid {
    refs: Vec<IdRef>,
    controls: BTreeMap<String, stored::Mapping>,
}

mod stored {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct Humanoid {
        pub(crate) controls: BTreeMap<String, Mapping>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct Mapping {
        /// An index into the reference table.
        pub(crate) param: u32,
        #[serde(deserialize_with = "stops")]
        pub(crate) stops: [f32; 3],
    }

    /// JSON has no NaN or infinity: serde_json writes them as null, which
    /// reads back as NaN so the data still loads.
    fn stops<'de, D: Deserializer<'de>>(d: D) -> Result<[f32; 3], D::Error> {
        let stops = <[Option<f32>; 3]>::deserialize(d)?;
        Ok(stops.map(|s| s.unwrap_or(f32::NAN)))
    }
}

impl Humanoid {
    pub fn len(&self) -> usize {
        self.controls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.controls.is_empty()
    }

    /// `None` also while the mapped param is removed and not yet reconciled.
    pub fn mapping(&self, control: &str) -> Option<ControlMapping> {
        self.resolve(self.controls.get(control)?)
    }

    /// Every mapping by control name, in name order.
    pub fn mappings(&self) -> impl Iterator<Item = (&str, ControlMapping)> {
        self.controls
            .iter()
            .filter_map(|(name, m)| Some((name.as_str(), self.resolve(m)?)))
    }

    /// The controls mapped onto `param`.
    pub fn controls_for<'a>(&'a self, param: &'a ParamId) -> impl Iterator<Item = &'a str> + 'a {
        self.controls
            .iter()
            .filter(
                move |(_, m)| matches!(&self.refs[m.param as usize], IdRef::Param(p) if p == param),
            )
            .map(|(name, _)| name.as_str())
    }

    /// Maps `control`, replacing and returning its previous mapping. A name
    /// outside the standard list is kept; check reports it.
    pub fn set(
        &mut self,
        control: impl Into<String>,
        mapping: ControlMapping,
    ) -> Option<ControlMapping> {
        let stored = stored::Mapping {
            param: self.intern(IdRef::Param(mapping.param)),
            stops: mapping.stops,
        };
        let previous = self.controls.insert(control.into(), stored);
        let out = previous.and_then(|m| self.resolve(&m));
        self.compact();
        out
    }

    pub fn remove(&mut self, control: &str) -> Option<ControlMapping> {
        let removed = self.controls.remove(control)?;
        let out = self.resolve(&removed);
        self.compact();
        out
    }

    /// Sets each mapped param from its control's value. A control without a
    /// value leaves its param alone, and a param a motion response drives is
    /// skipped, as is a non-finite result. The puppet clamps to the param's range when it evaluates.
    pub fn pose(&self, controls: &ControlValues, puppet: &mut Puppet) {
        for (name, m) in &self.controls {
            let Some(value) = controls.get(name) else {
                continue;
            };
            let IdRef::Param(param) = &self.refs[m.param as usize] else {
                continue;
            };
            if puppet.is_driven(param) {
                continue;
            }
            let value = value_at(m.stops, value);
            if value.is_finite() {
                puppet.set_param_value(param, value);
            }
        }
    }

    fn resolve(&self, m: &stored::Mapping) -> Option<ControlMapping> {
        let IdRef::Param(param) = self.refs.get(m.param as usize)? else {
            return None;
        };
        Some(ControlMapping {
            param: param.clone(),
            stops: m.stops,
        })
    }

    fn intern(&mut self, r: IdRef) -> u32 {
        let at = self.refs.iter().position(|x| *x == r).unwrap_or_else(|| {
            self.refs.push(r);
            self.refs.len() - 1
        });
        at as u32
    }

    /// Keeps exactly the references some mapping uses, in first-use order.
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.refs);
        let mut at = HashMap::new();
        for m in self.controls.values_mut() {
            m.param = renumber(&mut self.refs, &mut at, &old[m.param as usize]);
        }
    }
}

/// `r`'s index in `refs`, appending it on first use; `at` indexes `refs`.
fn renumber(refs: &mut Vec<IdRef>, at: &mut HashMap<IdRef, u32>, r: &IdRef) -> u32 {
    *at.entry(r.clone()).or_insert_with(|| {
        refs.push(r.clone());
        (refs.len() - 1) as u32
    })
}

impl Extension for Humanoid {
    const KEY: &'static str = "catchlight.humanoid";
    const VERSION: u32 = 1;

    fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
        let Payload::Json(value) = raw.payload else {
            return Err(DecodeError::new("the payload is bytes, not JSON"));
        };
        let stored: stored::Humanoid = serde_json::from_value(value).map_err(DecodeError::new)?;
        for (name, m) in &stored.controls {
            match raw.refs.get(m.param as usize) {
                Some(IdRef::Param(_) | IdRef::Removed(RemovedRef::Param(_))) => {}
                _ => {
                    return Err(DecodeError::new(format!(
                        "control {name:?} names reference {}, which is not a param",
                        m.param
                    )))
                }
            }
        }
        Ok(Self {
            refs: raw.refs,
            controls: stored.controls,
        })
    }

    fn encode(&self) -> RawExtension {
        let stored = stored::Humanoid {
            controls: self.controls.clone(),
        };
        RawExtension {
            version: Self::VERSION,
            refs: self.refs.clone(),
            // Every field is a string, an integer or a float.
            payload: Payload::Json(serde_json::to_value(stored).unwrap_or_default()),
        }
    }

    fn refs(&self) -> &[IdRef] {
        &self.refs
    }

    fn refs_mut(&mut self) -> &mut [IdRef] {
        &mut self.refs
    }

    fn check(&self, model: &Model) -> Vec<Warning> {
        let mut out = Vec::new();
        for (name, m) in &self.controls {
            let r = &self.refs[m.param as usize];
            if control(name).is_none() {
                out.push(Warning::new(
                    codes::NONSTANDARD_CONTROL,
                    format!("{name:?} is not a standard humanoid control"),
                ));
            }
            if m.stops.iter().any(|s| !s.is_finite()) {
                out.push(
                    Warning::new(
                        codes::NON_FINITE_STOP,
                        format!("control {name:?} maps onto {r} with a stop that is not finite"),
                    )
                    .with_subjects(vec![r.clone()]),
                );
            }
            if let IdRef::Param(param) = r {
                if model.response_owner(param).is_some() {
                    out.push(
                        Warning::new(
                            codes::DRIVEN_PARAM,
                            format!(
                                "control {name:?} maps onto {r}, which a motion response \
                                 drives, so the control never moves it"
                            ),
                        )
                        .with_subjects(vec![r.clone()]),
                    );
                }
            }
        }
        for (index, r) in self.refs.iter().enumerate() {
            let IdRef::Param(_) = r else { continue };
            let names: Vec<&str> = self
                .controls
                .iter()
                .filter(|(_, m)| m.param as usize == index)
                .map(|(name, _)| name.as_str())
                .collect();
            if names.len() > 1 {
                out.push(
                    Warning::new(
                        codes::SHARED_PARAM,
                        format!(
                            "controls {} all map onto {r}, so only one of them takes effect",
                            names.join(", ")
                        ),
                    )
                    .with_subjects(vec![r.clone()]),
                );
            }
        }
        out
    }

    fn reconcile(&mut self, _model: &Model) {
        let refs = &self.refs;
        self.controls
            .retain(|_, m| !refs[m.param as usize].is_removed());
        self.compact();
    }
}
