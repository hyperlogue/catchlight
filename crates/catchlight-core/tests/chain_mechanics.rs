#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Physical conformance checks independent of trajectory baselines: force
//! balance, covariance, analytic damping, contacts, and a double-pendulum
//! RK4 reference. Lengths are pixels and time is seconds; mass density is one.
use catchlight_core::physics::{ChainLink, ParticleChainData};
use catchlight_core::{Mat2, Vec2};
use std::num::NonZeroU8;

fn chain(n: usize, stiffness: f32, drawn: Vec2) -> ParticleChainData {
    let mut c = ParticleChainData::new(vec![
        ChainLink {
            length: 100.0 / n as f32,
            stiffness,
            damping: 0.0,
            limit: None,
            drawn,
            ..Default::default()
        };
        n
    ]);
    c.gravity = 980.0;
    c
}
fn tip(c: &ParticleChainData) -> Vec2 {
    c.particles.last().unwrap().pos
}
fn tilt(c: &ParticleChainData) -> f32 {
    let v = tip(c) - c.anchor;
    v.x.atan2(v.y).to_degrees()
}

#[test]
fn stiffness_must_not_switch_off_gravity() {
    let mut tilts = Vec::new();
    for stiffness in [0.0, 1e-6, 0.001, 0.1, 1.0] {
        let mut c = chain(1, stiffness, Mat2::from_angle(-0.5) * Vec2::Y);
        c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        tilts.push(tilt(&c));
    }
    eprintln!("stiffness [0, 1e-6, .001, .1, 1] settled tilt {tilts:?}");
    assert!(
        (tilts[0] - tilts[1]).abs() < 1e-4,
        "vanishing stiffness must approach the limp result continuously"
    );
}

#[test]
fn free_hair_is_invariant_under_frame_rotation() {
    let mut worst: f32 = 0.0;
    for n in [1, 2, 6, 8] {
        for speed in [0.3, 1.0, 3.0, 6.0] {
            for steps in [4, 16] {
                let mut c = chain(n, 0.0, Vec2::Y);
                c.gravity = 0.0;
                c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
                let start = tip(&c);
                for frame in 1..=60 {
                    c.tick(
                        Vec2::ZERO,
                        Mat2::from_angle(speed * frame as f32 / 60.0),
                        &[],
                        1.0 / 60.0,
                        NonZeroU8::new(steps).unwrap(),
                    );
                }
                let drift = start.distance(tip(&c));
                eprintln!("rotation n={n} speed={speed} substeps={steps} tip drift={drift:.6} final speed={:.4}", c.particles.last().unwrap().vel.length());
                worst = worst.max(drift);
            }
        }
    }
    assert!(
        worst < 0.001,
        "frame-only rotation moved a 100px free strand by {worst}px"
    );
}

#[test]
fn one_link_matches_small_angle_pendulum() {
    for fps in [30, 60, 120, 240] {
        let mut c = chain(1, 0.0, Vec2::Y);
        c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        c.particles[1].pos = Mat2::from_angle(0.05) * Vec2::Y * 100.0;
        let mut worst: f32 = 0.0;
        let mut energy_min = f64::INFINITY;
        let mut energy_max = 0.0_f64;
        let energy = |c: &ParticleChainData| {
            let p = c.particles[1];
            0.5 * f64::from(p.vel.length_squared()) + 980.0 * f64::from(100.0 - p.pos.y)
        };
        let initial = energy(&c);
        for frame in 1..=fps * 4 {
            c.tick(
                Vec2::ZERO,
                Mat2::IDENTITY,
                &[],
                1.0 / fps as f32,
                NonZeroU8::new(4).unwrap(),
            );
            let t = frame as f32 / fps as f32;
            let expected = 0.05 * (9.8_f32.sqrt() * t).cos();
            let measured = -tilt(&c).to_radians();
            worst = worst.max((measured - expected).abs());
            energy_min = energy_min.min(energy(&c) / initial);
            energy_max = energy_max.max(energy(&c) / initial);
        }
        eprintln!("pendulum fps={fps} worst angle error={worst:.7} energy ratio={energy_min:.5}..{energy_max:.5}");
        assert!(worst < 0.0002);
        assert!(energy_min > 0.9995 && energy_max < 1.0005);
    }
}

#[test]
fn bounded_rotation_should_not_move_free_hair() {
    for n in [1, 6, 8] {
        for (fps, steps) in [(30, 4), (60, 4), (120, 4), (60, 16)] {
            let mut c = chain(n, 0.0, Vec2::Y);
            c.gravity = 0.0;
            c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
            let start = tip(&c);
            let mut max_drift = 0.0_f32;
            for frame in 1..=fps * 3 {
                let t = frame as f32 / fps as f32;
                let angle = if t <= 2.0 {
                    0.5 * (std::f32::consts::TAU * t).sin()
                } else {
                    0.0
                };
                c.tick(
                    Vec2::ZERO,
                    Mat2::from_angle(angle),
                    &[],
                    1.0 / fps as f32,
                    NonZeroU8::new(steps).unwrap(),
                );
                max_drift = max_drift.max(tip(&c).distance(start));
            }
            assert!(
                max_drift < 0.001,
                "{n}, {fps}, {steps}: frame rotation moved free hair {max_drift}px"
            );
            eprintln!("bounded ±28.65deg 1Hz n={n} fps={fps} substeps={steps} max_drift={max_drift:.4} final_drift={:.4} final_speed={:.4}", tip(&c).distance(start), c.particles.last().unwrap().vel.length());
        }
    }
}

#[test]
fn authored_damping_matches_exponential_decay() {
    for fps in [30, 60, 120] {
        let mut c = chain(1, 0.0, Vec2::Y);
        c.gravity = 0.0;
        c.links[0].damping = 0.5;
        c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        c.particles[1].vel = c.particles[1].pos.perp();
        for _ in 0..fps {
            c.tick(
                Vec2::ZERO,
                Mat2::IDENTITY,
                &[],
                1.0 / fps as f32,
                NonZeroU8::new(4).unwrap(),
            );
        }
        assert!((c.particles[1].vel.length() - 50.0).abs() < 0.01);
    }
}

#[test]
fn zero_damping_springs_keep_their_amplitude_across_frame_rates() {
    for hz in [1.0, 4.0, 10.0, 40.0, 100.0] {
        for fps in [30, 60, 120, 240] {
            let mut c = chain(1, hz, Vec2::Y);
            c.gravity = 0.0;
            c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
            c.particles[1].pos = Mat2::from_angle(0.2) * Vec2::Y * 100.0;
            let mut late_peak = 0.0_f32;
            let mut phase_error = 0.0_f64;
            for frame in 1..=fps {
                c.tick(
                    Vec2::ZERO,
                    Mat2::IDENTITY,
                    &[],
                    1.0 / fps as f32,
                    NonZeroU8::new(4).unwrap(),
                );
                let expected = 0.2
                    * (std::f64::consts::TAU * f64::from(hz) * f64::from(frame) / f64::from(fps))
                        .cos();
                phase_error = phase_error.max((f64::from(-tilt(&c).to_radians()) - expected).abs());
                if frame >= fps / 2 {
                    late_peak = late_peak.max(tilt(&c).abs());
                }
            }
            assert!(
                phase_error < 0.005,
                "{hz}Hz at {fps}fps: phase error {phase_error} radians"
            );
            assert!(
                (late_peak - 0.2_f32.to_degrees()).abs() < 0.02,
                "{hz}Hz at {fps}fps lost unrequested energy: {late_peak}deg"
            );
            eprintln!("spring {hz}Hz damping=0 fps={fps} peak after .5s={late_peak:.5}deg");
        }
    }
}

#[test]
fn realistic_roll_with_gravity() {
    for n in [1, 6, 8] {
        for amplitude in [15.0_f32, 30.0] {
            let mut c = chain(n, 0.0, Vec2::Y);
            c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
            let start = tip(&c);
            let mut max_drift = 0.0_f32;
            for frame in 1..=300 {
                let t = frame as f32 / 60.0;
                let angle = amplitude.to_radians() * (std::f32::consts::TAU * t).sin();
                c.tick(
                    Vec2::ZERO,
                    Mat2::from_angle(angle),
                    &[],
                    1.0 / 60.0,
                    NonZeroU8::new(4).unwrap(),
                );
                max_drift = max_drift.max(tip(&c).distance(start));
            }
            assert!(max_drift < 0.001, "{n}, {amplitude}: {max_drift}");
            eprintln!(
                "gravity n={n} amplitude={amplitude} max_drift={max_drift:.5} tip_speed={:.5}",
                c.particles.last().unwrap().vel.length()
            );
        }
    }
}

#[test]
fn double_pendulum_against_independent_rk4() {
    fn derivative(y: [f64; 4]) -> [f64; 4] {
        let [a, b, va, vb] = y;
        let delta = a - b;
        let cross = delta.cos();
        let fa = -2.0 * (980.0 / 50.0) * a.sin() - delta.sin() * vb * vb;
        let fb = -(980.0 / 50.0) * b.sin() + delta.sin() * va * va;
        let det = 2.0 - cross * cross;
        [
            va,
            vb,
            (fa - cross * fb) / det,
            (2.0 * fb - cross * fa) / det,
        ]
    }
    let mut ref_state = [0.4_f64, -0.2, 0.3, -0.4];
    let h = 1.0 / 24000.0;
    let mut reference = Vec::new();
    for i in 1..=48000 {
        let k1 = derivative(ref_state);
        let k2 = derivative(std::array::from_fn(|j| ref_state[j] + h * 0.5 * k1[j]));
        let k3 = derivative(std::array::from_fn(|j| ref_state[j] + h * 0.5 * k2[j]));
        let k4 = derivative(std::array::from_fn(|j| ref_state[j] + h * k3[j]));
        for j in 0..4 {
            ref_state[j] += h / 6.0 * (k1[j] + 2.0 * k2[j] + 2.0 * k3[j] + k4[j]);
        }
        if i % 100 == 0 {
            reference.push(ref_state);
        }
    }
    for fps in [30, 60, 120, 240] {
        let mut c = chain(2, 0.0, Vec2::Y);
        c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        let r0 = Mat2::from_angle(0.4) * Vec2::Y * 50.0;
        let r1 = Mat2::from_angle(-0.2) * Vec2::Y * 50.0;
        c.particles[1].pos = r0;
        c.particles[2].pos = r0 + r1;
        c.particles[1].vel = r0.perp() * 0.3;
        c.particles[2].vel = c.particles[1].vel + r1.perp() * -0.4;
        let mut max_err = 0.0_f64;
        for frame in 1..=fps * 2 {
            c.tick(
                Vec2::ZERO,
                Mat2::IDENTITY,
                &[],
                1.0 / fps as f32,
                NonZeroU8::new(4).unwrap(),
            );
            let expected = reference[frame * 240 / fps - 1];
            for (j, p) in c.particles.windows(2).enumerate() {
                let rod = p[1].pos - p[0].pos;
                let a = f64::from((-rod.x).atan2(rod.y));
                max_err = max_err.max((a - expected[j]).abs());
            }
        }
        assert!(
            max_err.to_degrees() < 0.01,
            "{fps}fps: {}deg",
            max_err.to_degrees()
        );
        eprintln!(
            "double pendulum RK4 reference fps={fps} max angle error={:.5}deg",
            max_err.to_degrees()
        );
    }
}

#[test]
fn spring_and_gravity_balance_at_rest() {
    for hz in [0.001_f32, 0.1, 1.0, 10.0] {
        let mut c = chain(1, hz, Mat2::from_angle(-0.5) * Vec2::Y);
        c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        let angle = -tilt(&c).to_radians();
        let torque = (std::f32::consts::TAU * hz).powi(2) * (angle + 0.5) + 9.8 * angle.sin();
        assert!(
            torque.abs() < 0.001,
            "{hz}Hz: residual specific torque {torque}"
        );
        assert!(c.is_at_rest(1e-6));
    }
}

#[test]
fn limits_have_no_soft_force_and_stop_impacts_without_bouncing() {
    let mut c = chain(1, 0.0, Vec2::Y);
    c.gravity = 0.0;
    c.links[0].limit = Some(0.1 / std::f32::consts::PI);
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    c.particles[1].pos = Mat2::from_angle(0.08) * Vec2::Y * 100.0;
    let start = tip(&c);
    c.tick(
        Vec2::ZERO,
        Mat2::IDENTITY,
        &[],
        1.0 / 60.0,
        NonZeroU8::new(4).unwrap(),
    );
    assert!(
        tip(&c).distance(start) < 1e-4,
        "a hard stop must exert no force before contact"
    );
    c.particles[1].vel = c.particles[1].pos.perp() * 5.0;
    c.tick(
        Vec2::ZERO,
        Mat2::IDENTITY,
        &[],
        1.0 / 60.0,
        NonZeroU8::new(4).unwrap(),
    );
    assert!((-tilt(&c).to_radians() - 0.1).abs() < 1e-5);
    assert!(
        c.particles[1].vel.length() < 1e-4,
        "inelastic contact reflected the velocity"
    );
}

#[test]
fn coupled_impacts_do_not_create_energy() {
    let mut c = chain(2, 0.0, Vec2::Y);
    c.gravity = 0.0;
    for l in &mut c.links {
        l.limit = Some(0.1 / std::f32::consts::PI);
    }
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    let a = Mat2::from_angle(0.09) * Vec2::Y * 50.0;
    let b = Mat2::from_angle(0.05) * Vec2::Y * 50.0;
    c.particles[1].pos = a;
    c.particles[2].pos = a + b;
    c.particles[1].vel = a.perp() * 5.0;
    c.particles[2].vel = a.perp() * 5.0 + b.perp() * 3.0;
    let energy = |c: &ParticleChainData| {
        c.particles
            .iter()
            .map(|p| 25.0 * p.vel.length_squared())
            .sum::<f32>()
    };
    let initial = energy(&c);
    for _ in 0..60 {
        c.tick(
            Vec2::ZERO,
            Mat2::IDENTITY,
            &[],
            1.0 / 60.0,
            NonZeroU8::new(4).unwrap(),
        );
        assert!(
            energy(&c) <= initial * 1.001,
            "a passive impact added kinetic energy"
        );
    }
    assert!(
        energy(&c) < initial * 0.5,
        "the impacts must dissipate energy"
    );
}

#[test]
fn a_mirror_edit_rebases_the_frame_without_launching_particles() {
    let mut c = chain(2, 0.0, Mat2::from_angle(0.3) * Vec2::Y);
    c.gravity = 0.0;
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    let start = tip(&c);
    c.tick(
        Vec2::ZERO,
        Mat2::from_diagonal(Vec2::new(-1.0, 1.0)),
        &[],
        1.0 / 60.0,
        NonZeroU8::new(4).unwrap(),
    );
    assert!(tip(&c).distance(start) < 1e-4);
    assert!(c.particles.last().unwrap().vel.length() < 1e-4);
}

#[test]
fn static_settling_rejects_an_unstable_posed_equilibrium() {
    let mut c = chain(1, 0.1, Vec2::Y);
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[-1.0]);
    assert!(
        tip(&c).y > 90.0,
        "a weak spring cannot hold the mass upright: {:?}",
        tip(&c)
    );
}
