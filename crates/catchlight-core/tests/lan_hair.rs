#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The bundled model's long locks swing from a rolling head while its short
//! swept fringe retains its shape. Exercise Puppet and the evaluated meshes
//! so attachments, output params, limits and visible deformation participate.
//! Requires the Lan Git LFS object, like the visual suite.
//!
//! Lan's head turns, tilts and bobs by mesh-group warps, never by turning a
//! node. The head-roll warp carries the scalp and leaves the lengths hanging,
//! so the held pose supplies the hang. Responses sample the current posed
//! art for transient motion, and no hair spine is slid against its art.
//!
//! Long locks use a broad scalar response and its slower child. The short
//! fringe uses one response. All read named vertices on pose-only art;
//! the lift responses project vertical motion into the existing lift warps.

use catchlight_core::{Model, NodeId, NodeKind, ParamId, Puppet, Vec2, Vec3};

const DT: f32 = 1.0 / 60.0;

/// Cross-section midpoint in the evaluated mesh, selected by authored Y.
/// Measures the strand on its rendered mesh.
fn section_center(puppet: &Puppet, model: &Model, id: &str, y: f32) -> Vec2 {
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(model, puppet).unwrap();
    let mesh = geometry
        .mesh(&catchlight_core::NodeId::new(id).unwrap())
        .unwrap();
    let mut crossings = Vec::new();
    for i in 0..mesh.triangle_count() {
        let [a, b, c] = mesh.triangle(i).unwrap();
        for (a, b) in [(a, b), (b, c), (c, a)] {
            let a = mesh.vertex(a as usize).unwrap();
            let b = mesh.vertex(b as usize).unwrap();
            if (a.rest.y - b.rest.y).abs() < 1e-6 {
                continue;
            }
            let t = (y - a.rest.y) / (b.rest.y - a.rest.y);
            if (0.0..=1.0).contains(&t) {
                crossings.push((
                    a.rest.x + (b.rest.x - a.rest.x) * t,
                    a.world.lerp(b.world, t).truncate(),
                ));
            }
        }
    }
    crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
    (crossings.first().unwrap().1 + crossings.last().unwrap().1) * 0.5
}

/// Where the art renders a point of a part, given in the part's authored
/// space: the point carried by the evaluated triangle that holds it at rest.
/// `None` when the point lies off the part's mesh.
fn rendered(model: &Model, puppet: &Puppet, part: &NodeId, rest: Vec2) -> Option<Vec2> {
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(model, puppet).unwrap();
    let mesh = geometry.mesh(part).ok()?;
    for (_, [a, b, c]) in mesh.triangles() {
        let [a, b, c] = [a, b, c].map(|i| mesh.vertex(i as usize).unwrap());
        let [pa, pb, pc] = [a, b, c].map(|v| v.rest);
        let area = (pb - pa).perp_dot(pc - pa);
        if area.abs() < 1e-9 {
            continue;
        }
        let u = (rest - pa).perp_dot(pc - pa) / area;
        let v = (pb - pa).perp_dot(rest - pa) / area;
        if u >= 0.0 && v >= 0.0 && u + v <= 1.0 {
            return Some((a.world * (1.0 - u - v) + b.world * u + c.world * v).truncate());
        }
    }
    None
}

/// The spines that bend the painted hair locks.
fn lock_spines(model: &Model) -> impl Iterator<Item = &NodeId> {
    model
        .node_ids()
        .filter(|id| id.as_str().contains("Hair-Lock"))
}

/// Read the same named root the driver samples, through an independently
/// posed puppet. This checks the rendered warp and the slot reference together.
fn root_on_art(model: &Model, puppet: &Puppet, part: &NodeId) -> Vec2 {
    let slot = catchlight_core::SlotId::new("sway-root").unwrap();
    let v = model.slot(part, &slot).unwrap().vertex().unwrap();
    catchlight_core::geometry::EvaluatedGeometry::new(model, puppet)
        .unwrap()
        .mesh(part)
        .unwrap()
        .vertex(v as usize)
        .unwrap()
        .world
        .truncate()
}

fn worst_root_gap(model: &Model, live: &Puppet, posed: &Puppet) -> (f32, String) {
    let mut worst = (0.0, String::new());
    let mut count = 0;
    for id in model
        .node_ids()
        .filter(|id| id.as_str().starts_with("motion/") && !id.as_str().ends_with("/tip"))
    {
        let inputs = live.response_inputs(id).unwrap();
        let catchlight_core::ResponseSource::PartSlot { part, .. } = &inputs[0].source else {
            continue;
        };
        let sample = Vec2::from_array(inputs[0].position.unwrap());
        let gap = sample.distance(root_on_art(model, posed, part));
        if gap > worst.0 {
            worst = (gap, id.to_string());
        }
        count += 1;
    }
    assert!(count >= 17);
    worst
}

#[test]
fn front_locks_keep_their_length_while_the_head_rolls() {
    let model = lan();
    let roll = ParamId::new("head_roll").unwrap();
    // Cross-section centres from the scalp to the painted tip, by authored Y.
    let samples = [
        (
            "front-hair-lock-2",
            [940.0, 820.0, 700.0, 580.0, 460.0, 432.0],
        ),
        (
            "front-hair-lock-18",
            [760.0, 690.0, 620.0, 550.0, 480.0, 430.0],
        ),
    ];
    let length = |puppet: &Puppet, id: &str, ys: &[f32]| -> f32 {
        let centres: Vec<_> = ys
            .iter()
            .map(|&y| section_center(puppet, &model, id, y))
            .collect();
        centres.windows(2).map(|w| w[0].distance(w[1])).sum()
    };
    let mut rest = Puppet::new(&model);
    rest.set_physics_enabled(false);
    rest.tick(&model, DT);
    // The roll warp holds the tips where they hang, which alone would stretch
    // a lock by up to a fourteenth; head-roll keys slide each lock's art
    // along its guide to keep its length. A strand that grows and shrinks
    // with the head reads as jelly.
    for physics in [false, true] {
        for angle in [-30.0, -15.0, 15.0, 30.0] {
            let mut puppet = Puppet::new(&model);
            puppet.set_physics_enabled(physics);
            puppet.set_param_value(&roll, angle);
            if physics {
                puppet.settle_physics(&model);
            }
            puppet.tick(&model, DT);
            for (id, ys) in &samples {
                let drawn = length(&rest, id, ys);
                let posed = length(&puppet, id, ys);
                assert!(
                    (posed / drawn - 1.0).abs() < 0.02,
                    "{id} at roll {angle} (physics {physics}): length {posed}, drawn {drawn}"
                );
            }
        }
    }
}

#[test]
fn front_locks_settle_promptly_after_a_head_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    let roll = ParamId::new("head_roll").unwrap();
    let samples = [
        ("front-hair-lock-2", 540.0),
        ("front-hair-lock-2", 432.0),
        ("front-hair-lock-18", 560.0),
        ("front-hair-lock-18", 430.0),
    ];
    // Roll 12 units over a quarter second, then hold. The tip should come
    // to within half a pixel of rest in about a second (the design measured
    // 1.2 s for a 10 degree step); much longer reads as goo.
    let mut held = Vec::new();
    for frame in 0..150 {
        let t = (frame as f32 / 15.0).min(1.0);
        puppet.set_param_value(&roll, 12.0 * t * t * (3.0 - 2.0 * t));
        puppet.tick(&model, DT);
        if frame >= 14 {
            held.push(samples.map(|(id, y)| section_center(&puppet, &model, id, y)));
        }
    }
    let last = *held.last().unwrap();
    for (i, (id, y)) in samples.into_iter().enumerate() {
        let settled = held
            .iter()
            .rposition(|h| h[i].distance(last[i]) > 0.5)
            .map_or(0, |k| k + 1);
        assert!(
            settled as f32 * DT < 1.25,
            "{id} at rest y={y}: still moving {}s after the head stopped",
            settled as f32 * DT
        );
    }
}

#[test]
fn back_hair_falls_beside_the_neck_under_a_rolled_head() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.set_param_value(&ParamId::new("head_roll").unwrap(), -30.0);
    puppet.settle_physics(&model);
    puppet.tick(&model, DT);
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(&model, &puppet).unwrap();
    // The outer back-hair contour beside the cheek and neck. Mesh boundaries
    // include a small transparent margin around the painted silhouette.
    // Measured on this build, where the roll warp and the length-keeping
    // roll keys shape the fall: a settled response adds nothing to a held pose.
    for (y, expected_x) in [(775.0, -120.21), (725.0, -141.26), (675.0, -165.12)] {
        let mut left = f32::INFINITY;
        for id in model
            .node_ids()
            .filter(|id| id.as_str().starts_with("back-hair-lock-"))
        {
            let mesh = geometry.mesh(id).unwrap();
            for (_, [a, b, c]) in mesh.triangles() {
                for (a, b) in [(a, b), (b, c), (c, a)] {
                    let a = mesh.vertex(a as usize).unwrap().world;
                    let b = mesh.vertex(b as usize).unwrap().world;
                    if (a.y - b.y).abs() < 1e-6 {
                        continue;
                    }
                    let t = (y - a.y) / (b.y - a.y);
                    if (0.0..=1.0).contains(&t) {
                        left = left.min(a.x + (b.x - a.x) * t);
                    }
                }
            }
        }
        assert!(
            (left - expected_x).abs() < 5.0,
            "back hair at y={y}: edge {left}, reference {expected_x}"
        );
    }
}

/// **A lock's tip stays behind its root.** Head roll is a warp, so the art
/// moves with the head before a response acts; what the response adds to the
/// tip must run against the way its root went.
#[test]
fn long_lock_mesh_tips_lag_their_roots_on_a_small_head_roll() {
    let model = lan();
    let mut live = Puppet::new(&model);
    live.settle_physics(&model);
    live.tick(&model, DT);
    let mut carried = Puppet::new(&model);
    carried.set_physics_enabled(false);
    let samples = [
        ("Front-Hair-Lock-2-Spine", "front-hair-lock-2", 432.0),
        ("Front-Hair-Lock-18-Spine", "front-hair-lock-18", 430.0),
        ("Back-Hair-Lock-19-Spine", "back-hair-lock-19", 350.0),
    ];
    let root =
        |puppet: &Puppet, part: &str| root_on_art(&model, puppet, &NodeId::new(part).unwrap());
    let before = samples.map(|(_, part, _)| root(&live, part));
    let roll = ParamId::new("head_roll").unwrap();
    live.set_param_value(&roll, 1.0);
    carried.set_param_value(&roll, 1.0);
    live.tick(&model, DT);
    carried.tick(&model, DT);
    for (i, (spine, part, y)) in samples.into_iter().enumerate() {
        let moved = root(&live, part).x - before[i].x;
        let swing =
            section_center(&live, &model, part, y).x - section_center(&carried, &model, part, y).x;
        assert!(
            moved.abs() > 0.1,
            "{spine}: the roll moved its root {moved}px"
        );
        assert!(
            swing * moved < 0.0 && swing.abs() > moved.abs() * 0.25,
            "{part}: root moved {moved}px, the response moved the tip {swing}px"
        );
    }
}

#[test]
fn front_locks_do_not_ring_after_a_tiny_head_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    let mut carried = Puppet::new(&model);
    carried.set_physics_enabled(false);
    let roll = ParamId::new("head_roll").unwrap();
    let samples = [("front-hair-lock-2", 432.0), ("front-hair-lock-18", 430.0)];
    let mut swing = Vec::new();
    // A half-unit roll over a quarter second, then hold the head still. What
    // the response adds to the visible tips is one lag and one follow-through;
    // three quarters of a second after the head stops, under a seventh of
    // that swing may be left. Hair that keeps wobbling after a small
    // movement reads as jelly.
    for frame in 0..=75 {
        let t = (frame as f32 / 15.0).min(1.0);
        let value = 0.5 * t * t * (3.0 - 2.0 * t);
        puppet.set_param_value(&roll, value);
        carried.set_param_value(&roll, value);
        puppet.tick(&model, DT);
        carried.tick(&model, DT);
        swing.push(samples.map(|(id, y)| {
            section_center(&puppet, &model, id, y).distance(section_center(&carried, &model, id, y))
        }));
    }
    for (i, (id, _)) in samples.into_iter().enumerate() {
        let peak = swing.iter().map(|s| s[i]).fold(0.0_f32, f32::max);
        let left = swing[60..].iter().map(|s| s[i]).fold(0.0_f32, f32::max);
        assert!(peak > 0.1, "{id}: the response swung the tip only {peak}px");
        assert!(
            left < peak * 0.15,
            "{id}: still swinging {left}px of a {peak}px swing after the head stopped"
        );
    }
}

/// The head reaches the hair through its warps alone: no hair spine, and no
/// part directly under one, carries a translation key on a head param.
/// A spine slid one way with its art slid back renders the same, but leaves
/// the node somewhere the art is not.
#[test]
fn no_hair_spine_is_slid_against_its_art() {
    let model = lan();
    let head = [
        ParamId::new("head_yaw").unwrap(),
        ParamId::new("head_pitch").unwrap(),
        ParamId::new("head_roll").unwrap(),
    ];
    for binding in model.bindings() {
        let key = binding.key();
        let node = model.node(&key.node).unwrap();
        let on_spine = |n: &catchlight_core::ModelNode| {
            matches!(n.kind, catchlight_core::model::ModelNodeKind::Spine(_))
        };
        let under_spine = on_spine(node)
            || node
                .parent()
                .and_then(|p| model.node(p))
                .is_some_and(on_spine);
        let slide = matches!(
            key.target,
            catchlight_core::BindingTarget::Scalar(
                catchlight_core::ScalarTarget::Tx | catchlight_core::ScalarTarget::Ty
            )
        );
        assert!(
            !(under_spine && slide && head.iter().any(|p| key.params.contains(p))),
            "{} slides on a head param",
            key.node
        );
    }
}

fn lan() -> Model {
    Model::from_clm_bytes(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/models/lan.clm"
        ))
        .expect("fetch the Lan Git LFS object"),
    )
    .unwrap()
}

/// Each sampled root agrees with an independently posed copy of the part,
/// including nested warps on this frame.
#[test]
fn simulation_roots_stay_on_the_art_through_every_warp() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    let mut posed = Puppet::new(&model);
    posed.set_physics_enabled(false);
    let roll = ParamId::new("head_roll").unwrap();
    for angle in [-30.0, 30.0] {
        puppet.set_param_value(&roll, angle);
        puppet.settle_physics(&model);
        puppet.tick(&model, DT);
        posed.set_param_value(&roll, angle);
        posed.tick(&model, DT);
        let (gap, part) = worst_root_gap(&model, &puppet, &posed);
        assert!(
            gap < 0.01,
            "{part} at roll {angle}: simulation root is {gap}px off its art"
        );
    }
    for (name, reach) in [
        ("head_roll", 30.0),
        ("head_yaw", 30.0),
        ("head_pitch", 30.0),
        ("body_roll", 10.0),
    ] {
        let param = ParamId::new(name).unwrap();
        posed.apply_pose(&Default::default());
        let mut puppet = Puppet::new(&model);
        puppet.settle_physics(&model);
        for frame in 0..120 {
            let value = reach * (std::f32::consts::TAU * frame as f32 * DT).sin();
            puppet.set_param_value(&param, value);
            puppet.tick(&model, DT);
            posed.set_param_value(&param, value);
            posed.tick(&model, DT);
            let (gap, part) = worst_root_gap(&model, &puppet, &posed);
            assert!(
                gap < 0.01,
                "{part} on {name}, frame {frame}: simulation root is {gap}px off its art"
            );
        }
    }
}

#[test]
fn shaped_fringe_stays_close_to_its_drawing_across_head_poses() {
    let model = lan();
    let mut simulated = Puppet::new(&model);
    let mut drawn = Puppet::new(&model);
    drawn.set_physics_enabled(false);
    // Held poses keep the shaped fringe close to its painted position.
    for (name, value) in [
        ("head_roll", 0.0),
        ("head_roll", -30.0),
        ("head_roll", 30.0),
        ("head_yaw", -30.0),
        ("head_yaw", 30.0),
        ("head_pitch", -30.0),
        ("head_pitch", 30.0),
    ] {
        let param = ParamId::new(name).unwrap();
        simulated.set_param_value(&param, value);
        drawn.set_param_value(&param, value);
        simulated.settle_physics(&model);
        simulated.tick(&model, DT);
        drawn.tick(&model, DT);
        assert_fringe_shape(&model, &simulated, &drawn, &format!("{name}={value}"), 0.5);
        simulated.set_param_value(&param, 0.0);
        drawn.set_param_value(&param, 0.0);
    }
}

fn assert_fringe_shape(model: &Model, simulated: &Puppet, drawn: &Puppet, pose: &str, limit: f32) {
    use catchlight_core::geometry::EvaluatedGeometry;
    let actual = EvaluatedGeometry::new(model, simulated).unwrap();
    let reference = EvaluatedGeometry::new(model, drawn).unwrap();
    for id in [
        "front-hair-lock-13",
        "front-hair-lock-22",
        "front-hair-lock-23",
        "front-hair-lock-23-under",
        "front-hair-lock-24",
    ] {
        let id = catchlight_core::NodeId::new(id).unwrap();
        let mesh = actual.mesh(&id).unwrap();
        let rest = reference.mesh(&id).unwrap();
        let worst = mesh
            .vertices()
            .zip(rest.vertices())
            .map(|(a, b)| a.world.distance(b.world))
            .fold(0.0_f32, f32::max);
        assert!(
            worst < limit,
            "{id} at {pose}: fringe moved {worst}px away from its drawing"
        );
    }
}

#[test]
fn fringe_keeps_its_shape_while_the_head_rolls() {
    let model = lan();
    let mut simulated = Puppet::new(&model);
    let mut drawn = Puppet::new(&model);
    drawn.set_physics_enabled(false);
    simulated.settle_physics(&model);
    let roll = ParamId::new("head_roll").unwrap();
    // A full roll each way within a second is the hardest shake the head
    // has. The fringe swings with it, and its stops hold the swing well
    // short of the eyes.
    for frame in 0..120 {
        let value = 30.0 * (std::f32::consts::TAU * frame as f32 * DT).sin();
        simulated.set_param_value(&roll, value);
        drawn.set_param_value(&roll, value);
        simulated.tick(&model, DT);
        drawn.tick(&model, DT);
        assert_fringe_shape(
            &model,
            &simulated,
            &drawn,
            &format!("roll frame {frame}"),
            27.0,
        );
    }
}

#[test]
fn long_hair_hangs_down_under_a_rolled_head() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    let roll = ParamId::new("head_roll").unwrap();
    puppet.settle_physics(&model);
    // Rest spans and directions of the long locks' lower lengths, and of the
    // face, read off the evaluated meshes by authored height.
    let long = [
        "front-hair-lock-2",
        "front-hair-lock-18",
        "back-hair-lock-19",
        "back-hair-lock-15",
        "back-hair-lock-20",
        "back-hair-lock-16",
        "back-hair-lock-21",
        "back-hair-lock-14",
        "back-hair-lock-25",
        "back-hair-lock-26",
        "back-hair-lock-18",
    ];
    let lower = |puppet: &Puppet, id: &str| {
        let geometry = catchlight_core::geometry::EvaluatedGeometry::new(&model, puppet).unwrap();
        let mesh = geometry.mesh(&NodeId::new(id).unwrap()).unwrap();
        let low = mesh.vertices().map(|v| v.rest.y).fold(f32::MAX, f32::min);
        let high = mesh.vertices().map(|v| v.rest.y).fold(f32::MIN, f32::max);
        let at = |f: f32| section_center(puppet, &model, id, low + f * (high - low));
        (at(0.08) - at(0.35)).normalize()
    };
    let face = |puppet: &Puppet| {
        (section_center(puppet, &model, "face", 700.0)
            - section_center(puppet, &model, "face", 900.0))
        .normalize()
    };
    let mut drawn = Puppet::new(&model);
    drawn.settle_physics(&model);
    drawn.tick(&model, DT);
    for angle in [-30.0, 30.0] {
        puppet.set_param_value(&roll, angle);
        puppet.settle_physics(&model);
        puppet.tick(&model, DT);
        let tilt = face(&puppet).angle_to(face(&drawn)).abs().to_degrees();
        assert!(tilt > 10.0, "roll {angle} tilts the face {tilt} degrees");
        for id in long {
            let turn = lower(&puppet, id)
                .angle_to(lower(&drawn, id))
                .abs()
                .to_degrees();
            assert!(
                turn < tilt * 0.5,
                "{id} at {angle}: lower hair turned {turn} degrees, the face {tilt}"
            );
        }
        let mut locks = 0;
        for id in lock_spines(&model) {
            let idx = puppet.node_idx(id).unwrap();
            let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
                continue;
            };
            if spine.joints.len() <= 2 {
                continue;
            }
            locks += 1;
            let root_bend = spine.bends[0].abs() * 180.0;
            assert!(
                root_bend < 5.0,
                "{id} at {angle}: root detached from head direction by {root_bend} degrees"
            );
        }
        assert!(
            locks >= 10,
            "must exercise the bundled long front and back locks"
        );
    }
}

/// Body roll is a warp in Lan's rig: it moves the art without turning the
/// spine nodes. The responses follow the warped art, so the long locks
/// swing with it, and since the move is a translation of the head they come
/// back to their drawing once the body holds still.
#[test]
fn long_locks_swing_with_a_body_roll_and_return_to_the_drawing() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    let roll = ParamId::new("body_roll").unwrap();
    let locks = ["Front-Hair-Lock-2-Spine", "Back-Hair-Lock-19-Spine"];
    let bends = |puppet: &Puppet, id: &str| -> Vec<f32> {
        let idx = puppet
            .node_idx(&catchlight_core::NodeId::new(id).unwrap())
            .unwrap();
        let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
            unreachable!()
        };
        spine.bends.clone()
    };
    let mut swing = [0.0f32; 2];
    for frame in 1..=240 {
        puppet.set_param_value(&roll, 10.0 * (frame as f32 / 6.0).min(1.0));
        puppet.tick(&model, DT);
        for (i, id) in locks.iter().enumerate() {
            let turn: f32 = bends(&puppet, id).iter().sum();
            swing[i] = swing[i].max(turn.abs());
        }
    }
    for (i, id) in locks.iter().enumerate() {
        assert!(
            swing[i] > 0.02,
            "{id}: a body roll swings the lock, peak {}",
            swing[i]
        );
        let rest = bends(&puppet, id);
        assert!(
            rest.iter().all(|b| b.abs() < 1e-3),
            "{id}: back on the drawing after the body holds: {rest:?}"
        );
    }
}

/// **The frame rate does not change the swing.** A head roll ramp under a yaw
/// sine reaches the hair through the warped root, the keyformed guide and
/// the node transform, all evaluated on the frame they render; 30 and 60 fps
/// land on the 240 fps trajectory at the same instants.
#[test]
fn hair_swings_alike_at_any_frame_rate() {
    let model = lan();
    let locks = [("front-hair-lock-2", 432.0), ("back-hair-lock-19", 350.0)];
    let run = |fps: f32| -> Vec<Vec<(f32, Vec2)>> {
        let mut puppet = Puppet::new(&model);
        puppet.settle_physics(&model);
        let mut out = vec![Vec::new(); locks.len()];
        for f in 1..=(2.0 * fps) as usize {
            let t = f as f32 / fps;
            puppet.set_param_value(&ParamId::new("head_roll").unwrap(), 40.0 * t.min(0.5));
            puppet.set_param_value(&ParamId::new("head_yaw").unwrap(), 15.0 * (3.0 * t).sin());
            puppet.tick(&model, 1.0 / fps);
            for (i, &(lock, y)) in locks.iter().enumerate() {
                out[i].push((t, section_center(&puppet, &model, lock, y)));
            }
        }
        out
    };
    let reference = run(240.0);
    for fps in [30.0, 60.0] {
        for (lock, (trace, truth)) in locks.iter().zip(run(fps).iter().zip(&reference)) {
            for (t, tip) in trace {
                let k = (t * 240.0).round() as usize - 1;
                let off = tip.distance(truth[k].1);
                assert!(
                    off < 0.1,
                    "{lock:?} at {fps} fps, t={t:.3}: {off:.3} px off 240 fps"
                );
            }
        }
    }
}

/// **Hair sleeps wherever the caller places the puppet.** A viewport passes a
/// root transform; the responses read posed slots in model space,
/// so the render fold under that root cannot feed rounding back in and keep
/// them awake.
#[test]
fn hair_sleeps_under_a_placed_root() {
    let model = lan();
    let root = catchlight_core::Mat4::from_translation(Vec3::new(120.0, -40.0, 0.0))
        * catchlight_core::Mat4::from_rotation_z(0.37)
        * catchlight_core::Mat4::from_scale(Vec3::new(0.73, 0.73, 1.0));
    for yaw in [0.0, 20.0] {
        let mut puppet = Puppet::new(&model);
        puppet.set_param_value(&ParamId::new("head_yaw").unwrap(), yaw);
        puppet.settle_physics(&model);
        let mut awake = 0;
        for frame in 0..600 {
            let motion = puppet.tick_with_root(&model, root, DT);
            if frame >= 500 && motion.physics {
                awake += 1;
            }
        }
        assert_eq!(awake, 0, "yaw {yaw}: hair still awake under a placed root");
    }
}

/// **A held drag on a lock lets the hair settle.** The slot sampler sees
/// the same scratch transform the render does, so the slot follows the
/// dragged art instead of alternating between it and the undragged one.
#[test]
fn a_held_drag_on_a_lock_lets_the_hair_settle() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    let lock = puppet
        .node_idx(&catchlight_core::NodeId::new("front-hair-lock-2").unwrap())
        .unwrap();
    let at = puppet.get(lock).unwrap().transform;
    assert!(puppet.set_scratch_transform(
        lock,
        catchlight_core::ScratchTransform {
            translation: Some(at.translation + Vec3::new(25.0, 0.0, 0.0)),
            rotation: Some(at.rotation + Vec3::new(0.0, 0.0, 0.2)),
            ..Default::default()
        },
    ));
    let mut awake = 0;
    for frame in 0..900 {
        let motion = puppet.tick(&model, DT);
        if frame >= 800 && motion.physics {
            awake += 1;
        }
    }
    assert_eq!(awake, 0, "the hair never settled under a held drag");
}

/// The vertical response drives the authored lift warp during a nod and
/// returns the fringe and long lock to the held drawing.
#[test]
fn hair_lifts_as_the_head_drops_and_comes_back_to_the_drawing() {
    let model = lan();
    let pitch = ParamId::new("head_pitch").unwrap();
    let samples = [("front-hair-lock-23", 825.0), ("front-hair-lock-2", 432.0)];
    let heights = |puppet: &Puppet| samples.map(|(id, y)| section_center(puppet, &model, id, y).y);
    let mut carried = Puppet::new(&model);
    carried.set_physics_enabled(false);
    carried.tick(&model, DT);
    let rest = heights(&carried);
    // Whichever way the pitch param drops the fringe.
    carried.set_param_value(&pitch, 15.0);
    carried.tick(&model, DT);
    let down = if heights(&carried)[0] < rest[0] {
        15.0
    } else {
        -15.0
    };
    carried.set_param_value(&pitch, 0.0);
    let mut live = Puppet::new(&model);
    live.settle_physics(&model);
    let mut lift = [0.0_f32; 2];
    let mut awake = true;
    for frame in 0..240 {
        let t = (frame as f32 / 12.0).min(1.0);
        let value = down * t * t * (3.0 - 2.0 * t);
        live.set_param_value(&pitch, value);
        carried.set_param_value(&pitch, value);
        awake = live.tick(&model, DT).physics;
        carried.tick(&model, DT);
        let (hair, head) = (heights(&live), heights(&carried));
        for i in 0..2 {
            lift[i] = lift[i].max(hair[i] - head[i]);
        }
    }
    for (i, (id, _)) in samples.into_iter().enumerate() {
        assert!(
            lift[i] > 2.0,
            "{id}: lifted {}px over the dropping head",
            lift[i]
        );
        let settled = (heights(&live)[i] - heights(&carried)[i]).abs();
        assert!(settled < 0.05, "{id}: rests {settled}px off its pose");
    }
    assert!(!awake, "the hair is still moving 4 s after the nod");
}

/// **Every guide runs along its lock.** Each joint but the last lies on the
/// lock's art, and the last sits at the painted tip, so a bend turns the art
/// about the hair's own line and the simulated tip is the visible one.
#[test]
fn every_guide_runs_along_its_lock() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.set_physics_enabled(false);
    puppet.tick(&model, DT);
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(&model, &puppet).unwrap();
    for id in lock_spines(&model) {
        let node = model.node(id).unwrap();
        let catchlight_core::model::ModelNodeKind::Spine(spine) = &node.kind else {
            continue;
        };
        let parts: Vec<_> = node
            .children()
            .iter()
            .filter(|child| {
                matches!(
                    model.node(child).unwrap().kind,
                    catchlight_core::model::ModelNodeKind::Part(_)
                )
            })
            .collect();
        // A joint, from spine space into a part's authored space.
        let within = |part: &NodeId, joint: [f32; 2]| {
            catchlight_core::components::Transform::from(&model.node(part).unwrap().transform)
                .to_matrix()
                .inverse()
                .transform_point3(Vec3::new(joint[0], joint[1], 0.0))
                .truncate()
        };
        let (tip, along) = spine.joints().split_last().unwrap();
        for (k, joint) in std::iter::once(&[0.0, 0.0]).chain(along).enumerate() {
            assert!(
                parts
                    .iter()
                    .any(|part| rendered(&model, &puppet, part, within(part, *joint)).is_some()),
                "{id}: joint {k} lies off the lock's art"
            );
        }
        let gap = parts
            .iter()
            .flat_map(|part| {
                let at = within(part, *tip);
                geometry
                    .mesh(part)
                    .unwrap()
                    .vertices()
                    .map(move |v| v.rest.distance(at))
                    .collect::<Vec<_>>()
            })
            .fold(f32::MAX, f32::min);
        assert!(
            gap < 6.0,
            "{id}: the guide ends {gap}px from the painted tip"
        );
    }
}
