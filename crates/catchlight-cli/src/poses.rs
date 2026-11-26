//! `poses`: what every key pose of a rig does to it, as one deterministic CBOR
//! value. CPU only; motion responses are disabled so their outputs sweep like
//! any other param. `ciborium` writes fields in declaration order, so struct
//! field order here is the byte layout.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use catchlight_core::{
    BindingParams, Model, NodeId, NodeIdx, NodeKind, ParamId, Pose, Puppet, Vec2, Vec3,
};
use serde::{Deserialize, Serialize};

use crate::Error;

/// Model units. Each map is sparse by its own threshold.
const VERT_EPS: f32 = 1e-3;
const OPACITY_EPS: f32 = 1e-5;
const Z_EPS: f32 = 1e-4;
const MAX_SAMPLES: u64 = 100_000;
const MAX_SAMPLE_WORK: u64 = 250_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEntry {
    pub id: NodeId,
    pub name: String,
}

/// Full for `rest`, sparse against `rest` for every other pose. Keys index `parts`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cap {
    /// Little-endian f32 `x0 y0 x1 y1 …` in world space, mesh vertex order.
    pub verts: BTreeMap<u32, serde_bytes::ByteBuf>,
    pub opacity: BTreeMap<u32, f32>,
    pub z_order: BTreeMap<u32, f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyPose {
    pub value: f32,
    pub cap: Cap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamPoses {
    pub id: ParamId,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Normalized; a sampling schedule, not shared grid ownership: each binding
    /// still evaluates on its own axes. Unbound params use `[0, 1]`.
    pub key_positions: Vec<f32>,
    pub poses: Vec<KeyPose>,
}

/// `value` is `[a, b]`, in the pair's order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairPose {
    pub value: [f32; 2],
    pub cap: Cap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairPoses {
    pub a: ParamId,
    pub b: ParamId,
    /// `b` outer, `a` inner.
    pub poses: Vec<PairPose>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Poses {
    pub parts: Vec<NodeEntry>,
    pub rest: Cap,
    pub params: Vec<ParamPoses>,
    pub pairs: Vec<PairPoses>,
}

pub struct Written {
    pub out: PathBuf,
    pub bytes: usize,
    pub parts: usize,
    pub params: usize,
    pub pairs: usize,
}

impl std::fmt::Display for Written {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wrote {} ({} bytes): {} parts, {} params, {} param pairs",
            self.out.display(),
            self.bytes,
            self.parts,
            self.params,
            self.pairs
        )
    }
}

pub fn run(path: &Path, out: &Path) -> Result<Written, Error> {
    let model = crate::file::load_model(path)?;
    let poses = build(&model)?;
    let mut bytes = Vec::new();
    ciborium::into_writer(&poses, &mut bytes).map_err(|source| Error::Cbor {
        path: out.to_path_buf(),
        source,
    })?;
    crate::file::write(out, &bytes)?;
    Ok(Written {
        out: out.to_path_buf(),
        bytes: bytes.len(),
        parts: poses.parts.len(),
        params: poses.params.len(),
        pairs: poses.pairs.len(),
    })
}

pub fn build(model: &Model) -> Result<Poses, Error> {
    let positions = sample_positions(model);
    let pairs = pairs_of(model);
    check_work(model, &positions, &pairs)?;
    let mut puppet = Puppet::new(model);
    puppet.set_motion_responses_enabled(false);

    let parts = parts_table(&puppet);
    let part_ids: Vec<NodeIdx> = parts.iter().map(|(idx, _)| *idx).collect();

    let rest_full = pose_at(&mut puppet, model, &Pose::new(), &part_ids);
    let rest = rest_full.whole();

    let params = model
        .param_ids()
        .iter()
        .filter_map(|id| {
            let param = model.param(id)?;
            let key_positions = positions.get(id)?;
            let poses = key_positions
                .iter()
                .map(|position| {
                    let value = value_at(param.min, param.max, *position);
                    let mut pose = Pose::new();
                    pose.set(id.clone(), value);
                    let full = pose_at(&mut puppet, model, &pose, &part_ids);
                    KeyPose {
                        value,
                        cap: full.diff(&rest_full),
                    }
                })
                .collect();
            Some(ParamPoses {
                id: id.clone(),
                name: param.name.to_string(),
                min: param.min,
                max: param.max,
                default: param.default,
                key_positions: key_positions.clone(),
                poses,
            })
        })
        .collect();

    let pairs = pairs
        .into_iter()
        .filter_map(|(a, b)| {
            let (pa, pb) = (model.param(&a)?, model.param(&b)?);
            let a_positions = positions.get(&a)?;
            let b_positions = positions.get(&b)?;
            let mut poses = Vec::with_capacity(a_positions.len() * b_positions.len());
            // `b` outer, `a` inner: a row of this grid is a sweep of `a`.
            for bp in b_positions {
                for ap in a_positions {
                    let (av, bv) = (value_at(pa.min, pa.max, *ap), value_at(pb.min, pb.max, *bp));
                    let mut pose = Pose::new();
                    pose.set(a.clone(), av);
                    pose.set(b.clone(), bv);
                    let full = pose_at(&mut puppet, model, &pose, &part_ids);
                    poses.push(PairPose {
                        value: [av, bv],
                        cap: full.diff(&rest_full),
                    });
                }
            }
            Some(PairPoses { a, b, poses })
        })
        .collect();

    Ok(Poses {
        parts: parts.into_iter().map(|(_, entry)| entry).collect(),
        rest,
        params,
        pairs,
    })
}

fn check_work(
    model: &Model,
    positions: &BTreeMap<&ParamId, Vec<f32>>,
    pairs: &[(ParamId, ParamId)],
) -> Result<(), Error> {
    let mut samples = positions
        .values()
        .fold(1_u64, |total, axis| total.saturating_add(axis.len() as u64));
    for (a, b) in pairs {
        let a = positions.get(a).map_or(0, |axis| axis.len() as u64);
        let b = positions.get(b).map_or(0, |axis| axis.len() as u64);
        samples = samples.saturating_add(a.saturating_mul(b));
    }
    let mut per_sample = (model.node_count() as u64)
        .saturating_add(model.param_ids().len() as u64)
        .saturating_add(model.bindings().count() as u64);
    for id in model.nodes_in_order() {
        if let Some(mesh) = model.node_mesh(&id) {
            per_sample = per_sample.saturating_add(mesh.vertex_count() as u64);
        }
    }
    for (budget, requested, limit) in [
        ("samples", samples, MAX_SAMPLES),
        (
            "sample_work",
            samples.saturating_mul(per_sample),
            MAX_SAMPLE_WORK,
        ),
    ] {
        if requested > limit {
            return Err(Error::PosesLimit {
                budget,
                limit,
                requested,
            });
        }
    }
    Ok(())
}

fn value_at(min: f32, max: f32, position: f32) -> f32 {
    min + position * (max - min)
}

fn parts_table(puppet: &Puppet) -> Vec<(NodeIdx, NodeEntry)> {
    let mut parts = Vec::new();
    for (idx, node) in puppet.iter() {
        if !matches!(node.kind, NodeKind::Part(_)) {
            continue;
        }
        let Some(id) = puppet.node_id(idx) else {
            continue;
        };
        parts.push((
            idx,
            NodeEntry {
                id: id.clone(),
                name: node.name.clone(),
            },
        ));
    }
    parts
}

struct Full {
    verts: Vec<Vec<f32>>,
    opacity: Vec<f32>,
    z_order: Vec<f32>,
}

impl Full {
    fn whole(&self) -> Cap {
        Cap {
            verts: enumerate(self.verts.iter().map(|v| pack(v))),
            opacity: enumerate(self.opacity.iter().copied()),
            z_order: enumerate(self.z_order.iter().copied()),
        }
    }

    fn diff(&self, rest: &Full) -> Cap {
        let moved = |(a, b): (&Vec<f32>, &Vec<f32>)| {
            a.len() != b.len() || a.iter().zip(b).any(|(x, y)| (x - y).abs() > VERT_EPS)
        };
        Cap {
            verts: sparse(
                self.verts.iter().zip(&rest.verts).map(moved),
                self.verts.iter().map(|v| pack(v)),
            ),
            opacity: sparse(
                changed(&self.opacity, &rest.opacity, |a, b| {
                    (a - b).abs() > OPACITY_EPS
                }),
                self.opacity.iter().copied(),
            ),
            z_order: sparse(
                changed(&self.z_order, &rest.z_order, |a, b| (a - b).abs() > Z_EPS),
                self.z_order.iter().copied(),
            ),
        }
    }
}

fn changed<'a, T: Copy + 'a>(
    now: &'a [T],
    rest: &'a [T],
    differs: impl Fn(T, T) -> bool + 'a,
) -> impl Iterator<Item = bool> + 'a {
    now.iter().zip(rest).map(move |(a, b)| differs(*a, *b))
}

fn enumerate<T>(values: impl Iterator<Item = T>) -> BTreeMap<u32, T> {
    values
        .enumerate()
        .map(|(i, value)| (i as u32, value))
        .collect()
}

fn sparse<T>(
    keep: impl Iterator<Item = bool>,
    values: impl Iterator<Item = T>,
) -> BTreeMap<u32, T> {
    keep.zip(values)
        .enumerate()
        .filter(|(_, (keep, _))| *keep)
        .map(|(i, (_, value))| (i as u32, value))
        .collect()
}

fn pack(values: &[f32]) -> serde_bytes::ByteBuf {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    serde_bytes::ByteBuf::from(bytes)
}

fn pose_at(puppet: &mut Puppet, model: &Model, pose: &Pose, parts: &[NodeIdx]) -> Full {
    puppet.apply_pose(pose);
    puppet.tick(model, 0.0);
    Full {
        verts: parts.iter().map(|idx| world_verts(puppet, *idx)).collect(),
        opacity: parts
            .iter()
            .map(|idx| puppet.opacity(*idx).unwrap_or(1.0))
            .collect(),
        z_order: parts.iter().map(|idx| puppet.accumulated_z(*idx)).collect(),
    }
}

/// `transform × (vertex − origin + deform)`, matching the renderer.
fn world_verts(puppet: &Puppet, idx: NodeIdx) -> Vec<f32> {
    let Some(node) = puppet.get(idx) else {
        return Vec::new();
    };
    let NodeKind::Part(part) = &node.kind else {
        return Vec::new();
    };
    let transform = puppet.transforms().get(idx);
    let origin = part.mesh.origin;
    let deform = puppet.combined_deform(idx).unwrap_or(&[]);
    let mut out = Vec::with_capacity(part.mesh.vertices.len() * 2);
    for (i, vertex) in part.mesh.vertices.iter().enumerate() {
        let offset = deform.get(i).copied().unwrap_or(Vec2::ZERO);
        let local = *vertex - origin + offset;
        let world = transform.project_point3(Vec3::new(local.x, local.y, 0.0));
        out.push(world.x);
        out.push(world.y);
    }
    out
}

/// `a < b` by Id and sorted, independent of binding authoring order.
fn pairs_of(model: &Model) -> Vec<(ParamId, ParamId)> {
    let pairs: BTreeSet<(ParamId, ParamId)> = model
        .bindings()
        .filter_map(|binding| match binding.params() {
            BindingParams::Two(x, y) if x < y => Some((x.clone(), y.clone())),
            BindingParams::Two(x, y) => Some((y.clone(), x.clone())),
            BindingParams::One(_) => None,
        })
        .collect();
    pairs.into_iter().collect()
}

fn sample_positions(model: &Model) -> BTreeMap<&ParamId, Vec<f32>> {
    let mut positions: BTreeMap<_, Vec<f32>> = model
        .param_ids()
        .iter()
        .map(|id| (id, Vec::new()))
        .collect();
    for binding in model.bindings() {
        for (id, axis) in binding.params().iter().zip(binding.key_positions()) {
            if let Some(union) = positions.get_mut(id) {
                union.extend_from_slice(axis);
            }
        }
    }
    for union in positions.values_mut() {
        if union.is_empty() {
            union.extend([0.0, 1.0]);
        } else {
            union.sort_by(f32::total_cmp);
            union.dedup();
        }
    }
    positions
}
