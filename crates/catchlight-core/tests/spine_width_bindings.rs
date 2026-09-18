#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The projected-width recipe uses the public binding/spine interface.
//! Synthetic strips establish evaluation order and bounds; pixel ownership,
//! coverage and clipping of real artwork still require rendered acceptance.

#[path = "../../../tests/support/spine_width.rs"]
mod support;

use catchlight_core::{Mat2, Model, NodeId, NodeKind, Puppet, Vec2};
use support::{envelope, smooth, width_delta, Fixture, LIMIT, ROWS};

fn world_positions(puppet: &Puppet, part: &NodeId) -> Vec<Vec2> {
    let idx = puppet.node_idx(part).unwrap();
    let NodeKind::Part(p) = &puppet.get(idx).unwrap().kind else {
        panic!("part")
    };
    p.mesh
        .vertices
        .iter()
        .zip(puppet.combined_deform(idx).unwrap())
        .map(|(v, d)| {
            puppet
                .transforms()
                .get(idx)
                .transform_point3((*v - p.mesh.origin + *d).extend(0.0))
                .truncate()
        })
        .collect()
}

fn check_frame(f: &Fixture, puppet: &Puppet, yaw: f32, body: f32) {
    for (lock_index, lock) in f.locks.iter().enumerate() {
        let b = lock
            .bends
            .each_ref()
            .map(|p| puppet.param_value(p).unwrap());
        let spine_idx = puppet.node_idx(&lock.spine).unwrap();
        let inverse = puppet.transforms().get(spine_idx).inverse();
        let world = world_positions(puppet, &lock.parts[0]);
        let companion = world_positions(puppet, &lock.parts[1]);
        for (a, b) in world.iter().zip(&companion) {
            assert!(a.distance(*b) < 1e-4, "companion left the surface");
        }
        let local: Vec<_> = world
            .iter()
            .map(|p| inverse.transform_point3(p.extend(0.0)).truncate())
            .collect();
        for row in 0..ROWS {
            let t = row as f32 / (ROWS - 1) as f32;
            let ratio = 1.0
                + envelope(t)
                    * ((1.0 - smooth(t)) * width_delta(yaw, b[0])
                        + smooth(t) * width_delta(yaw, b[1]));
            let angle = std::f32::consts::PI
                * if t <= 0.5 {
                    2.0 * t * b[0]
                } else {
                    b[0] + (2.0 * t - 1.0) * b[1]
                };
            let expected = Mat2::from_angle(angle) * Vec2::new(10.0 * ratio, 0.0);
            let cross_section = local[2 * row + 1] - local[2 * row];
            assert!(
                cross_section.distance(expected) < 1e-4,
                "row {row}: {cross_section:?} != {expected:?}, bends {b:?}"
            );
            assert!((0.97999..=1.02001).contains(&(cross_section.length() / 10.0)));
        }
        for tri in 0..ROWS - 1 {
            let v = 2 * tri;
            for [a, b, c] in [[v, v + 1, v + 3], [v, v + 3, v + 2]] {
                assert!(
                    (local[b] - local[a]).perp_dot(local[c] - local[a]) < 0.0,
                    "triangle reversed"
                );
            }
        }
        let root_x = if lock_index == 0 { -12.0 } else { 12.0 };
        for (i, x) in [-5.0, 5.0].into_iter().enumerate() {
            let expected = Mat2::from_angle(-0.08 * body)
                * (Mat2::from_angle(0.12 * yaw) * Vec2::new(root_x + x, 0.0)
                    + Vec2::new(15.0 * yaw, 0.0))
                + Vec2::new(0.0, 8.0 * body);
            assert!(
                world[i].distance(expected) < 1e-4,
                "root placement applied twice"
            );
        }
    }
}

#[test]
fn live_bends_drive_width_before_bending_in_the_same_frame_at_three_rates() {
    let f = Fixture::new();
    for hz in [30, 60, 120] {
        let mut puppet = Puppet::new(&f.model);
        puppet.settle_physics(&f.model);
        let mut max_difference = 0.0f32;
        let mut max_change = 0.0f32;
        let mut previous = 0.0;
        for frame in 0..hz * 3 {
            let time = frame as f32 / hz as f32;
            let yaw = if time < 1.0 {
                (time * std::f32::consts::TAU).sin()
            } else {
                0.0
            };
            let body = (time * 3.0).sin() * 0.7;
            puppet.set_param_value(&f.yaw, yaw);
            puppet.set_param_value(&f.body, body);
            puppet.tick(&f.model, 1.0 / hz as f32);
            check_frame(&f, &puppet, yaw, body);
            let short = puppet.param_value(&f.locks[0].bends[0]).unwrap();
            let long = puppet.param_value(&f.locks[1].bends[0]).unwrap();
            max_difference = max_difference.max((short - long).abs());
            max_change = max_change.max((short - previous).abs());
            previous = short;
        }
        assert!(
            max_difference > 0.001,
            "short and long locks must respond independently at {hz} Hz"
        );
        assert!(
            max_change > 0.001,
            "exercise changing outputs, not just a held pose"
        );
    }
}

#[test]
fn combined_extreme_and_intermediate_inputs_keep_one_width_budget() {
    let f = Fixture::new();
    let mut puppet = Puppet::new(&f.model);
    puppet.set_physics_enabled(false);
    for yaw in [-1.0, -0.35, 0.0, 0.65, 1.0] {
        for body in [-1.0, 0.0, 1.0] {
            for upper in [-LIMIT, 0.0, LIMIT] {
                for lower in [-LIMIT, 0.4 * LIMIT, LIMIT] {
                    puppet.set_param_value(&f.yaw, yaw);
                    puppet.set_param_value(&f.body, body);
                    for lock in &f.locks {
                        puppet.set_param_value(&lock.bends[0], upper);
                        puppet.set_param_value(&lock.bends[1], lower);
                    }
                    puppet.tick(&f.model, 0.0);
                    check_frame(&f, &puppet, yaw, body);
                }
            }
        }
    }
}

#[test]
fn neutral_width_is_exactly_inactive_and_the_recipe_round_trips() {
    let f = Fixture::new();
    let encoded = f.model.to_clm_bytes().unwrap();
    let reopened = Model::from_clm_bytes(&encoded).unwrap();
    assert!(f.model.authored_eq(&reopened).unwrap());
    assert_eq!(reopened.to_clm_bytes().unwrap(), encoded);
    let mut puppet = Puppet::new(&reopened);
    puppet.set_physics_enabled(false);
    puppet.tick(&reopened, 0.0);
    for lock in &f.locks {
        for part in &lock.parts {
            let idx = puppet.node_idx(part).unwrap();
            assert!(puppet
                .combined_deform(idx)
                .unwrap()
                .iter()
                .all(|d| *d == Vec2::ZERO));
        }
    }
    // Move away from neutral and back, including repeated cached ticks: no
    // stale width or spine source may survive when all cells become identity.
    puppet.set_param_value(&f.yaw, 0.7);
    for lock in &f.locks {
        puppet.set_param_value(&lock.bends[0], LIMIT / 2.0);
        puppet.set_param_value(&lock.bends[1], -LIMIT / 3.0);
    }
    puppet.tick(&reopened, 0.0);
    check_frame(&f, &puppet, 0.7, 0.0);
    puppet.set_param_value(&f.yaw, 0.0);
    for lock in &f.locks {
        for bend in &lock.bends {
            puppet.set_param_value(bend, 0.0);
        }
    }
    for _ in 0..2 {
        puppet.tick(&reopened, 0.0);
        for lock in &f.locks {
            for part in &lock.parts {
                assert!(puppet
                    .combined_deform(puppet.node_idx(part).unwrap())
                    .unwrap()
                    .iter()
                    .all(|d| *d == Vec2::ZERO));
            }
        }
    }
}

#[test]
fn kicking_one_lock_does_not_drive_its_sibling() {
    let f = Fixture::new();
    let mut puppet = Puppet::new(&f.model);
    puppet.settle_physics(&f.model);
    puppet.tick(&f.model, 1.0 / 60.0);
    let untouched = world_positions(&puppet, &f.locks[1].parts[0]);
    assert!(puppet.kick_chain(
        puppet.node_idx(&f.locks[0].spine).unwrap(),
        Vec2::new(2.0, 0.0)
    ));
    puppet.tick(&f.model, 1.0 / 60.0);
    assert!(puppet.param_value(&f.locks[0].bends[0]).unwrap().abs() > 1e-4);
    assert_eq!(world_positions(&puppet, &f.locks[1].parts[0]), untouched);
    assert_eq!(puppet.param_value(&f.locks[1].bends[0]), Some(0.0));
}
