use crate::{Expression, Setting};

/// The standard expression names, each an emotion. A model may add any other
/// name.
pub const EMOTIONS: [&str; 5] = ["happy", "angry", "sad", "relaxed", "surprised"];

/// The standard viseme names, each for the group of sounds it shows. A model
/// may add any other name.
pub const VISEMES: [&str; 21] = [
    "ae", // cat, about, cut
    "aa", // father
    "ao", // thought
    "eh", // bed, book
    "er", // bird
    "iy", // see, yes, bit
    "uw", // too, we
    "ow", // go
    "aw", // now
    "oy", // boy
    "ay", // my
    "hh", // hat
    "rr", // red
    "ll", // let
    "ss", // see, zoo
    "sh", // she, chin, joy
    "th", // this
    "ff", // fan, van
    "dd", // dog, top, no, thin
    "kk", // cat, go, sing
    "pp", // pat, bat, man
];

/// The controls every standard viseme sets, in the order of
/// `VISEME_VALUES`' columns.
pub const VISEME_CONTROLS: [&str; 7] = [
    "mouth_open",
    "mouth_smile",
    "mouth_width",
    "teeth_upper_raise",
    "mouth_funnel",
    "lip_lower_drop",
    "tongue_out",
];

/// One row per name in [`VISEMES`].
const VISEME_VALUES: [[f32; 7]; 21] = [
    [0.80, 0.00, 0.25, 0.90, 0.00, 0.12, 0.00],
    [0.95, -0.10, 0.15, 1.00, 0.00, 0.12, 0.00],
    [0.70, 0.00, -0.65, 1.00, 0.85, 0.12, 0.00],
    [0.45, 0.25, 0.65, 0.20, 0.00, 0.12, 0.00],
    [0.35, 0.00, -0.15, 0.40, 0.00, 0.12, 0.00],
    [0.20, 0.40, 1.00, 0.00, 0.00, 0.10, 0.00],
    [0.40, 0.00, -0.70, 1.00, 1.00, 0.12, 0.00],
    [0.65, 0.00, -0.70, 1.00, 0.60, 0.12, 0.00],
    [0.70, 0.10, 0.20, 0.70, 0.00, 0.12, 0.00],
    [0.60, 0.00, -0.45, 0.80, 0.70, 0.12, 0.00],
    [0.60, 0.30, 0.50, 0.35, 0.00, 0.12, 0.00],
    [0.40, 0.00, 0.10, 0.80, 0.00, 0.12, 0.00],
    [0.32, 0.00, -0.45, 0.60, 0.55, 0.12, 0.00],
    [0.40, 0.10, 0.25, 0.15, 0.00, 0.12, 0.15],
    [0.18, 0.20, 0.65, 0.00, 0.00, 0.08, 0.00],
    [0.23, 0.00, -0.50, 0.10, 0.65, 0.12, 0.00],
    [0.22, 0.00, 0.20, 0.20, 0.00, 0.12, 0.55],
    [0.13, 0.20, 0.50, 0.00, 0.00, 0.00, 0.00],
    [0.18, 0.00, 0.25, 0.20, 0.00, 0.10, 0.12],
    [0.35, 0.05, 0.15, 0.60, 0.00, 0.12, 0.00],
    [0.00, 0.05, 0.00, 0.00, 0.00, 0.00, 0.00],
];

/// The standard visemes as control settings, for a tool to offer as the
/// viseme group's defaults; never written into a model on its own. Zeros are
/// settings too, so a viseme fully replaces the tracked mouth.
#[must_use]
pub fn standard_visemes() -> Vec<Expression> {
    VISEMES
        .iter()
        .zip(VISEME_VALUES)
        .map(|(name, values)| {
            Expression::new(
                *name,
                VISEME_CONTROLS
                    .iter()
                    .zip(values)
                    .map(|(control, value)| Setting::control(*control, value))
                    .collect(),
            )
        })
        .collect()
}
