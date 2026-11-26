//! A puppet: a model being animated.
//!
//! Hot paths address nodes and params by `NodeIdx` / param slot resolved at
//! bake, never by `Arc<str>` Id; Ids appear only at the API edge.

mod arena;
mod bake;
mod fold;
mod response;
pub use response::{ResponseClock, ResponseInputSample, ResponseTrace};

pub use arena::GlobalTransforms;

pub(crate) use arena::Arena;
pub(crate) use bake::BakeMemo;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec3};

use crate::components::{Node, NodeIdx, NodeKind};
use crate::deform::DeformSource;
use crate::id::{NodeId, ParamId};
use crate::model::{Model, Pose};
use crate::node::NodeTree;

use bake::{Baked, BakedBinding, BakedParam};

/// `weight` is authority, at or above zero: 0 asserts nothing, 1 fully
/// specifies the value, larger weights only matter relative to other claims.
#[derive(Debug, Clone, Copy)]
struct Contribution {
    slot: u32,
    source: NodeIdx,
    value: f32,
    weight: f32,
}

/// An order-independent weighted mean: there is no principled ordering
/// between a caller's pose and a driver's write. `claims` is kept in claim
/// order so the float sum is reproducible.
fn resolve_contributions(entries: &[Contribution], claims: &[u32], base: f32) -> f32 {
    let mut total = 0.0;
    let mut sum = 0.0;
    for &i in claims {
        let Some(e) = entries.get(i as usize) else {
            continue;
        };
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

/// A live edit to one node's properties, re-applied by every fold.
///
/// Values are absolute, not deltas, so the previewed number is the committed
/// one; `None` keeps what the fold produced.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScratchTransform {
    /// Local translation, in the parent's frame.
    pub translation: Option<Vec3>,
    /// Euler XYZ radians.
    pub rotation: Option<Vec3>,
    /// Z is not scaled.
    pub scale: Option<Vec2>,
    pub z_order: Option<f32>,
    /// Ignored on nodes that carry no opacity — only parts and composites do.
    pub opacity: Option<f32>,
}

/// Self-driven motion still in progress after a [`Puppet::tick`]. Pose,
/// scratch and model edits are not motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Motion {
    pub motion_responses: bool,
}

#[derive(Debug, Clone, Copy)]
struct Located {
    /// Before clamping; the deform fold's memo key.
    value: f32,
    normalized: f32,
    lo: usize,
    hi: usize,
    frac: f32,
}

impl Located {
    const REST: Self = Self {
        value: 0.0,
        normalized: 0.0,
        lo: 0,
        hi: 0,
        frac: 0.0,
    };
}

/// A model being animated. Bake-derived data is shared through `Arc`, so a
/// clone copies only the frame.
#[derive(Clone)]
pub struct Puppet {
    model_identity: u64,
    baked_generation: u64,
    arena: Arena,
    transforms: GlobalTransforms,

    node_of_id: Arc<HashMap<NodeId, NodeIdx>>,
    /// Indexed by `NodeIdx.0`.
    id_of_node: Arc<Vec<NodeId>>,

    params: Arc<Vec<BakedParam>>,
    slot_of_param: Arc<HashMap<ParamId, u32>>,
    /// `None` reads the param's default.
    param_values: Vec<Option<f32>>,
    /// Values posed for params this model does not have; promoted by the next
    /// rebake that gives the Id a slot.
    param_values_overflow: HashMap<ParamId, f32>,
    param_contributions: Vec<Contribution>,
    /// Parallel to `params`: indices into `param_contributions`, in claim order.
    /// Per-slot rows keep a claim's cost proportional to the row, not the rig.
    slot_claims: Vec<smallvec::SmallVec<[u32; 2]>>,
    /// Parallel to `params`: whether any claim targets that slot.
    param_contributed: Vec<bool>,
    param_generation: u64,
    last_tick_folded_param_generation: Option<u64>,

    bindings: Arc<Vec<BakedBinding>>,
    mesh_group_inputs: Arc<Vec<bool>>,
    mesh_groups_dirty: bool,
    responses: response::Responses,
    motion_responses_enabled: bool,
    spine_targets: Arc<Vec<Vec<Option<u32>>>>,

    /// Params whose last change was a discontinuity, consumed by the next
    /// tick after it syncs.
    jumps: Vec<ParamId>,

    /// Sparse: a drag holds one or two entries.
    scratch_transforms: HashMap<NodeIdx, ScratchTransform>,

    located: Vec<Located>,
}

impl std::fmt::Debug for Puppet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Puppet")
            .field("model_identity", &self.model_identity)
            .field("baked_generation", &self.baked_generation)
            .field("nodes", &self.arena.len())
            .field("params", &self.params.len())
            .field("param_generation", &self.param_generation)
            .finish_non_exhaustive()
    }
}

/// A param's range and default as the puppet's bake holds them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamLimits {
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

impl Puppet {
    /// The model caches the first bake per generation; later puppets share it.
    pub fn new(model: &Model) -> Self {
        let mut puppet = Self {
            model_identity: model.identity(),
            baked_generation: model.generation(),
            arena: Arena::new(),
            transforms: GlobalTransforms::new(),
            node_of_id: Arc::default(),
            id_of_node: Arc::default(),
            params: Arc::default(),
            slot_of_param: Arc::default(),
            param_values: Vec::new(),
            param_values_overflow: HashMap::new(),
            param_contributions: Vec::new(),
            slot_claims: Vec::new(),
            param_contributed: Vec::new(),
            param_generation: 0,
            last_tick_folded_param_generation: None,
            bindings: Arc::default(),
            mesh_group_inputs: Arc::default(),
            mesh_groups_dirty: true,
            responses: response::Responses::default(),
            motion_responses_enabled: true,
            spine_targets: Arc::default(),
            jumps: Vec::new(),
            scratch_transforms: HashMap::new(),
            located: Vec::new(),
        };
        puppet.install(model.bake_memo().get_or_bake(model));
        puppet.responses = response::Responses::bake(&puppet, model, Default::default());
        puppet
    }

    pub fn baked_generation(&self) -> u64 {
        self.baked_generation
    }

    pub fn model_identity(&self) -> u64 {
        self.model_identity
    }

    /// Rebake if `model` moved since the last bake, carrying the pose across by
    /// Id. Driver claims are not carried; responses republish on their next step.
    ///
    /// `model` must be the model this puppet was built from; only debug builds
    /// assert it.
    pub fn sync(&mut self, model: &Model) {
        debug_assert_eq!(
            self.model_identity,
            model.identity(),
            "a puppet was driven against a model it was not built from; \
             a puppet animates the model it was baked from",
        );
        if self.baked_generation == model.generation() {
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
        let scratch: Vec<(NodeId, ScratchTransform)> = self
            .scratch_transforms
            .iter()
            .filter_map(|(idx, edit)| Some((self.id_of_node.get(idx.0 as usize)?.clone(), *edit)))
            .collect();

        let previous = std::mem::take(&mut self.responses);
        self.install(model.bake_memo().get_or_bake(model));
        self.responses = response::Responses::bake(self, model, previous);
        self.baked_generation = model.generation();

        for (id, edit) in scratch {
            let Some(&idx) = self.node_of_id.get(&id) else {
                continue;
            };
            self.scratch_transforms.insert(idx, edit);
        }
        for (id, value) in pose {
            self.set_param_value(&id, value);
        }
    }

    /// Every slot-keyed memo is dropped: the slots have moved.
    fn install(&mut self, baked: Baked) {
        let Baked {
            arena,
            node_of_id,
            id_of_node,
            params,
            slot_of_param,
            bindings,
            spine_targets,
        } = baked;
        self.arena = arena;
        self.node_of_id = node_of_id;
        self.id_of_node = id_of_node;
        self.param_values = vec![None; params.len()];
        self.slot_claims = vec![smallvec::SmallVec::new(); params.len()];
        self.param_contributed = vec![false; params.len()];
        self.located = vec![Located::REST; params.len()];
        self.params = params;
        self.slot_of_param = slot_of_param;
        self.bindings = bindings;
        self.spine_targets = spine_targets;
        self.bake_mesh_group_inputs();
        self.param_values_overflow.clear();
        self.param_contributions.clear();
        self.scratch_transforms.clear();
        self.transforms = GlobalTransforms::new();
        self.param_generation = self.param_generation.wrapping_add(1);
        self.last_tick_folded_param_generation = None;
    }

    pub fn root(&self) -> NodeIdx {
        self.arena.root()
    }

    pub fn tree(&self) -> &NodeTree {
        &self.arena.tree
    }

    pub fn len(&self) -> usize {
        self.arena.len()
    }

    /// Whether the puppet holds nothing but its root slot, which every
    /// puppet has.
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
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

    pub fn node_idx(&self, id: &NodeId) -> Option<NodeIdx> {
        self.node_of_id.get(id).copied()
    }

    pub fn node_id(&self, idx: NodeIdx) -> Option<&NodeId> {
        self.id_of_node.get(idx.0 as usize)
    }

    pub fn transforms(&self) -> &GlobalTransforms {
        &self.transforms
    }

    /// The node's `z_order` plus every ancestor's, including disabled or
    /// transparent ones. Higher draws in front.
    pub fn accumulated_z(&self, idx: NodeIdx) -> f32 {
        let mut z = 0.0;
        let mut at = Some(idx);
        while let Some(id) = at {
            let Some(node) = self.get(id) else { break };
            z += node.z_order;
            at = self.tree().get_parent(id);
        }
        z
    }

    /// Not inherited: a non-opaque composite applies its opacity once, at the
    /// blit of its own target.
    pub fn opacity(&self, idx: NodeIdx) -> Option<f32> {
        match &self.get(idx)?.kind {
            NodeKind::Part(part) => Some(part.opacity),
            NodeKind::Composite(composite) => Some(composite.opacity),
            _ => None,
        }
    }

    /// Recompute the evaluated transforms without folding anything.
    pub fn compute_transforms(&mut self) {
        let mut out = std::mem::take(&mut self.transforms);
        self.arena.compute_transforms(&mut out);
        self.transforms = out;
    }

    /// A tick does this itself; needed after writing a scratch deform outside one.
    pub fn combine_deforms(&mut self) {
        self.arena.combine_deforms();
    }

    pub fn combined_deform(&self, id: NodeIdx) -> Option<&[Vec2]> {
        match &self.arena.get(id)?.kind {
            NodeKind::Part(p) => Some(p.deform_stack.combined()),
            NodeKind::MeshGroup(mg) => Some(mg.deform_stack.combined()),
            _ => None,
        }
    }

    /// Write the scratch deform, an edit in progress that every later fold keeps.
    /// It ends at [`Self::clear_scratch_deform`] or a rebake.
    pub fn set_scratch_deform(&mut self, id: NodeIdx, offsets: &[Vec2]) -> bool {
        if let Some(sampler) = &mut self.responses.sampler {
            sampler.set_scratch_deform(id, offsets);
        }
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
        self.invalidate_fold();
        true
    }

    pub fn clear_scratch_deform(&mut self, id: NodeIdx) -> bool {
        if let Some(sampler) = &mut self.responses.sampler {
            sampler.clear_scratch_deform(id);
        }
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        match &mut node.kind {
            NodeKind::Part(p) => p.deform_stack.clear_source(DeformSource::Scratch),
            NodeKind::MeshGroup(mg) => mg.deform_stack.clear_source(DeformSource::Scratch),
            _ => return false,
        }
        self.invalidate_fold();
        true
    }

    /// Write one node's scratch transform; `false` when the arena has no such
    /// node. Applied after the bindings fold, before the transform walk.
    ///
    /// Ends only at [`Self::clear_scratch_transform`] or
    /// [`Self::clear_all_scratch`]. It survives a rebake, because the commit that
    /// ends a drag is a model edit and the node would otherwise flicker back.
    pub fn set_scratch_transform(&mut self, id: NodeIdx, edit: ScratchTransform) -> bool {
        if self.arena.get(id).is_none() {
            return false;
        }
        self.scratch_transforms.insert(id, edit);
        self.invalidate_fold();
        true
    }

    /// `false` when the node had none.
    pub fn clear_scratch_transform(&mut self, id: NodeIdx) -> bool {
        if self.scratch_transforms.remove(&id).is_none() {
            return false;
        }
        self.invalidate_fold();
        true
    }

    /// End every scratch transform and scratch deform.
    pub fn clear_all_scratch(&mut self) {
        self.scratch_transforms.clear();
        for i in 0..self.arena.deform_node_ids.len() {
            let id = self.arena.deform_node_ids[i];
            self.clear_scratch_deform(id);
        }
        self.invalidate_fold();
    }

    pub fn scratch_transform(&self, id: NodeIdx) -> Option<&ScratchTransform> {
        self.scratch_transforms.get(&id)
    }

    /// The fold is memoized on the pose generation, which a scratch edit does
    /// not move.
    fn invalidate_fold(&mut self) {
        self.mesh_groups_dirty = true;
        self.last_tick_folded_param_generation = None;
    }

    fn apply_scratch_transforms(&mut self) {
        if self.scratch_transforms.is_empty() {
            return;
        }
        let scratch = std::mem::take(&mut self.scratch_transforms);
        for (&id, edit) in &scratch {
            let Some(node) = self.arena.get_mut(id) else {
                continue;
            };
            let mut moved = false;
            if let Some(t) = edit.translation {
                node.transform.translation = t;
                moved = true;
            }
            if let Some(r) = edit.rotation {
                node.transform.rotation = r;
                moved = true;
            }
            if let Some(s) = edit.scale {
                node.transform.scale = s;
                moved = true;
            }
            if let Some(z) = edit.z_order {
                node.z_order = z;
            }
            if let Some(o) = edit.opacity {
                match &mut node.kind {
                    NodeKind::Part(part) => part.opacity = o,
                    NodeKind::Composite(composite) => composite.opacity = o,
                    _ => {}
                }
            }
            if moved {
                self.arena.mark_transform_dirty(id);
            }
        }
        self.scratch_transforms = scratch;
    }

    pub fn param_ids(&self) -> impl Iterator<Item = &ParamId> {
        self.params.iter().map(|p| &p.id)
    }

    /// Posed value with driver claims folded in; `None` when nothing posed or
    /// claims it.
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
        let claims = self.slot_claims.get(slot as usize).map_or(&[][..], |c| c);
        if claims.is_empty() {
            return base;
        }
        let default = self
            .params
            .get(slot as usize)
            .map(|p| p.default)
            .unwrap_or(0.0);
        Some(resolve_contributions(
            &self.param_contributions,
            claims,
            base.unwrap_or(default),
        ))
    }

    pub fn set_param_value(&mut self, param: &ParamId, value: f32) {
        match self.slot_of_param.get(param).copied() {
            Some(slot) => self.set_slot_value(slot, value),
            None => {
                if self.param_values_overflow.get(param).copied() == Some(value) {
                    return;
                }
                self.param_values_overflow.insert(param.clone(), value);
                self.param_generation = self.param_generation.wrapping_add(1);
            }
        }
    }

    fn set_slot_value(&mut self, slot: u32, value: f32) {
        if self.param_values.get(slot as usize).copied().flatten() == Some(value) {
            return;
        }
        if let Some(v) = self.param_values.get_mut(slot as usize) {
            *v = Some(value);
        }
        self.bump_param_generation(slot);
    }

    pub fn clear_param_value(&mut self, param: &ParamId) {
        match self.slot_of_param.get(param).copied() {
            Some(slot) => {
                if self
                    .param_values
                    .get_mut(slot as usize)
                    .and_then(|v| v.take())
                    .is_some()
                {
                    self.bump_param_generation(slot);
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
    pub fn apply_pose(&mut self, pose: &Pose) {
        for slot in 0..self.params.len() {
            let p = &self.params[slot];
            let value = pose.get(&p.id).unwrap_or(p.default);
            self.set_slot_value(slot as u32, value);
        }
        for (id, value) in pose.iter() {
            if !self.slot_of_param.contains_key(id) {
                self.set_param_value(id, value);
            }
        }
    }

    pub fn pose(&self) -> Pose {
        self.params
            .iter()
            .enumerate()
            .map(|(slot, p)| (p.id.clone(), self.resolved(slot as u32)))
            .collect()
    }

    /// Replaces `source`'s previous claim. Returns whether the resolved value
    /// moved.
    fn contribute(&mut self, slot: u32, source: NodeIdx, value: f32, weight: f32) -> bool {
        self.record_contribution(Contribution {
            slot,
            source,
            value,
            weight,
        })
    }

    fn record_contribution(&mut self, contribution: Contribution) -> bool {
        let slot = contribution.slot;
        let Some(claims) = self.slot_claims.get(slot as usize) else {
            return false;
        };
        let before = self.resolved(slot);
        let held = claims.iter().find(|&&i| {
            self.param_contributions
                .get(i as usize)
                .is_some_and(|e| e.source == contribution.source)
        });
        match held.copied() {
            Some(i) => {
                if let Some(e) = self.param_contributions.get_mut(i as usize) {
                    *e = contribution;
                }
            }
            None => {
                let at = self.param_contributions.len() as u32;
                self.param_contributions.push(contribution);
                if let Some(row) = self.slot_claims.get_mut(slot as usize) {
                    row.push(at);
                }
                if let Some(flag) = self.param_contributed.get_mut(slot as usize) {
                    *flag = true;
                }
            }
        }
        if before == self.resolved(slot) {
            return false;
        }
        self.bump_param_generation(slot);
        true
    }

    fn clear_param_contributions(&mut self) {
        if self.param_contributions.is_empty() {
            return;
        }
        for e in std::mem::take(&mut self.param_contributions) {
            if let Some(row) = self.slot_claims.get_mut(e.slot as usize) {
                row.clear();
            }
        }
        self.mesh_groups_dirty = true;
        self.param_generation = self.param_generation.wrapping_add(1);
        for flag in self.param_contributed.iter_mut() {
            *flag = false;
        }
    }

    fn param_base(&self, slot: u32) -> Option<f32> {
        let p = self.params.get(slot as usize)?;
        Some(
            self.param_values
                .get(slot as usize)
                .copied()
                .flatten()
                .unwrap_or(p.default),
        )
    }

    fn resolved(&self, slot: u32) -> f32 {
        let Some(base) = self.param_base(slot) else {
            return 0.0;
        };
        if self
            .param_contributed
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
        {
            let claims = self.slot_claims.get(slot as usize).map_or(&[][..], |c| c);
            resolve_contributions(&self.param_contributions, claims, base)
        } else {
            base
        }
    }

    /// Includes guides read by an ancestor spine, even in a sibling branch.
    fn geometry_ancestors(&self, seeds: impl IntoIterator<Item = NodeIdx>) -> HashSet<NodeIdx> {
        let mut nodes = HashSet::new();
        let mut pending: Vec<_> = seeds.into_iter().collect();
        while let Some(id) = pending.pop() {
            if !nodes.insert(id) {
                continue;
            }
            pending.extend(self.tree().get_parent(id));
            if let Some(NodeKind::Spine(spine)) = self.get(id).map(|n| &n.kind) {
                pending.extend(spine.guide_part());
            }
        }
        nodes
    }

    fn geometry_inputs(&self) -> impl Iterator<Item = (NodeIdx, u32)> + '_ {
        let bindings = self
            .bindings
            .iter()
            .filter(|b| b.target.moves_geometry())
            .flat_map(|b| std::iter::once(b.x).chain(b.y).map(move |p| (b.node, p)));
        let bends = self
            .arena
            .spine_node_ids
            .iter()
            .zip(self.spine_targets.iter())
            .flat_map(|(&node, params)| params.iter().flatten().map(move |&p| (node, p)));
        bindings.chain(bends)
    }

    fn bake_mesh_group_inputs(&mut self) {
        self.mesh_groups_dirty = true;
        let mut inputs = vec![false; self.params.len()];
        if !self.arena.mesh_group_node_ids.is_empty() {
            let mut related =
                self.geometry_ancestors(self.arena.mesh_group_node_ids.iter().copied());
            for &group in &self.arena.mesh_group_node_ids {
                related.extend(self.tree().get_all_descendants(group));
            }
            for (node, slot) in self.geometry_inputs() {
                if related.contains(&node) {
                    inputs[slot as usize] = true;
                }
            }
        }
        self.mesh_group_inputs = Arc::new(inputs);
    }

    fn bump_param_generation(&mut self, slot: u32) {
        self.mesh_groups_dirty |= self
            .mesh_group_inputs
            .get(slot as usize)
            .copied()
            .unwrap_or(false);
        self.param_generation = self.param_generation.wrapping_add(1);
    }

    /// Marks a change to `param` as a discontinuity, such as a loop wrap, so
    /// motion responses reading it restart there instead of springing across
    /// it. Taken by the next tick.
    pub fn mark_jump(&mut self, param: &ParamId) {
        if !self.jumps.contains(param) {
            self.jumps.push(param.clone());
        }
    }

    /// Whether a motion response owns `param`, so a pose written to it would
    /// be overridden.
    pub fn is_driven(&self, param: &ParamId) -> bool {
        self.motion_responses_enabled && self.reserved_output(param)
    }

    pub fn param_limits(&self, param: &ParamId) -> Option<ParamLimits> {
        let p = self.params.get(*self.slot_of_param.get(param)? as usize)?;
        Some(ParamLimits {
            min: p.min,
            max: p.max,
            default: p.default,
        })
    }

    fn take_jumps(&mut self) {
        if self.jumps.is_empty() {
            return;
        }
        let slots: smallvec::SmallVec<[u32; 4]> = self
            .jumps
            .drain(..)
            .filter_map(|p| self.slot_of_param.get(&p).copied())
            .collect();
        self.responses.reset_changed_inputs(&slots);
    }

    /// The posed value, ignoring driver claims. `None` means the model default.
    pub fn param_value_posed(&self, param: &ParamId) -> Option<f32> {
        match self.slot_of_param.get(param) {
            Some(&slot) => self.param_values.get(slot as usize).copied().flatten(),
            None => self.param_values_overflow.get(param).copied(),
        }
    }

    /// Release motion-response outputs and evaluate only the authored pose.
    pub fn set_motion_responses_enabled(&mut self, enabled: bool) {
        self.motion_responses_enabled = enabled;
        if !enabled {
            self.clear_param_contributions();
            self.responses.reset();
        }
    }

    pub fn motion_responses_enabled(&self) -> bool {
        self.motion_responses_enabled
    }

    /// Evaluate the next frame, rebaking first if `model` moved.
    pub fn tick(&mut self, model: &Model, dt: f32) -> Motion {
        self.tick_with_root(model, Mat4::IDENTITY, dt)
    }

    /// Drivers still sample the model-local pose, independent of `root`.
    pub fn tick_with_root(&mut self, model: &Model, root: Mat4, dt: f32) -> Motion {
        let _span = tracing::trace_span!("tick").entered();
        self.sync(model);
        self.take_jumps();
        let mut out = std::mem::take(&mut self.transforms);
        let motion = Motion {
            motion_responses: self.advance_responses(model, dt, false),
        };
        let params_changed = self.last_tick_folded_param_generation != Some(self.param_generation);
        if params_changed {
            self.fold_frame(&mut out, root);
        } else {
            self.arena.compute_transforms_with_root(&mut out, root);
        }
        self.transforms = out;
        motion
    }

    fn fold_frame(&mut self, out: &mut GlobalTransforms, root: Mat4) {
        self.reset_frame();
        self.apply_params();
        self.apply_scratch_transforms();
        // Transforms before propagation, so mesh groups see this frame's pose.
        self.arena.compute_transforms_with_root(out, root);
        // Spines before mesh groups, so a group deforms art the spine has bent.
        // Unconditional: `reset_frame` dropped every spine source.
        if !self.arena.spine_node_ids.is_empty() {
            self.apply_spine_bends();
            self.arena.propagate_spine_deforms(out);
        }
        if !self.arena.mesh_group_node_ids.is_empty() {
            if self.mesh_groups_dirty {
                self.arena.propagate_mesh_group_deforms(out, root);
                self.mesh_groups_dirty = false;
            } else {
                self.arena.restore_mesh_group_deforms(out, root);
            }
        }
        self.arena.apply_welds(out);
        self.arena.combine_deforms();
        self.last_tick_folded_param_generation = Some(self.param_generation);
    }

    /// A bend is the raw param value, not its normalized range position. A link
    /// with no target reads zero.
    fn apply_spine_bends(&mut self) {
        let _span = tracing::trace_span!("apply_spine_bends").entered();
        for i in 0..self.arena.spine_node_ids.len() {
            let id = self.arena.spine_node_ids[i];
            let Some(targets) = self.spine_targets.get(i) else {
                continue;
            };
            let bends: smallvec::SmallVec<[f32; 8]> = targets
                .iter()
                .map(|slot| slot.map_or(0.0, |slot| self.resolved(slot)))
                .collect();
            if let Some(NodeKind::Spine(spine)) = self.arena.get_mut(id).map(|n| &mut n.kind) {
                spine.bends.clear();
                spine.bends.extend_from_slice(&bends);
                spine.bends.resize(spine.joints.len(), 0.0);
            }
        }
    }

    fn reset_frame(&mut self) {
        self.last_tick_folded_param_generation = None;
        self.arena.reset_dynamic_state();
        self.arena.reset_deforms();
    }

    /// Additive, so it must follow `reset_frame`.
    pub(crate) fn apply_params(&mut self) {
        let _span = tracing::trace_span!("apply_params").entered();
        for slot in 0..self.params.len() {
            self.located[slot] = self.locate(slot as u32);
        }
        let bindings = Arc::clone(&self.bindings);
        for b in bindings.iter() {
            self.fold_binding(b);
        }
    }

    fn locate(&self, slot: u32) -> Located {
        let Some(p) = self.params.get(slot as usize) else {
            return Located::REST;
        };
        let value = self.resolved(slot);
        let span = p.max - p.min;
        let normed = if span.abs() > 1e-9 {
            ((value.clamp(p.min, p.max) - p.min) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Located {
            value,
            normalized: normed,
            ..Located::REST
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn unrelated_input_reuses_the_group_deforms_and_shifts_of_the_posed_model() {
        let mut model = Model::from_clm_bytes(
            &std::fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/models/lan.clm"
            ))
            .unwrap(),
        )
        .unwrap();
        let control = ParamId::new("visibility").unwrap();
        model
            .add_param_with_id(
                control.clone(),
                crate::ModelParam::new(crate::Name::truncated("Visibility"), 0., 1., 1.),
            )
            .unwrap();
        let part = model
            .node_ids()
            .find(|id| {
                matches!(
                    model.node(id).map(|n| &n.kind),
                    Some(crate::ModelNodeKind::Part(_))
                )
            })
            .unwrap()
            .clone();
        let opacity = crate::BindingKey::new(
            control.clone(),
            part.clone(),
            crate::BindingTarget::Scalar(crate::ScalarTarget::Opacity),
        );
        model.set_binding_key(&opacity, [0, 0], 0.).unwrap();
        let mut cached = Puppet::new(&model);
        cached.set_motion_responses_enabled(false);
        cached.set_param_value(&ParamId::new("head_roll").unwrap(), 20.);
        cached.tick(&model, 0.);
        assert!(!cached.arena.mg_written_sources.is_empty());
        cached.set_param_value(&control, 0.5);
        assert!(!cached.mesh_groups_dirty);
        let mut reference = cached.clone();
        reference.invalidate_fold();
        cached.tick(&model, 0.);
        reference.tick(&model, 0.);
        for &id in &cached.arena.deform_node_ids {
            assert_eq!(cached.transforms.get(id), reference.transforms.get(id));
            if let (Some(NodeKind::Part(a)), Some(NodeKind::Part(b))) = (
                cached.get(id).map(|n| &n.kind),
                reference.get(id).map(|n| &n.kind),
            ) {
                assert_eq!(a.deform_stack.combined(), b.deform_stack.combined());
                assert_eq!(a.opacity, b.opacity);
            }
        }
    }

    /// Unedited models share generation 0, so identity is what catches a mismatch.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a puppet was driven against a model it was not built from")]
    fn ticking_against_another_model_at_the_same_generation_is_a_programmer_error() {
        let a = Model::new();
        let b = Model::new();
        assert_eq!(a.generation(), b.generation());
        let mut puppet = Puppet::new(&a);
        puppet.tick(&b, 0.0);
    }

    #[test]
    fn a_scratch_deform_outlives_a_tick_and_dies_with_the_bake() {
        use crate::formats::clm::{ClmIndices, ClmMesh};
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind, ModelPart};

        let mut model = Model::new();
        let mut hex = SeededHex::new(11);
        let root = model.root().expect("a fresh model has one root").clone();
        let part = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh {
                        verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0],
                        uvs: vec![0.0; 6],
                        indices: ClmIndices::U16(vec![0, 1, 2]),
                        origin: [0.0, 0.0],
                    })),
                ),
                &mut hex,
            )
            .expect("add part");

        let mut puppet = Puppet::new(&model);
        puppet.tick(&model, 0.0);
        let idx = puppet.node_idx(&part).expect("part baked");

        let drag = [Vec2::new(3.0, -4.0); 3];
        assert!(puppet.set_scratch_deform(idx, &drag));
        puppet.combine_deforms();
        assert_eq!(puppet.combined_deform(idx), Some(&drag[..]));

        puppet.tick(&model, 0.016);
        assert_eq!(
            puppet.combined_deform(idx),
            Some(&drag[..]),
            "a tick must not swallow an edit in progress",
        );

        assert!(puppet.clear_scratch_deform(idx));
        puppet.combine_deforms();
        assert_eq!(puppet.combined_deform(idx), Some(&[Vec2::ZERO; 3][..]));

        assert!(puppet.set_scratch_deform(idx, &drag));
        puppet.combine_deforms();
        model
            .update_node(&part, |n| n.z_order = 1.0)
            .expect("edit the part");
        puppet.tick(&model, 0.0);
        assert_eq!(
            puppet.combined_deform(idx),
            Some(&[Vec2::ZERO; 3][..]),
            "a rebake starts from the model, so the drag is gone",
        );
    }

    /// A clone is the same model, as an undo snapshot is.
    #[test]
    fn a_clone_of_the_model_still_drives_the_puppet() {
        let model = Model::new();
        let snapshot = model.clone();
        let mut puppet = Puppet::new(&model);
        puppet.tick(&snapshot, 0.0);
        assert_eq!(puppet.model_identity(), snapshot.identity());
    }

    #[test]
    fn a_scratch_transform_outlives_ticks_and_rebakes_and_dies_with_its_writer() {
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind};

        let mut model = Model::new();
        let mut hex = SeededHex::new(5);
        let root = model.root().expect("a fresh model has one root").clone();
        let parent = model
            .add_node(
                &root,
                ModelNode::new("parent", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add parent");
        let child = model
            .add_node(
                &parent,
                ModelNode::new("child", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add child");

        let mut puppet = Puppet::new(&model);
        puppet.tick(&model, 0.0);
        let parent_idx = puppet.node_idx(&parent).expect("parent baked");
        let child_idx = puppet.node_idx(&child).expect("child baked");
        let at_rest = puppet.transforms().get(child_idx).w_axis.x;

        let drag = ScratchTransform {
            translation: Some(Vec3::new(25.0, 0.0, 0.0)),
            ..ScratchTransform::default()
        };
        assert!(puppet.set_scratch_transform(parent_idx, drag));
        assert_eq!(puppet.scratch_transform(parent_idx), Some(&drag));

        // The second tick is the one a memoized fold would skip.
        for _ in 0..2 {
            puppet.tick(&model, 0.016);
            assert!(
                (puppet.transforms().get(parent_idx).w_axis.x - 25.0).abs() < 1e-4,
                "a tick must not swallow an edit in progress",
            );
            assert!(
                (puppet.transforms().get(child_idx).w_axis.x - at_rest - 25.0).abs() < 1e-4,
                "the child must follow its previewed parent",
            );
        }

        model
            .update_node(&child, |n| n.z_order = 1.0)
            .expect("edit the model");
        puppet.tick(&model, 0.0);
        let parent_idx = puppet.node_idx(&parent).expect("parent still baked");
        let child_idx = puppet.node_idx(&child).expect("child still baked");
        assert!(
            (puppet.transforms().get(child_idx).w_axis.x - at_rest - 25.0).abs() < 1e-4,
            "a rebake re-keys the scratch by Id rather than dropping it",
        );

        assert!(puppet.clear_scratch_transform(parent_idx));
        assert!(!puppet.clear_scratch_transform(parent_idx), "already gone");
        assert_eq!(puppet.scratch_transform(parent_idx), None);
        puppet.tick(&model, 0.0);
        assert!(
            (puppet.transforms().get(child_idx).w_axis.x - at_rest).abs() < 1e-4,
            "a cleared preview folds away",
        );

        assert!(puppet.set_scratch_transform(parent_idx, drag));
        puppet.clear_all_scratch();
        assert_eq!(puppet.scratch_transform(parent_idx), None);
        puppet.tick(&model, 0.0);
        assert!(
            (puppet.transforms().get(child_idx).w_axis.x - at_rest).abs() < 1e-4,
            "clear_all_scratch ends every edit in progress",
        );
    }

    fn claim_index_agrees(puppet: &Puppet) {
        let mut counted = 0;
        for (slot, row) in puppet.slot_claims.iter().enumerate() {
            for &i in row {
                let e = puppet
                    .param_contributions
                    .get(i as usize)
                    .expect("a row points at a claim that exists");
                assert_eq!(
                    e.slot as usize, slot,
                    "slot {slot}'s row points at a claim on slot {}",
                    e.slot,
                );
                counted += 1;
            }
            assert_eq!(
                !row.is_empty(),
                puppet.param_contributed[slot],
                "slot {slot}'s fast-path flag disagrees with its row",
            );
        }
        assert_eq!(
            counted,
            puppet.param_contributions.len(),
            "every claim is indexed exactly once",
        );
    }

    #[test]
    fn puppets_of_one_generation_share_a_bake() {
        use crate::id::SeededHex;
        use crate::model::ScalarTarget;
        use crate::model::{BindingKey, BindingTarget, ModelNode, ModelNodeKind, ModelParam};
        use crate::Name;

        let mut hex = SeededHex::new(5);
        let mut model = Model::new();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("slide"),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        let root = model.root().expect("a fresh model has one root").clone();
        let group = model
            .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
            .expect("add group");
        let key = BindingKey::new(
            param.clone(),
            group.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        model.set_binding_key(&key, [0, 0], 0.0).expect("key");
        model.set_binding_key(&key, [1, 0], 40.0).expect("key");

        let shared = |a: &Puppet, b: &Puppet| {
            Arc::ptr_eq(&a.bindings, &b.bindings)
                && Arc::ptr_eq(&a.arena.tree, &b.arena.tree)
                && Arc::ptr_eq(&a.node_of_id, &b.node_of_id)
        };
        let mut first = Puppet::new(&model);
        let mut second = Puppet::new(&model);
        assert!(shared(&first, &second));
        let snapshot = model.clone();
        assert!(shared(&first, &Puppet::new(&snapshot)));

        let idx = first.node_idx(&group).expect("group baked");
        let tx = |p: &Puppet| p.get(idx).expect("node").transform.translation.x;
        first.set_param_value(&param, 1.0);
        first.tick(&model, 0.0);
        second.tick(&model, 0.0);
        assert_eq!((tx(&first), tx(&second)), (40.0, 0.0));

        model.set_binding_key(&key, [1, 0], 10.0).expect("key");
        let after = Puppet::new(&model);
        assert!(!shared(&first, &after));
        first.sync(&model);
        assert!(shared(&first, &after));
        first.tick(&model, 0.0);
        assert_eq!(tx(&first), 10.0, "the pose crossed the rebake");
        let mut old = Puppet::new(&snapshot);
        old.set_param_value(&param, 1.0);
        old.tick(&snapshot, 0.0);
        assert_eq!(tx(&old), 40.0);
    }

    #[test]
    fn a_clone_shares_the_baked_model_and_owns_its_frame() {
        use crate::formats::clm::{ClmIndices, ClmMesh};
        use crate::id::SeededHex;
        use crate::model::{BindingKey, BindingTarget, ModelNode, ModelNodeKind, ModelParam};
        use crate::model::{ModelPart, ScalarTarget};
        use crate::Name;

        let mut hex = SeededHex::new(3);
        let mut model = Model::new();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("slide"),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        let root = model.root().expect("a fresh model has one root").clone();
        let mesh = ClmMesh {
            verts: vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            uvs: vec![0.0; 6],
            indices: ClmIndices::U16(vec![0, 1, 2]),
            origin: [0.0, 0.0],
        };
        let part = model
            .add_node(
                &root,
                ModelNode::new("art", ModelNodeKind::Part(ModelPart::new(mesh))),
                &mut hex,
            )
            .expect("add part");
        let key = BindingKey::new(
            param.clone(),
            part.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        // A lone cell would fill the whole axis.
        model.set_binding_key(&key, [0, 0], 0.0).expect("key");
        model.set_binding_key(&key, [1, 0], 40.0).expect("key");

        let mut original = Puppet::new(&model);
        original.tick(&model, 0.0);
        let mut fork = original.clone();
        let idx = original.node_idx(&part).expect("part baked");

        let mesh_of = |p: &Puppet| match &p.get(idx).expect("node").kind {
            NodeKind::Part(part) => Arc::clone(&part.mesh),
            _ => unreachable!("the node is a part"),
        };
        assert!(Arc::ptr_eq(&mesh_of(&original), &mesh_of(&fork)));
        assert!(Arc::ptr_eq(&original.bindings, &fork.bindings));
        assert!(Arc::ptr_eq(&original.arena.tree, &fork.arena.tree));
        assert!(Arc::ptr_eq(&original.id_of_node, &fork.id_of_node));

        fork.set_param_value(&param, 1.0);
        fork.tick(&model, 0.0);
        let tx = |p: &Puppet| p.get(idx).expect("node").transform.translation.x;
        assert_eq!(tx(&fork), 40.0);
        assert_eq!(tx(&original), 0.0, "the original's frame is its own");
    }

    fn two_claimants() -> (Model, Puppet, ParamId, NodeIdx, NodeIdx) {
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind, ModelParam};
        use crate::Name;

        let mut hex = SeededHex::new(3);
        let mut model = Model::new();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("aim"),
                    min: -10.0,
                    max: 10.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        let root = model.root().expect("a fresh model has one root").clone();
        let nodes: Vec<NodeId> = ["a", "b"]
            .into_iter()
            .map(|name| {
                model
                    .add_node(&root, ModelNode::new(name, ModelNodeKind::Group), &mut hex)
                    .expect("add group")
            })
            .collect();
        let puppet = Puppet::new(&model);
        let a = puppet.node_idx(&nodes[0]).expect("a baked");
        let b = puppet.node_idx(&nodes[1]).expect("b baked");
        (model, puppet, param, a, b)
    }

    #[test]
    fn two_claims_on_one_slot_average() {
        let (_model, mut puppet, param, a, b) = two_claimants();
        let slot = puppet.slot_of_param.get(&param).copied().expect("a slot");
        puppet.set_param_value(&param, 4.0);

        assert!(puppet.contribute(slot, a, 0.0, 1.0));
        assert_eq!(puppet.param_value(&param), Some(0.0), "one full claim wins");

        assert!(puppet.contribute(slot, b, 1.0, 1.0));
        assert_eq!(
            puppet.param_value(&param),
            Some(0.5),
            "two full claims meet in the middle rather than the later winning",
        );
        claim_index_agrees(&puppet);

        assert!(puppet.contribute(slot, b, 1.0, 0.0));
        assert_eq!(puppet.param_value(&param), Some(0.0));
        assert_eq!(puppet.param_contributions.len(), 2, "an upsert, not a push");
        claim_index_agrees(&puppet);
    }

    #[test]
    fn a_claim_made_twice_counts_once() {
        let (_model, mut puppet, param, a, _b) = two_claimants();
        let slot = puppet.slot_of_param.get(&param).copied().expect("a slot");

        assert!(puppet.contribute(slot, a, 2.0, 1.0));
        assert!(puppet.contribute(slot, a, 6.0, 1.0));
        assert_eq!(
            puppet.param_value(&param),
            Some(6.0),
            "the second claim replaced the first instead of averaging with it",
        );
        assert_eq!(puppet.param_contributions.len(), 1);
        assert_eq!(puppet.slot_claims[slot as usize].len(), 1);
        claim_index_agrees(&puppet);

        assert!(!puppet.contribute(slot, a, 6.0, 1.0));
        assert_eq!(puppet.param_contributions.len(), 1);
        claim_index_agrees(&puppet);
    }

    #[test]
    fn clearing_the_claims_empties_the_index() {
        let (_model, mut puppet, param, a, b) = two_claimants();
        let slot = puppet.slot_of_param.get(&param).copied().expect("a slot");
        puppet.set_param_value(&param, 3.0);
        puppet.contribute(slot, a, 0.0, 1.0);
        puppet.contribute(slot, b, 1.0, 1.0);
        claim_index_agrees(&puppet);

        puppet.clear_param_contributions();
        assert!(puppet.param_contributions.is_empty());
        assert!(
            puppet.slot_claims.iter().all(|row| row.is_empty()),
            "a cleared claim leaves no row behind",
        );
        claim_index_agrees(&puppet);
        assert_eq!(
            puppet.param_value(&param),
            Some(3.0),
            "and the param is back to what was posed",
        );
    }
}
