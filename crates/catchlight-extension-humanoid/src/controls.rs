use std::borrow::Cow;
use std::collections::BTreeMap;

/// One of the standard humanoid controls. Left and right are the character's
/// own; positive x is toward the character's left, positive y up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Control {
    pub name: &'static str,
    /// Uses only 0..1 rather than -1..1.
    pub one_sided: bool,
}

impl Control {
    pub fn min(&self) -> f32 {
        if self.one_sided {
            0.0
        } else {
            -1.0
        }
    }

    pub fn max(&self) -> f32 {
        1.0
    }

    pub fn clamp(&self, value: f32) -> f32 {
        value.clamp(self.min(), self.max())
    }
}

const fn two(name: &'static str) -> Control {
    Control {
        name,
        one_sided: false,
    }
}

const fn one(name: &'static str) -> Control {
    Control {
        name,
        one_sided: true,
    }
}

/// The standard controls: the two-sided ones, then the one-sided ones.
pub const CONTROLS: [Control; 35] = [
    two("head_yaw"),
    two("head_pitch"),
    two("head_roll"),
    two("body_yaw"),
    two("body_pitch"),
    two("body_roll"),
    two("body_move_x"),
    two("body_move_y"),
    two("leg_left_swing"),
    two("leg_right_swing"),
    two("eye_left_open"),
    two("eye_right_open"),
    two("eye_left_gaze_x"),
    two("eye_right_gaze_x"),
    two("eye_left_gaze_y"),
    two("eye_right_gaze_y"),
    two("brow_left_height"),
    two("brow_right_height"),
    two("brow_left_inner_height"),
    two("brow_right_inner_height"),
    two("mouth_smile"),
    two("mouth_width"),
    two("mouth_x"),
    one("eye_left_squint"),
    one("eye_right_squint"),
    one("cheek_left_raise"),
    one("cheek_right_raise"),
    one("cheek_puff"),
    one("mouth_open"),
    one("mouth_funnel"),
    one("mouth_press"),
    one("teeth_upper_height"),
    one("lip_lower_height"),
    one("tongue_out"),
    one("blush"),
];

/// The standard control named `name`; `None` for any other name.
pub fn control(name: &str) -> Option<&'static Control> {
    CONTROLS.iter().find(|c| c.name == name)
}

/// The range a value for `name` is clamped to: the standard control's, or
/// -1..1 for a name outside the standard list.
pub fn clamp_control(name: &str, value: f32) -> f32 {
    match control(name) {
        Some(c) => c.clamp(value),
        None => value.clamp(-1.0, 1.0),
    }
}

/// A sparse set of control values by name. A control without a value leaves
/// its mapped param alone.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControlValues {
    values: BTreeMap<Cow<'static, str>, f32>,
}

impl ControlValues {
    pub fn new() -> Self {
        Self::default()
    }

    /// Clamps `value` to the control's range. A NaN is ignored.
    pub fn set(&mut self, name: &str, value: f32) {
        if value.is_nan() {
            return;
        }
        let value = clamp_control(name, value);
        match self.values.get_mut(name) {
            Some(slot) => *slot = value,
            None => {
                let key = match control(name) {
                    Some(c) => Cow::Borrowed(c.name),
                    None => Cow::Owned(name.to_owned()),
                };
                self.values.insert(key, value);
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<f32> {
        self.values.get(name).copied()
    }

    pub fn remove(&mut self, name: &str) -> Option<f32> {
        self.values.remove(name)
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, f32)> {
        self.values.iter().map(|(k, &v)| (k.as_ref(), v))
    }
}

impl<'a> FromIterator<(&'a str, f32)> for ControlValues {
    fn from_iter<I: IntoIterator<Item = (&'a str, f32)>>(iter: I) -> Self {
        let mut out = Self::new();
        for (name, value) in iter {
            out.set(name, value);
        }
        out
    }
}
