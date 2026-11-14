#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Strand invariants independent of trajectory baselines: exact lengths, a
//! neutral rest that reads zero, return to rest after a translation, frame-rate
//! agreement, bounded catch-up and sleep. Lengths are pixels, time seconds.
use catchlight_core::physics::{ChainLink, ParticleChainData, MAX_TICK_SECONDS};
use catchlight_core::{Mat2, Vec2};

/// A six-link strand drawn with a gentle curve, stiffer at the root, hung
/// under gravity with Lan-like settings.
fn strand() -> ParticleChainData {
    let mut c = ParticleChainData::new(
        (0..6)
            .map(|i| ChainLink {
                length: 30.0,
                drawn: Mat2::from_angle(0.08 * i as f32) * Vec2::Y,
                stiffness: if i == 0 { 12.0 } else { 2.2 - 0.16 * i as f32 },
                limit: if i == 0 { Some(0.014) } else { Some(0.25) },
                ..Default::default()
            })
            .collect(),
    );
    c.gravity = 11_000.0;
    c.damping = 6.0;
    c
}

fn bends(c: &ParticleChainData) -> Vec<f32> {
    let mut out = Vec::new();
    c.link_bends(&mut out);
    out
}

/// How far the rendered bends move the tip off the drawing, in the node
/// frame.
fn tip(c: &ParticleChainData) -> Vec2 {
    let b = bends(c);
    let mut at = Vec2::ZERO;
    let mut turn = 0.0f32;
    for (i, l) in c.links.iter().enumerate() {
        turn -= b[i] * std::f32::consts::PI;
        at += (Mat2::from_angle(turn) * l.drawn - l.drawn) * l.length;
    }
    at
}

/// Root path: a head that sways and turns, then holds still at 1 s.
fn motion(t: f32) -> (Vec2, Mat2) {
    let s = (t.min(1.0) * 4.0).sin();
    (Vec2::new(25.0 * s, 6.0 * s), Mat2::from_angle(0.15 * s))
}

#[test]
fn lengths_stay_exact() {
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    let mut worst = 0.0f32;
    for f in 1..=600 {
        let t = f as f32 / 60.0;
        let a = (t * 9.0).sin();
        c.tick(
            Vec2::new(120.0 * a, 30.0 * a),
            Mat2::from_angle(0.5 * a),
            &[],
            1.0 / 60.0,
        );
        for (i, l) in c.links.iter().enumerate() {
            let got = c.particles[i + 1].pos.distance(c.particles[i].pos);
            worst = worst.max((got / l.length - 1.0).abs());
        }
    }
    assert!(worst < 1e-5, "worst relative length error {worst}");
}

#[test]
fn neutral_output_is_zero() {
    for carry in [
        Mat2::IDENTITY,
        Mat2::from_angle(0.4),
        Mat2::from_cols(Vec2::new(-1.0, 0.0), Vec2::new(0.0, 1.0)),
        Mat2::from_cols(Vec2::new(1.5, 0.2), Vec2::new(-0.1, 0.8)),
    ] {
        let mut c = strand();
        c.rest_carry = carry;
        c.settle_to_rest(Vec2::new(5.0, -3.0), carry, &[]);
        assert!(
            bends(&c).iter().all(|b| *b == 0.0),
            "{carry}: {:?}",
            bends(&c)
        );
        c.tick(Vec2::new(5.0, -3.0), carry, &[], 1.0 / 60.0);
        assert!(
            bends(&c).iter().all(|b| *b == 0.0),
            "{carry}: {:?}",
            bends(&c)
        );
        assert!(
            c.rest_sag().iter().any(|b| b.abs() > 1e-4),
            "gravity sags the strand, and the output hides it"
        );
    }
}

/// The neutral rest belongs to the authored pose: settled in a turned frame,
/// the strand hangs under gravity rather than holding its drawing there.
#[test]
fn a_turned_frame_settles_off_the_drawing() {
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::from_angle(0.5), &[]);
    assert!(bends(&c)[1].abs() > 1e-3, "{:?}", bends(&c));
}

#[test]
fn a_translation_returns_the_strand_to_rest() {
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    let mut swing = 0.0f32;
    for f in 1..=60 {
        let t = f as f32 / 60.0;
        c.tick(
            Vec2::new(30.0 * (t / 0.1).min(1.0), 0.0),
            Mat2::IDENTITY,
            &[],
            1.0 / 60.0,
        );
        swing = swing.max(tip(&c).length());
    }
    for _ in 0..600 {
        c.tick(Vec2::new(30.0, 0.0), Mat2::IDENTITY, &[], 1.0 / 60.0);
    }
    assert!(c.is_at_rest(), "the strand went to sleep");
    assert!(swing > 5.0, "the move swung the strand: {swing}");
    assert!(
        tip(&c).length() < 0.05,
        "back on the drawing: {} (swing was {swing})",
        tip(&c)
    );
}

#[test]
fn frame_rates_agree() {
    let run = |fps: f32| {
        let mut c = strand();
        c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        let frames = (2.0 * fps).round() as usize;
        (1..=frames)
            .map(|f| {
                let t = f as f32 / fps;
                let (root, carry) = motion(t);
                c.tick(root, carry, &[], 1.0 / fps);
                (t, tip(&c))
            })
            .collect::<Vec<_>>()
    };
    let reference = run(240.0);
    for fps in [30.0, 60.0, 144.0] {
        let mut worst = 0.0f32;
        for (t, p) in run(fps) {
            let k = ((t * 240.0).round() as usize).clamp(1, reference.len()) - 1;
            worst = worst.max(p.distance(reference[k].1));
        }
        assert!(worst < 0.25, "{fps} fps differs from 240 fps by {worst} px");
    }
}

#[test]
fn a_tick_runs_at_most_the_step_cap() {
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    for (i, dt) in [0.25f32, 10.0, 1e9].into_iter().enumerate() {
        c.tick(
            Vec2::new(10.0 * (i + 1) as f32, 0.0),
            Mat2::IDENTITY,
            &[],
            dt,
        );
        assert_eq!(c.steps_last_tick(), 8, "dt {dt}: 8 steps at 120 Hz");
        assert_eq!(MAX_TICK_SECONDS * 120.0, 8.0);
    }
    c.tick(Vec2::new(5.0, 0.0), Mat2::IDENTITY, &[], 1.0 / 60.0);
    assert_eq!(c.steps_last_tick(), 2, "a 60 fps frame is two 120 Hz steps");
    assert!(bends(&c).iter().all(|b| b.is_finite()));
}

#[test]
fn a_sleeping_strand_runs_no_steps() {
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    assert!(c.is_at_rest());
    c.tick(Vec2::ZERO, Mat2::IDENTITY, &[], 1.0 / 60.0);
    assert_eq!(c.steps_last_tick(), 0, "settled and unchanged");

    c.tick(Vec2::new(3.0, 0.0), Mat2::IDENTITY, &[], 1.0 / 60.0);
    assert!(c.steps_last_tick() > 0, "a moved root wakes it");
    let mut frames = 0;
    while !c.is_at_rest() {
        c.tick(Vec2::new(3.0, 0.0), Mat2::IDENTITY, &[], 1.0 / 60.0);
        frames += 1;
        assert!(frames < 600, "the strand never slept");
    }
    let before = bends(&c);
    for _ in 0..10 {
        c.tick(Vec2::new(3.0, 0.0), Mat2::IDENTITY, &[], 1.0 / 60.0);
        assert_eq!(c.steps_last_tick(), 0);
    }
    assert_eq!(bends(&c), before, "a sleeping strand's output holds still");

    c.tick(Vec2::new(3.0, 0.0), Mat2::IDENTITY, &[0.1], 1.0 / 60.0);
    assert!(c.steps_last_tick() > 0, "a new pose wakes it");
}

#[test]
fn a_root_jump_longer_than_the_strand_moves_it_rigidly() {
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    c.tick(Vec2::new(5000.0, 0.0), Mat2::IDENTITY, &[], 1.0 / 60.0);
    assert!(
        tip(&c).length() < 0.01,
        "no swing from a teleport: {}",
        tip(&c)
    );
    assert!((c.particles[1].pos.x - 5000.0).abs() < 50.0);
}

#[test]
fn a_kick_swings_and_settles() {
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    c.kick(Vec2::new(20.0, 0.0), &[]);
    assert!(!c.is_at_rest());
    c.tick(Vec2::ZERO, Mat2::IDENTITY, &[], 1.0 / 60.0);
    assert!(
        bends(&c)[0] > 0.0,
        "a +X kick bends toward +X: {:?}",
        bends(&c)
    );
    for _ in 0..600 {
        c.tick(Vec2::ZERO, Mat2::IDENTITY, &[], 1.0 / 60.0);
    }
    assert!(c.is_at_rest());
    assert!(tip(&c).length() < 0.05);
}

/// The step cap is simulated time, not a step count, so a fast physics rate
/// still runs in real time at an ordinary frame rate.
#[test]
fn a_fast_rate_keeps_real_time() {
    for rate in [480.0f32, 1000.0] {
        let mut c = strand();
        c.rate = rate;
        c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
        let mut steps = 0;
        for f in 1..=30 {
            c.tick(Vec2::new(f as f32, 0.0), Mat2::IDENTITY, &[], 1.0 / 30.0);
            steps += c.steps_last_tick();
        }
        assert!(
            (steps as f32 - rate).abs() <= 1.0,
            "{rate} Hz ran {steps} steps in a second of 30 fps frames"
        );
    }
}

/// A tick with no time that turns the frame or changes the pose leaves the
/// strand awake, so the next real ticks hang it where a fresh settle would.
#[test]
fn a_zero_time_tick_does_not_freeze_a_new_input() {
    let turned = Mat2::from_angle(0.5);
    let pose = [0.0, 0.1, 0.0, 0.0, 0.0, 0.0];
    let mut fresh = strand();
    fresh.settle_to_rest(Vec2::ZERO, turned, &pose);
    let mut c = strand();
    c.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    assert!(c.is_at_rest());
    c.tick(Vec2::ZERO, turned, &pose, 0.0);
    assert!(!c.is_at_rest(), "a zero-time input change wakes the strand");
    for _ in 0..600 {
        c.tick(Vec2::ZERO, turned, &pose, 1.0 / 60.0);
    }
    for (a, b) in bends(&c).iter().zip(bends(&fresh)) {
        assert!((a - b).abs() < 1e-3, "settled {a} vs fresh {b}");
    }
}
