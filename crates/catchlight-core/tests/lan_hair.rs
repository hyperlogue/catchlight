#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test aborts on a broken fixture"
)]
//! Lan's hair responses under head motion. Requires the Lan Git LFS object.

use catchlight_core::{Model, NodeId, ParamId, Puppet, Vec2, Vec3};

const DT: f32 = 1.0 / 60.0;

const FRONT_LONG: [&str; 2] = ["Front-Hair-Strand-2-Spine", "Front-Hair-Strand-5-Spine"];
const FRINGE: [&str; 2] = ["Front-Hair-Strand-3-Spine", "Front-Hair-Strand-4-Spine"];

/// Where the art renders a point given in the part's authored space. `None`
/// when the point lies off the part's mesh.
fn rendered(model: &Model, puppet: &Puppet, part: &NodeId, rest: Vec2) -> Option<Vec2> {
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(model, puppet).unwrap();
    rendered_in(&geometry, part, rest)
}

fn rendered_in(
    geometry: &catchlight_core::geometry::EvaluatedGeometry,
    part: &NodeId,
    rest: Vec2,
) -> Option<Vec2> {
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

fn hair_part(spine: &str) -> NodeId {
    NodeId::new(if spine.starts_with("Front") {
        "front-hair"
    } else {
        "back-hair"
    })
    .unwrap()
}

fn strand_spines(model: &Model) -> impl Iterator<Item = &NodeId> {
    model
        .node_ids()
        .filter(|id| id.as_str().contains("Hair-Strand"))
}

/// Model space: its root, then each joint.
fn guide(model: &Model, spine: &str) -> Vec<Vec2> {
    let id = NodeId::new(spine).unwrap();
    let mut to_model = catchlight_core::Mat4::IDENTITY;
    let mut at = Some(&id);
    while let Some(node) = at.and_then(|id| model.node(id)) {
        to_model =
            catchlight_core::components::Transform::from(&node.transform).to_matrix() * to_model;
        at = node.parent();
    }
    let catchlight_core::model::ModelNodeKind::Spine(data) = &model.node(&id).unwrap().kind else {
        panic!("{spine} is not a spine")
    };
    std::iter::once([0.0, 0.0])
        .chain(data.joints().iter().copied())
        .map(|[x, y]| to_model.transform_point3(Vec3::new(x, y, 0.0)).truncate())
        .collect()
}

fn along(line: &[Vec2], f: f32) -> Vec2 {
    let total: f32 = line.windows(2).map(|w| w[0].distance(w[1])).sum();
    let mut left = total * f;
    for w in line.windows(2) {
        let span = w[0].distance(w[1]);
        if left <= span {
            return w[0].lerp(w[1], left / span);
        }
        left -= span;
    }
    *line.last().unwrap()
}

#[derive(Clone)]
struct Sample {
    spine: &'static str,
    part: NodeId,
    rest: Vec2,
}

/// A guide drifts off the painted strand near its tip, so the sample sits at
/// the strand's cross-section centre nearest the guide.
fn sample(model: &Model, spine: &'static str, f: f32) -> Sample {
    let part = hair_part(spine);
    let mut puppet = Puppet::new(model);
    puppet.set_motion_responses_enabled(false);
    puppet.tick(model, DT);
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(model, &puppet).unwrap();
    let mesh = geometry.mesh(&part).unwrap();
    let triangles: Vec<[Vec2; 3]> = mesh
        .triangles()
        .map(|(_, t)| t.map(|i| mesh.vertex(i as usize).unwrap().rest))
        .collect();
    let inside = |p: Vec2| {
        triangles.iter().any(|[a, b, c]| {
            let area = (*b - *a).perp_dot(*c - *a);
            let u = (p - *a).perp_dot(*c - *a) / area;
            let v = (*b - *a).perp_dot(p - *a) / area;
            area.abs() > 1e-9 && u >= 0.0 && v >= 0.0 && u + v <= 1.0
        })
    };
    let on_guide = along(&guide(model, spine), f);
    let step = |k: i32| on_guide + Vec2::new(k as f32, 0.0);
    let hit = (0..40)
        .flat_map(|k| [k, -k])
        .find(|&k| inside(step(k)))
        .unwrap_or_else(|| panic!("{spine} at {on_guide:?} lies off its art"));
    let left = (hit - 60..=hit)
        .rev()
        .take_while(|&k| inside(step(k)))
        .last()
        .unwrap();
    let right = (hit..=hit + 60)
        .take_while(|&k| inside(step(k)))
        .last()
        .unwrap();
    Sample {
        spine,
        part,
        rest: step(left).lerp(step(right), 0.5),
    }
}

fn at(model: &Model, puppet: &Puppet, s: &Sample) -> Vec2 {
    points(model, puppet, std::slice::from_ref(s))[0]
}

fn points(model: &Model, puppet: &Puppet, samples: &[Sample]) -> Vec<Vec2> {
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(model, puppet).unwrap();
    samples
        .iter()
        .map(|s| {
            rendered_in(&geometry, &s.part, s.rest)
                .unwrap_or_else(|| panic!("{} at {:?} lies off its art", s.spine, s.rest))
        })
        .collect()
}

fn fringe(model: &Model) -> Vec<Sample> {
    FRINGE
        .iter()
        .flat_map(|spine| (0..=10).map(move |k| sample(model, spine, k as f32 / 10.0 * 0.95)))
        .collect()
}

fn root_on_art(
    model: &Model,
    puppet: &Puppet,
    part: &NodeId,
    slot: &catchlight_core::SlotId,
) -> Vec2 {
    let v = model.slot(part, slot).unwrap().vertex().unwrap();
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
        .filter(|id| id.as_str().starts_with("motion/"))
    {
        let inputs = live.response_inputs(id).unwrap();
        let catchlight_core::ResponseSource::PartSlot { part, slot, .. } = &inputs[0].source else {
            continue;
        };
        let sample = Vec2::from_array(inputs[0].position.unwrap());
        let gap = sample.distance(root_on_art(model, posed, part, slot));
        if gap > worst.0 {
            worst = (gap, id.to_string());
        }
        count += 1;
    }
    assert!(count >= 10, "only {count} rooted responses");
    worst
}

#[test]
fn front_strands_keep_their_length_while_the_head_rolls() {
    let model = lan();
    let roll = ParamId::new("head_roll").unwrap();
    let strands =
        FRONT_LONG.map(|spine| [0.02, 0.2, 0.4, 0.6, 0.8, 0.95].map(|f| sample(&model, spine, f)));
    let length = |puppet: &Puppet, points: &[Sample]| -> f32 {
        let centres: Vec<_> = points.iter().map(|s| at(&model, puppet, s)).collect();
        centres.windows(2).map(|w| w[0].distance(w[1])).sum()
    };
    let mut rest = Puppet::new(&model);
    rest.set_motion_responses_enabled(false);
    rest.tick(&model, DT);
    // The roll deform stretches this build's strands by under 4% at full reach.
    for responses_enabled in [false, true] {
        for angle in [-30.0, -15.0, 15.0, 30.0] {
            let mut puppet = Puppet::new(&model);
            puppet.set_motion_responses_enabled(responses_enabled);
            puppet.set_param_value(&roll, angle);
            if responses_enabled {
                puppet.reset_motion_responses(&model);
            }
            puppet.tick(&model, DT);
            for points in &strands {
                let drawn = length(&rest, points);
                let posed = length(&puppet, points);
                assert!(
                    (posed / drawn - 1.0).abs() < 0.045,
                    "{} at roll {angle} (motion responses enabled: {responses_enabled}): length {posed}, drawn {drawn}",
                    points[0].spine
                );
            }
        }
    }
}

#[test]
fn front_strands_settle_promptly_after_a_head_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.reset_motion_responses(&model);
    let roll = ParamId::new("head_roll").unwrap();
    let samples = [
        sample(&model, FRONT_LONG[0], 0.6),
        sample(&model, FRONT_LONG[0], 0.95),
        sample(&model, FRONT_LONG[1], 0.6),
        sample(&model, FRONT_LONG[1], 0.95),
    ];
    // Roll 12 units over a quarter second, then hold. The tip should settle
    // within half a pixel in about a second.
    let mut held = Vec::new();
    for frame in 0..150 {
        let t = (frame as f32 / 15.0).min(1.0);
        puppet.set_param_value(&roll, 12.0 * t * t * (3.0 - 2.0 * t));
        puppet.tick(&model, DT);
        if frame >= 14 {
            held.push(points(&model, &puppet, &samples));
        }
    }
    let last = held.last().unwrap().clone();
    for (i, s) in samples.iter().enumerate() {
        let settled = held
            .iter()
            .rposition(|h| h[i].distance(last[i]) > 0.5)
            .map_or(0, |k| k + 1);
        assert!(
            settled as f32 * DT < 1.25,
            "{} at rest {:?}: still moving {}s after the head stopped",
            s.spine,
            s.rest,
            settled as f32 * DT
        );
    }
}

#[test]
fn back_hair_falls_beside_the_neck_under_a_rolled_head() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.set_param_value(&ParamId::new("head_roll").unwrap(), -30.0);
    puppet.reset_motion_responses(&model);
    puppet.tick(&model, DT);
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(&model, &puppet).unwrap();
    // Mesh boundaries include a small transparent margin around the silhouette.
    for (y, expected_x) in BACK_HAIR_EDGE {
        let mut left = f32::INFINITY;
        let mesh = geometry.mesh(&NodeId::new("back-hair").unwrap()).unwrap();
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
        assert!(
            (left - expected_x).abs() < 5.0,
            "back hair at y={y}: edge {left}, reference {expected_x}"
        );
    }
}

const BACK_HAIR_EDGE: [(f32, f32); 3] = [(775.0, -121.44), (725.0, -139.94), (675.0, -160.64)];

#[test]
fn front_strands_do_not_ring_after_a_tiny_head_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.reset_motion_responses(&model);
    let mut carried = Puppet::new(&model);
    carried.set_motion_responses_enabled(false);
    let roll = ParamId::new("head_roll").unwrap();
    let samples = FRONT_LONG.map(|spine| sample(&model, spine, 0.95));
    let mut swing = Vec::new();
    // After a half-unit roll, under a third of the swing may remain 0.75 s after
    // the head stops.
    for frame in 0..=75 {
        let t = (frame as f32 / 15.0).min(1.0);
        let value = 0.5 * t * t * (3.0 - 2.0 * t);
        puppet.set_param_value(&roll, value);
        carried.set_param_value(&roll, value);
        puppet.tick(&model, DT);
        carried.tick(&model, DT);
        let (live, still) = (
            points(&model, &puppet, &samples),
            points(&model, &carried, &samples),
        );
        swing.push(
            live.iter()
                .zip(&still)
                .map(|(a, b)| a.distance(*b))
                .collect::<Vec<_>>(),
        );
    }
    for (i, s) in samples.iter().enumerate() {
        let peak = swing.iter().map(|w| w[i]).fold(0.0_f32, f32::max);
        let left = swing[60..].iter().map(|w| w[i]).fold(0.0_f32, f32::max);
        assert!(
            peak > 0.1,
            "{}: the response swung the tip only {peak}px",
            s.spine
        );
        assert!(
            left < peak * 0.3,
            "{}: still swinging {left}px of a {peak}px swing after the head stopped",
            s.spine
        );
    }
}

/// The head reaches the hair through mesh group deforms alone.
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

#[test]
fn simulation_roots_stay_on_the_art_through_every_mesh_group() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    let mut posed = Puppet::new(&model);
    posed.set_motion_responses_enabled(false);
    let roll = ParamId::new("head_roll").unwrap();
    for angle in [-30.0, 30.0] {
        puppet.set_param_value(&roll, angle);
        puppet.reset_motion_responses(&model);
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
        puppet.reset_motion_responses(&model);
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
    let fringe = fringe(&model);
    let mut simulated = Puppet::new(&model);
    let mut drawn = Puppet::new(&model);
    drawn.set_motion_responses_enabled(false);
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
        simulated.reset_motion_responses(&model);
        simulated.tick(&model, DT);
        drawn.tick(&model, DT);
        assert_fringe_shape(
            &model,
            &fringe,
            &simulated,
            &drawn,
            &format!("{name}={value}"),
            0.5,
        );
        simulated.set_param_value(&param, 0.0);
        drawn.set_param_value(&param, 0.0);
    }
}

fn assert_fringe_shape(
    model: &Model,
    fringe: &[Sample],
    simulated: &Puppet,
    drawn: &Puppet,
    pose: &str,
    limit: f32,
) {
    let (actual, reference) = (
        points(model, simulated, fringe),
        points(model, drawn, fringe),
    );
    for ((s, a), b) in fringe.iter().zip(actual).zip(reference) {
        let off = a.distance(b);
        assert!(
            off < limit,
            "{} at {pose}: fringe moved {off}px away from its drawing",
            s.spine
        );
    }
}

#[test]
fn fringe_keeps_its_shape_while_the_head_rolls() {
    let model = lan();
    let fringe = fringe(&model);
    let mut simulated = Puppet::new(&model);
    let mut drawn = Puppet::new(&model);
    drawn.set_motion_responses_enabled(false);
    simulated.reset_motion_responses(&model);
    let roll = ParamId::new("head_roll").unwrap();
    for frame in 0..120 {
        let value = 30.0 * (std::f32::consts::TAU * frame as f32 * DT).sin();
        simulated.set_param_value(&roll, value);
        drawn.set_param_value(&roll, value);
        simulated.tick(&model, DT);
        drawn.tick(&model, DT);
        assert_fringe_shape(
            &model,
            &fringe,
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
    puppet.reset_motion_responses(&model);
    let long: Vec<_> = strand_spines(&model)
        .filter(|id| guide(&model, id.as_str()).len() > 3)
        .map(|id| {
            let spine: &'static str = id.as_str().to_owned().leak();
            (sample(&model, spine, 0.65), sample(&model, spine, 0.92))
        })
        .collect();
    assert!(
        long.len() >= 10,
        "must exercise the long front and back strands"
    );
    let lower = |puppet: &Puppet, (high, low): &(Sample, Sample)| {
        (at(&model, puppet, low) - at(&model, puppet, high)).normalize()
    };
    let face = NodeId::new("face").unwrap();
    let face = |puppet: &Puppet| {
        let at = |y: f32| rendered(&model, puppet, &face, Vec2::new(0.0, y)).unwrap();
        (at(700.0) - at(900.0)).normalize()
    };
    let mut drawn = Puppet::new(&model);
    drawn.reset_motion_responses(&model);
    drawn.tick(&model, DT);
    for angle in [-30.0, 30.0] {
        puppet.set_param_value(&roll, angle);
        puppet.reset_motion_responses(&model);
        puppet.tick(&model, DT);
        let tilt = face(&puppet).angle_to(face(&drawn)).abs().to_degrees();
        assert!(tilt > 10.0, "roll {angle} tilts the face {tilt} degrees");
        for strand in &long {
            let turn = lower(&puppet, strand)
                .angle_to(lower(&drawn, strand))
                .abs()
                .to_degrees();
            assert!(
                turn < tilt * 0.5,
                "{} at {angle}: lower hair turned {turn} degrees, the face {tilt}",
                strand.0.spine
            );
        }
    }
}

#[test]
fn long_strands_swing_with_a_body_roll_and_return_to_the_drawing() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.reset_motion_responses(&model);
    let mut carried = Puppet::new(&model);
    carried.set_motion_responses_enabled(false);
    let roll = ParamId::new("body_roll").unwrap();
    let tips = [
        sample(&model, FRONT_LONG[0], 0.95),
        sample(&model, "Back-Hair-Strand-4-Spine", 0.95),
    ];
    let mut swing = [0.0f32; 2];
    let mut off = [0.0f32; 2];
    for frame in 1..=240 {
        let value = 10.0 * (frame as f32 / 6.0).min(1.0);
        puppet.set_param_value(&roll, value);
        carried.set_param_value(&roll, value);
        puppet.tick(&model, DT);
        carried.tick(&model, DT);
        let (live, still) = (
            points(&model, &puppet, &tips),
            points(&model, &carried, &tips),
        );
        for i in 0..tips.len() {
            off[i] = live[i].distance(still[i]);
            swing[i] = swing[i].max(off[i]);
        }
    }
    for (i, tip) in tips.iter().enumerate() {
        assert!(
            swing[i] > 2.0,
            "{}: a body roll swings the strand, peak {}px",
            tip.spine,
            swing[i]
        );
        assert!(
            off[i] < 0.05,
            "{}: {}px off the drawing after the body holds",
            tip.spine,
            off[i]
        );
    }
}

/// 30 and 60 fps land on the 240 fps trajectory at the same instants.
#[test]
fn hair_swings_alike_at_any_frame_rate() {
    let model = lan();
    let tips = [
        sample(&model, FRONT_LONG[0], 0.95),
        sample(&model, "Back-Hair-Strand-4-Spine", 0.95),
    ];
    let run = |fps: f32| -> Vec<Vec<(f32, Vec2)>> {
        let mut puppet = Puppet::new(&model);
        puppet.reset_motion_responses(&model);
        let mut out = vec![Vec::new(); tips.len()];
        for f in 1..=(2.0 * fps) as usize {
            let t = f as f32 / fps;
            puppet.set_param_value(&ParamId::new("head_roll").unwrap(), 40.0 * t.min(0.5));
            puppet.set_param_value(&ParamId::new("head_yaw").unwrap(), 15.0 * (3.0 * t).sin());
            puppet.tick(&model, 1.0 / fps);
            for (i, p) in points(&model, &puppet, &tips).into_iter().enumerate() {
                out[i].push((t, p));
            }
        }
        out
    };
    let reference = run(240.0);
    for fps in [30.0, 60.0] {
        for (tip, (trace, truth)) in tips.iter().zip(run(fps).iter().zip(&reference)) {
            for (t, at) in trace {
                let k = (t * 240.0).round() as usize - 1;
                let off = at.distance(truth[k].1);
                assert!(
                    off < 0.1,
                    "{} at {fps} fps, t={t:.3}: {off:.3} px off 240 fps",
                    tip.spine
                );
            }
        }
    }
}

/// Hair sleeps under any root transform: responses read model-space slots.
#[test]
fn hair_sleeps_under_a_placed_root() {
    let model = lan();
    let root = catchlight_core::Mat4::from_translation(Vec3::new(120.0, -40.0, 0.0))
        * catchlight_core::Mat4::from_rotation_z(0.37)
        * catchlight_core::Mat4::from_scale(Vec3::new(0.73, 0.73, 1.0));
    for yaw in [0.0, 20.0] {
        let mut puppet = Puppet::new(&model);
        puppet.set_param_value(&ParamId::new("head_yaw").unwrap(), yaw);
        puppet.reset_motion_responses(&model);
        let mut awake = 0;
        for frame in 0..600 {
            let motion = puppet.tick_with_root(&model, root, DT);
            if frame >= 500 && motion.motion_responses {
                awake += 1;
            }
        }
        assert_eq!(awake, 0, "yaw {yaw}: hair still awake under a placed root");
    }
}

/// A held drag on the hair lets it settle.
#[test]
fn a_held_drag_on_the_hair_lets_it_settle() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.reset_motion_responses(&model);
    let hair = puppet
        .node_idx(&NodeId::new("front-hair").unwrap())
        .unwrap();
    let at = puppet.get(hair).unwrap().transform;
    assert!(puppet.set_scratch_transform(
        hair,
        catchlight_core::ScratchTransform {
            translation: Some(at.translation + Vec3::new(25.0, 0.0, 0.0)),
            rotation: Some(at.rotation + Vec3::new(0.0, 0.0, 0.2)),
            ..Default::default()
        },
    ));
    let mut awake = 0;
    for frame in 0..900 {
        let motion = puppet.tick(&model, DT);
        if frame >= 800 && motion.motion_responses {
            awake += 1;
        }
    }
    assert_eq!(awake, 0, "the hair never settled under a held drag");
}

#[test]
fn hair_lifts_as_the_head_drops_and_comes_back_to_the_drawing() {
    let model = lan();
    let pitch = ParamId::new("head_pitch").unwrap();
    let samples = [
        sample(&model, FRINGE[0], 0.9),
        sample(&model, FRONT_LONG[0], 0.95),
    ];
    let heights = |puppet: &Puppet| {
        let p = points(&model, puppet, &samples);
        [p[0].y, p[1].y]
    };
    let mut carried = Puppet::new(&model);
    carried.set_motion_responses_enabled(false);
    carried.tick(&model, DT);
    let rest = heights(&carried);
    carried.set_param_value(&pitch, 15.0);
    carried.tick(&model, DT);
    let down = if heights(&carried)[0] < rest[0] {
        15.0
    } else {
        -15.0
    };
    carried.set_param_value(&pitch, 0.0);
    let mut live = Puppet::new(&model);
    live.reset_motion_responses(&model);
    let mut lift = [0.0_f32; 2];
    let mut awake = true;
    for frame in 0..480 {
        let t = (frame as f32 / 12.0).min(1.0);
        let value = down * t * t * (3.0 - 2.0 * t);
        live.set_param_value(&pitch, value);
        carried.set_param_value(&pitch, value);
        awake = live.tick(&model, DT).motion_responses;
        carried.tick(&model, DT);
        let (hair, head) = (heights(&live), heights(&carried));
        for i in 0..2 {
            lift[i] = lift[i].max(hair[i] - head[i]);
        }
    }
    for (i, s) in samples.iter().enumerate() {
        assert!(
            lift[i] > 0.5,
            "{}: lifted {}px over the dropping head",
            s.spine,
            lift[i]
        );
        let settled = (heights(&live)[i] - heights(&carried)[i]).abs();
        assert!(
            settled < 0.05,
            "{}: rests {settled}px off its pose",
            s.spine
        );
    }
    assert!(!awake, "the hair is still moving 8 s after the nod");
}

#[test]
fn every_guide_runs_along_its_strand() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    puppet.set_motion_responses_enabled(false);
    puppet.tick(&model, DT);
    let geometry = catchlight_core::geometry::EvaluatedGeometry::new(&model, &puppet).unwrap();
    let mut strands = 0;
    for id in strand_spines(&model) {
        strands += 1;
        let part = hair_part(id.as_str());
        let line = guide(&model, id.as_str());
        let (tip, along) = line.split_last().unwrap();
        for (k, joint) in along.iter().enumerate() {
            assert!(
                rendered(&model, &puppet, &part, *joint).is_some(),
                "{id}: joint {k} lies off the hair's art"
            );
        }
        let gap = geometry
            .mesh(&part)
            .unwrap()
            .vertices()
            .map(|v| v.rest.distance(*tip))
            .fold(f32::MAX, f32::min);
        assert!(
            gap < 6.0,
            "{id}: the guide ends {gap}px from the painted tip"
        );
    }
    assert!(strands >= 14, "only {strands} strand guides");
}
