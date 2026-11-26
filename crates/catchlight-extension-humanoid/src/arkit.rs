use crate::ControlValues;

/// `ARKit`'s 52 face blend shapes, by `ARKit`'s own names. Its left and right
/// are the face's own, as ours are.
pub const ARKIT_BLEND_SHAPES: [&str; 52] = [
    "browDownLeft",
    "browDownRight",
    "browInnerUp",
    "browOuterUpLeft",
    "browOuterUpRight",
    "cheekPuff",
    "cheekSquintLeft",
    "cheekSquintRight",
    "eyeBlinkLeft",
    "eyeBlinkRight",
    "eyeLookDownLeft",
    "eyeLookDownRight",
    "eyeLookInLeft",
    "eyeLookInRight",
    "eyeLookOutLeft",
    "eyeLookOutRight",
    "eyeLookUpLeft",
    "eyeLookUpRight",
    "eyeSquintLeft",
    "eyeSquintRight",
    "eyeWideLeft",
    "eyeWideRight",
    "jawForward",
    "jawLeft",
    "jawOpen",
    "jawRight",
    "mouthClose",
    "mouthDimpleLeft",
    "mouthDimpleRight",
    "mouthFrownLeft",
    "mouthFrownRight",
    "mouthFunnel",
    "mouthLeft",
    "mouthLowerDownLeft",
    "mouthLowerDownRight",
    "mouthPressLeft",
    "mouthPressRight",
    "mouthPucker",
    "mouthRight",
    "mouthRollLower",
    "mouthRollUpper",
    "mouthShrugLower",
    "mouthShrugUpper",
    "mouthSmileLeft",
    "mouthSmileRight",
    "mouthStretchLeft",
    "mouthStretchRight",
    "mouthUpperUpLeft",
    "mouthUpperUpRight",
    "noseSneerLeft",
    "noseSneerRight",
    "tongueOut",
];

/// The face controls from `ARKit` blend shape coefficients, each 0..1, which
/// `shape` looks up by `ARKit` name (0 for a shape the source lacks). Head,
/// body, leg and blush controls have no blend shape and stay unset.
pub fn controls_from_arkit(shape: impl Fn(&str) -> f32) -> ControlValues {
    let mean = |a: &str, b: &str| (shape(a) + shape(b)) / 2.0;
    let mut out = ControlValues::new();
    let mut set = |name: &str, value: f32| out.set(name, value);

    set(
        "eye_left_open",
        shape("eyeWideLeft") - shape("eyeBlinkLeft"),
    );
    set(
        "eye_right_open",
        shape("eyeWideRight") - shape("eyeBlinkRight"),
    );
    set("eye_left_squint", shape("eyeSquintLeft"));
    set("eye_right_squint", shape("eyeSquintRight"));
    // Looking out with the left eye, and in with the right, is toward the
    // character's left.
    set(
        "eye_left_gaze_x",
        shape("eyeLookOutLeft") - shape("eyeLookInLeft"),
    );
    set(
        "eye_right_gaze_x",
        shape("eyeLookInRight") - shape("eyeLookOutRight"),
    );
    set(
        "eye_left_gaze_y",
        shape("eyeLookUpLeft") - shape("eyeLookDownLeft"),
    );
    set(
        "eye_right_gaze_y",
        shape("eyeLookUpRight") - shape("eyeLookDownRight"),
    );
    set(
        "brow_left_height",
        shape("browOuterUpLeft") - shape("browDownLeft"),
    );
    set(
        "brow_right_height",
        shape("browOuterUpRight") - shape("browDownRight"),
    );
    let inner_up = shape("browInnerUp");
    set("brow_left_inner_height", inner_up - shape("browDownLeft"));
    set("brow_right_inner_height", inner_up - shape("browDownRight"));
    set("cheek_left_raise", shape("cheekSquintLeft"));
    set("cheek_right_raise", shape("cheekSquintRight"));
    set("cheek_puff", shape("cheekPuff"));
    set("mouth_open", shape("jawOpen") - shape("mouthClose"));
    set(
        "mouth_smile",
        mean("mouthSmileLeft", "mouthSmileRight") - mean("mouthFrownLeft", "mouthFrownRight"),
    );
    set(
        "mouth_width",
        mean("mouthStretchLeft", "mouthStretchRight") - shape("mouthPucker"),
    );
    set(
        "mouth_x",
        (shape("mouthLeft") - shape("mouthRight")) + (shape("jawLeft") - shape("jawRight")),
    );
    set("mouth_funnel", shape("mouthFunnel"));
    set("mouth_press", mean("mouthPressLeft", "mouthPressRight"));
    set(
        "teeth_upper_raise",
        mean("mouthUpperUpLeft", "mouthUpperUpRight"),
    );
    set(
        "lip_lower_drop",
        mean("mouthLowerDownLeft", "mouthLowerDownRight"),
    );
    set("tongue_out", shape("tongueOut"));
    out
}
