//! The official `catchlight.expressions` extension: a model's named
//! expressions, such as emotions, and beside them its visemes. Each gives
//! values to humanoid controls or to the model's own params, and plays by
//! weight; visemes blend over expressions.
//!
//! Control settings act before the control mappings, param settings after.
//! `catchlight_extension_animations::AnimationPlayer::advance_with` applies
//! them in that order each frame, with any clip playing; by hand, for one
//! frame, it is:
//!
//! ```no_run
//! # use catchlight_core::{Model, Puppet};
//! # use catchlight_extension_expressions::{BlendScratch, Expressions, Weights};
//! # use catchlight_extension_humanoid::{controls_from_arkit, Humanoid};
//! # fn frame(model: &Model, puppet: &mut Puppet, tracking: impl Fn(&str) -> f32, dt: f32) {
//! let mut controls = controls_from_arkit(tracking);
//! let expressions: Weights = [("happy", 0.6)].into_iter().collect();
//! let visemes: Weights = [("aa", 1.0)].into_iter().collect();
//! let data = model.extension::<Expressions>().ok();
//! let mut scratch = BlendScratch::default();
//! if let Some(data) = data {
//!     data.blend_controls(&expressions, &visemes, &mut controls, &mut scratch);
//! }
//! if let Ok(humanoid) = model.extension::<Humanoid>() {
//!     humanoid.pose(&controls, puppet);
//! }
//! if let Some(data) = data {
//!     data.blend_params(&expressions, &visemes, puppet, &mut scratch);
//! }
//! puppet.tick(model, dt);
//! # }
//! ```

pub mod authoring;
mod standard;

pub use standard::{EMOTIONS, VISEME_CONTROLS, VISEMES, standard_visemes};

use std::collections::{BTreeMap, HashMap};

use catchlight_core::extension::{
    DecodeError, Extension, ExtensionError, IdRef, MergeRefused, Payload, RawExtension, Receipt,
    RemovedRef, Warning,
};
use catchlight_core::{Model, ParamId, Puppet};
use catchlight_extension_humanoid::{ControlValues, Humanoid, control};

/// The warning codes [`Expressions`] raises from its check, for either group.
pub mod codes {
    pub const MAPPED_PARAM: &str = "catchlight.expressions/mapped-param";
    pub const UNMAPPED_CONTROL: &str = "catchlight.expressions/unmapped-control";
    pub const NONSTANDARD_CONTROL: &str = "catchlight.expressions/nonstandard-control";
    pub const VALUE_OUT_OF_RANGE: &str = "catchlight.expressions/value-out-of-range";
    pub const NON_FINITE_VALUE: &str = "catchlight.expressions/non-finite-value";
    pub const DUPLICATE_TARGET: &str = "catchlight.expressions/duplicate-target";
}

/// Which of the two groups an expression belongs to. Each group has its own
/// names, and visemes blend over expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Expression,
    Viseme,
}

impl Group {
    /// In blending order.
    pub const ALL: [Self; 2] = [Self::Expression, Self::Viseme];

    /// What one member is called: `expression` or `viseme`.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Expression => "expression",
            Self::Viseme => "viseme",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

/// A named set of settings: an expression, or a viseme, which is made the
/// same way.
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

/// How strongly each member of one group plays, by name; a sparse map onto
/// 0..1.
#[derive(Debug, Default, PartialEq)]
pub struct Weights {
    weights: BTreeMap<String, f32>,
}

impl Clone for Weights {
    fn clone(&self) -> Self {
        Self {
            weights: self.weights.clone(),
        }
    }

    /// Keeps the entries both hold, so copying the same names every frame
    /// does not allocate.
    fn clone_from(&mut self, source: &Self) {
        self.weights
            .retain(|name, _| source.weights.contains_key(name));
        for (name, &weight) in &source.weights {
            match self.weights.get_mut(name) {
                Some(slot) => *slot = weight,
                None => {
                    self.weights.insert(name.clone(), weight);
                }
            }
        }
    }
}

impl Weights {
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

    pub fn len(&self) -> usize {
        self.weights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, f32)> {
        self.weights.iter().map(|(k, &v)| (k.as_str(), v))
    }
}

impl<'a> FromIterator<(&'a str, f32)> for Weights {
    fn from_iter<I: IntoIterator<Item = (&'a str, f32)>>(iter: I) -> Self {
        let mut out = Self::new();
        for (name, weight) in iter {
            out.set(name, weight);
        }
        out
    }
}

/// A model's expressions and visemes, each group in author order with no two
/// members sharing a name. Param settings name their param only through the
/// reference table, which both groups share.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Expressions {
    refs: Vec<IdRef>,
    /// Indexed by [`Group::index`].
    groups: [Vec<stored::Expression>; 2],
}

mod stored {
    use serde::{Deserialize, Deserializer, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct Expressions {
        #[serde(default)]
        pub(crate) expressions: Vec<Expression>,
        #[serde(default)]
        pub(crate) visemes: Vec<Expression>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct Expression {
        pub(crate) name: String,
        pub(crate) settings: Vec<Setting>,
        /// Whether each setting takes part in blending: its value is finite,
        /// and no later setting has the same target.
        #[serde(skip)]
        pub(crate) playable: Vec<bool>,
    }

    impl Expression {
        pub(crate) fn mark_playable(&mut self) {
            let settings = &self.settings;
            self.playable.clear();
            self.playable
                .extend(settings.iter().enumerate().map(|(i, s)| {
                    s.value().is_finite() && !settings[i + 1..].iter().any(|t| t.same_target(s))
                }));
        }
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

    /// JSON has no NaN or infinity: `serde_json` writes them as null, which
    /// reads back as NaN so the data still loads.
    fn value<'de, D: Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
        Ok(Option::<f32>::deserialize(d)?.unwrap_or(f32::NAN))
    }
}

/// Folds weighted values onto one target:
/// `current + (sum(w * v) / W - current) * min(W, 1)` with `W = sum(w)`, so
/// weights summing to 1 give exactly the weighted mix.
#[derive(Debug, Default, Clone, Copy)]
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

/// Working space that blending reuses, so blending every frame allocates
/// nothing once it has grown to fit the model.
#[derive(Debug, Clone, Default)]
pub struct BlendScratch {
    /// Keyed by the member and setting index that first reached a control.
    controls: Vec<((usize, usize), Mix)>,
    params: Vec<(u32, Mix)>,
    /// Each param's ref index and its value as blended so far, `None` when
    /// the base withheld one.
    blended: Vec<(u32, Option<f32>)>,
    values: Vec<(ParamId, f32)>,
}

/// What [`Expressions::merge_addon`] added to each group.
struct Added([Vec<String>; 2]);

impl Expressions {
    pub fn from_groups(
        expressions: impl IntoIterator<Item = Expression>,
        visemes: impl IntoIterator<Item = Expression>,
    ) -> Self {
        let mut out = Self::default();
        for expression in expressions {
            out.insert(Group::Expression, expression);
        }
        for viseme in visemes {
            out.insert(Group::Viseme, viseme);
        }
        out
    }

    /// Expressions only, no visemes.
    pub fn from_expressions(expressions: impl IntoIterator<Item = Expression>) -> Self {
        Self::from_groups(expressions, [])
    }

    /// How many members `group` has.
    pub fn count(&self, group: Group) -> usize {
        self.group(group).len()
    }

    /// Whether both groups are empty.
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(Vec::is_empty)
    }

    pub fn names(&self, group: Group) -> impl Iterator<Item = &str> {
        self.group(group).iter().map(|e| e.name.as_str())
    }

    pub fn contains(&self, group: Group, name: &str) -> bool {
        self.position(group, name).is_some()
    }

    /// A param setting whose param was removed is left out until reconcile
    /// drops it.
    pub fn get(&self, group: Group, name: &str) -> Option<Expression> {
        self.position(group, name)
            .map(|i| self.resolve(&self.group(group)[i]))
    }

    /// Every member of `group` in author order, as [`Self::get`] reads it.
    pub fn all(&self, group: Group) -> impl Iterator<Item = Expression> {
        self.group(group).iter().map(|e| self.resolve(e))
    }

    /// Adds `expression` to `group`, or replaces the member of the same name
    /// in place and returns it.
    pub fn insert(&mut self, group: Group, expression: Expression) -> Option<Expression> {
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
            playable: Vec::new(),
        };
        let previous = match self.position(group, &stored.name) {
            Some(i) => {
                let old = std::mem::replace(&mut self.groups[group.index()][i], stored);
                Some(self.resolve(&old))
            }
            None => {
                self.groups[group.index()].push(stored);
                None
            }
        };
        self.compact();
        previous
    }

    pub fn remove(&mut self, group: Group, name: &str) -> Option<Expression> {
        let i = self.position(group, name)?;
        let expression = self.groups[group.index()].remove(i);
        let out = self.resolve(&expression);
        self.compact();
        Some(out)
    }

    /// Blends the weighted control settings into `controls`: the expressions
    /// onto what was there, then the visemes onto that. A control with no
    /// value yet counts as 0.
    pub fn blend_controls(
        &self,
        expressions: &Weights,
        visemes: &Weights,
        controls: &mut ControlValues,
        scratch: &mut BlendScratch,
    ) {
        for (group, weights) in Group::ALL.into_iter().zip([expressions, visemes]) {
            let name = |at| self.control_at(group, at);
            self.mix(
                group,
                weights,
                |at, s| matches!(s, stored::Setting::Control(_)).then_some(at),
                |a, b| name(a) == name(b),
                &mut scratch.controls,
            );
            for &(at, mix) in &scratch.controls {
                let Some(name) = name(at) else {
                    continue;
                };
                let current = controls.get(name).unwrap_or(0.0);
                controls.set(name, mix.onto(current));
            }
        }
    }

    /// Blends the weighted param settings onto `puppet`, from each param's
    /// posed value or else its default: expressions, then visemes over them.
    /// A param a motion response drives is skipped.
    pub fn blend_params(
        &self,
        expressions: &Weights,
        visemes: &Weights,
        puppet: &mut Puppet,
        scratch: &mut BlendScratch,
    ) {
        let values = self.param_values(expressions, visemes, scratch, |param| {
            if puppet.is_driven(param) {
                return None;
            }
            Some(
                puppet
                    .param_value_posed(param)
                    .or_else(|| puppet.param_limits(param).map(|l| l.default))
                    .unwrap_or(0.0),
            )
        });
        for (param, value) in values {
            puppet.set_param_value(param, *value);
        }
    }

    /// The value each param the weighted param settings reach blends to, from
    /// the value `base` gives it, in two layers as [`Self::blend_params`]
    /// does. A param `base` gives no value for is left out.
    pub fn param_values<'s>(
        &self,
        expressions: &Weights,
        visemes: &Weights,
        scratch: &'s mut BlendScratch,
        mut base: impl FnMut(&ParamId) -> Option<f32>,
    ) -> &'s [(ParamId, f32)] {
        let out = &mut scratch.blended;
        out.clear();
        for (group, weights) in Group::ALL.into_iter().zip([expressions, visemes]) {
            self.mix(
                group,
                weights,
                |_, s| match s {
                    stored::Setting::Param(s) => Some(s.param),
                    stored::Setting::Control(_) => None,
                },
                |a, b| a == b,
                &mut scratch.params,
            );
            for &(index, mix) in &scratch.params {
                let at = match out.iter().position(|(i, _)| *i == index) {
                    Some(at) => at,
                    None => {
                        let value = match &self.refs[index as usize] {
                            IdRef::Param(param) => base(param),
                            _ => None,
                        };
                        out.push((index, value));
                        out.len() - 1
                    }
                };
                if let Some(value) = &mut out[at].1 {
                    *value = mix.onto(*value);
                }
            }
        }
        scratch.values.clear();
        scratch
            .values
            .extend(out.iter().filter_map(|&(index, value)| {
                match (&self.refs[index as usize], value) {
                    (IdRef::Param(param), Some(value)) => Some((param.clone(), value)),
                    _ => None,
                }
            }));
        &scratch.values
    }

    /// Fills `mixes` with one mix per target the weighted members of `group`
    /// reach, in first-use order. `key` names the target of the setting at a
    /// member and setting index, or `None` to skip it, and `same` tells
    /// whether two keys name one target.
    fn mix<K: Copy>(
        &self,
        group: Group,
        weights: &Weights,
        key: impl Fn((usize, usize), &stored::Setting) -> Option<K>,
        same: impl Fn(K, K) -> bool,
        mixes: &mut Vec<(K, Mix)>,
    ) {
        mixes.clear();
        for (name, weight) in weights.iter().filter(|(_, w)| *w > 0.0) {
            let Some(member) = self.position(group, name) else {
                continue;
            };
            let expression = &self.group(group)[member];
            for (i, setting) in expression.settings.iter().enumerate() {
                if !expression.playable[i] {
                    continue;
                }
                let Some(k) = key((member, i), setting) else {
                    continue;
                };
                let at = match mixes.iter().position(|&(m, _)| same(m, k)) {
                    Some(at) => at,
                    None => {
                        mixes.push((k, Mix::default()));
                        mixes.len() - 1
                    }
                };
                mixes[at].1.add(weight, setting.value());
            }
        }
    }

    /// The control that the setting at a member and setting index of `group`
    /// sets.
    fn control_at(&self, group: Group, (member, setting): (usize, usize)) -> Option<&str> {
        match &self.group(group)[member].settings[setting] {
            stored::Setting::Control(s) => Some(&s.control),
            stored::Setting::Param(_) => None,
        }
    }

    fn group(&self, group: Group) -> &[stored::Expression] {
        &self.groups[group.index()]
    }

    fn position(&self, group: Group, name: &str) -> Option<usize> {
        self.group(group).iter().position(|e| e.name == name)
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

    /// Keeps exactly the references some setting uses, in first-use order:
    /// expressions first, then visemes, and marks which settings play.
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.refs);
        let mut at = HashMap::new();
        for setting in self
            .groups
            .iter_mut()
            .flatten()
            .flat_map(|e| &mut e.settings)
        {
            if let stored::Setting::Param(s) = setting {
                s.param = renumber(&mut self.refs, &mut at, &old[s.param as usize]);
            }
        }
        for expression in self.groups.iter_mut().flatten() {
            expression.mark_playable();
        }
    }

    /// What [`Extension::check`] finds in `group` alone.
    pub fn check_group(&self, group: Group, model: &Model) -> Vec<Warning> {
        // `None` when the humanoid data cannot be read, so mappings are unknown.
        let humanoid = match model.extension::<Humanoid>() {
            Ok(humanoid) => Some(Some(humanoid)),
            Err(ExtensionError::Missing) => Some(None),
            Err(_) => None,
        };
        let mut out = Vec::new();
        let mut unmapped_without_humanoid = false;
        let noun = group.noun();
        for expression in self.group(group) {
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
                            "{noun} {name:?} sets {} twice; the last setting wins",
                            self.describe(setting)
                        ),
                    ));
                }
                let value = setting.value();
                if !value.is_finite() {
                    out.push(Warning::new(
                        codes::NON_FINITE_VALUE,
                        format!(
                            "{noun} {name:?} gives {} a value that is not finite, so it \
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
                                    "{noun} {name:?} sets {:?}, which is not a standard \
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
                                    "{noun} {name:?} sets control {:?} to {value}, outside \
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
                                        "{noun} {name:?} sets {r}, which control {} maps; \
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
                        "{noun} {name:?} sets {}, which the model maps onto no param, so \
                             those settings do nothing",
                        unmapped.join(", ")
                    ),
                ));
            }
        }
        if unmapped_without_humanoid {
            out.push(Warning::new(
                codes::UNMAPPED_CONTROL,
                format!(
                    "{noun}s set humanoid controls, but the model maps no control onto a \
                     param, so those settings do nothing"
                ),
            ));
        }
        out
    }

    /// Whether a setting in `group` names `r`, removed or not.
    pub fn uses(&self, group: Group, r: &IdRef) -> bool {
        self.group(group)
            .iter()
            .flat_map(|e| &e.settings)
            .any(|s| matches!(s, stored::Setting::Param(s) if self.refs[s.param as usize] == *r))
    }

    fn describe(&self, setting: &stored::Setting) -> String {
        match setting {
            stored::Setting::Control(s) => format!("control {:?}", s.control),
            stored::Setting::Param(s) => self.refs[s.param as usize].to_string(),
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

impl Extension for Expressions {
    const KEY: &'static str = "catchlight.expressions";
    const VERSION: u32 = 1;

    fn decode(raw: RawExtension) -> Result<Self, DecodeError> {
        let Payload::Json(value) = raw.payload else {
            return Err(DecodeError::new("the payload is bytes, not JSON"));
        };
        let stored: stored::Expressions =
            serde_json::from_value(value).map_err(DecodeError::new)?;
        let mut groups = [stored.expressions, stored.visemes];
        for (group, members) in Group::ALL.into_iter().zip(&groups) {
            let noun = group.noun();
            for (i, expression) in members.iter().enumerate() {
                if members[..i].iter().any(|e| e.name == expression.name) {
                    return Err(DecodeError::new(format!(
                        "two {noun}s are named {:?}",
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
                                "a setting of {noun} {:?} names reference {}, which is not a \
                                 param",
                                expression.name, s.param
                            )));
                        }
                    }
                }
            }
        }
        for expression in groups.iter_mut().flatten() {
            expression.mark_playable();
        }
        Ok(Self {
            refs: raw.refs,
            groups,
        })
    }

    fn encode(&self) -> RawExtension {
        let [expressions, visemes] = self.groups.clone();
        let stored = stored::Expressions {
            expressions,
            visemes,
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
        Group::ALL
            .into_iter()
            .flat_map(|group| self.check_group(group, model))
            .collect()
    }

    fn reconcile(&mut self, _model: &Model) {
        let refs = &self.refs;
        for expression in self.groups.iter_mut().flatten() {
            expression.settings.retain(|s| match s {
                stored::Setting::Param(s) => !refs[s.param as usize].is_removed(),
                stored::Setting::Control(_) => true,
            });
        }
        self.compact();
    }

    fn merge_addon(&self, addon: &Self) -> Result<(Self, Receipt), MergeRefused> {
        let mut merged = self.clone();
        let mut added: [Vec<String>; 2] = Default::default();
        for group in Group::ALL {
            for expression in addon.group(group) {
                if self.contains(group, &expression.name) {
                    return Err(MergeRefused::Conflict(format!(
                        "the model already has a{} {} named {:?}",
                        if group == Group::Expression { "n" } else { "" },
                        group.noun(),
                        expression.name
                    )));
                }
                let mut expression = expression.clone();
                for setting in &mut expression.settings {
                    if let stored::Setting::Param(s) = setting {
                        s.param = merged.intern(addon.refs[s.param as usize].clone());
                    }
                }
                added[group.index()].push(expression.name.clone());
                merged.groups[group.index()].push(expression);
            }
        }
        merged.compact();
        Ok((merged, Receipt::new(Added(added))))
    }

    fn unmerge_addon(&mut self, receipt: &Receipt) {
        let Some(Added(added)) = receipt.get::<Added>() else {
            return;
        };
        for (members, added) in self.groups.iter_mut().zip(added) {
            members.retain(|e| !added.contains(&e.name));
        }
        self.compact();
    }
}
