//! Deterministic fingerprint of the SimplePhysics *driver* over time.
//!
//! The physics unit tests assert endpoints ("settles within X of rest") and
//! the GPU visual baselines only capture the settled pose — neither pins the
//! transient (overshoot, oscillation frequency, decay shape). A driver change
//! that converges to the same rest but along a different curve passes both yet
//! is a real regression. This drives the puppet's `tick_physics` + reads the
//! mapped `param_value` each frame, comparing the whole trajectory against a
//! committed baseline with per-sample tolerance. It is CPU-only and
//! deterministic, so unlike the GPU baselines it is safe to gate CI.
//!
//! Regenerate after an intentional physics change:
//!   UPDATE_PHYSICS_BASELINE=1 cargo test -p catchlight-core --test physics_trajectory

use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode, SimplePhysicsData};
use catchlight_core::{GlobalTransforms, Node, NodeKind, Puppet, Vec2};
use std::collections::BTreeMap;
use std::path::PathBuf;

const TARGET_UUID: u32 = 42;
const DT: f32 = 1.0 / 60.0;
const FRAMES: usize = 300;
const SAMPLE_EVERY: usize = 5;
// Per-sample absolute tolerance on the mapped param output. Cross-arch f32
// jitter over 300 RK4 frames stays well under this; a real change to the
// integrator, mapping, or output scaling shifts the curve by far more.
const TOL: f32 = 2e-3;

/// Drive one SimplePhysics node through the puppet's per-frame physics path
/// and record the mapped param output (`AngleLength`/`XY` etc.) at a fixed
/// cadence. Goes through `tick_physics` (anchor from world transform, map
/// mode, output scale, param write), not the integrator in isolation.
fn run_scenario(data: SimplePhysicsData) -> Vec<[f32; 2]> {
    let mut puppet = Puppet::new();
    let node = Node {
        kind: NodeKind::SimplePhysics(Box::new(data)),
        ..Default::default()
    };
    puppet.insert_child(puppet.root(), node, None);

    let mut transforms = GlobalTransforms::new();
    puppet.compute_transforms(&mut transforms);

    let mut samples = Vec::with_capacity(FRAMES / SAMPLE_EVERY + 1);
    for f in 0..FRAMES {
        puppet.tick_physics(&transforms, DT);
        if f % SAMPLE_EVERY == 0 {
            let v = puppet.param_value(TARGET_UUID).unwrap_or(Vec2::ZERO);
            samples.push([v.x, v.y]);
        }
    }
    samples
}

/// Rigid pendulum released ~53° off vertical, lightly damped: many visible
/// oscillations decaying toward rest. `anchor_initialized: true` keeps the
/// hand-set perturbed bob (the first tick would otherwise snap it to the
/// world anchor).
fn rigid_perturbed() -> SimplePhysicsData {
    SimplePhysicsData {
        kind: PendulumKind::RigidPendulum,
        map_mode: PhysicsParamMapMode::AngleLength,
        target_param_id: Some(TARGET_UUID),
        gravity: 981.0,
        length: 100.0,
        angle_damping: 0.05,
        bob: Vec2::new(80.0, 60.0),
        anchor: Vec2::ZERO,
        anchor_initialized: true,
        ..Default::default()
    }
}

/// Spring pendulum displaced both radially (stretched) and laterally, mapped
/// through `XY` with a non-unit `output_scale` so the mapping and scaling are
/// part of the fingerprint, not just the integrator.
fn spring_stretched() -> SimplePhysicsData {
    SimplePhysicsData {
        kind: PendulumKind::SpringPendulum,
        map_mode: PhysicsParamMapMode::XY,
        target_param_id: Some(TARGET_UUID),
        gravity: 981.0,
        length: 100.0,
        frequency: 1.0,
        angle_damping: 0.1,
        length_damping: 0.1,
        output_scale: Vec2::new(1.5, 0.75),
        bob: Vec2::new(25.0, 135.0),
        spring_vel: Vec2::ZERO,
        anchor: Vec2::ZERO,
        anchor_initialized: true,
        ..Default::default()
    }
}

/// A stiff spring, whose substep is short enough that a 60 fps frame needs
/// several of them. The two scenarios above are soft enough to fit a frame
/// in one step, so without this the multi-step loop — the part the adaptive
/// substep rewrote — has no trajectory gate at all.
fn spring_stiff() -> SimplePhysicsData {
    SimplePhysicsData {
        kind: PendulumKind::SpringPendulum,
        map_mode: PhysicsParamMapMode::XY,
        target_param_id: Some(TARGET_UUID),
        gravity: 981.0,
        length: 100.0,
        frequency: 30.0,
        angle_damping: 0.1,
        length_damping: 0.1,
        bob: Vec2::new(25.0, 135.0),
        spring_vel: Vec2::ZERO,
        anchor: Vec2::ZERO,
        anchor_initialized: true,
        ..Default::default()
    }
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
