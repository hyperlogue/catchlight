#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The bundled model's long locks hang below a rolling head while its short
//! swept fringe retains its shape. Exercise Puppet and the evaluated meshes
//! so attachments, output params, limits and visible deformation participate.
//! Requires the Lan Git LFS object, like the visual suite.
//!
//! Long locks keep a stiff root that follows the scalp and soft lengths that
//! swing about the drawing: the strand's neutral rest is the drawing, so a
//! settled default pose renders as painted. Chain damping, not air drag,
//! settles them, so they swing without trailing the head. The second joint
//! has room to counter head roll; narrow stops along the lower lengths must
//! not prevent them from hanging under gravity.
//! Back locks have a short supported root and a gradual transition into their
//! hanging length, so head roll cannot carry a long upper section sideways.
//! Front guides follow the painted tips. Authored head-roll deforms shape the
//! front curves and back silhouette before the live spine transports them.
//! Front deforms bend each lock along its own length without stretching it;
//! the mesh checks below cover those curves as well as particle motion.
//! The connected right fringe shares the side lock's supported upper link
//! and head-roll deformation field, so its painted cuts cannot pull apart.
//! The left swept fringe uses stiff elastic links. Simulation roots follow
//! the guide embedded in each lock's art, and no hair spine is slid against
//! its art on head yaw or pitch: the rig keeps both on one attachment.

use catchlight_core::{Model, NodeKind, ParamId, Puppet, Vec2, Vec3};

const DT: f32 = 1.0 / 60.0;

/// Cross-section midpoint in the evaluated mesh, selected by authored Y.
/// This observes the visible deformation, not the particle-chain endpoint.
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

#[test]
fn front_locks_keep_their_length_while_the_head_rolls() {
    let model = lan();
    let roll = ParamId::new("head_roll").unwrap();
    // Cross-section centres from the scalp to the painted tip, by authored Y.
    let samples = [
        (
            "front-hair-lock-2",
            [940.0, 820.0, 700.0, 580.0, 460.0, 423.98],
        ),
        (
            "front-hair-lock-7",
            [940.0, 820.0, 700.0, 570.0, 495.0, 420.93],
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
    // Head-roll deforms may bend the locks, but hair does not stretch. A
    // strand that grows and shrinks with the head reads as jelly.
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
                    (posed / drawn - 1.0).abs() < 0.03,
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
        ("front-hair-lock-2", 423.98),
        ("front-hair-lock-7", 570.0),
        ("front-hair-lock-7", 420.93),
    ];
    // Roll 12 units over a quarter second, then hold. The strand should come
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
fn back_hair_outline_follows_the_reviewed_fall() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.set_param_value(&ParamId::new("head_roll").unwrap(), -30.0);
    puppet.settle_physics(&model);
    puppet.tick(&model, DT);
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(&model, &puppet).unwrap();
    // The outer back-hair contour beside the cheek and neck. Mesh boundaries
    // include a small transparent margin around the painted silhouette.
    // Measured with the root-driven strand, whose neutral pose is the
    // drawing: the fall is the drawing plus the change gravity makes.
    for (y, expected_x) in [(775.0, -109.33), (725.0, -127.61), (675.0, -151.23)] {
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

#[test]
fn long_lock_mesh_tips_lag_a_small_head_roll() {
    let model = lan();
    let mut live = Puppet::new(&model);
    live.settle_physics(&model);
    live.tick(&model, DT);
    let samples = [("front-hair-lock-2", 423.98), ("front-hair-lock-7", 420.93)];
    let before = samples.map(|(id, y)| section_center(&live, &model, id, y));
    let mut carried = Puppet::new(&model);
    carried.set_physics_enabled(false);
    for id in model.node_ids() {
        let NodeKind::Spine(spine) = &live.get(live.node_idx(id).unwrap()).unwrap().kind else {
            continue;
        };
        if spine.chain.is_none() {
            continue;
        }
        let catchlight_core::model::ModelNodeKind::Spine(authored) = &model.node(id).unwrap().kind
        else {
            unreachable!()
        };
        for param in authored.targets().iter().flatten() {
            carried.set_param_value(param, live.param_value(param).unwrap_or(0.0));
        }
    }
    let roll = ParamId::new("head_roll").unwrap();
    live.set_param_value(&roll, 1.0);
    carried.set_param_value(&roll, 1.0);
    live.tick(&model, DT);
    carried.tick(&model, DT);
    for (i, (id, y)) in samples.into_iter().enumerate() {
        let actual = section_center(&live, &model, id, y).distance(before[i]);
        let rigid = section_center(&carried, &model, id, y).distance(before[i]);
        assert!(
            actual < rigid * 0.95,
            "{id}: mesh tip moved {actual}px, rigid carry {rigid}px"
        );
    }
}

#[test]
fn front_locks_do_not_ring_after_a_tiny_head_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    let roll = ParamId::new("head_roll").unwrap();
    let samples = [("front-hair-lock-2", 423.98), ("front-hair-lock-7", 420.93)];
    let mut held = Vec::new();
    // A half-unit roll over a quarter second, then hold the head still. The
    // visible tips may ease toward rest, but must not travel back and forth:
    // hair that wobbles after a small movement reads as jelly.
    for frame in 0..=75 {
        let t = (frame as f32 / 15.0).min(1.0);
        puppet.set_param_value(&roll, 0.5 * t * t * (3.0 - 2.0 * t));
        puppet.tick(&model, DT);
        if frame >= 15 {
            held.push(samples.map(|(id, y)| section_center(&puppet, &model, id, y)));
        }
    }
    for (i, (id, _)) in samples.into_iter().enumerate() {
        let travel: f32 = held.windows(2).map(|w| w[1][i].distance(w[0][i])).sum();
        let direct = held.first().unwrap()[i].distance(held.last().unwrap()[i]);
        let ringing = travel - direct;
        assert!(
            ringing < 0.5,
            "{id}: {ringing}px of extra back-and-forth tip travel after a tiny roll"
        );
    }
}

/// Head yaw and pitch reach the hair through the head's warp alone: no hair
/// spine, and no part directly under one, carries a translation key on them.
/// A spine slid one way with its art slid back renders the same, but leaves
/// the node somewhere the art is not.
#[test]
fn no_hair_spine_is_slid_against_its_art() {
    let model = lan();
    let head = [
        ParamId::new("head_yaw").unwrap(),
        ParamId::new("head_pitch").unwrap(),
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
            "{} slides on head yaw or pitch",
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

#[test]
fn fringe_simulation_roots_stay_on_the_visible_scalp_during_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    let roll = ParamId::new("head_roll").unwrap();
    for angle in [-30.0, 30.0] {
        puppet.set_param_value(&roll, angle);
        puppet.settle_physics(&model);
        puppet.tick(&model, DT);
        let mut fringe = 0;
        for id in model.node_ids() {
            let idx = puppet.node_idx(id).unwrap();
            let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
                continue;
            };
            let Some(chain) = &spine.chain else { continue };
            if chain.links.len() >= 6 || !id.as_str().contains("Hair-Lock") {
                continue;
            }
            fringe += 1;
            // Mesh-group warps may legitimately move the whole attachment.
            // Compare against the same authored point in each child part,
            // not an unposed parent transform that omits those warps.
            for child in model.node(id).unwrap().children() {
                let base = model.node(child).unwrap();
                if !matches!(base.kind, catchlight_core::model::ModelNodeKind::Part(_)) {
                    continue;
                }
                let root_in_art = catchlight_core::components::Transform::from(&base.transform)
                    .to_matrix()
                    .inverse()
                    .transform_point3(Vec3::ZERO);
                let visible = puppet
                    .transforms()
                    .get(puppet.node_idx(child).unwrap())
                    .transform_point3(root_in_art);
                let offset = chain.anchor.distance(Vec2::new(visible.x, -visible.y));
                assert!(
                    offset < 0.01,
                    "{id} at {angle}: simulation root is {offset}px away from {child}'s attachment"
                );
            }
        }
        assert!(fringe > 0);
    }
}

#[test]
fn shaped_fringe_stays_close_to_its_drawing_across_head_poses() {
    let model = lan();
    let mut simulated = Puppet::new(&model);
    let mut drawn = Puppet::new(&model);
    drawn.set_physics_enabled(false);
    // The short swept fringe is shaped hair, not a long loose lock. Keep its
    // actual mesh within 10 model pixels of the authored pose so it cannot
    // turn into a curtain over the eyes. Long-lock gravity is tested below.
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
        assert_fringe_shape(&model, &simulated, &drawn, &format!("{name}={value}"), 10.0);
        simulated.set_param_value(&param, 0.0);
        drawn.set_param_value(&param, 0.0);
    }
}

fn assert_fringe_shape(model: &Model, simulated: &Puppet, drawn: &Puppet, pose: &str, limit: f32) {
    use catchlight_core::geometry::EvaluatedGeometry;
    let actual = EvaluatedGeometry::new(model, simulated).unwrap();
    let reference = EvaluatedGeometry::new(model, drawn).unwrap();
    // Select the visible fringe pieces rather than inferring their role
    // from link count: the right cluster shares the side lock's upper support.
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
            15.0,
        );
    }
}

#[test]
fn long_hair_hangs_down_while_its_roots_follow_head_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    let roll = ParamId::new("head_roll").unwrap();
    puppet.settle_physics(&model);
    let initial: Vec<_> = model
        .node_ids()
        .filter_map(|id| {
            let idx = puppet.node_idx(id)?;
            let NodeKind::Spine(spine) = &puppet.get(idx)?.kind else {
                return None;
            };
            let chain = spine.chain.as_ref()?;
            (chain.links.len() >= 6 && id.as_str().contains("Hair-Lock")).then(|| {
                let tip = chain.particles.last().unwrap().pos;
                (idx, tip, chain.carry.inverse() * (tip - chain.anchor))
            })
        })
        .collect();
    // On a small first movement the tip should lag the head, not be carried
    // rigidly and only start moving under gravity on later frames.
    puppet.set_param_value(&roll, 1.0);
    puppet.tick(&model, DT);
    for (idx, old_tip, local_tip) in initial {
        let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
            unreachable!()
        };
        let chain = spine.chain.as_ref().unwrap();
        let carried_tip = chain.anchor + chain.carry * local_tip;
        let tip = chain.particles.last().unwrap().pos;
        assert!(
            tip.distance(old_tip) < carried_tip.distance(old_tip) * 0.95,
            "the tip must lag the head immediately: moved {}, rigid carry {}",
            tip.distance(old_tip),
            carried_tip.distance(old_tip)
        );
    }
    // The root is the guide's root where the art renders it, on the same
    // frame: this frame's posed guide, carried by this frame's transform.
    for angle in [-30.0, 30.0] {
        puppet.set_param_value(&roll, angle);
        for frame in 0..300 {
            puppet.tick(&model, DT);
            for id in model
                .node_ids()
                .filter(|id| id.as_str().starts_with("Back-Hair-Lock"))
            {
                let idx = puppet.node_idx(id).unwrap();
                let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
                    continue;
                };
                let chain = spine.chain.as_ref().unwrap();
                let root = puppet
                    .transforms()
                    .get(idx)
                    .transform_point3(spine.guide_root().extend(0.0));
                assert!(
                    chain.anchor.distance(Vec2::new(root.x, -root.y)) < 0.01,
                    "{id} at {angle}, frame {frame}: back-hair root lagged behind its art"
                );
            }
        }
        let mut locks = 0;
        for id in model.node_ids() {
            let idx = puppet.node_idx(id).unwrap();
            let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
                continue;
            };
            let Some(chain) = &spine.chain else { continue };
            if chain.links.len() < 6 || !id.as_str().contains("Hair-Lock") {
                continue;
            }
            locks += 1;
            // The simulation root is the visible strand's root: the guide
            // embedded in the art, wherever the spine node itself was moved.
            let expected = puppet
                .transforms()
                .get(idx)
                .transform_point3(spine.guide_root().extend(0.0));
            assert!(
                chain.anchor.distance(Vec2::new(expected.x, -expected.y)) < 0.01,
                "{id} at {angle}: simulation root must remain on the scalp: anchor={:?}, expected={expected:?}", chain.anchor
            );
            let particles = &chain.particles;
            let root = particles[1].pos - particles[0].pos;
            let drawn = chain.carry * chain.links[0].drawn;
            let root_bend = root
                .normalize()
                .angle_to(drawn.normalize())
                .abs()
                .to_degrees();
            assert!(
                root_bend < 5.0,
                "{id} at {angle}: root detached from head direction by {root_bend} degrees"
            );
            if id.as_str().starts_with("Back-Hair-Lock") {
                assert!(root.x.abs() < 8.0,
                    "{id} at {angle}: supported root carries the back hair {}px sideways before it can hang down",
                    root.x.abs());
            }
            let n = particles.len();
            let tail = particles[n - 1].pos - particles[n - 2].pos;
            let tilt = tail.angle_to(Vec2::Y).abs().to_degrees();
            // Stiffness pulls the tips toward their drawn turn, so they hang
            // close to, not exactly along, gravity.
            assert!(
                tilt < 10.0,
                "{id} at {angle}: lower hair remains {tilt} degrees from gravity"
            );
        }
        assert!(
            locks >= 10,
            "must exercise the bundled long front and back locks"
        );
    }
}

/// Body roll is a warp in Lan's rig: it moves the art without turning the
/// spine nodes. The chain roots follow the warped art, so the long locks
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
        let mut out = Vec::new();
        spine.chain.as_ref().unwrap().link_bends(&mut out);
        out
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

/// Where a lock's simulated tip renders in its node frame, from the bends the
/// chain writes: the drawn links turned by each bend.
fn simulated_tip(puppet: &Puppet, spine: &str) -> Vec2 {
    let cmul = |a: Vec2, b: Vec2| Vec2::new(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
    let idx = puppet
        .node_idx(&catchlight_core::NodeId::new(spine).unwrap())
        .unwrap();
    let NodeKind::Spine(s) = &puppet.get(idx).unwrap().kind else {
        panic!("{spine} is not a spine")
    };
    let chain = s.chain.as_ref().unwrap();
    let mut bends = Vec::new();
    chain.link_bends(&mut bends);
    let (mut at, mut prev_drawn, mut prev_u) =
        (Vec2::ZERO, chain.links[0].drawn, chain.links[0].drawn);
    for (link, bend) in chain.links.iter().zip(bends) {
        let rel = cmul(link.drawn, Vec2::new(prev_drawn.x, -prev_drawn.y));
        let turn = Vec2::from_angle(-bend * std::f32::consts::PI);
        let u = cmul(cmul(prev_u, rel), turn);
        at += u * link.length;
        prev_drawn = link.drawn;
        prev_u = u;
    }
    at
}

/// **The frame rate does not change the swing.** A head roll ramp under a yaw
/// sine reaches the hair through the warped root, the keyformed guide and
/// the node transform, all evaluated on the frame they render; 30 and 60 fps
/// land on the 240 fps trajectory at the same instants.
#[test]
fn hair_swings_alike_at_any_frame_rate() {
    let model = lan();
    let locks = ["Front-Hair-Lock-2-Spine", "Back-Hair-Lock-19-Spine"];
    let run = |fps: f32| -> Vec<Vec<(f32, Vec2)>> {
        let mut puppet = Puppet::new(&model);
        puppet.settle_physics(&model);
        let mut out = vec![Vec::new(); locks.len()];
        for f in 1..=(2.0 * fps) as usize {
            let t = f as f32 / fps;
            puppet.set_param_value(&ParamId::new("head_roll").unwrap(), 40.0 * t.min(0.5));
            puppet.set_param_value(&ParamId::new("head_yaw").unwrap(), 15.0 * (3.0 * t).sin());
            puppet.tick(&model, 1.0 / fps);
            for (i, lock) in locks.iter().enumerate() {
                out[i].push((t, simulated_tip(&puppet, lock)));
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
                    "{lock} at {fps} fps, t={t:.3}: {off:.3} px off 240 fps"
                );
            }
        }
    }
}
