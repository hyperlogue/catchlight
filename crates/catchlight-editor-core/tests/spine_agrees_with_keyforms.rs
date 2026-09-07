#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A spine's turn against the keyforms the editor bakes for a particle chain.
//!
//! The two describe the same bend, and the sign convention is the whole point
//! of pinning them together: positive turns the tip toward +X, `bend * pi`
//! counterclockwise in the Y-up frame. This is the one place both are in
//! scope, so the check lives here rather than in either crate alone.
//!
//! **They agree exactly where a link's ramp is saturated and deliberately
//! differ inside it.** `chain_keyforms` writes `w * (R d - d)`, a straight
//! blend between the drawn offset and the fully turned one, so a vertex
//! halfway through the crease sits on the chord and the art is a little
//! shorter there. A spine turns by `w * theta` instead, which is a rotation at
//! every `w` and keeps the length. The saturated vertices are where the two
//! must be the same number, and they are.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::id::SeededHex;
use catchlight_core::model::{ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelSpine};
use catchlight_core::{Model, Name, Puppet, Vec2};
use catchlight_editor_core::{chain_keyforms, fit_strand};

const DT: f32 = 1.0 / 60.0;
const ROWS: usize = 9;
const TOP: f32 = 80.0;
const BOTTOM: f32 = -80.0;
const LINKS: u32 = 2;

/// The strip both sides measure: two columns, `ROWS` rows, top row first.
fn strand() -> ClmMesh {
    let mut verts = Vec::with_capacity(ROWS * 4);
    let mut uvs = Vec::with_capacity(ROWS * 4);
    for r in 0..ROWS {
        let t = r as f32 / (ROWS - 1) as f32;
        let y = TOP + (BOTTOM - TOP) * t;
        verts.extend_from_slice(&[-12.0, y, 12.0, y]);
        uvs.extend_from_slice(&[0.0, t, 1.0, t]);
    }
    let mut indices = Vec::with_capacity(6 * (ROWS - 1));
    for r in 0..ROWS - 1 {
        let v = (r * 2) as u16;
        indices.extend_from_slice(&[v, v + 1, v + 3, v, v + 3, v + 2]);
    }
    ClmMesh {
        verts,
        uvs,
        indices: ClmIndices::U16(indices),
        origin: [0.0, 0.0],
    }
}

/// The strip hung off a spine laid along the fit: the spine sits on the fit's
/// root and the art hangs back under it at the place it was drawn.
fn spine_offsets(mesh: &ClmMesh, lengths: &[f32], root: [f32; 2], bends: &[f32]) -> Vec<Vec2> {
    let mut hex = SeededHex::new(3);
    let mut model = Model::new();
    let model_root = model.root().expect("root").clone();
    let params: Vec<_> = (0..lengths.len())
        .map(|i| {
            model
                .add_param(
                    ModelParam {
                        name: Name::truncated(format!("bend{i}")),
                        min: -1.0,
                        max: 1.0,
                        default: 0.0,
                        key_positions: vec![0.0, 0.5, 1.0],
                    },
                    &mut hex,
                )
                .expect("add param")
        })
        .collect();
    let mut travelled = 0.0f32;
    let joints: Vec<[f32; 2]> = lengths
        .iter()
        .map(|l| {
            travelled += l;
            [0.0, -travelled]
        })
        .collect();
    let mut spine_node = ModelNode::new("spine", ModelNodeKind::Spine(ModelSpine::new(joints)));
    spine_node.transform.translation = [root[0], root[1], 0.0];
    let spine = model
        .add_node(&model_root, spine_node, &mut hex)
        .expect("add the spine");
    let mut art = ModelNode::new("art", ModelNodeKind::Part(ModelPart::new(mesh.clone())));
    art.transform.translation = [-root[0], -root[1], 0.0];
    let part = model.add_node(&spine, art, &mut hex).expect("add the art");
    model
        .set_spine_targets(&spine, params.iter().cloned().map(Some).collect())
        .expect("aim the spine");

    let mut puppet = Puppet::new(&model);
    for (param, &b) in params.iter().zip(bends) {
        puppet.set_param_value(param, b);
    }
    puppet.tick(&model, DT);
    let idx = puppet.node_idx(&part).expect("baked");
    puppet.combined_deform(idx).expect("a stack").to_vec()
}

/// Arc length down the strand for the vertex in row `row`.
fn arc(row: usize) -> f32 {
    TOP - (TOP + (BOTTOM - TOP) * (row as f32 / (ROWS - 1) as f32))
}

/// Past the end of a bent link the ramp is saturated, and there the spine's
/// offset is `chain_keyforms`' offset — same direction, same size, on both
/// signs of the bend.
#[test]
fn a_saturated_ramp_matches_the_keyform_cell() {
    let mesh = strand();
    let fit = fit_strand(&mesh, LINKS, None).expect("the strip is a fittable strand");
    let span = fit.lengths[0];
    for bend in [0.5, 1.0 / 6.0, -1.0 / 3.0] {
        let keyform = chain_keyforms(&mesh, &fit, 0, bend);
        let spine = spine_offsets(&mesh, &fit.lengths, fit.root, &[bend, 0.0]);
        let mut checked = 0;
        for row in 0..ROWS {
            // Only where the first link's ramp has reached 1: inside the
            // crease the two shapes differ on purpose.
            if arc(row) < span - 1e-3 {
                continue;
            }
            for col in 0..2 {
                let i = row * 2 + col;
                let want = Vec2::new(keyform[i * 2], keyform[i * 2 + 1]);
                assert!(
                    (spine[i] - want).length() < 1e-3,
                    "bend {bend}, vertex {i}: spine {:?}, keyform {want:?}",
                    spine[i]
                );
                checked += 1;
            }
        }
        assert!(checked >= 8, "only {checked} saturated vertices checked");
    }
}

/// Inside the crease the spine keeps the art's length and the keyform does
/// not: the keyform blends toward the turned offset along a chord, so its
/// vertex sits closer to the joint than the drawn one did.
#[test]
fn inside_the_ramp_the_spine_keeps_the_length_the_keyform_loses() {
    let mesh = strand();
    let fit = fit_strand(&mesh, LINKS, None).expect("a fittable strand");
    let bend = 0.5;
    let keyform = chain_keyforms(&mesh, &fit, 0, bend);
    let spine = spine_offsets(&mesh, &fit.lengths, fit.root, &[bend, 0.0]);
    // Row 2 of nine sits a quarter of the way down the first link.
    let i = 2 * 2;
    let rest = Vec2::new(mesh.verts[i * 2], mesh.verts[i * 2 + 1]);
    let joint = Vec2::new(fit.root[0], fit.root[1]);
    let drawn = (rest - joint).length();
    let by_spine = (rest + spine[i] - joint).length();
    let by_keyform = (rest + Vec2::new(keyform[i * 2], keyform[i * 2 + 1]) - joint).length();
    assert!(
        (by_spine - drawn).abs() < 1e-3,
        "the spine moved the vertex from {drawn} to {by_spine} from its joint"
    );
    assert!(
        by_keyform < drawn - 1e-2,
        "the keyform was expected to shorten {drawn} and gave {by_keyform}"
    );
}
