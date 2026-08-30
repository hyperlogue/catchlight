#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Deterministic fingerprint of the SimplePhysics *driver* over time.
//!
//! The physics unit tests assert endpoints ("settles within X of rest") and
//! the GPU visual baselines only capture the settled pose — neither pins the
//! transient (overshoot, oscillation frequency, decay shape). A driver change
//! that converges to the same rest but along a different curve passes both yet
//! is a real regression. This ticks a puppet and reads the driver's two target
//! params each frame, comparing the whole trajectory against a committed
//! baseline with per-sample tolerance. It is CPU-only and deterministic, so
//! unlike the GPU baselines it is safe to gate CI.
//!
//! The rig is one driver on its own under the root, so the anchor is the
//! origin and every sampled number is the driver's: the model exists to carry
//! the authored pendulum and the two params it writes.
//!
//! Regenerate after an intentional physics change:
//!   UPDATE_PHYSICS_BASELINE=1 cargo test -p catchlight-core --test physics_trajectory

use catchlight_core::formats::clm::ClmPhysics;
use catchlight_core::id::SeededHex;
use catchlight_core::model::{ModelNode, ModelNodeKind, ModelParam, ModelPhysics};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::{Model, Name, ParamId, Puppet, Vec2};
use std::collections::BTreeMap;
use std::path::PathBuf;

const DT: f32 = 1.0 / 60.0;
const FRAMES: usize = 300;
const SAMPLE_EVERY: usize = 5;
// Per-sample absolute tolerance on the mapped param output. Cross-arch f32
// jitter over 300 RK4 frames stays well under this; a real change to the
// integrator, mapping, or output scaling shifts the curve by far more.
const TOL: f32 = 2e-3;

/// The authored pendulum plus where its bob starts. The bob is a *runtime*
/// position — the model has no field for it, because a model describes a
/// pendulum and not the swing it happens to be mid-way through — so the
/// scenario places it on the baked puppet.
struct Scenario {
    driver: ModelPhysics,
    bob: Vec2,
}

/// A model carrying one driver aimed at two params, and a puppet with the
/// scenario's bob already in place.
///
/// Model-level physics is set to `1.0 * 1.0` so the node's own `gravity` is
/// the effective one: the bake folds `pixels_per_meter * gravity` into every
/// driver, and these scenarios name the folded number directly.
fn rig(scenario: Scenario) -> (Model, Puppet, [ParamId; 2]) {
    let mut hex = SeededHex::new(11);
    let mut model = Model::new();
    model.set_physics(ClmPhysics {
        pixels_per_meter: 1.0,
        gravity: 1.0,
    });
    let params = ["out.x", "out.y"].map(|name| {
        model
            .add_param(
                ModelParam {
                    name: Name::truncated(name),
                    min: -1000.0,
                    max: 1000.0,
                    default: 0.0,
                    key_positions: vec![0.0, 1.0],
                },
                &mut hex,
            )
            .expect("add param")
    });
    let root = model.root().expect("a fresh model has one root").clone();
    let node = model
        .add_node(
            &root,
            ModelNode::new("driver", ModelNodeKind::SimplePhysics(scenario.driver)),
            &mut hex,
        )
        .expect("add driver");
    model
        .set_physics_targets(&node, [Some(params[0].clone()), Some(params[1].clone())])
        .expect("aim the driver");

    let mut puppet = Puppet::new(&model);
    let idx = puppet.node_idx(&node).expect("the driver baked");
    assert!(puppet.place_driver(idx, scenario.bob), "placed the bob");
    (model, puppet, params)
}

/// Drive one SimplePhysics node through a puppet's per-frame physics path and
/// record the mapped param output (`AngleLength`/`XY` etc.) at a fixed
/// cadence. Goes through `tick` (anchor from world transform, map mode,
/// output scale, param write), not the integrator in isolation.
fn run_scenario(scenario: Scenario) -> Vec<[f32; 2]> {
    let (model, mut puppet, params) = rig(scenario);
    let mut samples = Vec::with_capacity(FRAMES / SAMPLE_EVERY + 1);
    for f in 0..FRAMES {
        puppet.tick(&model, DT);
        if f % SAMPLE_EVERY == 0 {
            samples.push([
                puppet.param_value(&params[0]).unwrap_or(0.0),
                puppet.param_value(&params[1]).unwrap_or(0.0),
            ]);
        }
    }
    samples
}

/// Rigid pendulum released ~53° off vertical, lightly damped: many visible
/// oscillations decaying toward rest.
fn rigid_perturbed() -> Scenario {
    let mut driver = ModelPhysics::new(PendulumKind::RigidPendulum);
    driver.map_mode = PhysicsParamMapMode::AngleLength;
    driver.gravity = 981.0;
    driver.length = 100.0;
    driver.angle_damping = 0.05;
    Scenario {
        driver,
        bob: Vec2::new(80.0, 60.0),
    }
}

/// Spring pendulum displaced both radially (stretched) and laterally, mapped
/// through `XY` with a non-unit `output_scale` so the mapping and scaling are
/// part of the fingerprint, not just the integrator.
fn spring_stretched() -> Scenario {
    let mut driver = ModelPhysics::new(PendulumKind::SpringPendulum);
    driver.map_mode = PhysicsParamMapMode::XY;
    driver.gravity = 981.0;
    driver.length = 100.0;
    driver.frequency = 1.0;
    driver.angle_damping = 0.1;
    driver.length_damping = 0.1;
    driver.output_scale = [1.5, 0.75];
    Scenario {
        driver,
        bob: Vec2::new(25.0, 135.0),
    }
}

/// A stiff spring, whose substep is short enough that a 60 fps frame needs
/// several of them. The two scenarios above are soft enough to fit a frame
/// in one step, so without this the multi-step loop — the part the adaptive
/// substep rewrote — has no trajectory gate at all.
fn spring_stiff() -> Scenario {
    let mut driver = ModelPhysics::new(PendulumKind::SpringPendulum);
    driver.map_mode = PhysicsParamMapMode::XY;
    driver.gravity = 981.0;
    driver.length = 100.0;
    driver.frequency = 30.0;
    driver.angle_damping = 0.1;
    driver.length_damping = 0.1;
    Scenario {
        driver,
        bob: Vec2::new(25.0, 135.0),
    }
}

/// The placement the trajectories above are built on, on its own: a driver
/// left alone hangs straight down under its anchor on its first tick, and one
/// that was placed swings from where it was put instead. Without this the
/// whole fingerprint could be a pendulum at rest.
#[test]
fn a_placed_pendulum_swings_and_an_untouched_one_hangs() {
    let (model, mut puppet, params) = rig(rigid_perturbed());
    puppet.tick(&model, DT);
    let placed = puppet.param_value(&params[0]).expect("angle written");
    assert!(
        placed.abs() > 0.1,
        "a placed pendulum is off vertical: {placed}"
    );

    let mut hanging = Puppet::new(&model);
    hanging.tick(&model, DT);
    let at_rest = hanging.param_value(&params[0]).expect("angle written");
    assert!(
        at_rest.abs() < 1e-6,
        "an untouched driver hangs under its anchor: {at_rest}"
    );

    assert!(
        !hanging.place_driver(hanging.root(), Vec2::ZERO),
        "the root is not a driver"
    );
}

fn current_trajectories() -> BTreeMap<String, Vec<[f32; 2]>> {
    let mut m = BTreeMap::new();
    m.insert(
        "rigid_perturbed".to_string(),
        run_scenario(rigid_perturbed()),
    );
    m.insert(
        "spring_stretched".to_string(),
        run_scenario(spring_stretched()),
    );
    m.insert("spring_stiff".to_string(), run_scenario(spring_stiff()));
    m
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/physics_trajectory.json")
}

#[test]
fn physics_driver_trajectory_matches_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let current = current_trajectories();
    let path = baseline_path();

    if std::env::var_os("UPDATE_PHYSICS_BASELINE").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(&current)?)?;
        eprintln!("updated physics baseline at {}", path.display());
        return Ok(());
    }

    let raw = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "missing physics baseline {} ({e}); regenerate with \
             UPDATE_PHYSICS_BASELINE=1 cargo test -p catchlight-core --test physics_trajectory",
            path.display()
        )
    })?;
    let expected: BTreeMap<String, Vec<[f32; 2]>> = serde_json::from_str(&raw)?;

    for (name, cur) in &current {
        let Some(exp) = expected.get(name) else {
            return Err(format!("baseline has no scenario {name:?}; regenerate baseline").into());
        };
        assert_eq!(
            cur.len(),
            exp.len(),
            "{name}: sample count changed ({} vs {})",
            cur.len(),
            exp.len()
        );

        let mut worst = 0.0f32;
        let mut worst_at = (0usize, 'x');
        for (i, (c, e)) in cur.iter().zip(exp).enumerate() {
            let dx = (c[0] - e[0]).abs();
            let dy = (c[1] - e[1]).abs();
            if dx > worst {
                worst = dx;
                worst_at = (i, 'x');
            }
            if dy > worst {
                worst = dy;
                worst_at = (i, 'y');
            }
        }
        assert!(
            worst <= TOL,
            "{name}: trajectory drifted from baseline; worst |Δ|={worst:.6} at sample {} ({}) > tol {TOL}. \
             If intentional, regenerate with UPDATE_PHYSICS_BASELINE=1.",
            worst_at.0,
            worst_at.1,
        );
    }
    Ok(())
}
