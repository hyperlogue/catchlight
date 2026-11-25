//! The official `catchlight.expressions` extension: a model's named
//! expressions, such as emotions and visemes. Each gives values to humanoid
//! controls or to the model's own params, and plays by weight.
//!
//! Control settings act before the control mappings, param settings after:
//!
//! ```no_run
//! # use catchlight_core::{Model, Puppet};
//! # use catchlight_extension_animations::AnimationPlayer;
//! # use catchlight_extension_expressions::{ExpressionWeights, Expressions};
//! # use catchlight_extension_humanoid::{controls_from_arkit, Humanoid};
//! # fn frame(
//! #     model: &Model,
//! #     puppet: &mut Puppet,
//! #     player: &mut AnimationPlayer,
//! #     tracking: impl Fn(&str) -> f32,
//! #     dt: f32,
//! # ) {
//! let mut controls = controls_from_arkit(tracking);
//! let mut weights = ExpressionWeights::new();
//! weights.set("happy", 0.6);
//! weights.set("aa", 1.0);
//! let expressions = model.extension::<Expressions>().ok();
//! if let Some(expressions) = expressions {
//!     expressions.blend_controls(&weights, &mut controls);
//! }
//! if let Ok(humanoid) = model.extension::<Humanoid>() {
//!     humanoid.pose(&controls, puppet);
//! }
//! if let Some(expressions) = expressions {
//!     expressions.blend_params(&weights, puppet);
//! }
//! player.advance(puppet, dt);
//! puppet.tick(model, dt);
//! # }
//! ```

mod standard;

pub use standard::{standard_visemes, EMOTIONS, VISEMES, VISEME_CONTROLS};

use std::collections::BTreeMap;

use catchlight_core::extension::{
    DecodeError, Extension, ExtensionError, IdRef, MergeRefused, Payload, RawExtension, Receipt,
    RemovedRef, Warning,
};
use catchlight_core::{Model, ParamId, Puppet};
use catchlight_extension_humanoid::{control, ControlValues, Humanoid};

/// The warning codes [`Expressions`] raises from its check.
pub mod codes {
    pub const MAPPED_PARAM: &str = "catchlight.expressions/mapped-param";
    pub const UNMAPPED_CONTROL: &str = "catchlight.expressions/unmapped-control";
    pub const NONSTANDARD_CONTROL: &str = "catchlight.expressions/nonstandard-control";
    pub const VALUE_OUT_OF_RANGE: &str = "catchlight.expressions/value-out-of-range";
    pub const NON_FINITE_VALUE: &str = "catchlight.expressions/non-finite-value";
    pub const DUPLICATE_TARGET: &str = "catchlight.expressions/duplicate-target";
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A humanoid control by name; the value is in the control's range.
    Control(String),
    /// One of the model's params; the value is a raw param value.
    Param(ParamId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub target: Target,
    pub value: f32,
}

impl Setting {
    pub fn control(name: impl Into<String>, value: f32) -> Self {
        Self {
            target: Target::Control(name.into()),
            value,
        }
    }

    pub fn param(param: ParamId, value: f32) -> Self {
        Self {
            target: Target::Param(param),
            value,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub name: String,
    pub settings: Vec<Setting>,
}

impl Expression {
    pub fn new(name: impl Into<String>, settings: Vec<Setting>) -> Self {
        Self {
            name: name.into(),
            settings,
        }
    }
}

/// How strongly each expression plays, by name; a sparse map onto 0..1.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExpressionWeights {
    weights: BTreeMap<String, f32>,
}

impl ExpressionWeights {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clamps `weight` to 0..1. A NaN is ignored.
    pub fn set(&mut self, name: &str, weight: f32) {
        if weight.is_nan() {
            return;
        }
        let weight = weight.clamp(0.0, 1.0);
        match self.weights.get_mut(name) {
            Some(slot) => *slot = weight,
            None => {
                self.weights.insert(name.to_owned(), weight);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<f32> {
        self.weights.get(name).copied()
    }

    pub fn remove(&mut self, name: &str) -> Option<f32> {
        self.weights.remove(name)
    }

    pub fn clear(&mut self) {
        self.weights.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, f32)> {
        self.weights.iter().map(|(k, &v)| (k.as_str(), v))
    }
}

impl<'a> FromIterator<(&'a str, f32)> for ExpressionWeights {
    fn from_iter<I: IntoIterator<Item = (&'a str, f32)>>(iter: I) -> Self {
        let mut out = Self::new();
        for (name, weight) in iter {
            out.set(name, weight);
        }
        out
    }
}

/// A model's expressions in author order, no two sharing a name. Param
/// settings name their param only through the reference table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Expressions {
    refs: Vec<IdRef>,
    expressions: Vec<stored::Expression>,
}

mod stored {
    use serde::{Deserialize, Deserializer, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct Expressions {
        pub(crate) expressions: Vec<Expression>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct Expression {
        pub(crate) name: String,
        pub(crate) settings: Vec<Setting>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    pub(crate) enum Setting {
        Control(ControlSetting),
        Param(ParamSetting),
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct ControlSetting {
        pub(crate) control: String,
        #[serde(deserialize_with = "value")]
        pub(crate) value: f32,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct ParamSetting {
        /// An index into the reference table.
        pub(crate) param: u32,
        #[serde(deserialize_with = "value")]
        pub(crate) value: f32,
    }

    impl Setting {
        pub(crate) fn value(&self) -> f32 {
            match self {
                Self::Control(s) => s.value,
                Self::Param(s) => s.value,
            }
        }

        pub(crate) fn same_target(&self, other: &Self) -> bool {
            match (self, other) {
                (Self::Control(a), Self::Control(b)) => a.control == b.control,
                (Self::Param(a), Self::Param(b)) => a.param == b.param,
                _ => false,
            }
        }
    }

    /// JSON has no NaN or infinity: serde_json writes them as null, which
    /// reads back as NaN so the data still loads.
    fn value<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(Option::<f32>::deserialize(d)?.unwrap_or(f32::NAN))
    }
}

/// Folds weighted values onto one target:
/// `current + (sum(w * v) / W - current) * min(W, 1)` with `W = sum(w)`, so
/// weights summing to 1 give exactly the weighted mix.
#[derive(Default, Clone, Copy)]
struct Mix {
    weight: f32,
    sum: f32,
}

impl Mix {
    fn add(&mut self, weight: f32, value: f32) {
        self.weight += weight;
        self.sum += weight * value;
    }

    fn onto(self, current: f32) -> f32 {
        let target = self.sum / self.weight;
        if self.weight >= 1.0 {
            target
        } else {
            current + (target - current) * self.weight
        }
    }
}

impl Expressions {
    pub fn from_expressions(expressions: impl IntoIterator<Item = Expression>) -> Self {
        let mut out = Self::default();
        for expression in expressions {
            out.insert(expression);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.expressions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.expressions.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.expressions.iter().map(|e| e.name.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.position(name).is_some()
    }

    /// A param setting whose param was removed is left out until reconcile
    /// drops it.
    pub fn get(&self, name: &str) -> Option<Expression> {
        self.position(name)
            .map(|i| self.resolve(&self.expressions[i]))
    }

    /// Every expression in author order, as [`Self::get`] reads it.
    pub fn expressions(&self) -> Vec<Expression> {
        self.expressions.iter().map(|e| self.resolve(e)).collect()
    }

    /// Adds `expression`, or replaces the one of the same name in place and
    /// returns it.
    pub fn insert(&mut self, expression: Expression) -> Option<Expression> {
        let stored = stored::Expression {
            name: expression.name,
            settings: expression
                .settings
                .into_iter()
                .map(|s| match s.target {
                    Target::Control(control) => stored::Setting::Control(stored::ControlSetting {
                        control,
                        value: s.value,
                    }),
                    Target::Param(param) => stored::Setting::Param(stored::ParamSetting {
                        param: self.intern(IdRef::Param(param)),
                        value: s.value,
                    }),
                })
                .collect(),
        };
        let previous = match self.position(&stored.name) {
            Some(i) => {
                let old = std::mem::replace(&mut self.expressions[i], stored);
                Some(self.resolve(&old))
            }
            None => {
                self.expressions.push(stored);
                None
            }
        };
        self.compact();
        previous
    }

    pub fn remove(&mut self, name: &str) -> Option<Expression> {
        let i = self.position(name)?;
        let expression = self.expressions.remove(i);
        let out = self.resolve(&expression);
        self.compact();
        Some(out)
    }

    /// Blends the weighted expressions' control settings into `controls`. A
    /// control with no value yet counts as 0.
    pub fn blend_controls(&self, weights: &ExpressionWeights, controls: &mut ControlValues) {
        let mut mixes: Vec<(&str, Mix)> = Vec::new();
        for (expression, weight) in self.weighted(weights) {
            for (i, setting) in expression.settings.iter().enumerate() {
                let stored::Setting::Control(s) = setting else {
                    continue;
                };
                if !playable(&expression.settings, i) {
                    continue;
                }
                let at = match mixes.iter().position(|(name, _)| *name == s.control) {
                    Some(at) => at,
                    None => {
                        mixes.push((s.control.as_str(), Mix::default()));
                        mixes.len() - 1
                    }
                };
                mixes[at].1.add(weight, s.value);
            }
        }
        for (name, mix) in mixes {
            let current = controls.get(name).unwrap_or(0.0);
            controls.set(name, mix.onto(current));
        }
    }

    /// Blends the weighted expressions' param settings onto `puppet`, from
    /// each param's posed value. A param a motion response drives is skipped.
    pub fn blend_params(&self, weights: &ExpressionWeights, puppet: &mut Puppet) {
        let mut mixes: Vec<(u32, Mix)> = Vec::new();
        for (expression, weight) in self.weighted(weights) {
            for (i, setting) in expression.settings.iter().enumerate() {
                let stored::Setting::Param(s) = setting else {
                    continue;
                };
                if !playable(&expression.settings, i) {
                    continue;
                }
                let at = match mixes.iter().position(|(param, _)| *param == s.param) {
                    Some(at) => at,
                    None => {
                        mixes.push((s.param, Mix::default()));
                        mixes.len() - 1
                    }
                };
                mixes[at].1.add(weight, s.value);
            }
        }
        for (index, mix) in mixes {
            let IdRef::Param(param) = &self.refs[index as usize] else {
                continue;
            };
            if puppet.is_driven(param) {
                continue;
            }
            let current = puppet
                .param_value_posed(param)
                .or_else(|| puppet.param_limits(param).map(|l| l.default))
                .unwrap_or(0.0);
            puppet.set_param_value(param, mix.onto(current));
        }
    }

    fn weighted<'a>(
        &'a self,
        weights: &'a ExpressionWeights,
    ) -> impl Iterator<Item = (&'a stored::Expression, f32)> + 'a {
        weights
            .iter()
            .filter(|(_, w)| *w > 0.0)
            .filter_map(|(name, w)| {
                let i = self.position(name)?;
                Some((&self.expressions[i], w))
            })
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.expressions.iter().position(|e| e.name == name)
    }

    fn resolve(&self, expression: &stored::Expression) -> Expression {
        Expression {
            name: expression.name.clone(),
            settings: expression
                .settings
                .iter()
                .filter_map(|s| match s {
                    stored::Setting::Control(s) => Some(Setting::control(&s.control, s.value)),
                    stored::Setting::Param(s) => match self.refs.get(s.param as usize)? {
                        IdRef::Param(param) => Some(Setting::param(param.clone(), s.value)),
                        _ => None,
                    },
                })
                .collect(),
        }
    }

    fn intern(&mut self, r: IdRef) -> u32 {
        let at = self.refs.iter().position(|x| *x == r).unwrap_or_else(|| {
            self.refs.push(r);
            self.refs.len() - 1
        });
        at as u32
    }

    /// Keeps exactly the references some setting uses, in first-use order.
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.refs);
        let mut expressions = std::mem::take(&mut self.expressions);
        for setting in expressions.iter_mut().flat_map(|e| &mut e.settings) {
            if let stored::Setting::Param(s) = setting {
                s.param = self.intern(old[s.param as usize].clone());
            }
        }
        self.expressions = expressions;
    }
}

/// Whether the setting at `i` takes part in blending: its value is finite,
/// and no later setting in the expression has the same target.
fn playable(settings: &[stored::Setting], i: usize) -> bool {
    settings[i].value().is_finite()
        && !settings[i + 1..]
            .iter()
            .any(|s| s.same_target(&settings[i]))
}

impl Extension for Expressions {
    const KEY: &'static str = "catchlight.expressions";
    const VERSION: u32 = 1;

    fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
        let Payload::Json(value) = raw.payload else {
            return Err(DecodeError::new("the payload is bytes, not JSON"));
        };
        let stored: stored::Expressions =
            serde_json::from_value(value).map_err(DecodeError::new)?;
        for (i, expression) in stored.expressions.iter().enumerate() {
            if stored.expressions[..i]
                .iter()
                .any(|e| e.name == expression.name)
            {
                return Err(DecodeError::new(format!(
                    "two expressions are named {:?}",
                    expression.name
                )));
            }
            for setting in &expression.settings {
                let stored::Setting::Param(s) = setting else {
                    continue;
                };
                match raw.refs.get(s.param as usize) {
                    Some(IdRef::Param(_) | IdRef::Removed(RemovedRef::Param(_))) => {}
                    _ => {
                        return Err(DecodeError::new(format!(
                            "a setting of expression {:?} names reference {}, which is not a \
                             param",
                            expression.name, s.param
                        )))
                    }
                }
            }
        }
        Ok(Self {
            refs: raw.refs,
            expressions: stored.expressions,
        })
    }

    fn encode(&self) -> RawExtension {
        let stored = stored::Expressions {
            expressions: self.expressions.clone(),
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

    fn refs_mut(&mut self) -> &mut Vec<IdRef> {
        &mut self.refs
    }

    fn check(&self, model: &Model) -> Vec<Warning> {
        // `None` when the humanoid data cannot be read, so mappings are unknown.
        let humanoid = match model.extension::<Humanoid>() {
            Ok(humanoid) => Some(Some(humanoid)),
            Err(ExtensionError::Missing) => Some(None),
            Err(_) => None,
        };
        let mut out = Vec::new();
        let mut unmapped_without_humanoid = false;
        for expression in &self.expressions {
            let name = &expression.name;
            let mut unmapped: Vec<&str> = Vec::new();
            for (i, setting) in expression.settings.iter().enumerate() {
                if expression.settings[..i]
                    .iter()
                    .any(|s| s.same_target(setting))
                {
                    out.push(Warning::new(
                        codes::DUPLICATE_TARGET,
                        format!(
                            "expression {name:?} sets {} twice; the last setting wins",
                            self.describe(setting)
                        ),
                    ));
                }
                let value = setting.value();
                if !value.is_finite() {
                    out.push(Warning::new(
                        codes::NON_FINITE_VALUE,
                        format!(
                            "expression {name:?} gives {} a value that is not finite, so it \
                             does not play",
                            self.describe(setting)
                        ),
                    ));
                }
                match setting {
                    stored::Setting::Control(s) => {
                        let standard = control(&s.control);
                        if standard.is_none() {
                            out.push(Warning::new(
                                codes::NONSTANDARD_CONTROL,
                                format!(
                                    "expression {name:?} sets {:?}, which is not a standard \
                                     humanoid control",
                                    s.control
                                ),
                            ));
                        }
                        let (min, max) = standard.map_or((-1.0, 1.0), |c| (c.min(), c.max()));
                        if value.is_finite() && !(min..=max).contains(&value) {
                            out.push(Warning::new(
                                codes::VALUE_OUT_OF_RANGE,
                                format!(
                                    "expression {name:?} sets control {:?} to {value}, outside \
                                     its range {min}..{max}",
                                    s.control
                                ),
                            ));
                        }
                        match humanoid {
                            Some(Some(h)) if h.mapping(&s.control).is_none() => {
                                if !unmapped.contains(&s.control.as_str()) {
                                    unmapped.push(&s.control);
                                }
                            }
                            Some(None) => unmapped_without_humanoid = true,
                            _ => {}
                        }
                    }
                    stored::Setting::Param(s) => {
                        let r = &self.refs[s.param as usize];
                        let (IdRef::Param(param), Some(Some(h))) = (r, humanoid) else {
                            continue;
                        };
                        let controls: Vec<&str> = h.controls_for(param).collect();
                        if !controls.is_empty() {
                            out.push(
                                Warning::new(
                                    codes::MAPPED_PARAM,
                                    format!(
                                        "expression {name:?} sets {r}, which control {} maps; \
                                         set the control instead",
                                        controls.join(", ")
                                    ),
                                )
                                .with_subjects(vec![r.clone()]),
                            );
                        }
                    }
                }
            }
            if !unmapped.is_empty() {
                out.push(Warning::new(
                    codes::UNMAPPED_CONTROL,
                    format!(
                        "expression {name:?} sets {}, which the model maps onto no param, so \
                         those settings do nothing",
                        unmapped.join(", ")
                    ),
                ));
            }
        }
        if unmapped_without_humanoid {
            out.push(Warning::new(
                codes::UNMAPPED_CONTROL,
                "expressions set humanoid controls, but the model maps no control onto a param, \
                 so those settings do nothing",
            ));
        }
        out
    }

    fn reconcile(&mut self, _model: &Model) {
        let refs = &self.refs;
        for expression in &mut self.expressions {
            expression.settings.retain(|s| match s {
                stored::Setting::Param(s) => !refs[s.param as usize].is_removed(),
                stored::Setting::Control(_) => true,
            });
        }
        self.compact();
    }

    fn merge_addon(&self, addon: &Self) -> Result<(Self, Receipt), MergeRefused> {
        let mut merged = self.clone();
        let mut added = Vec::with_capacity(addon.expressions.len());
        for expression in &addon.expressions {
            if self.contains(&expression.name) {
                return Err(MergeRefused::Conflict(format!(
                    "the model already has an expression named {:?}",
                    expression.name
                )));
            }
            let mut expression = expression.clone();
            for setting in &mut expression.settings {
                if let stored::Setting::Param(s) = setting {
                    s.param = merged.intern(addon.refs[s.param as usize].clone());
                }
            }
            added.push(expression.name.clone());
            merged.expressions.push(expression);
        }
        merged.compact();
        Ok((merged, Receipt::new(added)))
    }

    fn unmerge_addon(&mut self, receipt: &Receipt) {
        let Some(added) = receipt.get::<Vec<String>>() else {
            return;
        };
        self.expressions.retain(|e| !added.contains(&e.name));
        self.compact();
    }
}

impl Expressions {
    fn describe(&self, setting: &stored::Setting) -> String {
        match setting {
            stored::Setting::Control(s) => format!("control {:?}", s.control),
            stored::Setting::Param(s) => self.refs[s.param as usize].to_string(),
        }
    }
}
