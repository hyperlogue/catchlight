#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The bundled model's long locks hang below a rolling head while its short
//! swept fringe retains its shape. Exercise Puppet and the evaluated meshes
//! so attachments, output params, limits and visible deformation participate.
//! Requires the Lan Git LFS object, like the visual suite.
//!
//! Long locks keep a sprung root that follows the scalp. Back lengths are
//! limp; the two long front locks retain a soft elastic response. Their air
//! drag suppresses ringing after small movements; too much leaves them
//! creeping toward rest long after the head stops. The second joint has room to counter head roll; narrow stops along
//! the lower lengths must not prevent them from hanging under gravity.
//! Back locks have a short supported root and a gradual transition into their
//! hanging length, so head roll cannot carry a long upper section sideways.
//! Front guides follow the painted tips. Authored head-roll deforms shape the
//! front curves and back silhouette before the live spine transports them.
//! Front deforms bend each lock along its own length without stretching it;
//! the mesh checks below cover those curves as well as particle motion.
//! The connected right fringe shares the side lock's supported upper link
//! and head-roll deformation field, so its painted cuts cannot pull apart.
//! The left swept fringe uses stiff elastic links. Simulation roots and the
//! artwork must share attachments; inverse translations on the parts conceal
//! a displaced simulation pivot until the links bend.

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
    // Roll 12 units over a quarter second, then hold. Heavy air drag on soft
    // springs creeps toward rest for most of a second, which reads as goo.
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
            settled as f32 * DT < 0.5,
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
    for (y, expected_x) in [(775.0, -107.61), (725.0, -116.16), (675.0, -126.26)] {
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
    // visible tips may ease toward rest, but must not travel back and forth
    // nearly as far as the original small movement after it has stopped.
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
            ringing < 0.1,
            "{id}: {ringing}px of extra back-and-forth tip travel after a tiny roll"
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
                let root = puppet.transforms().get(idx).w_axis;
                assert!(
                    chain.anchor.distance(Vec2::new(root.x, -root.y)) < 0.01,
                    "{id} at {angle}, frame {frame}: back-hair root lagged behind its attachment"
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
            // The scalp attachment is fixed in the parent's frame. Moving
            // the spine and counter-moving its art puts the simulation's
            // pivot somewhere other than the visible strand's root.
            let node = model.node(id).unwrap();
            let parent = puppet.node_idx(node.parent().unwrap()).unwrap();
            let expected = puppet
                .transforms()
                .get(parent)
                .transform_point3(Vec3::from_array(node.transform.translation));
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
            assert!(
                tilt < 8.0,
                "{id} at {angle}: lower hair remains {tilt} degrees from gravity"
            );
        }
        assert!(
            locks >= 10,
            "must exercise the bundled long front and back locks"
        );
    }
}
