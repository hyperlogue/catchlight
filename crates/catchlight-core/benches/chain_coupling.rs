//! CPU puppet timing and trajectory traces; see `chain_coupling.md` for
//! commands, fixture settings and measured results. No GPU work is timed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use catchlight_core::{
    formats::clm::{ClmIndices, ClmMesh, ClmPhysics},
    id::SeededHex,
    BindingKey, BindingTarget, ChainLink, LinkFeel, Mat2, Model, ModelChain, ModelNode,
    ModelNodeKind, ModelParam, ModelPart, ModelSpine, Name, NodeId, ParamId, ParticleChainData,
    Puppet, ScalarTarget, Vec2,
};
use std::{f32::consts::TAU, hint::black_box, num::NonZeroU8, time::Instant};

const DT: f32 = 1.0 / 60.0;
const FRAMES: usize = 3000;
const WARMUP: usize = 240;
struct Rig {
    model: Model,
    turn: ParamId,
    bend: ParamId,
    part: Option<NodeId>,
}

fn rig(locks: usize, links: usize, mesh: bool, amplitude: f32) -> Rig {
    let mut hex = SeededHex::new(42);
    let mut model = Model::new();
    let root = model.root().unwrap().clone();
    let turn = model
        .add_param(
            ModelParam {
                name: Name::truncated("head translation"),
                min: -1.0,
                max: 1.0,
                default: 0.0,
                key_positions: vec![0.0, 0.5, 1.0],
            },
            &mut hex,
        )
        .unwrap();
    let head = model
        .add_node(
            &root,
            ModelNode::new("head", ModelNodeKind::Group),
            &mut hex,
        )
        .unwrap();
    let key = BindingKey::new(
        turn.clone(),
        head.clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    for (cell, value) in [(0, -amplitude), (1, 0.0), (2, amplitude)] {
        model.set_binding_key(&key, [cell, 0], value).unwrap();
    }
    let mut first_bend = None;
    let mut part = None;
    for lock in 0..locks {
        let mut targets = Vec::new();
        for link in 0..links {
            let id = model
                .add_param(
                    ModelParam {
                        name: Name::truncated(format!("bend {lock}.{link}")),
                        min: -0.5,
                        max: 0.5,
                        default: 0.0,
                        key_positions: vec![0.0, 0.5, 1.0],
                    },
                    &mut hex,
                )
                .unwrap();
            first_bend.get_or_insert_with(|| id.clone());
            targets.push(Some(id));
        }
        let mut spine = ModelSpine::new(
            (1..=links)
                .map(|i| [0.0, -160.0 * i as f32 / links as f32])
                .collect(),
        );
        let mut chain = ModelChain::new(links);
        chain.set_links(vec![
            LinkFeel {
                stiffness: 4.0,
                damping: 0.3,
                gravity_scale: 1.0,
                limit: LinkFeel::default().limit,
            };
            links
        ]);
        spine.set_chain(Some(chain));
        let mut node = ModelNode::new(format!("hair {lock}"), ModelNodeKind::Spine(spine));
        node.transform.translation = [40.0 * lock as f32, 0.0, 0.0];
        let spine_id = model.add_node(&head, node, &mut hex).unwrap();
        model.set_spine_targets(&spine_id, targets).unwrap();
        if mesh {
            let mut verts = Vec::new();
            let mut uvs = Vec::new();
            let mut indices = Vec::new();
            for row in 0..10 {
                let f = row as f32 / 9.0;
                verts.extend([-12.0, -160.0 * f, 12.0, -160.0 * f]);
                uvs.extend([0.0, f, 1.0, f]);
                if row < 9 {
                    let v = row as u16 * 2;
                    indices.extend([v, v + 1, v + 3, v, v + 3, v + 2]);
                }
            }
            let node = ModelNode::new(
                "hair art",
                ModelNodeKind::Part(ModelPart::new(ClmMesh {
                    verts,
                    uvs,
                    indices: ClmIndices::U16(indices),
                    origin: [0.0; 2],
                })),
            );
            part.get_or_insert(model.add_node(&spine_id, node, &mut hex).unwrap());
        }
    }
    Rig {
        model,
        turn,
        bend: first_bend.unwrap(),
        part,
    }
}

fn timing(frequency: bool, repeat: Option<usize>) {
    println!("timing,repeat,locks,links,vertices_per_lock,drive_px,substeps,physics_hz,min_us,median_us,p90_us");
    let counts: &[u8] = if frequency { &[4, 8, 16] } else { &[4] };
    let lock_counts: &[usize] = if frequency { &[20] } else { &[10, 20] };
    let meshes: &[bool] = if frequency { &[true] } else { &[false, true] };
    for repeat in repeat.map_or(1..=3, |r| r..=r) {
        for &locks in lock_counts {
            for links in [2, 8] {
                for &mesh in meshes {
                    for amplitude in [10.0, 120.0] {
                        let mut rig = rig(locks, links, mesh, amplitude);
                        for offset in 0..counts.len() {
                            let steps = counts[(offset + repeat) % counts.len()];
                            let mut physics = *rig.model.physics();
                            physics.chain_substeps = NonZeroU8::new(steps).unwrap();
                            rig.model.set_physics(physics);
                            let mut puppet = Puppet::new(&rig.model);
                            puppet.settle_physics(&rig.model);
                            let mut samples = Vec::with_capacity(FRAMES);
                            let mut span = (f32::INFINITY, f32::NEG_INFINITY);
                            for frame in 0..WARMUP + FRAMES {
                                puppet.set_param_value(&rig.turn, (frame as f32 * DT * TAU).sin());
                                let start = Instant::now();
                                black_box(puppet.tick(&rig.model, DT));
                                let elapsed = start.elapsed().as_nanos();
                                if frame >= WARMUP {
                                    samples.push(elapsed);
                                    let bend = puppet.param_value(&rig.bend).unwrap();
                                    span = (span.0.min(bend), span.1.max(bend));
                                }
                            }
                            assert!(span.1 - span.0 > 1e-5, "stationary chain");
                            if let Some(id) = &rig.part {
                                let deform = puppet
                                    .combined_deform(puppet.node_idx(id).unwrap())
                                    .unwrap();
                                assert!(deform.iter().any(|p| p.length_squared() > 1e-8));
                            }
                            samples.sort_unstable();
                            let us = |i: usize| samples[i] as f64 / 1000.0;
                            println!("timing,{repeat},{locks},{links},{},{amplitude},{steps},{},{:.3},{:.3},{:.3}",
                                if mesh { 20 } else { 0 }, u32::from(steps) * 60, us(0), us(FRAMES / 2), us(FRAMES * 9 / 10));
                        }
                    }
                }
            }
        }
    }
}

fn trajectory(links: usize, amplitude: f32, substeps: u8) -> Vec<Vec<Vec2>> {
    let mut chain = ParticleChainData::new(vec![
        ChainLink {
            length: 160.0 / links as f32,
            stiffness: 4.0,
            damping: 0.3,
            limit: LinkFeel::default().limit,
            ..ChainLink::default()
        };
        links
    ]);
    let gravity = ClmPhysics::default();
    chain.gravity = gravity.gravity * gravity.pixels_per_meter;
    chain.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
    let mut frames = Vec::new();
    for frame in 1..=600 {
        let anchor = Vec2::new(amplitude * (frame as f32 * DT * TAU).sin(), 0.0);
        chain.tick(
            anchor,
            Mat2::IDENTITY,
            &[],
            DT,
            NonZeroU8::new(substeps).unwrap(),
        );
        frames.push(chain.particles.iter().map(|p| p.pos).collect());
    }
    frames
}

fn error(a: &[Vec<Vec2>], b: &[Vec<Vec2>]) -> (f64, f32) {
    let mut squared = 0.0;
    let mut max = 0.0_f32;
    let mut count = 0;
    for (a, b) in a.iter().zip(b) {
        let distance = a.last().unwrap().distance(*b.last().unwrap());
        squared += f64::from(distance).powi(2);
        max = max.max(distance);
        count += 1;
    }
    ((squared / f64::from(count)).sqrt(), max)
}

fn quality() {
    println!("quality,links,drive_px,substeps,physics_hz,tip_rms_vs_fine_px,tip_max_vs_fine_px");
    for links in [2, 8] {
        for amplitude in [10.0, 120.0] {
            let fine = trajectory(links, amplitude, 128);
            let finer = trajectory(links, amplitude, 255);
            let reference_error = error(&fine, &finer);
            println!(
                "reference,{links},{amplitude},{:.6},{:.6}",
                reference_error.0, reference_error.1
            );
            for substeps in [4, 8, 16] {
                let measured = trajectory(links, amplitude, substeps);
                let e = error(&measured, &fine);
                println!(
                    "quality,{links},{amplitude},{substeps},{},{:.6},{:.6}",
                    u32::from(substeps) * 60,
                    e.0,
                    e.1
                );
            }
        }
    }
}

fn trace(subdivisions: usize) {
    println!("links,scenario,frame,joint,x,y");
    for links in [2, 8] {
        for scenario in ["mild", "stress", "turn", "scale", "pose", "kick"] {
            let mut chain = ParticleChainData::new(
                (0..links)
                    .map(|i| ChainLink {
                        length: 160.0 / links as f32,
                        stiffness: 4.0,
                        damping: 0.3,
                        limit: LinkFeel::default().limit,
                        drawn: if scenario == "scale" {
                            Mat2::from_angle(0.07 * i as f32) * Vec2::Y
                        } else {
                            Vec2::Y
                        },
                        ..ChainLink::default()
                    })
                    .collect(),
            );
            chain.gravity = 9800.0;
            chain.settle_to_rest(Vec2::ZERO, Mat2::IDENTITY, &[]);
            let mut from = Vec2::ZERO;
            let mut from_carry = Mat2::IDENTITY;
            for frame in 1..=600 {
                let t = frame as f32 * DT;
                let amplitude = if scenario == "stress" { 120.0 } else { 10.0 };
                let to = Vec2::new(amplitude * (t * TAU).sin(), 0.0);
                let carry = match scenario {
                    "turn" => Mat2::from_angle(0.3 * (t * TAU).sin()),
                    "scale" => Mat2::from_cols(
                        Vec2::new(1.0 + 0.3 * (t * TAU).sin(), 0.1 * (t * TAU).sin()),
                        Vec2::new(0.2 * (t * TAU).sin(), 1.0 - 0.2 * (t * TAU).sin()),
                    ),
                    _ => Mat2::IDENTITY,
                };
                let mut posed = vec![0.0; links];
                if scenario == "pose" {
                    posed[0] = 0.05 * (t * TAU * 0.5).sin();
                }
                if scenario == "kick" && frame == 180 {
                    for p in chain.particles.iter_mut().skip(1) {
                        p.pos.x += 2.0;
                        p.vel = Vec2::ZERO;
                    }
                }
                for k in 1..=subdivisions {
                    let f = k as f32 / subdivisions as f32;
                    chain.tick(
                        from.lerp(to, f),
                        Mat2::from_cols(
                            from_carry.x_axis.lerp(carry.x_axis, f),
                            from_carry.y_axis.lerp(carry.y_axis, f),
                        ),
                        &posed,
                        DT / subdivisions as f32,
                        NonZeroU8::MIN,
                    );
                }
                for (joint, p) in chain.particles.iter().enumerate().skip(1) {
                    println!(
                        "{links},{scenario},{frame},{joint},{:.9},{:.9}",
                        p.pos.x, p.pos.y
                    );
                }
                from = to;
                from_carry = carry;
            }
        }
    }
}

fn trace_puppet() {
    println!("links,drive_px,frame,param,value");
    for links in [2, 8] {
        for amplitude in [10.0, 120.0] {
            let rig = rig(20, links, true, amplitude);
            let mut puppet = Puppet::new(&rig.model);
            puppet.settle_physics(&rig.model);
            let ids: Vec<_> = puppet
                .param_ids()
                .filter(|id| **id != rig.turn)
                .cloned()
                .collect();
            for frame in 1..=600 {
                puppet.set_param_value(&rig.turn, (frame as f32 * DT * TAU).sin());
                puppet.tick(&rig.model, DT);
                for (i, id) in ids.iter().enumerate() {
                    println!(
                        "{links},{amplitude},{frame},{i},{:.9}",
                        puppet.param_value(id).unwrap()
                    );
                }
            }
        }
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "trace" {
        trace(
            std::env::args()
                .nth(2)
                .map(|n| n.parse().unwrap())
                .unwrap_or(4),
        );
        return;
    }
    if mode == "trace-puppet" {
        trace_puppet();
        return;
    }
    if mode == "frequency" {
        let repeat = std::env::args()
            .nth(2)
            .map(|r| r.parse().expect("repeat is a number"));
        timing(true, repeat);
        return;
    }
    if mode != "timing" {
        quality();
    }
    if mode != "quality" {
        timing(false, None);
    }
}
