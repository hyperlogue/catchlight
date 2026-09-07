//! What one tick of a chain-driven puppet costs, and which part of the tick
//! spends it.
//!
//! A flop count says the chain solver is negligible; this measures instead.
//! The scenarios are the split, because the pieces are not separable from
//! outside the crate. Three cuts, each the one below it plus a layer:
//! `chains_unbound_*` solves the chains and claims nothing, so it is the
//! integrator alone; `chains_solver_only_*` gives every link a param to claim
//! but binds nothing to it, so the difference is what a driver claim costs;
//! `bindings_only` folds the same bindings with the params posed by hand, and
//! the two full scenarios are all of it. The 10 / 100 / 400 rows separate the
//! per-claim linear scans in `Puppet::contribute` and
//! `retire_stale_driver_contributions`, which grow with the number of driver
//! outputs, from the solver, which does not.
//!
//! `spine_100x5` is the same hundred strands turned by a spine instead: the
//! same params posed the same way, composed at runtime rather than folded out
//! of per-link deform grids. It is the row `bindings_only` is there to be
//! compared against.
//!
//! Std only, no harness: `cargo bench -p catchlight-core`, optionally with a
//! substring to select scenarios. Numbers belong in the issue that asked for
//! them, not in a doc comment — a machine is not a baseline. The minimum is
//! printed beside the median because a tick is a fixed amount of work: on a
//! busy machine the low end is the code and the tail is the neighbours.

// A bench is not a test, so the workspace deny on these does not lift by
// itself; a build helper that cannot build has nothing to measure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::f32::consts::{PI, TAU};
use std::hint::black_box;
use std::time::Instant;

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::id::SeededHex;
use catchlight_core::{
    BindingKey, BindingTarget, InterpolateMode, LinkFeel, Model, ModelChain, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelSpine, Name, NodeId, ParamId, Puppet, ScalarTarget,
};

/// Seconds a tick advances: one frame at 60 Hz.
const DT: f32 = 1.0 / 60.0;
/// Ticks timed per scenario, and untimed ticks run first so the caches the
/// tick keeps (fold memos, the chain's scratch, the allocator's) are warm.
const TICKS: usize = 600;
const WARMUP: usize = 60;

/// Rows of vertices on a strand's strip, two vertices wide.
const ROWS: usize = 250;
/// The strip in model pixels: half its width, its top edge and its bottom.
const HALF_W: f32 = 12.0;
const TOP: f32 = 80.0;
const BOTTOM: f32 = -80.0;
/// Model pixels between one strand and the next, so a scenario is a row of
/// distinct strands rather than one drawn many times.
const SPACING: f32 = 40.0;

/// The bend values a link's deform binding is keyed at, in half turns, and
/// the range they span: a quarter turn each way, in thirds. The editor's own
/// keyform list, repeated here so the bench needs no editor crate.
const BEND_KEYS: [f32; 7] = [-0.5, -1.0 / 3.0, -1.0 / 6.0, 0.0, 1.0 / 6.0, 1.0 / 3.0, 0.5];
const BEND_MIN: f32 = -0.5;
const BEND_MAX: f32 = 0.5;

/// How far `turn` slides the strands at either end of its range, in model
/// pixels: far enough that the step swings a chain rather than nudging it.
const TURN_SHIFT: f32 = 120.0;
/// Fraction of a particle's velocity shed per second. Low enough that a
/// chain is still moving frames after the anchor steps.
const DAMPING: f32 = 0.3;

/// One measured configuration.
struct Scenario {
    name: &'static str,
    /// Strands: one chain, one strip part, or both.
    strands: usize,
    links: usize,
    /// Bend-spring frequency in Hz on every link; 0 is no spring.
    stiffness: f32,
    /// Build each strand's strip part and the per-link deform bindings on it.
    parts: bool,
    /// Build each strand's chain. Without one the bench poses the bend params
    /// itself, so the fold runs over the same values with no solver behind it.
    chains: bool,
    /// Aim the spine's links at the bend params — which is both the chain
    /// writing them and the spine reading them, since they are the same
    /// params. A chain that claims nothing still solves every tick, so this
    /// is the switch that isolates the integrator from the claim bookkeeping
    /// around it.
    outputs: bool,
    /// Hang the strip off the spine, so the spine's own composition moves the
    /// art. Mutually exclusive with `parts`, which is the historical path that
    /// binds per-link deform grids instead.
    spines: bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "chains_unsprung_100x5",
        strands: 100,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: true,
        outputs: true,
        spines: true,
    },
    Scenario {
        name: "chains_sprung_100x5",
        strands: 100,
        links: 5,
        stiffness: 4.0,
        parts: false,
        chains: true,
        outputs: true,
        spines: true,
    },
    Scenario {
        name: "bindings_only",
        strands: 100,
        links: 5,
        stiffness: 0.0,
        parts: true,
        chains: false,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "chains_unbound_10x5",
        strands: 10,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: true,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "chains_unbound_100x5",
        strands: 100,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: true,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "chains_unbound_400x5",
        strands: 400,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: true,
        outputs: false,
        spines: false,
    },
    Scenario {
        name: "chains_solver_only_10x5",
        strands: 10,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: true,
        outputs: true,
        spines: false,
    },
    Scenario {
        name: "chains_solver_only_100x5",
        strands: 100,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: true,
        outputs: true,
        spines: false,
    },
    Scenario {
        name: "chains_solver_only_400x5",
        strands: 400,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: true,
        outputs: true,
        spines: false,
    },
    Scenario {
        name: "spine_100x5",
        strands: 100,
        links: 5,
        stiffness: 0.0,
        parts: false,
        chains: false,
        outputs: true,
        spines: true,
    },
];

fn main() {
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    println!(
        "puppet tick, dt {:.4} s, {TICKS} timed ticks after {WARMUP} warm-up",
        DT
    );
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

/// Build the scenario, settle it, then time `TICKS` ticks of it. Returns the
/// minimum, median and p90 tick in microseconds.
///
/// Posing sits outside the timed region in every scenario, so what a chain
/// costs to write its params is inside the number and what the harness costs
/// to stand in for one is not.
fn run(s: &Scenario) -> (f64, f64, f64) {
    let built = Built::new(s);
    let mut puppet = Puppet::new(&built.model);
    puppet.settle_physics(&built.model);

    let mut samples = Vec::with_capacity(TICKS);
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut moving = false;
    for frame in 0..WARMUP + TICKS {
        built.pose(&mut puppet, frame);
        let start = Instant::now();
        let motion = puppet.tick(&built.model, DT);
        let elapsed = start.elapsed();
        let physics = motion.physics;
        black_box(motion);
        if frame >= WARMUP {
            samples.push(elapsed.as_nanos());
            // Outside the timed window: a scenario whose rig sits still is
            // measuring nothing, and a number off one is worse than none.
            moving |= physics;
            if let Some(v) = puppet.param_value(&built.bends[0]) {
                (lo, hi) = (lo.min(v), hi.max(v));
            }
        }
    }
    if built.bends_move {
        assert!(hi - lo > 1e-4, "{}: the bends never moved", s.name);
    } else {
        assert!(moving, "{}: the chains never moved", s.name);
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

/// A scenario's model and the params the bench poses on it.
struct Built {
    model: Model,
    /// Moves every strand's anchor, so the chains swing.
    turn: ParamId,
    /// One per link per strand. A chain writes these; without one the bench
    /// does.
    bends: Vec<ParamId>,
    /// Whether the bench poses `bends` itself.
    pose_bends: bool,
    /// Whether anything writes the bends this run — the bench or a chain.
    bends_move: bool,
    /// The first strand's strip, when the scenario has parts: what the
    /// post-run check reads to see the fold produced offsets.
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
                    key_positions: vec![0.0, 0.5, 1.0],
                },
                &mut hex,
            )
            .unwrap();
        // Every strand hangs off this group, so one scalar binding on it is
        // what puts the whole scenario in motion.
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
                                key_positions: BEND_KEYS
                                    .iter()
                                    .map(|k| (k - BEND_MIN) / (BEND_MAX - BEND_MIN))
                                    .collect(),
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
                node.transform.translation = [i as f32 * SPACING, 0.0, 0.0];
                let part = model.add_node(&head, node, &mut hex).unwrap();
                first_part.get_or_insert_with(|| part.clone());
                for (l, param) in strand.iter().enumerate() {
                    let key = BindingKey::new(param.clone(), part.clone(), BindingTarget::Deform);
                    for (k, &bend) in BEND_KEYS.iter().enumerate() {
                        model
                            .set_deform_vertices(
                                &key,
                                [k as u32, 0],
                                keyform(&mesh.verts, l, link_len, bend),
                            )
                            .unwrap();
                    }
                    // Cubic, because a bend lands between keys nearly always
                    // and the keys are samples of a rotation.
                    model
                        .set_binding_interpolate(&key, InterpolateMode::Cubic)
                        .unwrap();
                }
            }

            // A spine, optionally carrying a chain, optionally over art.
            if s.spines || s.chains {
                // The joints are in the spine's own space, and the spine sits
                // on the strip's top edge.
                let mut spine = ModelSpine::new(
                    (0..s.links)
                        .map(|l| [0.0, -link_len * (l + 1) as f32])
                        .collect(),
                );
                if s.chains {
                    let mut chain = ModelChain::new(s.links);
                    chain.set_links(vec![
                        LinkFeel {
                            gravity_scale: 1.0,
                            damping: DAMPING,
                            stiffness: s.stiffness,
                            limit: None,
                        };
                        s.links
                    ]);
                    spine.set_chain(Some(chain));
                }
                let mut node = ModelNode::new(format!("spine {i}"), ModelNodeKind::Spine(spine));
                node.transform.translation = [i as f32 * SPACING, TOP, 0.0];
                let id = model.add_node(&head, node, &mut hex).unwrap();
                if s.outputs {
                    model
                        .set_spine_targets(&id, strand.iter().cloned().map(Some).collect())
                        .unwrap();
                }
                if s.spines {
                    let mut art = ModelNode::new(
                        format!("strand {i}"),
                        ModelNodeKind::Part(ModelPart::new(mesh.clone())),
                    );
                    art.transform.translation = [0.0, -TOP, 0.0];
                    let part = model.add_node(&id, art, &mut hex).unwrap();
                    first_part.get_or_insert(part);
                }
            }
        }

        Self {
            model,
            turn,
            bends,
            pose_bends: !s.chains,
            bends_move: !s.chains || s.outputs,
            first_part,
        }
    }

    /// Pose the frame: a one-hertz sinusoid on `turn`, and on each bend param
    /// when no chain is writing it. The bends are spread in phase so no two
    /// bindings fold the same value.
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

/// The strip a strand is drawn on: two columns either side of x = 0, `ROWS`
/// rows from [`TOP`] down to [`BOTTOM`], top row first.
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

/// One link's deform keyform: the strip past the link's joint swung about it
/// by `bend` half turns, ramped in across the link's own span. The shape the
/// editor bakes for a strand, in the amount of arithmetic that matters here —
/// a non-identity cell the fold cannot skip.
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
