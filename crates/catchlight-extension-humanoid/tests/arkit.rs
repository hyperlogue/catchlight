#![expect(clippy::panic, reason = "a test aborts on a broken fixture")]

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use catchlight_extension_humanoid::{
    ARKIT_BLEND_SHAPES, CONTROLS, ControlValues, controls_from_arkit,
};

fn from(shapes: &[(&str, f32)]) -> ControlValues {
    let shapes: HashMap<&str, f32> = shapes.iter().copied().collect();
    controls_from_arkit(|name| shapes.get(name).copied().unwrap_or(0.0))
}

fn approx(values: &ControlValues, name: &str, want: f32) {
    let got = values.get(name).unwrap_or_else(|| panic!("{name} unset"));
    assert!((got - want).abs() < 1e-6, "{name}: {got} != {want}");
}

#[test]
fn it_reads_exactly_the_43_shapes_it_uses() {
    let read = RefCell::new(BTreeSet::new());
    controls_from_arkit(|name| {
        read.borrow_mut().insert(name.to_owned());
        0.0
    });
    let read = read.into_inner();
    let unused: BTreeSet<String> = [
        "jawForward",
        "mouthDimpleLeft",
        "mouthDimpleRight",
        "mouthRollLower",
        "mouthRollUpper",
        "mouthShrugLower",
        "mouthShrugUpper",
        "noseSneerLeft",
        "noseSneerRight",
    ]
    .map(String::from)
    .into();
    let all: BTreeSet<String> = ARKIT_BLEND_SHAPES.map(String::from).into();
    assert_eq!(all.len(), 52);
    assert_eq!(read.len(), 43);
    assert_eq!(read, &all - &unused);
}

#[test]
fn it_fills_only_the_face_controls() {
    let values = from(&[]);
    let names: Vec<&str> = values.iter().map(|(n, _)| n).collect();
    let face: Vec<&str> = {
        let mut face: Vec<&str> = CONTROLS
            .iter()
            .map(|c| c.name)
            .filter(|n| !n.starts_with("head_") && !n.starts_with("body_"))
            .filter(|n| !n.starts_with("leg_") && *n != "blush")
            .collect();
        face.sort_unstable();
        face
    };
    assert_eq!(names, face);
    assert_eq!(names.len(), 24);
}

#[test]
fn eyes_follow_open_squint_and_gaze_toward_the_characters_left() {
    let v = from(&[
        ("eyeWideLeft", 0.3),
        ("eyeBlinkLeft", 0.1),
        ("eyeBlinkRight", 0.8),
        ("eyeSquintLeft", 0.4),
        ("eyeSquintRight", 0.6),
        ("eyeLookOutLeft", 0.7),
        ("eyeLookInLeft", 0.2),
        ("eyeLookInRight", 0.5),
        ("eyeLookOutRight", 0.1),
        ("eyeLookUpLeft", 0.2),
        ("eyeLookDownLeft", 0.6),
        ("eyeLookUpRight", 0.9),
    ]);
    approx(&v, "eye_left_open", 0.2);
    approx(&v, "eye_right_open", -0.8);
    approx(&v, "eye_left_squint", 0.4);
    approx(&v, "eye_right_squint", 0.6);
    approx(&v, "eye_left_gaze_x", 0.5);
    approx(&v, "eye_right_gaze_x", 0.4);
    approx(&v, "eye_left_gaze_y", -0.4);
    approx(&v, "eye_right_gaze_y", 0.9);
}

#[test]
fn brows_and_cheeks() {
    let v = from(&[
        ("browOuterUpLeft", 0.5),
        ("browDownLeft", 0.2),
        ("browOuterUpRight", 0.1),
        ("browDownRight", 0.6),
        ("browInnerUp", 0.7),
        ("cheekSquintLeft", 0.3),
        ("cheekSquintRight", 0.4),
        ("cheekPuff", 0.9),
    ]);
    approx(&v, "brow_left_height", 0.3);
    approx(&v, "brow_right_height", -0.5);
    approx(&v, "brow_left_inner_height", 0.5);
    approx(&v, "brow_right_inner_height", 0.1);
    approx(&v, "cheek_left_raise", 0.3);
    approx(&v, "cheek_right_raise", 0.4);
    approx(&v, "cheek_puff", 0.9);
}

#[test]
fn mouth() {
    let v = from(&[
        ("jawOpen", 0.7),
        ("mouthClose", 0.2),
        ("mouthSmileLeft", 0.6),
        ("mouthSmileRight", 0.4),
        ("mouthFrownLeft", 0.1),
        ("mouthFrownRight", 0.3),
        ("mouthStretchLeft", 0.2),
        ("mouthStretchRight", 0.4),
        ("mouthPucker", 0.5),
        ("mouthLeft", 0.3),
        ("mouthRight", 0.1),
        ("jawLeft", 0.05),
        ("jawRight", 0.25),
        ("mouthFunnel", 0.35),
        ("mouthPressLeft", 0.2),
        ("mouthPressRight", 0.6),
        ("mouthUpperUpLeft", 0.1),
        ("mouthUpperUpRight", 0.3),
        ("mouthLowerDownLeft", 0.5),
        ("mouthLowerDownRight", 0.7),
        ("tongueOut", 0.8),
    ]);
    approx(&v, "mouth_open", 0.5);
    approx(&v, "mouth_smile", 0.3);
    approx(&v, "mouth_width", -0.2);
    approx(&v, "mouth_x", 0.0);
    approx(&v, "mouth_funnel", 0.35);
    approx(&v, "mouth_press", 0.4);
    approx(&v, "teeth_upper_raise", 0.2);
    approx(&v, "lip_lower_drop", 0.6);
    approx(&v, "tongue_out", 0.8);
}

#[test]
fn every_result_is_clamped_to_its_controls_range() {
    let v = from(&[
        ("mouthClose", 0.6),
        ("mouthLeft", 1.0),
        ("jawLeft", 0.5),
        ("browDownLeft", 1.0),
        ("browInnerUp", 0.0),
    ]);
    approx(&v, "mouth_open", 0.0);
    approx(&v, "mouth_x", 1.0);
    approx(&v, "brow_left_inner_height", -1.0);
    let v = from(&[("mouthRight", 1.0), ("jawRight", 1.0)]);
    approx(&v, "mouth_x", -1.0);
}
