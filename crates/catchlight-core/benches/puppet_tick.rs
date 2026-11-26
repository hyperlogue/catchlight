//! Measure the solver alone, its art outputs and the final geometry fold.
//! Pass a scenario substring to select individual workloads.

// A bench is not a test, so the workspace deny on these does not lift.
#![expect(clippy::unwrap_used, reason = "a test aborts on a broken fixture")]

use std::f32::consts::{PI, TAU};
use std::hint::black_box;
use std::time::Instant;

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::id::SeededHex;
use catchlight_core::{
    BindingKey, BindingTarget, InterpolateMode, Model, ModelNode, ModelNodeKind, ModelParam,
    ModelPart, ModelSpine, MotionResponse, Name, NodeId, ParamId, Puppet, ResponseInput,
    ResponseSource, ScalarTarget,
};

/// One frame at 60 Hz.
const DT: f32 = 1.0 / 60.0;
/// Untimed warm-up ticks keep fold memos and scratch allocations warm.
const TICKS: usize = 600;
const WARMUP: usize = 60;

const ROWS: usize = 250;
/// In model pixels.
const HALF_W: f32 = 12.0;
const TOP: f32 = 80.0;
const BOTTOM: f32 = -80.0;
const SPACING: f32 = 40.0;

/// In half turns: a quarter turn each way, in thirds. Mirrors the editor's
/// keyform list so the bench needs no editor crate.
const BEND_KEYS: [f32; 7] = [-0.5, -1.0 / 3.0, -1.0 / 6.0, 0.0, 1.0 / 6.0, 1.0 / 3.0, 0.5];
const BEND_MIN: f32 = -0.5;
const BEND_MAX: f32 = 0.5;

/// In model pixels.
const TURN_SHIFT: f32 = 120.0;
struct Scenario {
    name: &'static str,
    strands: usize,
    links: usize,
    parts: bool,
    responses: bool,
    outputs: bool,
    /// Mutually exclusive with `parts`.
    spines: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "responses_spine_100x5",
        strands: 100,
        links: 5,
        parts: false,
        responses: true,
        outputs: true,
        spines: true,
    },
    Scenario {
        name: "bindings_only",
        strands: 100,
        links: 5,
        parts: true,
        responses: false,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "responses_unbound_10x5",
        strands: 10,
        links: 5,
        parts: false,
        responses: true,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "responses_unbound_100x5",
        strands: 100,
        links: 5,
        parts: false,
        responses: true,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "responses_unbound_400x5",
        strands: 400,
        links: 5,
        parts: false,
        responses: true,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "responses_solver_only_10x5",
        strands: 10,
        links: 5,
        parts: false,
        responses: true,
        outputs: true,
        spines: false,
    },
    Scenario {
        name: "responses_solver_only_100x5",
        strands: 100,
        links: 5,
        parts: false,
        responses: true,
        outputs: true,
        spines: false,
    },
    Scenario {
        name: "responses_solver_only_400x5",
        strands: 400,
        links: 5,
        parts: false,
        responses: true,
        outputs: true,
        spines: false,
    },
    Scenario {
        name: "spine_100x5",
        strands: 100,
        links: 5,
        parts: false,
        responses: false,
        outputs: true,
        spines: true,
    },
];

fn main() {
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    println!("puppet tick, dt {DT:.4} s, {TICKS} timed ticks after {WARMUP} warm-up");
    println!(
        "{:<26}{:>8}{:>7}{:>8}{:>10}{:>12}{:>10}",
        "scenario", "strands", "links", "verts", "min us", "median us", "p90 us"
    );
    for s in SCENARIOS {
        if filter.as_deref().is_some_and(|f| !s.name.contains(f)) {
            continue;
        }
        let (min, median, p90) = run(s);
        println!(
            "{:<26}{:>8}{:>7}{:>8}{:>10.1}{:>12.1}{:>10.1}",
            s.name,
            s.strands,
            s.links,
            if s.parts || s.spines {
                (ROWS * 2).to_string()
            } else {
                "-".to_string()
            },
            min,
            median,
            p90,
        );
    }
}

/// Returns min, median and p90 tick in microseconds. Posing sits outside the
/// timed region.
fn run(s: &Scenario) -> (f64, f64, f64) {
    let built = Built::new(s);
    let mut puppet = Puppet::new(&built.model);
    puppet.reset_motion_responses(&built.model);

    let mut samples = Vec::with_capacity(TICKS);
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut moving = false;
    for frame in 0..WARMUP + TICKS {
        built.pose(&mut puppet, frame);
        let start = Instant::now();
        let motion = puppet.tick(&built.model, DT);
        let elapsed = start.elapsed();
        let responses_moving = motion.motion_responses;
        black_box(motion);
        if frame >= WARMUP {
            samples.push(elapsed.as_nanos());
            // A scenario whose rig sits still measures nothing.
            moving |= responses_moving;
            if let Some(v) = puppet.param_value(&built.bends[0]) {
                (lo, hi) = (lo.min(v), hi.max(v));
            }
        }
    }
    if built.bends_move {
        assert!(hi - lo > 1e-4, "{}: the bends never moved", s.name);
    } else {
        assert!(moving, "{}: the responses never moved", s.name);
    }
    if let Some(part) = &built.first_part {
        let idx = puppet.node_idx(part).unwrap();
        let deform = puppet.combined_deform(idx).unwrap();
        assert!(
            deform.iter().any(|d| d.length_squared() > 0.0),
            "{}: the bindings folded nothing",
            s.name
        );
    }
    black_box(&puppet);

    samples.sort_unstable();
    let at = |q: f64| {
        let i = ((samples.len() as f64 * q) as usize).min(samples.len() - 1);
        samples[i] as f64 / 1000.0
    };
    (at(0.0), at(0.5), at(0.9))
}

struct Built {
    model: Model,
    turn: ParamId,
    bends: Vec<ParamId>,
    pose_bends: bool,
    bends_move: bool,
    first_part: Option<NodeId>,
}

impl Built {
    fn new(s: &Scenario) -> Self {
        let mut hex = SeededHex::new(11);
        let mut model = Model::new();
        let root = model.root().unwrap().clone();

        let turn = model
            .add_param(
                ModelParam {
                    name: Name::truncated("turn"),
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .unwrap();
        let head = model
            .add_node(
                &root,
                ModelNode::new("head", ModelNodeKind::Group),
                &mut hex,
            )
            .unwrap();
        let turn_key = BindingKey::new(
            turn.clone(),
            head.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        model
            .add_binding_with_positions(&turn_key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        for (cell, value) in [(0, -TURN_SHIFT), (1, 0.0), (2, TURN_SHIFT)] {
            model.set_binding_key(&turn_key, [cell, 0], value).unwrap();
        }

        let mesh = strip_mesh();
        let link_len = (TOP - BOTTOM) / s.links as f32;
        let mut bends = Vec::with_capacity(s.strands * s.links);
        let mut first_part = None;
        for i in 0..s.strands {
            let strand: Vec<ParamId> = (0..s.links)
                .map(|l| {
                    model
                        .add_param(
                            ModelParam {
                                name: Name::truncated(format!("bend {i}.{l}")),
                                min: BEND_MIN,
                                max: BEND_MAX,
                                default: 0.0,
                            },
                            &mut hex,
                        )
                        .unwrap()
                })
                .collect();
            bends.extend(strand.iter().cloned());

            if s.parts {
                let mut node = ModelNode::new(
                    format!("strand {i}"),
                    ModelNodeKind::Part(ModelPart::new(mesh.clone())),
                );
                node.properties.transform.translation = [i as f32 * SPACING, 0.0, 0.0];
                let part = model.add_node(&head, node, &mut hex).unwrap();
                first_part.get_or_insert_with(|| part.clone());
                for (l, param) in strand.iter().enumerate() {
                    let key = BindingKey::new(param.clone(), part.clone(), BindingTarget::Deform);
                    model
                        .add_binding_with_positions(
                            &key,
                            vec![
                                BEND_KEYS
                                    .iter()
                                    .map(|k| (k - BEND_MIN) / (BEND_MAX - BEND_MIN))
                                    .collect(),
                            ],
                        )
                        .unwrap();
                    for (k, &bend) in BEND_KEYS.iter().enumerate() {
                        model
                            .set_deform_vertices(
                                &key,
                                [k as u32, 0],
                                keyform(&mesh.verts, l, link_len, bend),
                            )
                            .unwrap();
                    }
                    // Cubic: the keys are samples of a rotation.
                    model
                        .set_binding_interpolate(&key, InterpolateMode::Cubic)
                        .unwrap();
                }
            }

            if s.responses {
                let mut parent = head.clone();
                for (link, bend) in strand.iter().enumerate() {
                    let source = if link == 0 {
                        ResponseSource::PoseParam {
                            param: turn.clone(),
                        }
                    } else {
                        ResponseSource::Parent
                    };
                    let driver = MotionResponse {
                        inputs: vec![ResponseInput { source, gain: 0.75 }],
                        output_param: s.outputs.then(|| bend.clone()),
                        ..Default::default()
                    };
                    parent = model
                        .add_node(
                            &parent,
                            ModelNode::new(
                                format!("response {i}.{link}"),
                                ModelNodeKind::MotionResponse(driver),
                            ),
                            &mut hex,
                        )
                        .unwrap();
                }
            }
            if s.spines {
                // The joints are in the spine's own space, on the strip's top edge.
                let spine = ModelSpine::new(
                    (0..s.links)
                        .map(|l| [0.0, -link_len * (l + 1) as f32])
                        .collect(),
                );
                let mut node = ModelNode::new(format!("spine {i}"), ModelNodeKind::Spine(spine));
                node.properties.transform.translation = [i as f32 * SPACING, TOP, 0.0];
                let id = model.add_node(&head, node, &mut hex).unwrap();
                model
                    .set_spine_targets(&id, strand.iter().cloned().map(Some).collect())
                    .unwrap();
                let mut art = ModelNode::new(
                    format!("strand {i}"),
                    ModelNodeKind::Part(ModelPart::new(mesh.clone())),
                );
                art.properties.transform.translation = [0.0, -TOP, 0.0];
                let part = model.add_node(&id, art, &mut hex).unwrap();
                first_part.get_or_insert(part);
            }
        }

        Self {
            model,
            turn,
            bends,
            pose_bends: !s.responses,
            bends_move: !s.responses || s.outputs,
            first_part,
        }
    }

    /// Bends are spread in phase so no two bindings fold the same value.
    fn pose(&self, puppet: &mut Puppet, frame: usize) {
        let t = frame as f32 * DT;
        puppet.set_param_value(&self.turn, (t * TAU).sin());
        if self.pose_bends {
            for (i, param) in self.bends.iter().enumerate() {
                puppet.set_param_value(param, 0.4 * (t * TAU + i as f32 * 0.1).sin());
            }
        }
    }
}

fn strip_mesh() -> ClmMesh {
    let mut verts = Vec::with_capacity(ROWS * 4);
    let mut uvs = Vec::with_capacity(ROWS * 4);
    for r in 0..ROWS {
        let t = r as f32 / (ROWS - 1) as f32;
        let y = TOP + (BOTTOM - TOP) * t;
        verts.extend_from_slice(&[-HALF_W, y, HALF_W, y]);
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

/// A non-identity cell the fold cannot skip.
fn keyform(verts: &[f32], link: usize, link_len: f32, bend: f32) -> Vec<f32> {
    let joint_s = link as f32 * link_len;
    let joint_y = TOP - joint_s;
    let (sin, cos) = (bend * PI).sin_cos();
    let mut out = vec![0.0; verts.len()];
    for i in 0..verts.len() / 2 {
        let (x, y) = (verts[i * 2], verts[i * 2 + 1]);
        let s = TOP - y;
        if s <= joint_s {
            continue;
        }
        let w = ((s - joint_s) / link_len).clamp(0.0, 1.0);
        let (dx, dy) = (x, y - joint_y);
        out[i * 2] = w * (dx * cos - dy * sin - dx);
        out[i * 2 + 1] = w * (dx * sin + dy * cos - dy);
    }
    out
}
