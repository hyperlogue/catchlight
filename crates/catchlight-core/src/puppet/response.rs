//! Responses read posed inputs and the unclipped position of their immediate
//! parent. The model tree supplies the step order. Sample external inputs once
//! per tick; on long intervals subdivide the entire cascade together, so a
//! child sees both parent endpoints on the same substep.
use std::collections::{HashMap, HashSet};

use super::Puppet;
use crate::{
    Mat4, Model, ModelNodeKind, MotionResponse, NodeId, NodeIdx, ParamId, ResponseSource,
    SpringState, Vec2,
};

/// One term as actually sampled by the most recent tick. Unfilled slots
/// yield no position or scalar; consumers can identify which slot to repair.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ResponseInputSample {
    pub source: ResponseSource,
    pub gain: f32,
    pub position: Option<[f32; 2]>,
    pub sample: Option<f64>,
    pub contribution: Option<f64>,
}

/// Time is supplied by the caller. Fixed stepping interpolates emitted art
/// by the accumulator fraction; dependency edges always read solver values.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum ResponseClock {
    #[default]
    InputIntervals,
    Fixed60,
}

/// Last completed sample, in the authored response's units.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct ResponseTrace {
    pub target: f64,
    pub followed: f64,
    pub velocity: f64,
    pub lag: f64,
    pub emitted: f32,
    pub active: bool,
}

#[derive(Clone)]
enum Source {
    Pose(u32),
    Slot { point: usize, direction: [f64; 2] },
    Parent(usize),
}

#[derive(Clone)]
struct Term {
    source: Source,
    gain: f64,
    previous: f64,
    current: f64,
    boundary: f64,
}

#[derive(Clone)]
struct Row {
    id: NodeId,
    node: NodeIdx,
    parent: Option<NodeId>,
    config: MotionResponse,
    terms: Vec<Term>,
    vertices: Vec<Option<u32>>,
    omega: f64,
    enabled: bool,
    output: Option<(u32, f32, f32)>,
    state: SpringState,
    before: f64,
    previous_lag: f64,
    trace: ResponseTrace,
    sampled: bool,
    moving: bool,
}

#[derive(Clone, Default)]
pub(super) struct Responses {
    rows: Vec<Row>,
    clock: ResponseClock,
    remainder: f64,
    points: Vec<(NodeIdx, Option<u32>)>,
    positions: Vec<Option<Vec2>>,
    reserved: Vec<bool>,
    pub(super) sampler: Option<Box<Puppet>>,
}

impl Responses {
    pub(super) fn bake(puppet: &Puppet, model: &Model, previous: Self) -> Self {
        let mut out = Self {
            clock: previous.clock,
            remainder: previous.remainder,
            ..Self::default()
        };
        let mut indices = HashMap::new();
        let mut point_indices = HashMap::new();
        let mut retained = Vec::new();
        let mut old: HashMap<_, _> = previous
            .rows
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        for (slot, id) in puppet.id_of_node.iter().enumerate() {
            let Some(node) = model.node(id) else { continue };
            let ModelNodeKind::MotionResponse(config) = &node.kind else {
                continue;
            };
            let mut terms = Vec::new();
            let mut vertices = Vec::new();
            let mut keep = true;
            for input in &config.inputs {
                let source = match &input.source {
                    ResponseSource::PoseParam(p) => Source::Pose(puppet.slot_of_param[p]),
                    ResponseSource::Parent => {
                        let Some(i) = node.parent().and_then(|p| indices.get(p)).copied() else {
                            continue;
                        };
                        keep &= retained[i];
                        Source::Parent(i)
                    }
                    ResponseSource::PartSlot {
                        part,
                        slot,
                        angle_deg,
                        reference_length,
                    } => {
                        let vertex = model.slot(part, slot).and_then(|s| s.vertex());
                        vertices.push(vertex);
                        let point = *point_indices.entry((part, slot)).or_insert_with(|| {
                            let index = out.points.len();
                            out.points.push((puppet.node_of_id[part], vertex));
                            index
                        });
                        let (s, c) = f64::from(*angle_deg).to_radians().sin_cos();
                        let len = f64::from(*reference_length);
                        Source::Slot {
                            point,
                            direction: [c / len, s / len],
                        }
                    }
                };
                terms.push(Term {
                    source,
                    gain: input.gain.into(),
                    previous: 0.,
                    current: 0.,
                    boundary: 0.,
                });
            }
            let output = config.output_param.as_ref().map(|p| {
                let slot = puppet.slot_of_param[p];
                let param = &puppet.params[slot as usize];
                out.reserved.resize(puppet.params.len(), false);
                out.reserved[slot as usize] = true;
                (slot, param.min, param.max)
            });
            let mut row = Row {
                id: id.clone(),
                node: NodeIdx(slot as u32),
                parent: node.parent().cloned(),
                config: config.clone(),
                omega: std::f64::consts::TAU / f64::from(config.period_s),
                enabled: node.enabled,
                output,
                terms,
                vertices,
                state: SpringState::default(),
                before: 0.,
                previous_lag: 0.,
                trace: ResponseTrace::default(),
                sampled: false,
                moving: false,
            };
            let saved = old.remove(id).filter(|r| {
                keep && r.config.inputs == config.inputs
                    && r.config.period_s == config.period_s
                    && r.config.damping_ratio == config.damping_ratio
                    && r.parent == row.parent
                    && r.vertices == row.vertices
                    && r.enabled == row.enabled
            });
            retained.push(saved.is_some());
            if let Some(r) = saved {
                row.state = r.state;
                row.trace = r.trace;
                row.previous_lag = r.previous_lag;
                row.sampled = r.sampled;
                row.moving = r.moving;
                for (term, old) in row.terms.iter_mut().zip(r.terms) {
                    term.current = old.current;
                    term.boundary = old.boundary;
                }
            }
            indices.insert(id.clone(), out.rows.len());
            out.rows.push(row);
        }
        out.positions.resize(out.points.len(), None);
        out
    }

    pub(super) fn reset(&mut self) {
        self.remainder = 0.;
        for row in &mut self.rows {
            row.sampled = false;
            row.moving = false;
            row.trace = ResponseTrace::default();
        }
    }

    pub(super) fn owns(&self, slot: u32) -> bool {
        self.reserved.get(slot as usize) == Some(&true)
    }

    fn sample_points(&mut self, puppet: &Puppet, model: &Model) {
        if self.points.is_empty() {
            return;
        }
        // Isolate the pose-only geometry from the render arena's last driver
        // claims and from a caller-supplied camera/world matrix. The sampler
        // shares immutable binding grids and is only allocated for slot inputs.
        let sample = self.sampler.get_or_insert_with(|| {
            let mut pose = puppet.clone();
            pose.set_physics_enabled(false);
            pose.stop_animation();
            // A slot sees transforms and geometry on its ancestor path. A
            // spine also reads its guide part. Welds can reach across trees;
            // keep the full pose in that case so the normal weld pass agrees.
            if model.welds().is_empty() {
                let mut needed = HashSet::new();
                for &(node, _) in &self.points {
                    let mut next = Some(node);
                    while let Some(i) = next {
                        needed.insert(i);
                        if matches!(
                            pose.get(i).map(|n| &n.kind),
                            Some(crate::NodeKind::Spine(_))
                        ) {
                            needed.extend(pose.tree().get_all_descendants(i));
                        }
                        next = pose.tree().get_parent(i);
                    }
                }
                // Spines consume their guide meshes before group propagation;
                // only the sampled part vertices are observed after that pass.
                // Intermediate mesh groups still need every triangle vertex.
                let mut selected: HashMap<NodeIdx, Vec<usize>> = HashMap::new();
                for &(part, vertex) in &self.points {
                    if let Some(v) = vertex {
                        selected.entry(part).or_default().push(v as usize);
                    }
                }
                let groups: HashSet<_> = pose.arena.mesh_group_node_ids.iter().copied().collect();
                for node in &mut pose.arena.nodes {
                    if let crate::NodeKind::MeshGroup(mg) = &mut node.kind {
                        mg.pins.per_child.retain(|child, pins| {
                            if let Some(vertices) = selected.get(child) {
                                pins.sampled_vertices = Some(vertices.clone().into_boxed_slice());
                                true
                            } else {
                                needed.contains(child) && groups.contains(child)
                            }
                        });
                    }
                }
                pose.bindings.retain(|b| needed.contains(&b.node));
                pose.rebuild_param_effect_cache();
            }
            pose.last_tick_folded_param_generation = None;
            Box::new(pose)
        });
        for (i, value) in puppet.param_values.iter().enumerate() {
            let value = if self.reserved.get(i) == Some(&true) {
                None
            } else {
                *value
            };
            if sample.param_values[i] != value {
                sample.param_values[i] = value;
                sample.bump_param_generation_for_slot(i as u32);
            }
        }
        if sample.scratch_transforms != puppet.scratch_transforms {
            sample
                .scratch_transforms
                .clone_from(&puppet.scratch_transforms);
            sample.last_tick_folded_param_generation = None;
        }
        sample.tick_with_root(model, Mat4::IDENTITY, 0.);
        for (dst, &(node, vertex)) in self.positions.iter_mut().zip(&self.points) {
            *dst = vertex.and_then(|v| {
                let crate::NodeKind::Part(part) = &sample.get(node)?.kind else {
                    return None;
                };
                let i = v as usize;
                let local = *part.mesh.vertices.get(i)? - part.mesh.origin
                    + *part.deform_stack.combined().get(i)?;
                Some(
                    sample
                        .transforms()
                        .get(node)
                        .transform_point3(local.extend(0.))
                        .truncate(),
                )
            });
        }
    }

    fn step(&mut self, puppet: &mut Puppet, model: &Model, dt: f32, reset: bool) -> bool {
        self.sample_points(puppet, model);
        const STEP: f64 = 1.0 / 60.0;
        let dt = f64::from(dt);
        let fixed = self.clock == ResponseClock::Fixed60;
        let reset = reset
            || !dt.is_finite()
            || !(0.0..=0.25).contains(&dt)
            || (fixed && ((self.remainder + dt) / STEP).floor() > 4.);
        let old_remainder = self.remainder;
        for row in &mut self.rows {
            for term in &mut row.terms {
                term.previous = term.current;
                term.current = match term.source {
                    Source::Pose(slot) => f64::from(puppet.param_base(slot).unwrap_or_default()),
                    Source::Slot {
                        point,
                        direction: [x, y],
                    } => self.positions[point]
                        .map_or(f64::NAN, |p| x * f64::from(p.x) + y * f64::from(p.y)),
                    Source::Parent(_) => 0.,
                };
                if reset || !row.sampled {
                    term.previous = term.current;
                    term.boundary = term.current;
                }
            }
        }
        let (steps, h) = if reset {
            self.remainder = 0.;
            (1, 0.)
        } else if fixed {
            let elapsed = old_remainder + dt;
            let n = (elapsed / STEP).floor() as u32;
            self.remainder = elapsed - f64::from(n) * STEP;
            (n, STEP)
        } else {
            let n = ((dt as f32) * 120.).ceil().clamp(1., 30.) as u32;
            (n, dt / f64::from(n))
        };
        // With no completed fixed interval, initialize newly introduced rows
        // at the current pose while leaving running springs at their boundary.
        for step in 0..steps.max(1) {
            let t1 = if steps == 0 || reset || dt == 0. {
                1.
            } else if fixed {
                ((f64::from(step + 1) * h - old_remainder) / dt).clamp(0., 1.)
            } else {
                f64::from(step + 1) / f64::from(steps)
            };
            for i in 0..self.rows.len() {
                let (parents, suffix) = self.rows.split_at_mut(i);
                let row = &mut suffix[0];
                let mut start = 0.;
                let mut end = 0.;
                let mut valid = row.enabled;
                if steps == 0 && row.sampled {
                    continue;
                }
                for term in &mut row.terms {
                    let (a, b) = match term.source {
                        Source::Parent(j) => {
                            valid &= parents[j].trace.active;
                            (parents[j].before, parents[j].state.x)
                        }
                        _ => {
                            let d = term.current - term.previous;
                            let start = term.boundary;
                            term.boundary = term.previous + t1 * d;
                            (start, term.boundary)
                        }
                    };
                    start += term.gain * a;
                    end += term.gain * b;
                }
                valid &= start.is_finite() && end.is_finite();
                if !valid {
                    row.sampled = false;
                    row.moving = false;
                    row.trace = ResponseTrace::default();
                    continue;
                }
                let initialize = reset || !row.sampled;
                row.previous_lag = if initialize { 0. } else { row.trace.lag };
                row.before = if initialize { end } else { row.state.x };
                if initialize {
                    row.state = SpringState {
                        x: end,
                        velocity: 0.,
                    };
                } else if h > 0. {
                    row.state.advance(
                        start,
                        end,
                        h,
                        row.omega,
                        f64::from(row.config.damping_ratio),
                    );
                }
                row.sampled = true;
                row.moving = (row.state.x - end).abs() > 1e-7 || row.state.velocity.abs() > 1e-7;
                if !row.moving && start == end {
                    row.state = SpringState {
                        x: end,
                        velocity: 0.,
                    };
                }
                let lag = row.state.x - end;
                row.trace = ResponseTrace {
                    target: end,
                    followed: row.state.x,
                    velocity: row.state.velocity,
                    lag,
                    active: true,
                    emitted: row
                        .output
                        .map_or(0., |(_, min, max)| (lag as f32).clamp(min, max)),
                };
            }
        }
        let display = if fixed { self.remainder / STEP } else { 1. };
        for row in &mut self.rows {
            if let Some((slot, min, max)) = row.output {
                let lag = if row.trace.active {
                    row.previous_lag + (row.trace.lag - row.previous_lag) * display
                } else {
                    0.
                };
                row.trace.emitted = (lag as f32).clamp(min, max);
                puppet.contribute(slot, row.node, row.trace.emitted, 1.);
            }
        }
        self.rows.iter().any(|r| {
            r.moving
                || (fixed
                    && r.trace.active
                    && (r.previous_lag != r.trace.lag
                        || r.terms.iter().any(|t| t.current != t.boundary)))
        })
    }
}

impl Puppet {
    pub fn set_response_clock(&mut self, clock: ResponseClock) {
        if self.responses.clock != clock {
            self.responses.clock = clock;
            self.responses.reset();
        }
    }

    pub fn has_motion_responses(&self) -> bool {
        !self.responses.rows.is_empty()
    }
    pub fn response_inputs(&self, id: &NodeId) -> Option<Vec<ResponseInputSample>> {
        let bank = &self.responses;
        let row = bank.rows.iter().find(|row| &row.id == id)?;
        Some(
            row.terms
                .iter()
                .zip(&row.config.inputs)
                .map(|(term, input)| {
                    let position = match term.source {
                        Source::Slot { point, .. } => bank.positions[point].map(|p| p.to_array()),
                        _ => None,
                    };
                    let sample = match term.source {
                        Source::Parent(j) => {
                            bank.rows[j].trace.active.then_some(bank.rows[j].state.x)
                        }
                        _ => term.boundary.is_finite().then_some(term.boundary),
                    };
                    ResponseInputSample {
                        source: input.source.clone(),
                        gain: input.gain,
                        position,
                        sample,
                        contribution: sample.map(|x| x * term.gain),
                    }
                })
                .collect(),
        )
    }

    pub fn response_trace(&self, id: &NodeId) -> Option<ResponseTrace> {
        self.responses
            .rows
            .iter()
            .find(|r| &r.id == id)
            .map(|r| r.trace)
    }
    pub(super) fn advance_responses(&mut self, model: &Model, dt: f32, reset: bool) -> bool {
        if !self.physics_enabled || self.responses.rows.is_empty() {
            return false;
        }
        let mut bank = std::mem::take(&mut self.responses);
        let moving = bank.step(self, model, dt, reset);
        self.responses = bank;
        moving
    }
    /// Reset after a seek or teleport, initializing every child from its
    /// parent's new equilibrium in the same tree walk.
    pub fn reset_motion_responses(&mut self, model: &Model) {
        self.sync(model);
        self.advance_responses(model, 0., true);
    }
    pub(super) fn reserved_output(&self, id: &ParamId) -> bool {
        self.slot_of_param
            .get(id)
            .is_some_and(|&slot| self.responses.owns(slot))
    }
}
