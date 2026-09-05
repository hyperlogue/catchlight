//! `poses`: what every key pose of a rig does to it, as one CBOR value.
//!
//! This is the ground truth a rig evaluator scores a generated rig against.
//! It is CPU only — no GPU, no textures decoded — and it is deterministic:
//! two runs over the same file produce the same bytes.
//!
//! # What a pose is here
//!
//! Physics is switched off ([`Puppet::set_physics_enabled`]) and never
//! ticked, so a param a pendulum drives sweeps like any other. Each pose is
//! one [`Pose`] applied whole and one `tick(model, 0.0)`; `apply_pose` resets
//! every param to its default first, so a sweep of one param holds every
//! other at rest by construction.
//!
//! The rest pose is captured in full. Every other pose is a **diff against
//! rest**, and each of the four maps is sparse by its own threshold: a
//! vertex moved by more than 1e-3, an opacity by 1e-5, a z by 1e-4, an anchor
//! by 1e-3. A part absent from a map did not move that field.
//!
//! # The schema
//!
//! This crate's own, never a protocol type — the wire has no business
//! deciding what a baseline looks like. Written with `ciborium`, which emits
//! struct fields in declaration order, so the layout below is the byte
//! layout.
//!
//! ```text
//! { parts:   [{ id, name }…],   # every Part, tree order
//!   physics: [{ id, name }…],   # every SimplePhysics node, tree order
//!   rest:    Cap,               # in full
//!   params:  [{ id, name, min, max, default, key_positions, poses }…],
//!   pairs:   [{ a, b, poses }…] }
//! Cap = { verts: {idx: bytes}, opacity: {idx: f32},
//!         z_order: {idx: f32}, anchors: {idx: [x, y]} }
//! ```
//!
//! An `idx` in `verts`, `opacity` and `z_order` indexes `parts`; one in
//! `anchors` indexes `physics`. Ids are what a reader resolves against; the
//! indices only keep a hundred-part, sixty-param dump in the low megabytes.
//!
//! - **`verts`** are little-endian f32 `x0 y0 x1 y1 …` in mesh vertex order,
//!   each vertex at `transform × (vertex − origin + deform)`. That is the
//!   position the renderer draws it at: the GPU is handed `vertex − origin`,
//!   adds the combined deform in the shader and applies the same transform.
//! - **`z_order`** is accumulated over the tree, [`Puppet::accumulated_z`],
//!   which is the rule the collector sorts by.
//! - **`opacity`** is the part's own. It is not inherited; see
//!   [`Puppet::opacity`].
//! - **`anchors`** is the translation column of a physics node's world
//!   transform, **not negated** — unlike the node's internal `anchor`, which
//!   is stored in the driver's Y-down frame and so carries a flipped Y.
//!
//! # Params and pairs
//!
//! A param's `poses` run over its key positions, one per key, and a key at
//! position `pos` is posed at `min + pos × (max − min)`.
//!
//! `pairs` holds one entry per unordered param pair that some two-param
//! binding spans, deduplicated across bindings and ordered with `a < b` by Id
//! so the output does not depend on binding order. Its poses run over the
//! product of both key position lists, `b` outer and `a` inner. A per-param
//! sweep holds every other param at its default, so for a param in a pair it
//! samples exactly one row of that grid; `pairs` is the rest of the grid,
//! which is the part no single-param sweep can reach.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use catchlight_core::{
    BindingParams, Model, NodeId, NodeIdx, NodeKind, ParamId, Pose, Puppet, Vec2, Vec3,
};
use serde::{Deserialize, Serialize};

use crate::Error;

/// A vertex has moved when any component moves more than this, in model
/// units. The four thresholds are independent: a pose that only fades a part
/// carries it in `opacity` and in no other map.
const VERT_EPS: f32 = 1e-3;
const OPACITY_EPS: f32 = 1e-5;
const Z_EPS: f32 = 1e-4;
const ANCHOR_EPS: f32 = 1e-3;

/// One row of the `parts` or `physics` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEntry {
    pub id: NodeId,
    pub name: String,
}

/// Everything a pose can change, keyed by table index. Full for `rest`,
/// sparse for every other pose.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cap {
    /// Little-endian f32 `x0 y0 x1 y1 …`, keyed by index into `parts`.
    pub verts: BTreeMap<u32, serde_bytes::ByteBuf>,
    pub opacity: BTreeMap<u32, f32>,
    pub z_order: BTreeMap<u32, f32>,
    /// Keyed by index into `physics`, not `parts`.
    pub anchors: BTreeMap<u32, [f32; 2]>,
}

/// One key of one param.
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
    /// Normalised 0..1, as the model stores them.
    pub key_positions: Vec<f32>,
    pub poses: Vec<KeyPose>,
}

/// One cell of a two-param grid. `value` is `[a, b]`, in the pair's order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairPose {
    pub value: [f32; 2],
    pub cap: Cap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairPoses {
    pub a: ParamId,
    pub b: ParamId,
    /// The product of both key position lists, `b` outer and `a` inner.
    pub poses: Vec<PairPose>,
}

/// The whole dump. Field order here is the CBOR field order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Poses {
    pub parts: Vec<NodeEntry>,
    pub physics: Vec<NodeEntry>,
    pub rest: Cap,
    pub params: Vec<ParamPoses>,
    pub pairs: Vec<PairPoses>,
}

/// What a run wrote.
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

/// Dump `path`'s key poses to `out` as CBOR.
pub fn run(path: &Path, out: &Path) -> Result<Written, Error> {
    let model = crate::file::load_model(path)?;
    let poses = build(&model);
    let mut bytes = Vec::new();
    ciborium::into_writer(&poses, &mut bytes).map_err(|source| Error::Cbor {
        path: out.to_path_buf(),
        source,
    })?;
    std::fs::write(out, &bytes).map_err(|source| Error::io(out, source))?;
    Ok(Written {
        out: out.to_path_buf(),
        bytes: bytes.len(),
        parts: poses.parts.len(),
        params: poses.params.len(),
        pairs: poses.pairs.len(),
    })
}

/// Every key pose of `model`, without touching the filesystem — what the
/// tests drive.
pub fn build(model: &Model) -> Poses {
    let mut puppet = Puppet::new(model);
    // Never ticked, so a param a pendulum drives sweeps like any other.
    puppet.set_physics_enabled(false);

    let (parts, physics) = tables(&puppet);
    let part_ids: Vec<NodeIdx> = parts.iter().map(|(idx, _)| *idx).collect();
    let physics_ids: Vec<NodeIdx> = physics.iter().map(|(idx, _)| *idx).collect();

    let rest_full = pose_at(&mut puppet, model, &Pose::new(), &part_ids, &physics_ids);
    let rest = rest_full.whole();

    let params = model
        .param_ids()
        .iter()
        .filter_map(|id| {
            let param = model.param(id)?;
            let poses = param
                .key_positions
                .iter()
                .map(|position| {
                    let value = value_at(param.min, param.max, *position);
                    let mut pose = Pose::new();
                    pose.set(id.clone(), value);
                    let full = pose_at(&mut puppet, model, &pose, &part_ids, &physics_ids);
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
                key_positions: param.key_positions.clone(),
                poses,
            })
        })
        .collect();

    let pairs = pairs_of(model)
        .into_iter()
        .filter_map(|(a, b)| {
            let (pa, pb) = (model.param(&a)?, model.param(&b)?);
            let mut poses = Vec::with_capacity(pa.key_positions.len() * pb.key_positions.len());
            // `b` outer, `a` inner: a row of this grid is a sweep of `a`.
            for bp in &pb.key_positions {
                for ap in &pa.key_positions {
                    let (av, bv) = (value_at(pa.min, pa.max, *ap), value_at(pb.min, pb.max, *bp));
                    let mut pose = Pose::new();
                    pose.set(a.clone(), av);
                    pose.set(b.clone(), bv);
                    let full = pose_at(&mut puppet, model, &pose, &part_ids, &physics_ids);
                    poses.push(PairPose {
                        value: [av, bv],
                        cap: full.diff(&rest_full),
                    });
                }
            }
            Some(PairPoses { a, b, poses })
        })
        .collect();

    Poses {
        parts: parts.into_iter().map(|(_, entry)| entry).collect(),
        physics: physics.into_iter().map(|(_, entry)| entry).collect(),
        rest,
        params,
        pairs,
    }
}

/// The param-space value a key position maps to.
fn value_at(min: f32, max: f32, position: f32) -> f32 {
    min + position * (max - min)
}

/// The Parts and the physics nodes, both in tree order — a `NodeIdx` is a
/// node's position in the bake's pre-order walk, so slot order is tree order.
#[allow(clippy::type_complexity)]
fn tables(puppet: &Puppet) -> (Vec<(NodeIdx, NodeEntry)>, Vec<(NodeIdx, NodeEntry)>) {
    let mut parts = Vec::new();
    let mut physics = Vec::new();
    for (idx, node) in puppet.iter() {
        let table = match node.kind {
            NodeKind::Part(_) => &mut parts,
            NodeKind::SimplePhysics(_) => &mut physics,
            _ => continue,
        };
        let Some(id) = puppet.node_id(idx) else {
            continue;
        };
        table.push((
            idx,
            NodeEntry {
                id: id.clone(),
                name: node.name.clone(),
            },
        ));
    }
    (parts, physics)
}

/// Every field of every pose, dense. Diffing two of these is what makes a
/// [`Cap`] sparse.
struct Full {
    verts: Vec<Vec<f32>>,
    opacity: Vec<f32>,
    z_order: Vec<f32>,
    anchors: Vec<[f32; 2]>,
}

impl Full {
    /// The whole thing as a [`Cap`], which is what `rest` carries.
    fn whole(&self) -> Cap {
        Cap {
            verts: enumerate(self.verts.iter().map(|v| pack(v))),
            opacity: enumerate(self.opacity.iter().copied()),
            z_order: enumerate(self.z_order.iter().copied()),
            anchors: enumerate(self.anchors.iter().copied()),
        }
    }

    /// What this pose changed against `rest`, each map by its own threshold.
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
            anchors: sparse(
                changed(&self.anchors, &rest.anchors, |a, b| {
                    (a[0] - b[0]).abs() > ANCHOR_EPS || (a[1] - b[1]).abs() > ANCHOR_EPS
                }),
                self.anchors.iter().copied(),
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

/// Every value, keyed by its position.
fn enumerate<T>(values: impl Iterator<Item = T>) -> BTreeMap<u32, T> {
    values
        .enumerate()
        .map(|(i, value)| (i as u32, value))
        .collect()
}

/// The values whose flag is set, keyed by position.
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

/// Apply `pose` and evaluate one frame, then read everything off it.
fn pose_at(
    puppet: &mut Puppet,
    model: &Model,
    pose: &Pose,
    parts: &[NodeIdx],
    physics: &[NodeIdx],
) -> Full {
    puppet.apply_pose(pose);
    puppet.tick(model, 0.0);
    Full {
        verts: parts.iter().map(|idx| world_verts(puppet, *idx)).collect(),
        opacity: parts
            .iter()
            .map(|idx| puppet.opacity(*idx).unwrap_or(1.0))
            .collect(),
        z_order: parts.iter().map(|idx| puppet.accumulated_z(*idx)).collect(),
        anchors: physics
            .iter()
            .map(|idx| {
                let t = puppet.transforms().get(*idx).w_axis;
                [t.x, t.y]
            })
            .collect(),
    }
}

/// A part's vertices in world space: `transform × (vertex − origin + deform)`,
/// in mesh vertex order.
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

/// Every unordered param pair some two-param binding spans, `a < b` by Id and
/// sorted, so the list does not depend on the order bindings were authored.
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
