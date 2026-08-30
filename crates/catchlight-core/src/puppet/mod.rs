//! A puppet: a model being animated.
//!
//! A [`Puppet`] holds everything animating a [`Model`] produces and a model
//! never does — the pose, the drivers' state and the evaluated frame — so one
//! model can back many puppets and posing one never touches the model.
//!
//! Invariants this module enforces:
//!
//! - **A tick is one pipeline, in one order.** [`Puppet::tick`] is: fold
//!   animations → pose the physics anchors and step the drivers → fold the
//!   bindings → compute transforms → apply the `translate_children` mesh-group
//!   filter and recompute → propagate mesh-group deforms → solve welds →
//!   combine deforms. The code is an optimized form of exactly that, cached in
//!   three places, and the caching is where a bug hides: the anchor pre-pass
//!   is skipped unless the pose moved (`last_anchor_pose_generation`), the
//!   whole fold is skipped when neither the pose nor the pre-pass touched
//!   anything (`last_tick_folded_param_generation`), and the third transform
//!   walk runs only when the filter actually shifted something. A pre-pass
//!   that ran **forces** the final fold, because it reset colour and
//!   deactivated every deform stack.
//! - **The generation gate is the only staleness check.** A puppet records
//!   `model.generation()` when it bakes; every method that takes a `&Model`
//!   compares it first and rebakes when it moved. Nothing else may assume the
//!   arena still matches the model — which is also what makes installing an
//!   addon between two frames cost one rebake and no caller changes.
//! - **A rebake carries the pose and the drivers, by Id.** Param values,
//!   driver contributions and every `SimplePhysics` runtime field are saved
//!   against `ParamId` / `NodeId`, the arena is rebuilt, and they are put back
//!   where those Ids now live. Anything keyed by slot — every generation memo
//!   — is dropped, because the slots moved. A param or node the edit removed
//!   is dropped with it; a value for a param the edit has not added yet is
//!   kept aside and lands if it appears.
//! - **Hot paths address nodes and params by index, never by Id.** Ids are
//!   `Arc<str>` and comparing them is a string compare; a tick touches every
//!   node and every binding, so both are resolved to a `NodeIdx` / param slot
//!   once at bake and the loops index dense `Vec`s. Ids reappear only at the
//!   API edge, through `node_idx` / `node_id` and the pose methods.
//! - **`settle_physics` before the first render.** It iterates to the fixed
//!   point of "anchor → param value → transforms → anchor" so a freshly loaded
//!   model renders settled instead of swinging into place, and it leaves the
//!   puppet *unposed* — `tick` is what folds a renderable pose.
//!
//! Two things a puppet does not own. **Textures**: nothing here decodes an
//! image, so a part carries its albedo as an index into
//! [`Model::texture_ids`] and the render cache resolves it. **Masks**: a baked
//! [`crate::components::Mask`] carries the source node's `NodeIdx` in its
//! `source_uuid`, because the field is the legacy uuid namespace and a model
//! has none; [`Puppet::node_for_uuid`] is the identity for that reason.

mod arena;
mod bake;
mod fold;

pub use arena::GlobalTransforms;

pub(crate) use arena::Arena;

use std::collections::{HashMap, HashSet};

use glam::{Mat4, Vec2, Vec3};

use crate::components::{checked_affine_inverse, Node, NodeIdx, NodeKind};
use crate::deform::DeformSource;
use crate::id::{NodeId, ParamId};
use crate::model::{BindingTarget, Model, Pose, ScalarTarget};
use crate::node::NodeTree;
use crate::params::{bracket, frac};
use crate::physics::SimplePhysicsData;

use bake::{Baked, BakedBinding, BakedParam};

/// Squared anchor movement below which `settle_physics` calls a driver
/// converged. Anchors are in model units, so this is a sub-millipixel
/// displacement on models whose coordinates run to the thousands.
const SETTLE_EPS_SQ: f32 = 1e-6;

/// One driver's weighted claim on a param. `weight` is authority, at or above
/// zero: 0 asserts nothing, 1 fully specifies the value, and larger weights
/// only matter relative to whatever else claims the param.
#[derive(Debug, Clone, Copy)]
struct Contribution {
    slot: u32,
    source: NodeIdx,
    value: f32,
    weight: f32,
}

/// Fold weighted claims against the value the caller posed.
///
/// The rule is an **order-independent** weighted mean, deliberately not a
/// sequential source-over: there is no principled ordering between "the caller
/// posed this before the tick" and "a driver wrote it during the tick", so
/// inventing one would bury a semantic choice in call order. A caller that
/// wants ordered compositing resolves it on its own side and poses the result.
fn resolve_contributions(entries: &[Contribution], slot: u32, base: f32) -> f32 {
    let mut total = 0.0;
    let mut sum = 0.0;
    for e in entries.iter().filter(|e| e.slot == slot) {
        total += e.weight;
        sum += e.value * e.weight;
    }
    if total <= 0.0 {
        base
    } else if total < 1.0 {
        sum + base * (1.0 - total)
    } else {
        sum / total
    }
}

/// Where a pose puts one param on its own key positions.
#[derive(Debug, Clone, Copy)]
struct Located {
    /// The resolved param value, before clamping — the deform fold's memo key.
    value: f32,
    lo: usize,
    hi: usize,
    frac: f32,
}

impl Located {
    const REST: Self = Self {
        value: 0.0,
        lo: 0,
        hi: 0,
        frac: 0.0,
    };
}

/// A model being animated: its pose, its drivers' state and the frame its last
/// tick produced.
pub struct Puppet {
    /// The `Model::generation` this puppet is baked against.
    baked_generation: u64,
    arena: Arena,
    transforms: GlobalTransforms,

    node_of_id: HashMap<NodeId, NodeIdx>,
    /// Indexed by `NodeIdx.0`, so the arena's DFS order is the Id order.
    id_of_node: Vec<NodeId>,

    params: Vec<BakedParam>,
    slot_of_param: HashMap<ParamId, u32>,
    /// Posed value per param slot; `None` reads the param's default, which is
    /// the state `clear_param_value` leaves.
    param_values: Vec<Option<f32>>,
    /// Values posed for params this model does not (yet) have. Kept off the
    /// dense path so it never costs the hot loops, and promoted by the next
    /// rebake that gives the Id a slot.
    param_values_overflow: HashMap<ParamId, f32>,
    /// One entry per (param, source), upserted by the source rather than
    /// cleared each frame, so a driver's output survives into the next frame's
    /// anchor pre-pass. That persistence is what couples chained physics.
    param_contributions: Vec<Contribution>,
    /// Parallel to `params`: does any entry target that slot. Keeps the
    /// twice-a-frame fold to one bool load for the overwhelming majority of
    /// params, which have no contributor at all.
    param_contributed: Vec<bool>,
    param_generation: u64,
    last_tick_folded_param_generation: Option<u64>,

    bindings: Vec<BakedBinding>,
    /// Param slots whose bindings reach a mesh group or something under one.
    param_mesh_group_relevant: HashSet<u32>,
    mesh_group_param_generation: u64,
    last_tick_mesh_group_generation: Option<u64>,

    physics_enabled: bool,
    /// Parallel to `arena.physics_node_ids`.
    physics_targets: Vec<[Option<u32>; 2]>,
    physics_update_scratch: Vec<([Option<u32>; 2], NodeIdx, Vec2)>,
    /// `Some(G)` means the cached physics transforms and the node-level anchor
    /// inputs hold the anchor pose a fresh pre-pass at
    /// `param_generation == G` would produce.
    last_anchor_pose_generation: Option<u64>,

    /// Reused by the fold: where each param slot sits on its key positions.
    located: Vec<Located>,
}

impl Puppet {
    /// Bake `model` and record the generation it was baked at.
    pub fn new(model: &Model) -> Self {
        let mut puppet = Self {
            baked_generation: model.generation(),
            arena: Arena::new(),
            transforms: GlobalTransforms::new(),
            node_of_id: HashMap::new(),
            id_of_node: Vec::new(),
            params: Vec::new(),
            slot_of_param: HashMap::new(),
            param_values: Vec::new(),
            param_values_overflow: HashMap::new(),
            param_contributions: Vec::new(),
            param_contributed: Vec::new(),
            param_generation: 0,
            last_tick_folded_param_generation: None,
            bindings: Vec::new(),
            param_mesh_group_relevant: HashSet::new(),
            mesh_group_param_generation: 0,
            last_tick_mesh_group_generation: None,
            physics_enabled: true,
            physics_targets: Vec::new(),
            physics_update_scratch: Vec::new(),
            last_anchor_pose_generation: None,
            located: Vec::new(),
        };
        puppet.install(bake::bake(model));
        puppet
    }

    /// The `Model::generation` this puppet last baked against.
    pub fn baked_generation(&self) -> u64 {
        self.baked_generation
    }

    /// Rebake if `model` moved since the last bake, carrying the pose and the
    /// drivers across by Id.
    pub fn sync(&mut self, model: &Model) {
        if self.baked_generation == model.generation() && !self.id_of_node.is_empty() {
            return;
        }
        let pose: Vec<(ParamId, f32)> = self
            .param_values
            .iter()
            .enumerate()
            .filter_map(|(slot, v)| Some((self.params.get(slot)?.id.clone(), (*v)?)))
            .chain(
                self.param_values_overflow
                    .iter()
                    .map(|(id, v)| (id.clone(), *v)),
            )
            .collect();
        let contributions: Vec<(ParamId, NodeId, f32, f32)> = self
            .param_contributions
            .iter()
            .filter_map(|c| {
                Some((
                    self.params.get(c.slot as usize)?.id.clone(),
                    self.id_of_node.get(c.source.0 as usize)?.clone(),
                    c.value,
                    c.weight,
                ))
            })
            .collect();
        let drivers: Vec<(NodeId, SimplePhysicsData)> = self
            .arena
            .physics_node_ids
            .iter()
            .filter_map(|&idx| {
                let id = self.id_of_node.get(idx.0 as usize)?.clone();
                match &self.arena.get(idx)?.kind {
                    NodeKind::SimplePhysics(p) => Some((id, (**p).clone())),
                    _ => None,
                }
            })
            .collect();

        self.install(bake::bake(model));
        self.baked_generation = model.generation();

        for (id, value) in pose {
            self.set_param_value(&id, value);
        }
        for (param, source, value, weight) in contributions {
            let (Some(&slot), Some(&source)) =
                (self.slot_of_param.get(&param), self.node_of_id.get(&source))
            else {
                continue;
            };
            self.contribute(slot, source, value, weight);
        }
        for (id, saved) in drivers {
            let Some(&idx) = self.node_of_id.get(&id) else {
                continue;
            };
            if let Some(NodeKind::SimplePhysics(p)) = self.arena.get_mut(idx).map(|n| &mut n.kind) {
                // The authored half comes from the model; only the runtime
                // half survives an edit.
                p.offset_output_scale = saved.offset_output_scale;
                p.bob = saved.bob;
                p.spring_vel = saved.spring_vel;
                p.d_angle = saved.d_angle;
                p.anchor = saved.anchor;
                p.anchor_initialized = saved.anchor_initialized;
            }
        }
    }

    /// Replace everything derived from the model. Every slot-keyed memo is
    /// dropped: the slots themselves have moved.
    fn install(&mut self, baked: Baked) {
        let Baked {
            arena,
            node_of_id,
            id_of_node,
            params,
            slot_of_param,
            bindings,
            physics_targets,
        } = baked;
        self.arena = arena;
        self.node_of_id = node_of_id;
        self.id_of_node = id_of_node;
        self.param_values = vec![None; params.len()];
        self.param_contributed = vec![false; params.len()];
        self.located = vec![Located::REST; params.len()];
        self.params = params;
        self.slot_of_param = slot_of_param;
        self.bindings = bindings;
        self.physics_targets = physics_targets;
        self.param_values_overflow.clear();
        self.param_contributions.clear();
        self.transforms = GlobalTransforms::new();
        self.param_generation = self.param_generation.wrapping_add(1);
        self.last_tick_folded_param_generation = None;
        self.last_tick_mesh_group_generation = None;
        self.last_anchor_pose_generation = None;
        self.rebuild_param_effect_cache();
    }

    /// Param slots whose bindings can move a mesh group or something under
    /// one, so a pose change that touches none of them can skip the
    /// mesh-group passes.
    fn rebuild_param_effect_cache(&mut self) {
        self.param_mesh_group_relevant.clear();
        if self.arena.mesh_group_node_ids.is_empty() {
            return;
        }
        let mut related = HashSet::new();
        for &mg_id in &self.arena.mesh_group_node_ids {
            related.insert(mg_id);
            related.extend(self.arena.tree.get_all_descendants(mg_id));
        }
        for b in &self.bindings {
            let moves_geometry = matches!(
                b.target,
                BindingTarget::Deform
                    | BindingTarget::Scalar(
                        ScalarTarget::Tx
                            | ScalarTarget::Ty
                            | ScalarTarget::Sx
                            | ScalarTarget::Sy
                            | ScalarTarget::Rx
                            | ScalarTarget::Ry
                            | ScalarTarget::Rz
                    )
            );
            if moves_geometry && related.contains(&b.node) {
                self.param_mesh_group_relevant.insert(b.x);
                if let Some(y) = b.y {
                    self.param_mesh_group_relevant.insert(y);
                }
            }
        }
    }

    // ---- the arena, at the edge ------------------------------------------

    pub fn root(&self) -> NodeIdx {
        self.arena.root()
    }

    pub fn tree(&self) -> &NodeTree {
        &self.arena.tree
    }

    pub fn len(&self) -> usize {
        self.arena.len()
    }

    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    pub fn get(&self, id: NodeIdx) -> Option<&Node> {
        self.arena.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.arena.iter()
    }

    pub fn iter_deform_nodes(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.arena.iter_deform_nodes()
    }

    /// The arena slot a model node was baked into.
    pub fn node_idx(&self, id: &NodeId) -> Option<NodeIdx> {
        self.node_of_id.get(id).copied()
    }

    /// The model node an arena slot came from.
    pub fn node_id(&self, idx: NodeIdx) -> Option<&NodeId> {
        self.id_of_node.get(idx.0 as usize)
    }

    /// A baked mask names its source by arena slot, so this is the identity.
    /// It exists so a consumer written against the legacy runtime's uuid
    /// namespace resolves a mask the same way here.
    pub fn node_for_uuid(&self, uuid: u32) -> Option<NodeIdx> {
        let idx = NodeIdx::new(uuid);
        self.arena.get(idx).map(|_| idx)
    }

    /// The transforms the last tick produced.
    pub fn transforms(&self) -> &GlobalTransforms {
        &self.transforms
    }

    /// One node's combined per-vertex deform after the last tick.
    pub fn combined_deform(&self, id: NodeIdx) -> Option<&[Vec2]> {
        match &self.arena.get(id)?.kind {
            NodeKind::Part(p) => Some(p.deform_stack.combined()),
            NodeKind::MeshGroup(mg) => Some(mg.deform_stack.combined()),
            _ => None,
        }
    }

    /// Write the puppet's scratch deform — an edit in progress, shown live and
    /// never part of the model. Cleared, like every other source, at the start
    /// of the next tick's fold, so a live drag rewrites it per frame.
    pub fn set_scratch_deform(&mut self, id: NodeIdx, offsets: &[Vec2]) -> bool {
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        let stack = match &mut node.kind {
            NodeKind::Part(p) => &mut p.deform_stack,
            NodeKind::MeshGroup(mg) => &mut mg.deform_stack,
            _ => return false,
        };
        let buf = stack.source_buf_mut(DeformSource::Scratch);
        let n = buf.len().min(offsets.len());
        buf[..n].copy_from_slice(&offsets[..n]);
        for v in buf[n..].iter_mut() {
            *v = Vec2::ZERO;
        }
        true
    }

    pub fn clear_scratch_deform(&mut self, id: NodeIdx) -> bool {
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        match &mut node.kind {
            NodeKind::Part(p) => p.deform_stack.clear_source(DeformSource::Scratch),
            NodeKind::MeshGroup(mg) => mg.deform_stack.clear_source(DeformSource::Scratch),
            _ => return false,
        }
        true
    }

    // ---- the pose ---------------------------------------------------------

    /// The params this puppet was baked with, in the model's order.
    pub fn param_ids(&self) -> impl Iterator<Item = &ParamId> {
        self.params.iter().map(|p| &p.id)
    }

    /// The param's effective value: what was posed, folded with any driver
    /// claims. `None` only when nothing posed it and nothing claims it, so the
    /// caller falls back to the param's own default exactly as the fold does.
    pub fn param_value(&self, param: &ParamId) -> Option<f32> {
        let slot = self.slot_of_param.get(param).copied();
        let base = match slot {
            Some(slot) => self.param_values.get(slot as usize).copied().flatten(),
            None => self.param_values_overflow.get(param).copied(),
        };
        let slot = match slot {
            Some(slot) => slot,
            None => return base,
        };
        if !self.param_contributions.iter().any(|e| e.slot == slot) {
            return base;
        }
        let default = self
            .params
            .get(slot as usize)
            .map(|p| p.default)
            .unwrap_or(0.0);
        Some(resolve_contributions(
            &self.param_contributions,
            slot,
            base.unwrap_or(default),
        ))
    }

    /// Pose one param. Equivalent to a full-authority claim wherever nothing
    /// else writes it, which is every param on a model with no driver aimed at
    /// it; where a driver *does* claim it, this is what the driver's weight
    /// blends against.
    pub fn set_param_value(&mut self, param: &ParamId, value: f32) {
        match self.slot_of_param.get(param).copied() {
            Some(slot) => {
                if self.param_values.get(slot as usize).copied().flatten() == Some(value) {
                    return;
                }
                if let Some(v) = self.param_values.get_mut(slot as usize) {
                    *v = Some(value);
                }
                self.bump_param_generation_for_slot(slot);
            }
            None => {
                if self.param_values_overflow.get(param).copied() == Some(value) {
                    return;
                }
                self.param_values_overflow.insert(param.clone(), value);
                self.param_generation = self.param_generation.wrapping_add(1);
            }
        }
    }

    /// Drop a posed value, restoring the param to its default.
    pub fn clear_param_value(&mut self, param: &ParamId) {
        match self.slot_of_param.get(param).copied() {
            Some(slot) => {
                if self
                    .param_values
                    .get_mut(slot as usize)
                    .and_then(|v| v.take())
                    .is_some()
                {
                    self.bump_param_generation_for_slot(slot);
                }
            }
            None => {
                if self.param_values_overflow.remove(param).is_some() {
                    self.param_generation = self.param_generation.wrapping_add(1);
                }
            }
        }
    }

    /// Restore every param to its default, then apply `pose` as an overlay.
    /// A caller that poses only the params it controls needs the reset:
    /// without it a previous overlay's value would stick.
    pub fn apply_pose(&mut self, pose: &Pose) {
        for slot in 0..self.params.len() {
            let (id, default) = {
                let p = &self.params[slot];
                (p.id.clone(), p.default)
            };
            let value = pose.get(&id).unwrap_or(default);
            self.set_param_value(&id, value);
        }
        for (id, value) in pose.iter() {
            if !self.slot_of_param.contains_key(id) {
                self.set_param_value(id, value);
            }
        }
    }

    /// The pose the puppet currently holds, driver claims folded in.
    pub fn pose(&self) -> Pose {
        self.params
            .iter()
            .map(|p| (p.id.clone(), self.param_value(&p.id).unwrap_or(p.default)))
            .collect()
    }

    /// Record `source`'s weighted claim on `param`, replacing that source's
    /// previous one. Returns whether the resolved value moved.
    pub fn contribute_param_value(
        &mut self,
        param: &ParamId,
        source: NodeIdx,
        value: f32,
        weight: f32,
    ) -> bool {
        let Some(slot) = self.slot_of_param.get(param).copied() else {
            return false;
        };
        self.contribute(slot, source, value, weight)
    }

    fn contribute(&mut self, slot: u32, source: NodeIdx, value: f32, weight: f32) -> bool {
        let before = self.resolved(slot);
        match self
            .param_contributions
            .iter_mut()
            .find(|e| e.slot == slot && e.source == source)
        {
            Some(e) => {
                e.value = value;
                e.weight = weight;
            }
            None => {
                self.param_contributions.push(Contribution {
                    slot,
                    source,
                    value,
                    weight,
                });
                if let Some(flag) = self.param_contributed.get_mut(slot as usize) {
                    *flag = true;
                }
            }
        }
        if before == self.resolved(slot) {
            return false;
        }
        self.bump_param_generation_for_slot(slot);
        true
    }

    /// Drop every driver claim, restoring each param to what was posed.
    pub fn clear_param_contributions(&mut self) {
        if self.param_contributions.is_empty() {
            return;
        }
        for e in std::mem::take(&mut self.param_contributions) {
            self.bump_param_generation_for_slot(e.slot);
        }
        for flag in self.param_contributed.iter_mut() {
            *flag = false;
        }
    }

    /// The value the fold uses for a slot: what was posed (or the default),
    /// with driver claims folded in.
    fn resolved(&self, slot: u32) -> f32 {
        let Some(p) = self.params.get(slot as usize) else {
            return 0.0;
        };
        let base = self
            .param_values
            .get(slot as usize)
            .copied()
            .flatten()
            .unwrap_or(p.default);
        if self
            .param_contributed
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
        {
            resolve_contributions(&self.param_contributions, slot, base)
        } else {
            base
        }
    }

    fn bump_param_generation_for_slot(&mut self, slot: u32) {
        self.param_generation = self.param_generation.wrapping_add(1);
        if self.param_mesh_group_relevant.contains(&slot) {
            self.mesh_group_param_generation = self.mesh_group_param_generation.wrapping_add(1);
        }
    }

    // ---- drivers ----------------------------------------------------------

    pub fn has_simple_physics(&self) -> bool {
        !self.arena.physics_node_ids.is_empty()
    }

    /// When false, a tick skips physics entirely: drivers never overwrite
    /// their target params, so the same pose always yields the same frame.
    /// The editor freezes physics this way — its dt=0 preview cannot
    /// integrate, and chained drivers would otherwise leave
    /// pose-history-dependent residue in the authoring view.
    pub fn set_physics_enabled(&mut self, enabled: bool) {
        self.physics_enabled = enabled;
        if !enabled {
            // Otherwise each driver's last output would keep overriding the
            // pose for as long as physics stays frozen.
            self.clear_param_contributions();
        }
    }

    pub fn physics_enabled(&self) -> bool {
        self.physics_enabled
    }

    /// Advance every driver by `dt` seconds against `transforms`, then write
    /// each one's state into its target params.
    fn tick_physics(&mut self, transforms: &GlobalTransforms, dt: f32) -> bool {
        let _span = tracing::trace_span!("tick_physics").entered();
        for i in 0..self.arena.physics_node_ids.len() {
            let id = self.arena.physics_node_ids[i];
            let Some(anchor) = self.arena.physics_anchor(transforms, id) else {
                continue;
            };
            if let Some(NodeKind::SimplePhysics(p)) = self.arena.get_mut(id).map(|n| &mut n.kind) {
                p.tick(anchor, dt);
            }
        }
        self.write_physics_param_outputs(transforms)
    }

    /// Map every driver's state through its map mode and claim its target
    /// params with the result. Returns whether any resolved value moved.
    ///
    /// Drivers claim at full authority: a lone driver fully determines its
    /// target, and two drivers aimed at one param average rather than
    /// resolving by their position in the arena, which is document order and
    /// carries no meaning here.
    fn write_physics_param_outputs(&mut self, transforms: &GlobalTransforms) -> bool {
        self.physics_update_scratch.clear();
        for i in 0..self.arena.physics_node_ids.len() {
            let id = self.arena.physics_node_ids[i];
            let targets = self.physics_targets.get(i).copied().unwrap_or([None, None]);
            if targets == [None, None] {
                continue;
            }
            let Some(NodeKind::SimplePhysics(p)) = self.arena.get(id).map(|n| &n.kind) else {
                continue;
            };
            // local_only: the bob was integrated in the parent's frame already
            // (the anchor is local), so no inverse is needed. Otherwise invert
            // the node's model-local world matrix to rotate the displacement
            // back into the node's own frame.
            let world_inverse = if p.local_only {
                Some(Mat4::IDENTITY)
            } else {
                checked_affine_inverse(transforms.get(id))
            };
            let Some(world_inverse) = world_inverse else {
                continue;
            };
            // Conjugate by the Y-flip so the Y-down integrator's displacement
            // rotates back through the Y-up node frame — the matching half of
            // the flip `Arena::physics_anchor` applies going in.
            let flip = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
            let value = p.param_value(flip * world_inverse * flip);
            self.physics_update_scratch.push((targets, id, value));
        }
        let mut changed = self.retire_stale_driver_contributions();
        for i in 0..self.physics_update_scratch.len() {
            let (targets, source, value) = self.physics_update_scratch[i];
            // A driver writes two params, in the order its map mode produces
            // them: the pendulum's angle then its length, or whichever pair
            // the mode names.
            for (axis, slot) in targets.into_iter().enumerate() {
                let Some(slot) = slot else { continue };
                let v = if axis == 0 { value.x } else { value.y };
                if self.contribute(slot, source, v, 1.0) {
                    changed = true;
                }
            }
        }
        changed
    }

    /// Drop claims from drivers that no longer produce one — a driver whose
    /// target was retargeted or cleared by an edit.
    ///
    /// A stale entry keeps full authority rather than merely lingering:
    /// resolution is a mean, so a frozen claim alongside a live one pulls the
    /// param to the midpoint of the two instead of tracking the driver that is
    /// still running.
    fn retire_stale_driver_contributions(&mut self) -> bool {
        let mut entries = std::mem::take(&mut self.param_contributions);
        let before = entries.len();
        let mut retired: smallvec::SmallVec<[u32; 4]> = smallvec::SmallVec::new();
        entries.retain(|e| {
            let from_driver = self.arena.physics_node_ids.contains(&e.source);
            let live = self
                .physics_update_scratch
                .iter()
                .any(|(targets, source, _)| {
                    *source == e.source && targets.iter().flatten().any(|&s| s == e.slot)
                });
            if from_driver && !live {
                retired.push(e.slot);
                return false;
            }
            true
        });
        self.param_contributions = entries;
        if before == self.param_contributions.len() {
            return false;
        }
        for slot in retired {
            if let Some(flag) = self.param_contributed.get_mut(slot as usize) {
                *flag = self.param_contributions.iter().any(|e| e.slot == slot);
            }
            self.bump_param_generation_for_slot(slot);
        }
        true
    }

    /// Bring every driver to its analytic rest pose without simulating, so a
    /// freshly loaded or re-posed model renders settled on its first frame
    /// instead of visibly swinging into place.
    ///
    /// One driver's output can position another's anchor, so a single pass is
    /// not enough: each pass settles every driver against the current anchor
    /// pose, then re-folds the anchor bindings so those outputs propagate.
    /// Each driver's rest state is a fixed point of "anchor → param value →
    /// transforms → anchor", so an acyclic driver graph converges in at most
    /// one pass per driver; the extra pass is what observes the fixed point.
    ///
    /// Leaves the puppet in the anchor pose, with deform stacks cleared and
    /// colour at base — `tick` is what folds a renderable frame, and the
    /// resets here force it to. Render only after ticking.
    pub fn settle_physics(&mut self, model: &Model) {
        let _span = tracing::trace_span!("settle_physics").entered();
        self.sync(model);
        let n = self.arena.physics_node_ids.len();
        // Settling writes driver outputs, and a frozen puppet never ticks
        // physics again to refresh or retire them — they would sit on the
        // pose forever, which is the override `set_physics_enabled` removes.
        if n == 0 || !self.physics_enabled {
            return;
        }
        self.arena.ensure_physics_ancestor_mask();
        let mut transforms = std::mem::take(&mut self.arena.physics_transforms);
        let mut settled = false;

        for _ in 0..=n {
            self.reset_frame();
            self.apply_anchor_transform_bindings();
            self.arena
                .compute_physics_ancestor_transforms(&mut transforms);

            let mut moved = false;
            for i in 0..n {
                let id = self.arena.physics_node_ids[i];
                let Some(anchor) = self.arena.physics_anchor(&transforms, id) else {
                    continue;
                };
                if let Some(NodeKind::SimplePhysics(p)) =
                    self.arena.get_mut(id).map(|n| &mut n.kind)
                {
                    if !p.anchor_initialized || (p.anchor - anchor).length_squared() > SETTLE_EPS_SQ
                    {
                        moved = true;
                    }
                    p.settle_to_rest(anchor);
                }
            }
            self.write_physics_param_outputs(&transforms);

            if !moved {
                settled = true;
                break;
            }
        }
        self.arena.physics_transforms = transforms;

        if !settled {
            tracing::warn!(
                "physics failed to settle in {} passes; drivers likely form a \
                 dependency cycle. Leaving the last iterate in place.",
                n + 1
            );
        }
    }

    // ---- the tick ---------------------------------------------------------

    /// Evaluate the next frame: drivers step, the pose is folded through the
    /// bindings, and transforms and deforms are resolved. Rebakes first when
    /// `model` has moved since the last one.
    pub fn tick(&mut self, model: &Model, dt: f32) {
        self.tick_with_root(model, Mat4::IDENTITY, dt);
    }

    /// [`Self::tick`] with `root` folded into the top-level transform, so the
    /// puppet evaluates at an arbitrary world placement. Drivers still sample
    /// the model-local pose, independent of `root`.
    pub fn tick_with_root(&mut self, model: &Model, root: Mat4, dt: f32) {
        let _span = tracing::trace_span!("tick").entered();
        self.sync(model);
        let mut out = std::mem::take(&mut self.transforms);

        let has_physics = self.physics_enabled && self.has_simple_physics();
        let mut pre_pass_ran = false;
        let mut anchor_generation = 0;
        if has_physics {
            // Rebuild the anchor pose only when the pose or a driver output
            // moved since the cached one, or when a local_only driver is a
            // tc-filter target (see `Arena::physics_anchor_skip_allowed`).
            let stale = self.last_anchor_pose_generation != Some(self.param_generation)
                || !self.arena.physics_anchor_skip_allowed();
            if stale {
                // Capture the generation BEFORE the drivers bump it: if one
                // moves this frame, next frame's staleness check then forces
                // the rebuild that feeds chained physics.
                anchor_generation = self.param_generation;
                pre_pass_ran = true;
                self.reset_frame();
                self.apply_anchor_transform_bindings();
                self.arena.ensure_physics_ancestor_mask();
                let mut local = std::mem::take(&mut self.arena.physics_transforms);
                self.arena.compute_physics_ancestor_transforms(&mut local);
                self.arena.physics_transforms = local;
            }
            let local = std::mem::take(&mut self.arena.physics_transforms);
            self.tick_physics(&local, dt);
            self.arena.physics_transforms = local;
        }

        let params_changed = self.last_tick_folded_param_generation != Some(self.param_generation);
        let mesh_group_generation_changed =
            self.last_tick_mesh_group_generation != Some(self.mesh_group_param_generation);
        // A pre-pass reset colour and deactivated every deform stack, so the
        // frame that ran it must run the final fold too — otherwise it would
        // render the unposed puppet.
        let needs_final_apply = params_changed || pre_pass_ran;
        let has_mesh_group_work = !self.arena.mesh_group_node_ids.is_empty()
            && !self.param_mesh_group_relevant.is_empty()
            && (needs_final_apply || mesh_group_generation_changed);
        if needs_final_apply {
            self.reset_frame();
            self.apply_params();
            // Transforms BEFORE the propagation, so a mesh group and its
            // children sit where this frame's pose put them.
            self.arena.compute_transforms_with_root(&mut out, root);
            if has_mesh_group_work {
                // Re-walk only when the filter actually shifted something; a
                // model with no translate_children mesh group never needs it.
                if self.arena.apply_translate_children_filter(&out) {
                    self.arena.compute_transforms_with_root(&mut out, root);
                }
                self.arena.propagate_mesh_group_deforms(&out);
                self.last_tick_mesh_group_generation = Some(self.mesh_group_param_generation);
            }
            self.arena.apply_welds(&out);
            self.arena.combine_deforms();
            self.last_tick_folded_param_generation = Some(self.param_generation);
        } else {
            self.arena.compute_transforms_with_root(&mut out, root);
        }
        if pre_pass_ran {
            // Set at the very end: the final fold's reset cleared this, and
            // using the pre-tick generation is what lets a moved driver force
            // next frame's anchor rebuild.
            self.last_anchor_pose_generation = Some(anchor_generation);
        }
        self.transforms = out;
    }

    /// Restore the evaluated frame to the model's authored values and clear
    /// every deform stack — the start of a fold.
    fn reset_frame(&mut self) {
        self.last_tick_folded_param_generation = None;
        self.last_anchor_pose_generation = None;
        self.arena.reset_dynamic_state();
        self.arena.reset_deforms();
    }

    /// Fold every binding at the current pose.
    pub fn apply_params(&mut self) {
        let _span = tracing::trace_span!("apply_params").entered();
        self.apply_params_where(|_| true);
    }

    /// Physics pre-pass: build the anchor pose. Stepping a driver reads only
    /// node transforms (the anchors) and each driver's output scale, so
    /// restrict the fold to the targets feeding those. Deform and colour
    /// contributions would be wiped by the reset before the final fold anyway.
    ///
    /// Driver-output params are included at their *last-frame* values, so a
    /// driver whose output transform-binds another driver's anchor moves it —
    /// which is what makes chained physics work. The two-phase pipeline
    /// applies every driver's last-frame output uniformly, so chained drivers
    /// couple with a one-frame delay.
    fn apply_anchor_transform_bindings(&mut self) {
        let _span = tracing::trace_span!("apply_anchor_transform_bindings").entered();
        self.apply_params_where(|t| {
            matches!(
                t,
                BindingTarget::Scalar(
                    ScalarTarget::Tx
                        | ScalarTarget::Ty
                        | ScalarTarget::Sx
                        | ScalarTarget::Sy
                        | ScalarTarget::Rx
                        | ScalarTarget::Ry
                        | ScalarTarget::Rz
                        | ScalarTarget::OutputScaleX
                        | ScalarTarget::OutputScaleY
                )
            )
        });
    }

    fn apply_params_where(&mut self, include: impl Fn(BindingTarget) -> bool) {
        // Locate every param once, not once per binding that names it.
        for slot in 0..self.params.len() {
            self.located[slot] = self.locate(slot as u32);
        }
        // Move the bindings out so the fold can take `&mut Arena`; they are
        // put back before returning, and nothing here can early-return.
        let bindings = std::mem::take(&mut self.bindings);
        for b in &bindings {
            if !include(b.target) {
                continue;
            }
            self.fold_binding(b);
        }
        self.bindings = bindings;
    }

    /// Where the current pose puts one param on its key positions.
    fn locate(&self, slot: u32) -> Located {
        let Some(p) = self.params.get(slot as usize) else {
            return Located::REST;
        };
        let value = self.resolved(slot);
        if p.key_positions.is_empty() {
            return Located {
                value,
                ..Located::REST
            };
        }
        let span = p.max - p.min;
        let normed = if span.abs() > 1e-9 {
            ((value.clamp(p.min, p.max) - p.min) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (lo, hi) = bracket(&p.key_positions, normed);
        Located {
            value,
            lo,
            hi,
            frac: frac(normed, p.key_positions[lo], p.key_positions[hi]),
        }
    }
}
