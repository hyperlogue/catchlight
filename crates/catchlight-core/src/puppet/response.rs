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
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ResponseClock {
    #[default]
    InputIntervals,
    /// A continuous 60 Hz timeline for playback and export. An interval over
    /// one minute is treated as a discontinuity to bound catch-up work.
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
    Unavailable,
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
    pose_slots: Vec<usize>,
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
                    ResponseSource::PoseParam { param: p } => puppet
                        .slot_of_param
                        .get(p)
                        .copied()
                        .map_or(Source::Unavailable, Source::Pose),
                    ResponseSource::Parent => {
                        if let Some(i) = node.parent().and_then(|p| indices.get(p)).copied() {
                            keep &= retained[i];
                            Source::Parent(i)
                        } else {
                            Source::Unavailable
                        }
                    }
                    ResponseSource::PartSlot {
                        part,
                        slot,
                        angle_deg,
                        reference_length,
                    } => {
                        let vertex = model.slot(part, slot).and_then(|s| s.vertex());
                        vertices.push(vertex);
                        if let Some(&part_node) = puppet.node_of_id.get(part) {
                            let point = *point_indices.entry((part, slot)).or_insert_with(|| {
                                let index = out.points.len();
                                out.points.push((part_node, vertex));
                                index
                            });
                            let (s, c) = f64::from(*angle_deg).to_radians().sin_cos();
                            let len = f64::from(*reference_length);
                            Source::Slot {
                                point,
                                direction: [c / len, s / len],
                            }
                        } else {
                            Source::Unavailable
                        }
                    }
                };
                keep &= !matches!(source, Source::Unavailable);
                terms.push(Term {
                    source,
                    gain: input.gain.into(),
                    previous: 0.,
                    current: 0.,
                    boundary: 0.,
                });
            }
            let output = config.output_param.as_ref().and_then(|p| {
                let &slot = puppet.slot_of_param.get(p)?;
                let param = puppet.params.get(slot as usize)?;
                out.reserved.resize(puppet.params.len(), false);
                out.reserved[slot as usize] = true;
                Some((slot, param.min, param.max))
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

    /// On a clip cut, initialize affected rows and their followers at the new
    /// pose. Independent springs keep both velocity and fixed-clock phase.
    pub(super) fn reset_changed_inputs(&mut self, jumps: &[u32]) {
        if jumps.is_empty() {
            return;
        }
        let slot_pose_changed = self
            .pose_slots
            .iter()
            .any(|&slot| jumps.contains(&(slot as u32)));
        for i in 0..self.rows.len() {
            let affected = self.rows[i].terms.iter().any(|t| match t.source {
                Source::Pose(slot) => jumps.contains(&slot),
                Source::Slot { .. } => slot_pose_changed,
                Source::Parent(parent) => !self.rows[parent].sampled,
                Source::Unavailable => false,
            });
            if affected {
                self.rows[i].sampled = false;
            }
        }
    }

    pub(super) fn owns(&self, slot: u32) -> bool {
        self.reserved.get(slot as usize) == Some(&true)
    }

    fn bake_sampler(&mut self, puppet: &Puppet) {
        // Isolate the pose-only geometry from the render arena's last driver
        // claims and from a caller-supplied camera/world matrix. The sampler
        // shares immutable binding grids and is only allocated for slot inputs.
        let mut pose = puppet.clone();
        pose.set_motion_responses_enabled(false);
        pose.responses.reserved.clone_from(&self.reserved);
        pose.stop_animation();
        // Welds solve in sequence. Trace their inputs backwards so an
        // earlier seam that moves a sampled endpoint is retained too.
        let mut parts: HashSet<_> = self.points.iter().map(|&(part, _)| part).collect();
        pose.arena.welds.reverse();
        pose.arena.welds.retain(|weld| {
            if parts.contains(&weld.a) || parts.contains(&weld.b) {
                parts.extend([weld.a, weld.b]);
                true
            } else {
                false
            }
        });
        pose.arena.welds.reverse();
        let needed = pose.geometry_ancestors(parts);
        self.pose_slots = pose
            .geometry_slots(&needed)
            .filter(|&slot| !self.owns(slot))
            .map(|slot| slot as usize)
            .collect();
        self.pose_slots.sort_unstable();
        self.pose_slots.dedup();
        pose.arena.retain_geometry(&needed, &self.points);
        pose.spine_targets = pose
            .spine_targets
            .into_iter()
            .zip(&pose.arena.spine_node_ids)
            .filter_map(|(row, node)| needed.contains(node).then_some(row))
            .collect();
        pose.arena.spine_node_ids.retain(|n| needed.contains(n));
        pose.bindings.retain(|b| needed.contains(&b.node));
        // A render puppet may have an explicit pose on a reserved output.
        for (slot, reserved) in self.reserved.iter().enumerate() {
            if *reserved {
                pose.param_values[slot] = None;
            }
        }
        pose.invalidate_fold();
        self.sampler = Some(Box::new(pose));
    }

    fn sample_points(&mut self, puppet: &Puppet, model: &Model) {
        if self.points.is_empty() {
            return;
        }
        if self.sampler.is_none() {
            self.bake_sampler(puppet);
        }
        let Some(sample) = &mut self.sampler else {
            return;
        };
        for &i in &self.pose_slots {
            let value = puppet.param_values[i];
            if sample.param_values[i] != value {
                sample.param_values[i] = value;
                sample.bump_param_generation(i as u32);
            }
        }
        if sample.scratch_transforms != puppet.scratch_transforms {
            sample
                .scratch_transforms
                .clone_from(&puppet.scratch_transforms);
            sample.invalidate_fold();
        }
        if sample.last_tick_folded_param_generation == Some(sample.param_generation) {
            return;
        }
        sample.tick_with_root(model, Mat4::IDENTITY, 0.);
        let geometry = crate::geometry::EvaluatedGeometry::new(model, sample).ok();
        for (dst, &(node, vertex)) in self.positions.iter_mut().zip(&self.points) {
            *dst = vertex.and_then(|v| {
                let mesh = geometry.as_ref()?.mesh(sample.node_id(node)?).ok()?;
                Some(mesh.vertex(v as usize)?.world.truncate())
            });
        }
    }

    fn step(&mut self, puppet: &mut Puppet, model: &Model, dt: f32, reset: bool) -> bool {
        self.sample_points(puppet, model);
        const STEP: f64 = 1.0 / 60.0;
        let dt = f64::from(dt);
        let fixed = self.clock == ResponseClock::Fixed60;
        // The fixed clock also drives offline clips. A slow frame can contain
        // many solver boundaries. Bound exceptional gaps without resetting
        // ordinary low-rate clips, including one-second frame intervals.
        let max_interval = if fixed { 60. } else { 0.25 };
        let reset = reset || !(0.0..=max_interval).contains(&dt);
        let old_remainder = self.remainder;
        for row in &mut self.rows {
            for term in &mut row.terms {
                term.previous = term.current;
                term.current = match term.source {
                    Source::Unavailable => f64::NAN,
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
    pub(super) fn reset_changed_response_inputs(&mut self, jumps: &[u32]) {
        if jumps.is_empty() || !self.motion_responses_enabled || self.responses.rows.is_empty() {
            return;
        }
        let mut bank = std::mem::take(&mut self.responses);
        // A rebake can retain a moving row while invalidating its slot sampler.
        // Resolve the new pose dependencies before deciding which rows to reset.
        if !bank.points.is_empty() && bank.sampler.is_none() {
            bank.bake_sampler(self);
        }
        bank.reset_changed_inputs(jumps);
        self.responses = bank;
    }

    pub fn set_response_clock(&mut self, clock: ResponseClock) {
        if self.responses.clock != clock {
            self.responses.clock = clock;
            self.responses.reset();
        }
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
                        Source::Unavailable => None,
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
        if !self.motion_responses_enabled || self.responses.rows.is_empty() {
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{ModelNode, ModelParam, Name, ResponseInput};
    #[test]
    fn unresolved_sources_keep_their_authored_input_indices() {
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let parent = NodeId::new("parent").unwrap();
        let child = NodeId::new("child").unwrap();
        for name in ["input", "output"] {
            model
                .add_param_with_id(
                    ParamId::new(name).unwrap(),
                    ModelParam::new(Name::truncated(name), -1., 1., 0.),
                )
                .unwrap();
        }
        model
            .add_node_with_id(
                parent.clone(),
                &root,
                ModelNode::new("parent", ModelNodeKind::MotionResponse(Default::default())),
            )
            .unwrap();
        let input = ParamId::new("input").unwrap();
        let output = ParamId::new("output").unwrap();
        let config = MotionResponse {
            inputs: vec![
                ResponseInput {
                    source: ResponseSource::Parent,
                    gain: 1.,
                },
                ResponseInput {
                    source: ResponseSource::PoseParam {
                        param: output.clone(),
                    },
                    gain: 2.,
                },
                ResponseInput {
                    source: ResponseSource::PoseParam {
                        param: input.clone(),
                    },
                    gain: 3.,
                },
            ],
            ..Default::default()
        };
        model
            .add_node_with_id(
                child.clone(),
                &parent,
                ModelNode::new("child", ModelNodeKind::MotionResponse(config)),
            )
            .unwrap();
        let mut puppet = Puppet::new(&model);
        // Exercise a partial runtime map directly. A normally loaded complete
        // model validates all these references before reaching the baker.
        let index = puppet.node_idx(&parent).unwrap();
        puppet.id_of_node[index.0 as usize] = NodeId::new("missing").unwrap();
        puppet.slot_of_param.remove(&output);
        puppet.responses = Responses::bake(&puppet, &model, Default::default());
        puppet.set_param_value(&input, 0.25);
        puppet.tick(&model, 1. / 60.);
        let samples = puppet.response_inputs(&child).unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[0].sample, None);
        assert_eq!(samples[1].sample, None);
        assert_eq!(samples[2].contribution, Some(0.75));
        assert!(!puppet.response_trace(&child).unwrap().active);
    }
}
