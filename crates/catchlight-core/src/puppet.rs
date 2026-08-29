//! The `Puppet`: node tree, params, deforms, and the per-frame pipeline.
//!
//! **The per-frame pipeline is `Puppet::tick`**, not a bare
//! `compute_transforms`. Semantically it is: fold animations → pose the
//! physics anchors and step the drivers → apply params → compute transforms →
//! apply `translateChildren` mesh-group filters and recompute → propagate
//! mesh-group deforms → apply welds → combine deforms. **The code is an
//! optimized form of those semantics**, generation-cached in three places, and
//! that caching is where a bug hides: the anchor pre-pass is skipped unless
//! `param_generation` moved (`last_anchor_pose_generation`), the whole fold is
//! skipped when neither params nor the pre-pass touched anything
//! (`last_tick_folded_param_generation`), and the third transform walk runs
//! only when `apply_translate_children_filter` actually shifted something. A
//! pre-pass that ran forces the final apply, because it reset opacity/tint and
//! deactivated every deform stack.
//!
//! **`settle_physics` before the first render.** It iterates to the fixed
//! point of "anchor → param value → transforms → anchor" so a freshly loaded
//! rig renders settled instead of swinging into place, and it leaves the
//! puppet *unposed* — `tick` is what folds a renderable pose.

use std::collections::{HashMap, HashSet};

use glam::Mat4;

use crate::{
    components::{Node, NodeIdx, PuppetTexture},
    node::NodeTree,
};

/// Squared anchor movement below which `settle_physics` calls a driver
/// converged. Anchors are in puppet units, so this is a sub-millipixel
/// displacement on rigs whose coordinates run to the thousands.
const SETTLE_EPS_SQ: f32 = 1e-6;

/// One source's opinion about a parameter. `weight` is authority, at or
/// above zero: 0 asserts nothing, 1 fully specifies the value, and larger
/// weights only matter relative to whatever else claims the parameter.
#[derive(Debug, Clone, Copy)]
pub struct ParamContribution {
    pub value: glam::Vec2,
    pub weight: f32,
}

#[derive(Debug, Clone, Copy)]
struct ParamContributionEntry {
    uuid: u32,
    source: NodeIdx,
    contribution: ParamContribution,
}

/// Fold weighted opinions against the value the host committed.
///
/// The rule is an **order-independent** weighted mean, deliberately not a
/// sequential source-over: there is no principled ordering between "the
/// host wrote this before `tick`" and "a driver wrote it during `tick`",
/// so inventing one would bury a semantic choice in call order. A caller
/// that genuinely wants ordered compositing resolves it on its own side
/// and contributes the result at weight 1 — which is what the host's mixer
/// does.
///
/// | contributions | result |
/// | --- | --- |
/// | none | `base` |
/// | one at `w = 1` | that value |
/// | one at `w = 0.5` | halfway between `base` and that value |
/// | two at `w = 1` | their midpoint, *not* last-writer-wins |
/// | `w = 1` and `w = 3` | weighted 1:3 toward the second |
fn resolve_contributions(
    entries: &[ParamContributionEntry],
    uuid: u32,
    base: glam::Vec2,
) -> glam::Vec2 {
    let mut total = 0.0;
    let mut sum = glam::Vec2::ZERO;
    for e in entries.iter().filter(|e| e.uuid == uuid) {
        total += e.contribution.weight;
        sum += e.contribution.value * e.contribution.weight;
    }
    if total <= 0.0 {
        base
    } else if total < 1.0 {
        sum + base * (1.0 - total)
    } else {
        sum / total
    }
}

/// Computed global transforms for all nodes in a puppet.
/// Vec<Mat4> indexed by NodeIdx.0; aligns with the dense Puppet node
/// storage so point lookups are a bounds check + index rather than
/// a hash probe, and the DFS walk in compute_transforms_with_root
/// writes through contiguous memory.
#[derive(Debug, Clone)]
pub struct GlobalTransforms {
    transforms: Vec<Mat4>,
}

impl GlobalTransforms {
    pub fn new() -> Self {
        Self {
            transforms: Vec::new(),
        }
    }

    pub fn get(&self, id: NodeIdx) -> Mat4 {
        self.transforms
            .get(id.0 as usize)
            .copied()
            .unwrap_or(Mat4::IDENTITY)
    }

    pub fn is_empty(&self) -> bool {
        self.transforms.is_empty()
    }

    fn ensure_size(&mut self, size: usize) {
        if self.transforms.len() < size {
            self.transforms.resize(size, Mat4::IDENTITY);
        }
    }

    fn insert(&mut self, id: NodeIdx, transform: Mat4) {
        let idx = id.0 as usize;
        if idx >= self.transforms.len() {
            self.transforms.resize(idx + 1, Mat4::IDENTITY);
        }
        self.transforms[idx] = transform;
    }
}

impl Default for GlobalTransforms {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct Puppet {
    // Dense storage indexed by NodeIdx.0. Every NodeIdx allocated via
    // `allocate_id` is sequential and never removed, so NodeIdx doubles
    // as the slot index -- no HashMap<NodeIdx,_> indirection, full-node
    // passes are cache-linear, and point lookups are a bounds check + index.
    nodes: Vec<Node>,
    tree: NodeTree,
    uuid_to_node: HashMap<u32, NodeIdx>,
    textures: Vec<PuppetTexture>,
    next_id: u32,
    params: Vec<crate::params::Param>,
    /// Name -> index into `params`. Built once by `set_params`; lets
    /// `param_by_name` / `set_param_value_by_name` skip the linear
    /// `params.iter().find` for puppets with hundreds of params.
    param_index: HashMap<String, usize>,
    /// Uuid -> index into `params`. Built by `set_params` so the per-uuid
    /// write paths (`set_param_value`, animation / driver writes) reach
    /// the dense `param_values` slot with a single hash, while
    /// `apply_params_where` indexes it positionally with none.
    param_id_to_index: HashMap<u32, usize>,
    /// Current per-frame value for every param, dense and parallel to
    /// `params` (index = position). `None` falls back to the param's
    /// `defaults` at apply time — the state `clear_param_value` leaves.
    param_values: Vec<Option<glam::Vec2>>,
    /// Values written for uuids with no matching param (a physics
    /// `target_param_id` pointing nowhere, or a caller writing before
    /// `set_params`). Kept off the dense path so it never costs the hot
    /// loops, preserving the stored-and-readable semantics such writes
    /// have always had.
    param_values_overflow: HashMap<u32, glam::Vec2>,
    /// Weighted opinions from writers that sit *below* the host's
    /// committed value — physics drivers today. One entry per
    /// (param, source), upserted by the source rather than cleared each
    /// frame, so a driver's output survives into the next frame's anchor
    /// pre-pass. That persistence is what couples chained physics.
    param_contributions: Vec<ParamContributionEntry>,
    /// Parallel to `params`: does any entry target that slot. Keeps the
    /// twice-a-frame apply loop to one bool load for the overwhelming
    /// majority of params, which have no contributor at all.
    param_contributed: Vec<bool>,
    param_generation: u64,
    last_tick_folded_param_generation: Option<u64>,
    animations: Vec<crate::animation::Animation>,
    play_state: Option<crate::animation::AnimationPlayState>,
    // Frame-persistent scratch used by propagate_mesh_group_deforms to
    // carry computed offsets between a read-borrow of the MG node and a
    // write-borrow of the child node. Sized by the largest child
    // vert_count seen so far; reused across MGs and across frames.
    pub(crate) mg_propagate_scratch: Vec<glam::Vec2>,
    // Parallel scratch used by the dynamic-MG branch to read the child's
    // combined-minus-Node(mg_id) deform without allocating a per-frame
    // Vec. Sized by the largest child vert_count seen so far.
    pub(crate) mg_cur_deform_scratch: Vec<glam::Vec2>,
    // Per-MG scratch for `mg_vertices[i] + mg_combined[i]` precomputed
    // once per MG before the per-child loop. Avoids re-summing per
    // child-vert when many children of the same MG hit the same MG
    // triangle. Sized by the largest MG vertex count.
    pub(crate) mg_deformed_vertices_scratch: Vec<glam::Vec2>,
    // Cached pre-order of MeshGroup NodeIdxs. Invalidated on tree edits
    // (see insert_child). None = stale, recompute on next access.
    pub(crate) mg_pre_order_cache: Option<Vec<NodeIdx>>,
    // base_transform.to_matrix() cached per slot. 60-100ns per node of
    // quat/mat ops saved in compute_transforms when the node has no
    // active transform delta this frame.
    pub(crate) base_local_matrix: Vec<Mat4>,
    // Parallel to nodes: true when apply_params/tick wrote a delta into
    // node.transform this frame. Cleared by reset_dynamic_state.
    pub(crate) node_transform_dirty: Vec<bool>,
    pub(crate) deform_node_ids: Vec<NodeIdx>,
    /// Puppet-global weld list; solve order is list order (see
    /// [`crate::weld::apply_welds`]).
    pub(crate) welds: Vec<crate::weld::Weld>,
    // Frame-persistent scratches for the weld pass: each side's current
    // deform sum, read without disturbing the stack memos.
    pub(crate) weld_cur_a_scratch: Vec<glam::Vec2>,
    pub(crate) weld_cur_b_scratch: Vec<glam::Vec2>,
    pub(crate) physics_node_ids: Vec<NodeIdx>,
    /// When false, `tick` skips SimplePhysics entirely: drivers never
    /// overwrite their target params, so those params evaluate at their
    /// defaults (or whatever the caller poses) and the same pose always
    /// yields the same output. The editor freezes physics this way — its
    /// dt=0 preview can't integrate, and chained drivers would otherwise
    /// leave pose-history-dependent residue in the authoring view.
    physics_enabled: bool,
    pub(crate) mesh_group_node_ids: Vec<NodeIdx>,
    param_mesh_group_relevant: HashSet<u32>,
    mesh_group_param_generation: u64,
    last_tick_mesh_group_generation: Option<u64>,
    // Puppet-local (root=IDENTITY) transform scratch used when sampling
    // SimplePhysics anchors. Decouples the physics integrator from any
    // host world-scale: pendulum length and gravity are loaded in
    // puppet-local units, so anchors must be in matching units.
    pub(crate) physics_transforms: GlobalTransforms,
    physics_update_scratch: Vec<(u32, NodeIdx, glam::Vec2)>,
    /// `Some(G)` means `physics_transforms` and the node-level anchor
    /// inputs (`node.transform` translations, `offset_output_scale`) hold
    /// the anchor pose a fresh pre-pass at `param_generation == G` would
    /// produce. Lets `tick` skip the physics pre-pass + final apply once
    /// params and driver outputs are static. Cleared wherever
    /// `last_tick_folded_param_generation` is (`reset_dynamic_state`,
    /// `set_params`, `insert_child`), so any state change invalidates the
    /// cached pose.
    last_anchor_pose_generation: Option<u64>,
    /// Cached `physics_anchor_skip_allowed`. Depends only on tree
    /// structure + node flags, so invalidated on `insert_child`.
    physics_anchor_skip_cached: Option<bool>,
    /// Slots that are a SimplePhysics node or an ancestor of one — the
    /// only slots the physics pre-pass transform walk needs to fill (see
    /// `compute_physics_ancestor_transforms`). Invalidated on
    /// `insert_child`.
    physics_ancestor_mask: Option<Vec<bool>>,
    // Changes whenever compiled node state that can affect a consumer's
    // structural cache is replaced through a public mutation API.
    node_revision: u64,
}

impl Puppet {
    pub fn new() -> Self {
        let root_id = NodeIdx::new(0);
        let tree = NodeTree::new(root_id);

        // Slot 0 = root (NodeIdx::new(0)).
        let nodes = vec![Node::default()];

        Self {
            nodes,
            tree,
            uuid_to_node: HashMap::new(),
            textures: Vec::new(),
            next_id: 1,
            params: Vec::new(),
            param_index: HashMap::new(),
            param_id_to_index: HashMap::new(),
            param_values: Vec::new(),
            param_values_overflow: HashMap::new(),
            param_contributions: Vec::new(),
            param_contributed: Vec::new(),
            param_generation: 0,
            last_tick_folded_param_generation: None,
            animations: Vec::new(),
            play_state: None,
            mg_propagate_scratch: Vec::new(),
            mg_cur_deform_scratch: Vec::new(),
            mg_deformed_vertices_scratch: Vec::new(),
            mg_pre_order_cache: None,
            base_local_matrix: vec![Mat4::IDENTITY],
            node_transform_dirty: vec![false],
            deform_node_ids: Vec::new(),
            welds: Vec::new(),
            weld_cur_a_scratch: Vec::new(),
            weld_cur_b_scratch: Vec::new(),
            physics_node_ids: Vec::new(),
            physics_enabled: true,
            mesh_group_node_ids: Vec::new(),
            param_mesh_group_relevant: HashSet::new(),
            mesh_group_param_generation: 0,
            last_tick_mesh_group_generation: None,
            physics_transforms: GlobalTransforms::new(),
            physics_update_scratch: Vec::new(),
            last_anchor_pose_generation: None,
            physics_anchor_skip_cached: None,
            physics_ancestor_mask: None,
            node_revision: 0,
        }
    }

    pub fn root(&self) -> NodeIdx {
        self.tree.root
    }

    pub fn tree(&self) -> &NodeTree {
        &self.tree
    }

    pub fn textures(&self) -> &[PuppetTexture] {
        &self.textures
    }

    pub fn set_textures(&mut self, textures: Vec<PuppetTexture>) {
        self.textures = textures;
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get(&self, id: NodeIdx) -> Option<&Node> {
        self.nodes.get(id.0 as usize)
    }

    pub(crate) fn get_mut(&mut self, id: NodeIdx) -> Option<&mut Node> {
        self.nodes.get_mut(id.0 as usize)
    }

    pub fn node_revision(&self) -> u64 {
        self.node_revision
    }

    /// Replace a node's authored transform and reset its working transform to
    /// the same value. Runtime-only pose changes should use
    /// [`Self::update_node_transform`] instead.
    pub fn set_node_base_transform(&mut self, id: NodeIdx, transform: crate::Transform) -> bool {
        let slot = id.0 as usize;
        let Some(node) = self.nodes.get_mut(slot) else {
            return false;
        };
        node.base_transform = transform;
        node.transform = transform;
        if let Some(matrix) = self.base_local_matrix.get_mut(slot) {
            *matrix = transform.to_matrix();
        }
        if let Some(dirty) = self.node_transform_dirty.get_mut(slot) {
            *dirty = false;
        }
        self.rebuild_all_mesh_group_attachments();
        self.invalidate_pose_memos();
        self.bump_node_revision();
        true
    }

    /// Mutate only the current working transform. The authored base and all
    /// node-kind registries remain unchanged.
    pub fn update_node_transform<R>(
        &mut self,
        id: NodeIdx,
        update: impl FnOnce(&mut crate::Transform) -> R,
    ) -> Option<R> {
        let slot = id.0 as usize;
        let result = update(&mut self.nodes.get_mut(slot)?.transform);
        if let Some(dirty) = self.node_transform_dirty.get_mut(slot) {
            *dirty = true;
        }
        self.last_anchor_pose_generation = None;
        Some(result)
    }

    pub fn set_node_z_order(&mut self, id: NodeIdx, z_order: f32) -> bool {
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        node.z_order = z_order;
        true
    }

    pub fn set_node_enabled(&mut self, id: NodeIdx, enabled: bool) -> bool {
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        node.enabled = enabled;
        true
    }

    pub fn set_node_opacity(&mut self, id: NodeIdx, opacity: f32) -> bool {
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        match &mut node.kind {
            crate::NodeKind::Part(part) => part.opacity = opacity,
            crate::NodeKind::Composite(composite) => composite.opacity = opacity,
            _ => return false,
        }
        true
    }

    /// Replace the compiled node kind and rebuild every kind-dependent
    /// registry. Callers that change meshes must re-upload the puppet to any
    /// resident renderer.
    pub fn set_node_kind(&mut self, id: NodeIdx, kind: crate::NodeKind) -> bool {
        let old_physics = self.physics_node_ids.clone();
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        node.kind = kind;
        self.rebuild_node_kind_state(&old_physics);
        true
    }

    pub fn set_node_blend_mode(&mut self, id: NodeIdx, blend_mode: crate::BlendMode) -> bool {
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        match &mut node.kind {
            crate::NodeKind::Part(part) => part.blend_mode = blend_mode,
            crate::NodeKind::Composite(composite) => composite.blend_mode = blend_mode,
            _ => return false,
        }
        self.bump_node_revision();
        true
    }

    pub fn retain_node_masks(
        &mut self,
        id: NodeIdx,
        mut keep: impl FnMut(&crate::Mask) -> bool,
    ) -> bool {
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        let masks = match &mut node.kind {
            crate::NodeKind::Part(part) => &mut part.masks,
            crate::NodeKind::Composite(composite) => &mut composite.masks,
            _ => return false,
        };
        let old_len = masks.len();
        masks.retain(|mask| keep(mask));
        if masks.len() != old_len {
            self.bump_node_revision();
        }
        true
    }

    pub fn set_node_masks(&mut self, id: NodeIdx, masks: Vec<crate::Mask>) -> bool {
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        match &mut node.kind {
            crate::NodeKind::Part(part) => part.masks = masks,
            crate::NodeKind::Composite(composite) => composite.masks = masks,
            _ => return false,
        }
        self.bump_node_revision();
        true
    }

    /// Edit one pooled deform source without exposing the node kind or the
    /// stack's shape. Returns `None` for non-deform nodes.
    pub fn update_deform_source<R>(
        &mut self,
        id: NodeIdx,
        source: crate::deform::DeformSource,
        update: impl FnOnce(&mut [glam::Vec2]) -> R,
    ) -> Option<R> {
        let node = self.nodes.get_mut(id.0 as usize)?;
        let stack = match &mut node.kind {
            crate::NodeKind::Part(part) => &mut part.deform_stack,
            crate::NodeKind::MeshGroup(mesh_group) => &mut mesh_group.deform_stack,
            _ => return None,
        };
        Some(update(stack.source_buf_mut(source)))
    }

    pub fn reset_node_deforms(&mut self, id: NodeIdx) -> bool {
        let Some(node) = self.nodes.get_mut(id.0 as usize) else {
            return false;
        };
        match &mut node.kind {
            crate::NodeKind::Part(part) => part.deform_stack.reset(),
            crate::NodeKind::MeshGroup(mesh_group) => mesh_group.deform_stack.reset(),
            _ => return false,
        }
        true
    }

    fn bump_node_revision(&mut self) {
        self.node_revision = self.node_revision.wrapping_add(1);
    }

    fn invalidate_pose_memos(&mut self) {
        self.last_tick_folded_param_generation = None;
        self.last_tick_mesh_group_generation = None;
        self.last_anchor_pose_generation = None;
        self.physics_anchor_skip_cached = None;
        self.physics_ancestor_mask = None;
    }

    fn rebuild_node_kind_state(&mut self, old_physics: &[NodeIdx]) {
        self.deform_node_ids.clear();
        self.physics_node_ids.clear();
        self.mesh_group_node_ids.clear();
        for (slot, node) in self.nodes.iter().enumerate() {
            let id = NodeIdx::new(slot as u32);
            if matches!(
                &node.kind,
                crate::NodeKind::Part(_) | crate::NodeKind::MeshGroup(_)
            ) {
                self.deform_node_ids.push(id);
            }
            if matches!(&node.kind, crate::NodeKind::SimplePhysics(_)) {
                self.physics_node_ids.push(id);
            }
            if matches!(&node.kind, crate::NodeKind::MeshGroup(_)) {
                self.mesh_group_node_ids.push(id);
            }
        }

        let mut retired = smallvec::SmallVec::<[u32; 4]>::new();
        let nodes = &self.nodes;
        self.param_contributions.retain(|entry| {
            if !old_physics.contains(&entry.source) {
                return true;
            }
            let still_targets_uuid =
                nodes
                    .get(entry.source.0 as usize)
                    .and_then(|node| match &node.kind {
                        crate::NodeKind::SimplePhysics(physics) => physics.target_param_id,
                        _ => None,
                    })
                    == Some(entry.uuid);
            if !still_targets_uuid {
                retired.push(entry.uuid);
            }
            still_targets_uuid
        });
        self.param_contributed.fill(false);
        for entry in &self.param_contributions {
            if let Some(&idx) = self.param_id_to_index.get(&entry.uuid) {
                if let Some(flag) = self.param_contributed.get_mut(idx) {
                    *flag = true;
                }
            }
        }
        for uuid in retired {
            self.bump_param_generation_for_id(uuid);
        }

        self.mg_pre_order_cache = None;
        self.rebuild_param_effect_cache();
        self.rebuild_all_mesh_group_attachments();
        self.invalidate_pose_memos();
        self.bump_node_revision();
    }

    fn rebuild_all_mesh_group_attachments(&mut self) {
        self.reset_dynamic_state();
        self.reset_deforms();
        if self.mesh_group_node_ids.is_empty() {
            return;
        }
        let mut transforms = GlobalTransforms::new();
        self.compute_transforms(&mut transforms);
        let baked: Vec<_> = self
            .mesh_group_node_ids
            .iter()
            .map(|&id| {
                let attachments =
                    crate::meshgroup::bake_mesh_group_attachments(self, &transforms, id);
                let bitmap = self
                    .nodes
                    .get(id.0 as usize)
                    .and_then(|node| match &node.kind {
                        crate::NodeKind::MeshGroup(mesh_group) => {
                            crate::meshgroup::MgTriangleBitmap::build(&mesh_group.mesh)
                        }
                        _ => None,
                    });
                (id, attachments, bitmap)
            })
            .collect();
        for (id, attachments, bitmap) in baked {
            if let Some(crate::NodeKind::MeshGroup(mesh_group)) =
                self.nodes.get_mut(id.0 as usize).map(|node| &mut node.kind)
            {
                mesh_group.attachments = attachments;
                mesh_group.bitmap = bitmap;
            }
        }
    }

    pub(crate) fn mark_transform_dirty(&mut self, id: NodeIdx) {
        if let Some(d) = self.node_transform_dirty.get_mut(id.0 as usize) {
            *d = true;
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(slot, node)| (NodeIdx::new(slot as u32), node))
    }

    pub fn iter_deform_nodes(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.deform_node_ids
            .iter()
            .filter_map(|&id| self.nodes.get(id.0 as usize).map(|node| (id, node)))
    }

    /// Restore every node's transform / z_order to its loader-parsed
    /// baseline. Call at the start of a frame before `apply_params`
    /// so param-driven deltas don't accumulate across frames.
    pub fn reset_dynamic_state(&mut self) {
        let _span = tracing::trace_span!("reset_dynamic_state").entered();
        self.last_tick_folded_param_generation = None;
        // Restoring node.transform to base drops the cached anchor pose.
        self.last_anchor_pose_generation = None;
        for d in self.node_transform_dirty.iter_mut() {
            *d = false;
        }
        for node in self.nodes.iter_mut() {
            node.transform = node.base_transform;
            node.z_order = node.base_z_order;
            match &mut node.kind {
                crate::NodeKind::Part(p) => {
                    p.opacity = p.base_opacity;
                    p.tint = p.base_tint;
                    p.screen_tint = p.base_screen_tint;
                }
                crate::NodeKind::Composite(c) => {
                    c.opacity = c.base_opacity;
                    c.tint = c.base_tint;
                    c.screen_tint = c.base_screen_tint;
                }
                crate::NodeKind::SimplePhysics(p) => {
                    p.offset_output_scale = glam::Vec2::ONE;
                }
                _ => {}
            }
        }
    }

    /// Clear every DeformStack on Part / MeshGroup nodes. Call at the
    /// start of a frame before any sources push their contributions.
    pub fn reset_deforms(&mut self) {
        let _span = tracing::trace_span!("reset_deforms").entered();
        self.last_tick_folded_param_generation = None;
        for &id in &self.deform_node_ids {
            if let Some(node) = self.nodes.get_mut(id.0 as usize) {
                match &mut node.kind {
                    crate::NodeKind::Part(p) => p.deform_stack.reset(),
                    crate::NodeKind::MeshGroup(mg) => mg.deform_stack.reset(),
                    _ => {}
                }
            }
        }
    }

    /// Advance every SimplePhysics node by `dt` seconds. Uses each
    /// node's global translation (from the last compute_transforms)
    /// as the anchor; callers who need physics to react to animated
    /// parents should call compute_transforms before tick_physics.
    ///
    /// After stepping, any SimplePhysics node with a target_param_id
    /// writes its current `param_value()` into the puppet's
    /// param_values map. A subsequent `apply_params` call then folds
    /// that value into target node deforms / transforms.
    pub fn tick_physics(&mut self, transforms: &crate::puppet::GlobalTransforms, dt: f32) -> bool {
        let _span = tracing::trace_span!("tick_physics").entered();
        for i in 0..self.physics_node_ids.len() {
            let id = self.physics_node_ids[i];
            let Some(anchor) = self.physics_anchor(transforms, id) else {
                continue;
            };
            if let Some(crate::NodeKind::SimplePhysics(p)) =
                self.nodes.get_mut(id.0 as usize).map(|n| &mut n.kind)
            {
                p.tick(anchor, dt);
            }
        }
        self.write_physics_param_outputs(transforms)
    }

    /// Bring every physics driver to its analytic rest pose without
    /// simulating, so a freshly-loaded or re-posed rig renders settled on
    /// its first frame instead of visibly swinging into place.
    ///
    /// One driver's output can position another's anchor, so a single pass
    /// is not enough: each pass settles every driver against the current
    /// anchor pose, then re-folds the anchor bindings so those outputs
    /// propagate. Each driver's rest state is a fixed point of
    /// "anchor -> param value -> transforms -> anchor", so an acyclic
    /// driver graph converges in at most `physics_node_ids.len()` passes;
    /// the extra pass is what observes the fixed point.
    ///
    /// Leaves the puppet in the anchor pose, with deform stacks cleared and
    /// opacity/tint at base — `tick` is what folds a renderable pose, and
    /// the resets here force it to. Render only after ticking.
    pub fn settle_physics(&mut self) {
        let _span = tracing::trace_span!("settle_physics").entered();
        let n = self.physics_node_ids.len();
        // Settling writes driver outputs, and a frozen puppet never ticks
        // physics again to refresh or retire them — they would sit on the
        // authored pose forever, which is the override `set_physics_enabled`
        // exists to remove.
        if n == 0 || !self.physics_enabled {
            return;
        }
        self.ensure_physics_ancestor_mask();
        let mut transforms = std::mem::take(&mut self.physics_transforms);
        let mut settled = false;

        for _ in 0..=n {
            self.reset_dynamic_state();
            self.reset_deforms();
            self.apply_anchor_transform_bindings();
            self.compute_physics_ancestor_transforms(&mut transforms);

            let mut moved = false;
            for i in 0..n {
                let id = self.physics_node_ids[i];
                let Some(anchor) = self.physics_anchor(&transforms, id) else {
                    continue;
                };
                if let Some(crate::NodeKind::SimplePhysics(p)) =
                    self.nodes.get_mut(id.0 as usize).map(|n| &mut n.kind)
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
        self.physics_transforms = transforms;

        if !settled {
            tracing::warn!(
                "physics failed to settle in {} passes; drivers likely form a \
                 dependency cycle. Leaving the last iterate in place.",
                n + 1
            );
        }
    }

    /// Anchor point for a physics driver in the driver's own **Y-down**
    /// frame (gravity toward +Y, matching the reference pendulum). The
    /// node world is Y-up, so the Y is flipped here;
    /// `write_physics_param_outputs` conjugates `world_inverse` by the same
    /// flip to undo it on the output.
    ///
    /// The `local_only` branch uses `node.transform.translation`, including
    /// parameter-driven offsets from the anchor pre-pass rather than the
    /// frozen load pose.
    fn physics_anchor(
        &self,
        transforms: &crate::puppet::GlobalTransforms,
        id: NodeIdx,
    ) -> Option<crate::Vec2> {
        let node = self.nodes.get(id.0 as usize)?;
        let crate::NodeKind::SimplePhysics(p) = &node.kind else {
            return None;
        };
        let anchor = if p.local_only {
            crate::Vec2::new(node.transform.translation.x, node.transform.translation.y)
        } else {
            let world = transforms.get(id);
            crate::Vec2::new(world.w_axis.x, world.w_axis.y)
        };
        Some(crate::Vec2::new(anchor.x, -anchor.y))
    }

    /// Map every driver's current state through its `map_mode` and
    /// contribute the result to its target param. Returns whether any
    /// resolved param value moved.
    ///
    /// Drivers contribute at full authority: a lone driver fully
    /// determines its target, and two drivers aimed at one param average
    /// rather than resolving by their position in `physics_node_ids`,
    /// which is insertion order and carries no meaning.
    fn write_physics_param_outputs(
        &mut self,
        transforms: &crate::puppet::GlobalTransforms,
    ) -> bool {
        self.physics_update_scratch.clear();
        for i in 0..self.physics_node_ids.len() {
            let id = self.physics_node_ids[i];
            let update = match self.nodes.get(id.0 as usize).map(|n| &n.kind) {
                Some(crate::NodeKind::SimplePhysics(p)) => p.target_param_id.and_then(|u| {
                    // local_only=true: bob was integrated in the parent's frame
                    // already (anchor is local), so no inverse transform is
                    // needed. Otherwise inverse the
                    // node's puppet-local world matrix to rotate the displacement
                    // back into the node's local frame.
                    let world_inverse = if p.local_only {
                        Some(Mat4::IDENTITY)
                    } else {
                        crate::components::checked_affine_inverse(transforms.get(id))
                    }?;
                    // Conjugate by the Y-flip so the Y-down integrator's bob
                    // displacement rotates back through the Y-up node frame —
                    // the matching half of the tick() flip above.
                    let flip = Mat4::from_scale(glam::Vec3::new(1.0, -1.0, 1.0));
                    Some((u, id, p.param_value(flip * world_inverse * flip)))
                }),
                _ => None,
            };
            if let Some(update) = update {
                self.physics_update_scratch.push(update);
            }
        }
        let mut changed = false;
        if self.retire_stale_driver_contributions() {
            changed = true;
        }
        for i in 0..self.physics_update_scratch.len() {
            let (uuid, source, val) = self.physics_update_scratch[i];
            if self.contribute_param_value(uuid, source, val, 1.0) {
                changed = true;
            }
        }
        changed
    }

    /// Drop contributions from drivers that no longer produce one — a node
    /// whose `target_param_id` was retargeted or cleared.
    ///
    /// A stale entry keeps full authority rather than merely lingering:
    /// resolution is a mean, so a frozen contribution alongside a live one
    /// pulls the parameter to the midpoint of the two instead of tracking
    /// the driver that is still running. `physics_update_scratch` holds
    /// every live driver output, so anything sourced from a physics node
    /// and absent from it is dead.
    fn retire_stale_driver_contributions(&mut self) -> bool {
        let mut entries = std::mem::take(&mut self.param_contributions);
        let before = entries.len();
        let mut retired: smallvec::SmallVec<[u32; 4]> = smallvec::SmallVec::new();
        entries.retain(|e| {
            let from_driver = self.physics_node_ids.contains(&e.source);
            let live = self
                .physics_update_scratch
                .iter()
                .any(|(uuid, source, _)| *uuid == e.uuid && *source == e.source);
            if from_driver && !live {
                retired.push(e.uuid);
                return false;
            }
            true
        });
        self.param_contributions = entries;
        if before == self.param_contributions.len() {
            return false;
        }
        for uuid in retired {
            if let Some(&idx) = self.param_id_to_index.get(&uuid) {
                if let Some(flag) = self.param_contributed.get_mut(idx) {
                    *flag = self.param_contributions.iter().any(|e| e.uuid == uuid);
                }
            }
            self.bump_param_generation_for_id(uuid);
        }
        true
    }

    pub fn params(&self) -> &[crate::params::Param] {
        &self.params
    }

    /// Param UUIDs must be unique because `param_id_to_index` maps each UUID
    /// to one dense slot. Callers must normalize duplicates before this point.
    pub fn set_params(&mut self, params: Vec<crate::params::Param>) {
        // Dense values parallel to `params`, seeded at each param's
        // defaults. First entry wins deterministically on name / UUID collisions.
        self.param_values = params.iter().map(|p| Some(p.defaults)).collect();
        self.param_values_overflow.clear();
        self.param_index = HashMap::with_capacity(params.len());
        self.param_id_to_index = HashMap::with_capacity(params.len());
        for (i, p) in params.iter().enumerate() {
            self.param_index.entry(p.name.clone()).or_insert(i);
            self.param_id_to_index.entry(p.id).or_insert(i);
        }
        // A new param list re-seeds every base, so a contribution has
        // nothing left to blend against — and one naming a uuid the new list
        // drops would answer `param_value` forever.
        self.param_contributions.clear();
        self.param_contributed = vec![false; params.len()];
        self.params = params;
        self.rebuild_param_effect_cache();
        self.bump_param_generation();
        self.last_tick_folded_param_generation = None;
        self.last_tick_mesh_group_generation = None;
        self.last_anchor_pose_generation = None;
    }

    pub fn param_by_name(&self, name: &str) -> Option<&crate::params::Param> {
        self.param_index.get(name).and_then(|&i| self.params.get(i))
    }

    /// Commit `uuid`'s base value. Equivalent to a full-authority
    /// contribution whenever nothing else writes the same parameter, which
    /// is every parameter on a rig with no driver targeting it. Where a
    /// driver *does* contribute, this is the value the driver's weight
    /// blends against — see [`resolve_contributions`].
    pub fn set_param_value(&mut self, uuid: u32, val: glam::Vec2) {
        self.set_param_value_inner(uuid, val);
    }

    /// Record `source`'s weighted opinion about `uuid`, replacing that
    /// source's previous one. Returns whether the resolved value moved.
    ///
    /// Keying by source is what lets an entry be *replaced* rather than
    /// the table cleared wholesale each frame: contributions have to
    /// outlive the frame that wrote them, because the physics pre-pass
    /// poses the next frame's anchors from the previous frame's driver
    /// outputs.
    pub fn contribute_param_value(
        &mut self,
        uuid: u32,
        source: NodeIdx,
        value: glam::Vec2,
        weight: f32,
    ) -> bool {
        let before = self.param_value(uuid);
        match self
            .param_contributions
            .iter_mut()
            .find(|e| e.uuid == uuid && e.source == source)
        {
            Some(e) => e.contribution = ParamContribution { value, weight },
            None => {
                self.param_contributions.push(ParamContributionEntry {
                    uuid,
                    source,
                    contribution: ParamContribution { value, weight },
                });
                if let Some(&idx) = self.param_id_to_index.get(&uuid) {
                    if let Some(flag) = self.param_contributed.get_mut(idx) {
                        *flag = true;
                    }
                }
            }
        }
        if before == self.param_value(uuid) {
            return false;
        }
        self.bump_param_generation_for_id(uuid);
        true
    }

    /// Drop every contribution, restoring each parameter to its committed
    /// base value. Freezing physics does this, so a driver's last output
    /// can't keep overriding the authored pose once nothing refreshes it.
    pub fn clear_param_contributions(&mut self) {
        if self.param_contributions.is_empty() {
            return;
        }
        for e in std::mem::take(&mut self.param_contributions) {
            self.bump_param_generation_for_id(e.uuid);
        }
        for flag in self.param_contributed.iter_mut() {
            *flag = false;
        }
    }

    pub fn clear_param_value(&mut self, uuid: u32) {
        let cleared = match self.param_id_to_index.get(&uuid) {
            Some(&idx) => self
                .param_values
                .get_mut(idx)
                .and_then(|slot| slot.take())
                .is_some(),
            None => self.param_values_overflow.remove(&uuid).is_some(),
        };
        if cleared {
            self.bump_param_generation_for_id(uuid);
        }
    }

    pub fn set_param_value_by_name(&mut self, name: &str, val: glam::Vec2) -> bool {
        if let Some(uuid) = self
            .param_index
            .get(name)
            .and_then(|&i| self.params.get(i))
            .map(|p| p.id)
        {
            self.set_param_value_inner(uuid, val);
            true
        } else {
            false
        }
    }

    /// Restore every param to its authored default, then apply `pose` as
    /// a sparse overlay. Preview/viewport callers pass only the params
    /// they currently control; without the reset, a previous overlay
    /// value would stick on the session-cached puppet.
    pub fn apply_pose_overlay<S: AsRef<str>>(&mut self, pose: &[(S, glam::Vec2)]) {
        let defaults: Vec<(u32, glam::Vec2)> =
            self.params.iter().map(|p| (p.id, p.defaults)).collect();
        for (uuid, val) in defaults {
            self.set_param_value(uuid, val);
        }
        for (name, val) in pose {
            self.set_param_value_by_name(name.as_ref(), *val);
        }
    }

    /// The parameter's effective value: the committed base folded with any
    /// weighted contributions. `None` only when nothing has written it and
    /// nothing contributes to it, so the caller falls back to the param's
    /// own defaults exactly as `apply_params` does.
    pub fn param_value(&self, uuid: u32) -> Option<glam::Vec2> {
        let index = self.param_id_to_index.get(&uuid).copied();
        let base = match index {
            Some(idx) => self.param_values.get(idx).copied().flatten(),
            None => self.param_values_overflow.get(&uuid).copied(),
        };
        if !self.param_contributions.iter().any(|e| e.uuid == uuid) {
            return base;
        }
        let defaults = index
            .and_then(|i| self.params.get(i))
            .map(|p| p.defaults)
            .unwrap_or(glam::Vec2::ZERO);
        Some(resolve_contributions(
            &self.param_contributions,
            uuid,
            base.unwrap_or(defaults),
        ))
    }

    fn set_param_value_inner(&mut self, uuid: u32, val: glam::Vec2) -> bool {
        match self.param_id_to_index.get(&uuid) {
            Some(&idx) => {
                if self.param_values.get(idx).copied().flatten() == Some(val) {
                    return false;
                }
                if let Some(slot) = self.param_values.get_mut(idx) {
                    *slot = Some(val);
                }
            }
            None => {
                if self.param_values_overflow.get(&uuid).copied() == Some(val) {
                    return false;
                }
                self.param_values_overflow.insert(uuid, val);
            }
        }
        self.bump_param_generation_for_id(uuid);
        true
    }

    fn bump_param_generation(&mut self) {
        self.param_generation = self.param_generation.wrapping_add(1);
    }

    fn bump_param_generation_for_id(&mut self, uuid: u32) {
        self.bump_param_generation();
        if self.param_mesh_group_relevant.contains(&uuid) {
            self.mesh_group_param_generation = self.mesh_group_param_generation.wrapping_add(1);
        }
    }

    fn rebuild_param_effect_cache(&mut self) {
        self.param_mesh_group_relevant.clear();
        if self.mesh_group_node_ids.is_empty() {
            return;
        }

        let mut related = HashSet::new();
        for &mg_id in &self.mesh_group_node_ids {
            related.insert(mg_id);
            related.extend(self.tree.get_all_descendants(mg_id));
        }

        for param in &self.params {
            let relevant = param.bindings.iter().any(|binding| {
                related.contains(&binding.node)
                    && matches!(
                        &binding.values,
                        crate::params::BindingValues::Deform(_)
                            | crate::params::BindingValues::TransformTX(_)
                            | crate::params::BindingValues::TransformTY(_)
                            | crate::params::BindingValues::TransformSX(_)
                            | crate::params::BindingValues::TransformSY(_)
                            | crate::params::BindingValues::TransformRX(_)
                            | crate::params::BindingValues::TransformRY(_)
                            | crate::params::BindingValues::TransformRZ(_)
                    )
            });
            if relevant {
                self.param_mesh_group_relevant.insert(param.id);
            }
        }
    }

    pub fn animations(&self) -> &[crate::animation::Animation] {
        &self.animations
    }

    pub fn set_animations(&mut self, animations: Vec<crate::animation::Animation>) {
        self.animations = animations;
        // Any play state indexes into the old list.
        self.play_state = None;
    }

    pub fn has_playing_animation(&self) -> bool {
        self.play_state.is_some()
    }

    pub fn has_simple_physics(&self) -> bool {
        !self.physics_node_ids.is_empty()
    }

    pub fn set_physics_enabled(&mut self, enabled: bool) {
        self.physics_enabled = enabled;
        if !enabled {
            // Otherwise each driver's last output would keep overriding the
            // authored pose for as long as physics stays frozen.
            self.clear_param_contributions();
        }
    }

    /// Start playing the animation with the given name. Returns false
    /// if no matching animation exists. Looping defaults to true.
    pub fn play_animation(&mut self, name: &str) -> bool {
        if let Some(i) = self.animations.iter().position(|a| a.name == name) {
            self.play_state = Some(crate::animation::AnimationPlayState {
                index: i,
                time: 0.0,
                looping: true,
            });
            true
        } else {
            false
        }
    }

    pub fn stop_animation(&mut self) {
        self.play_state = None;
    }

    /// Advance animation playback by `dt` seconds and write the
    /// interpolated value of every lane into the matching
    /// `param_values` entry. Axis 0 writes x, axis 1 writes y;
    /// the other axis is left untouched (so two lanes targeting
    /// different axes of the same param compose correctly).
    pub fn tick_animations(&mut self, dt: f32) -> bool {
        let _span = tracing::trace_span!("tick_animations").entered();
        if !dt.is_finite() {
            return false;
        }
        let Some(mut state) = self.play_state else {
            return false;
        };
        let Some(anim) = self.animations.get(state.index) else {
            self.play_state = None;
            return false;
        };
        state.time += dt;
        // A looping animation snaps back to the loop region's start (the lead-in plays
        // once), and playback always clamps to the last frame.
        if anim.length > 0 && anim.timestep > 0.0 {
            let (loop_begin, loop_end) = anim.loop_region();
            let frame = (state.time / anim.timestep).round() as i64;
            if state.looping && frame >= loop_end as i64 {
                state.time = loop_begin as f32 * anim.timestep;
            }
            let frame = (state.time / anim.timestep).round() as i64;
            if frame + 1 >= anim.length as i64 {
                state.time = (anim.length - 1) as f32 * anim.timestep;
            }
        }
        let frame = if anim.timestep > 0.0 {
            state.time / anim.timestep
        } else {
            0.0
        };
        let mut changed = false;
        let mut changed_uuids: smallvec::SmallVec<[u32; 8]> = smallvec::SmallVec::new();
        for lane in &anim.lanes {
            let v = lane.value_at(frame);
            // Direct field access keeps the borrows disjoint from `anim`
            // (a `&self.animations`); a `&mut self` setter can't be called
            // here. Generation bumps are deferred to after the loop.
            let idx = self.param_id_to_index.get(&lane.param_id).copied();
            // Fall back to the param's own defaults, matching what
            // `apply_params_where` uses for an unwritten slot. Falling back to
            // ZERO instead would let a lane on one axis silently snap the
            // other axis away from its rest value.
            let defaults = idx
                .and_then(|i| self.params.get(i))
                .map(|p| p.defaults)
                .unwrap_or(glam::Vec2::ZERO);
            let current = match idx {
                Some(i) => self.param_values.get(i).copied().flatten(),
                None => self.param_values_overflow.get(&lane.param_id).copied(),
            }
            .unwrap_or(defaults);
            let next = match lane.axis {
                crate::params::ParamAxis::X => glam::Vec2::new(v, current.y),
                crate::params::ParamAxis::Y => glam::Vec2::new(current.x, v),
            };
            if next != current {
                match idx {
                    Some(i) => {
                        if let Some(slot) = self.param_values.get_mut(i) {
                            *slot = Some(next);
                        }
                    }
                    None => {
                        self.param_values_overflow.insert(lane.param_id, next);
                    }
                }
                changed_uuids.push(lane.param_id);
                changed = true;
            }
        }
        self.play_state = Some(state);
        for uuid in changed_uuids {
            self.bump_param_generation_for_id(uuid);
        }
        changed
    }

    /// Apply every param at its current value, pushing contributions
    /// into the target DeformStacks. Call after `reset_deforms` and
    /// before `combine_deforms` each frame.
    pub fn apply_params(&mut self) {
        let _span = tracing::trace_span!("apply_params").entered();
        self.apply_params_where(|_| true);
    }

    /// Physics pre-pass: build the anchor pose. tick_physics only reads
    /// node transforms (the anchors) and each driver's offset_output_scale,
    /// so restrict the pass to the binding kinds feeding those.
    /// Deform/Opacity/Tint contributions would be wiped by the resets
    /// before the final apply anyway.
    ///
    /// Driver-output params are included at their *last-frame* values
    /// (they persist in `param_values`), so a driver whose output
    /// transform-binds another driver's anchor moves it — this is what
    /// makes chained physics work (the reference rig's segmented hair: `Physics - Head
    /// (Back)` translates the `Head Back Lower (Physics)` node's anchor).
    /// The two-phase pipeline applies every driver's last-frame output
    /// uniformly, so chained drivers couple with a one-frame delay.
    fn apply_anchor_transform_bindings(&mut self) {
        use crate::params::BindingValues as B;
        let _span = tracing::trace_span!("apply_anchor_transform_bindings").entered();
        self.apply_params_where(|v| {
            matches!(
                v,
                B::TransformTX(_)
                    | B::TransformTY(_)
                    | B::TransformSX(_)
                    | B::TransformSY(_)
                    | B::TransformRX(_)
                    | B::TransformRY(_)
                    | B::TransformRZ(_)
                    | B::OutputScaleX(_)
                    | B::OutputScaleY(_)
            )
        });
    }

    fn apply_params_where(
        &mut self,
        binding_include: impl Fn(&crate::params::BindingValues) -> bool + Copy,
    ) {
        // Move params out temporarily so apply() can take &mut Puppet.
        // `param_values` stays parallel to `params`, so index positionally
        // — no per-param hash on this twice-a-frame hot path.
        let params = std::mem::take(&mut self.params);
        for (i, p) in params.iter().enumerate() {
            let base = self
                .param_values
                .get(i)
                .copied()
                .flatten()
                .unwrap_or(p.defaults);
            let val = if self.param_contributed.get(i).copied().unwrap_or(false) {
                resolve_contributions(&self.param_contributions, p.id, base)
            } else {
                base
            };
            p.apply_filtered(val, self, binding_include);
        }
        self.params = params;
    }

    /// Collapse each DeformStack's active sources into its combined
    /// per-vertex offset. Idempotent and cheap when nothing is dirty.
    pub fn combine_deforms(&mut self) {
        let _span = tracing::trace_span!("combine_deforms").entered();
        for &id in &self.deform_node_ids {
            if let Some(node) = self.nodes.get_mut(id.0 as usize) {
                match &mut node.kind {
                    crate::NodeKind::Part(p) => p.deform_stack.combine(),
                    crate::NodeKind::MeshGroup(mg) => mg.deform_stack.combine(),
                    _ => {}
                }
            }
        }
    }

    pub fn propagate_mesh_group_deforms(&mut self, transforms: &GlobalTransforms) {
        let _span = tracing::trace_span!("propagate_mesh_group_deforms").entered();
        crate::meshgroup::propagate_mesh_group_deforms(self, transforms);
    }

    pub fn welds(&self) -> &[crate::weld::Weld] {
        &self.welds
    }

    pub fn set_welds(&mut self, welds: Vec<crate::weld::Weld>) {
        self.welds = welds;
        // Welds change combined deforms, so a memoized static frame is stale.
        self.last_tick_folded_param_generation = None;
    }

    /// Solve every weld into the parts' `DeformSource::Weld` slots. Call
    /// after `propagate_mesh_group_deforms` and before `combine_deforms`.
    pub fn apply_welds(&mut self, transforms: &GlobalTransforms) {
        crate::weld::apply_welds(self, transforms);
    }

    /// Run the `translateChildren=true` MG filter on each tc=true
    /// MG's non-Drawable descendants (Origin Nodes, Empty Nodes,
    /// SimplePhysics nodes). Adds the MG's per-vertex warp at each
    /// target's base position into the target's
    /// `transform.translation`. Mark-dirty is performed so the next
    /// `compute_transforms` pass picks up the change.
    ///
    /// Applies a `translateChildren=true` MG's warp to descendant Origin
    /// nodes. Without this,
    /// the Origin Nodes only move via direct param-bound
    /// `transform.t.x/t.y` bindings, which don't fully replicate
    /// the head-rotation effect at e.g. a `Head:: Yaw-Pitch=(1,1)`.
    ///
    /// Returns whether any target was shifted; when false the caller's
    /// transforms are still valid and the re-walk can be skipped.
    pub fn apply_translate_children_filter(&mut self, transforms: &GlobalTransforms) -> bool {
        let _span = tracing::trace_span!("apply_translate_children_filter").entered();
        crate::meshgroup::apply_translate_children_filter(self, transforms)
    }

    /// NodeIdx for the given UUID, if any.
    pub fn node_for_uuid(&self, uuid: u32) -> Option<NodeIdx> {
        self.uuid_to_node.get(&uuid).copied()
    }

    /// Allocate a new id, attach as a child of `parent`, install `node`, and
    /// optionally register a UUID → NodeIdx mapping (first-wins dedup).
    ///
    /// This is the only supported way to grow the puppet, so the three
    /// backing stores (nodes / tree / uuid_to_node) stay in sync.
    pub fn insert_child(&mut self, parent: NodeIdx, node: Node, uuid: Option<u32>) -> NodeIdx {
        let id = self.allocate_id();
        // An unresolvable parent would leave the node registered in every side
        // index (deform / mesh-group / physics) but absent from the tree, so it
        // never renders while its physics driver still perturbs the rig.
        if let Err(err) = self.tree.add_child(parent, id) {
            tracing::warn!(
                "insert_child: {err:?}; node {} attached to the root instead",
                id.0
            );
            let root = self.tree.root;
            let _ = self.tree.add_child(root, id);
        }
        debug_assert_eq!(
            id.0 as usize,
            self.nodes.len(),
            "NodeIdx must equal slot index in dense storage"
        );
        let is_deform_node = matches!(
            &node.kind,
            crate::NodeKind::Part(_) | crate::NodeKind::MeshGroup(_)
        );
        let is_mesh_group = matches!(&node.kind, crate::NodeKind::MeshGroup(_));
        let is_physics = matches!(&node.kind, crate::NodeKind::SimplePhysics(_));
        self.base_local_matrix.push(node.base_transform.to_matrix());
        self.node_transform_dirty.push(false);
        self.nodes.push(node);
        if is_deform_node {
            self.deform_node_ids.push(id);
        }
        if is_mesh_group {
            self.mesh_group_node_ids.push(id);
        }
        if is_physics {
            self.physics_node_ids.push(id);
        }
        self.mg_pre_order_cache = None;
        // A new node changes the physics ancestor set and the tc-target
        // guard, and invalidates any cached anchor pose.
        self.physics_ancestor_mask = None;
        self.physics_anchor_skip_cached = None;
        self.last_anchor_pose_generation = None;
        if is_mesh_group && !self.params.is_empty() {
            self.rebuild_param_effect_cache();
            self.last_tick_mesh_group_generation = None;
        }
        self.last_tick_folded_param_generation = None;
        if let Some(u) = uuid {
            use std::collections::hash_map::Entry;
            match self.uuid_to_node.entry(u) {
                Entry::Vacant(e) => {
                    e.insert(id);
                }
                Entry::Occupied(_) => {
                    tracing::warn!("duplicate UUID {} in model, ignoring later occurrence", u);
                }
            }
        }
        self.bump_node_revision();
        id
    }

    fn allocate_id(&mut self) -> NodeIdx {
        let id = NodeIdx::new(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn compute_transforms(&self, out: &mut GlobalTransforms) {
        let _span = tracing::trace_span!("compute_transforms").entered();
        self.compute_transforms_with_root(out, Mat4::IDENTITY);
    }

    /// Canonical per-frame lifecycle: reset dynamic state, tick
    /// animations and physics, fold params, combine deforms, and
    /// recompute transforms. `out` carries last frame's transforms in
    /// and leaves this frame's transforms in when done. Physics samples
    /// the current non-driver param pose in puppet-local coords before
    /// writing its target params, independent of `root`.
    pub fn tick(&mut self, out: &mut GlobalTransforms, root: Mat4, dt: f32) {
        let _span = tracing::trace_span!("tick").entered();
        self.tick_animations(dt);
        let has_physics = self.physics_enabled && self.has_simple_physics();
        let mut pre_pass_ran = false;
        let mut anchor_generation = 0;
        if has_physics {
            // Rebuild the anchor pose only when params / driver outputs
            // moved since the cached pose, or when a local_only physics node
            // is a tc-filter target (see physics_anchor_skip_allowed).
            let stale = self.last_anchor_pose_generation != Some(self.param_generation)
                || !self.physics_anchor_skip_allowed();
            if stale {
                // Capture the generation BEFORE tick_physics bumps it: if a
                // driver moves this frame, next frame's staleness check then
                // forces the rebuild that feeds chained physics.
                anchor_generation = self.param_generation;
                pre_pass_ran = true;
                self.reset_dynamic_state();
                self.reset_deforms();
                self.apply_anchor_transform_bindings();
                self.ensure_physics_ancestor_mask();
                let mut local = std::mem::take(&mut self.physics_transforms);
                self.compute_physics_ancestor_transforms(&mut local);
                self.physics_transforms = local;
            }
            let local = std::mem::take(&mut self.physics_transforms);
            self.tick_physics(&local, dt);
            self.physics_transforms = local;
        }

        let params_changed = self.last_tick_folded_param_generation != Some(self.param_generation);
        let mesh_group_generation_changed =
            self.last_tick_mesh_group_generation != Some(self.mesh_group_param_generation);
        // A pre-pass reset opacity/tint and deactivated every DeformStack,
        // so the frame that ran it must run the final apply too — otherwise
        // it would render the unposed puppet.
        let needs_final_apply = params_changed || pre_pass_ran;
        let has_mesh_group_work = !self.mesh_group_node_ids.is_empty()
            && !self.param_mesh_group_relevant.is_empty()
            && (needs_final_apply || mesh_group_generation_changed);
        if needs_final_apply {
            self.reset_dynamic_state();
            self.reset_deforms();
            self.apply_params();
            // compute_transforms BEFORE propagate so MG/child relative
            // transforms reflect this frame's param-driven shifts (e.g.
            // Yaw-Pitch t.x/t.y on LIP MG, Mouth Shape t.y on Mouth Inner).
            self.compute_transforms_with_root(out, root);
            // Apply translateChildren=true filters from each tc MG to its
            // non-Drawable descendants (Origin Nodes etc.). Re-run
            // compute_transforms so descendants pick up the shifted
            // origins. Catchlight computes transforms eagerly, so re-walk
            // explicitly —
            // but only when the filter actually shifted something
            // (rigs without tc=true MGs never need the third walk).
            if has_mesh_group_work {
                if self.apply_translate_children_filter(out) {
                    self.compute_transforms_with_root(out, root);
                }
                self.propagate_mesh_group_deforms(out);
                self.last_tick_mesh_group_generation = Some(self.mesh_group_param_generation);
            }
            self.apply_welds(out);
            self.combine_deforms();
            self.last_tick_folded_param_generation = Some(self.param_generation);
        } else {
            self.compute_transforms_with_root(out, root);
        }
        if pre_pass_ran {
            // Set at the very end: the final apply's reset_dynamic_state
            // cleared this, and using the pre-tick generation is what lets a
            // moved driver force next frame's anchor rebuild.
            self.last_anchor_pose_generation = Some(anchor_generation);
        }
    }

    /// Fold `root` into the top-level transform of the puppet so it
    /// renders at an arbitrary world position. Used by bevy integration
    /// to apply the entity's `GlobalTransform` without re-pointing the
    /// shared camera uniform between puppets (which would hit the
    /// queue.write_buffer batching hazard documented in AGENTS.md).
    pub fn compute_transforms_with_root(&self, out: &mut GlobalTransforms, root: Mat4) {
        let _span = tracing::trace_span!("compute_transforms_with_root").entered();
        // Size the Vec once at frame start; each insert below is then a
        // direct index write. DFS covers every reachable node so we
        // don't need a post-walk clear to scrub stale slots.
        out.ensure_size(self.nodes.len());

        self.tree.with_dfs_order(|order| {
            for &id in order {
                let slot = id.0 as usize;
                let node = self.nodes.get(slot);
                // Skip Transform::to_matrix() if (a) apply_params didn't
                // write this frame and (b) the transform still matches
                // base (invariant preserved when no external code has
                // touched the Transform fields). A 36-byte equality
                // compare is cheaper than from_scale_rotation_translation
                // + Quat::from_euler.
                let local_matrix = if let Some(n) = node {
                    let dirty = self.node_transform_dirty.get(slot).copied().unwrap_or(true);
                    if !dirty && n.transform == n.base_transform {
                        self.base_local_matrix
                            .get(slot)
                            .copied()
                            .unwrap_or_else(|| n.transform.to_matrix())
                    } else {
                        n.transform.to_matrix()
                    }
                } else {
                    Mat4::IDENTITY
                };
                let lock_to_root = node.map(|n| n.lock_to_root).unwrap_or(false);

                let parent_matrix = if lock_to_root {
                    root
                } else {
                    self.tree
                        .get_parent(id)
                        .map(|parent| out.get(parent))
                        .unwrap_or(root)
                };

                let global_matrix = parent_matrix * local_matrix;
                out.insert(id, global_matrix);
            }
        });
    }

    /// True when the physics pre-pass may be skipped on a settled frame.
    ///
    /// A skip frame ticks physics against the cached `physics_transforms`
    /// and, for `local_only` anchors, against `node.transform.translation`
    /// — which by then holds the last final apply's pose, including any
    /// `translate_children` shift, whereas a fresh pre-pass would produce
    /// the pre-shift pose. A `local_only` physics node that is itself a
    /// tc-filter target would pop between the two, so when any such node
    /// exists the pre-pass runs every frame. World-anchored physics reads
    /// only the cached transforms and is unaffected.
    fn physics_anchor_skip_allowed(&mut self) -> bool {
        if let Some(cached) = self.physics_anchor_skip_cached {
            return cached;
        }
        let allowed = !self.any_local_physics_is_tc_target();
        self.physics_anchor_skip_cached = Some(allowed);
        allowed
    }

    /// Whether any `local_only` SimplePhysics node is a translate-children
    /// target of a `translate_children` MeshGroup. Mirrors the target walk
    /// in `meshgroup::translate_children_targets` (recurse through
    /// Part/Composite, stop at nested MeshGroups).
    fn any_local_physics_is_tc_target(&self) -> bool {
        for &mg_id in &self.mesh_group_node_ids {
            let tc = matches!(
                self.nodes.get(mg_id.0 as usize).map(|n| &n.kind),
                Some(crate::NodeKind::MeshGroup(mg)) if mg.translate_children
            );
            if !tc {
                continue;
            }
            let mut stack = self.tree.get_children(mg_id);
            while let Some(id) = stack.pop() {
                match self.nodes.get(id.0 as usize).map(|n| &n.kind) {
                    Some(crate::NodeKind::Part(_)) | Some(crate::NodeKind::Composite(_)) => {
                        stack.extend(self.tree.get_children(id));
                    }
                    Some(crate::NodeKind::MeshGroup(_)) => {}
                    Some(crate::NodeKind::SimplePhysics(p)) if p.local_only => return true,
                    _ => {}
                }
            }
        }
        false
    }

    fn ensure_physics_ancestor_mask(&mut self) {
        if self.physics_ancestor_mask.is_some() {
            return;
        }
        let mut mask = vec![false; self.nodes.len()];
        for &pid in &self.physics_node_ids {
            let mut cur = Some(pid);
            while let Some(id) = cur {
                match mask.get_mut(id.0 as usize) {
                    // Already marked -> its ancestors are marked too.
                    Some(true) => break,
                    Some(slot) => *slot = true,
                    None => break,
                }
                cur = self.tree.get_parent(id);
            }
        }
        self.physics_ancestor_mask = Some(mask);
    }

    /// Physics pre-pass transform walk restricted to physics nodes and
    /// their ancestors — the only slots `tick_physics` reads (both the
    /// anchor and `world_inverse`). Same per-node logic as
    /// `compute_transforms_with_root` with `root = IDENTITY`; a member's
    /// parent is always a member, so parent transforms are ready when
    /// needed, and stale non-member slots are never read.
    fn compute_physics_ancestor_transforms(&self, out: &mut GlobalTransforms) {
        out.ensure_size(self.nodes.len());
        let mask = self.physics_ancestor_mask.as_deref().unwrap_or(&[]);
        self.tree.with_dfs_order(|order| {
            for &id in order {
                let slot = id.0 as usize;
                if !mask.get(slot).copied().unwrap_or(false) {
                    continue;
                }
                let node = self.nodes.get(slot);
                let local_matrix = if let Some(n) = node {
                    let dirty = self.node_transform_dirty.get(slot).copied().unwrap_or(true);
                    if !dirty && n.transform == n.base_transform {
                        self.base_local_matrix
                            .get(slot)
                            .copied()
                            .unwrap_or_else(|| n.transform.to_matrix())
                    } else {
                        n.transform.to_matrix()
                    }
                } else {
                    Mat4::IDENTITY
                };
                let lock_to_root = node.map(|n| n.lock_to_root).unwrap_or(false);
                let parent_matrix = if lock_to_root {
                    Mat4::IDENTITY
                } else {
                    self.tree
                        .get_parent(id)
                        .map(|parent| out.get(parent))
                        .unwrap_or(Mat4::IDENTITY)
                };
                out.insert(id, parent_matrix * local_matrix);
            }
        });
    }
}

impl Default for Puppet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeKind;
    use glam::{Vec2, Vec3};

    #[test]
    fn create_puppet() {
        let puppet = Puppet::new();
        assert!(puppet.get(puppet.root()).is_some());
    }

    #[test]
    fn compute_transforms_single_node() {
        let mut puppet = Puppet::new();

        let root = puppet.root();
        if let Some(node) = puppet.get_mut(root) {
            node.transform.translation = Vec3::new(10.0, 20.0, 0.0);
        }

        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);

        let global = transforms.get(root);
        let pos = global.transform_point3(Vec3::ZERO);

        assert_eq!(pos, Vec3::new(10.0, 20.0, 0.0));
    }

    #[test]
    fn replacing_base_transform_refreshes_the_cached_matrix() {
        let mut puppet = Puppet::new();
        let root = puppet.root();
        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);
        assert_eq!(transforms.get(root), Mat4::IDENTITY);

        let authored = crate::Transform {
            translation: Vec3::new(12.0, -7.0, 0.0),
            ..Default::default()
        };
        assert!(puppet.set_node_base_transform(root, authored));
        puppet.compute_transforms(&mut transforms);

        assert_eq!(
            transforms.get(root).transform_point3(Vec3::ZERO),
            authored.translation
        );
    }

    #[test]
    fn replacing_base_transform_rebakes_mesh_group_attachments() {
        use crate::{deform::DeformStack, Mesh, MeshGroupData, MeshIndices, PartData};

        let mut puppet = Puppet::new();
        let group = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::MeshGroup(Box::new(MeshGroupData {
                    mesh: Mesh::new(
                        vec![
                            Vec2::new(0.0, 0.0),
                            Vec2::new(10.0, 0.0),
                            Vec2::new(10.0, 10.0),
                            Vec2::new(0.0, 10.0),
                        ],
                        vec![Vec2::ZERO; 4],
                        MeshIndices::U16(vec![0, 1, 2, 0, 2, 3]),
                        Vec2::ZERO,
                    ),
                    deform_stack: DeformStack::new(4),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let child = puppet.insert_child(
            group,
            Node {
                kind: NodeKind::Part(Box::new(PartData {
                    mesh: Mesh::new(
                        vec![Vec2::ZERO],
                        vec![Vec2::ZERO],
                        MeshIndices::U16(Vec::new()),
                        Vec2::ZERO,
                    ),
                    deform_stack: DeformStack::new(1),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let transform_at = |x| crate::Transform {
            translation: Vec3::new(x, 2.0, 0.0),
            ..Default::default()
        };
        let weights = |puppet: &Puppet| {
            let NodeKind::MeshGroup(group) = &puppet.get(group).expect("mesh group").kind else {
                panic!("not a mesh group");
            };
            group.attachments.per_child[&child].vertices[0].weights
        };

        assert!(puppet.set_node_base_transform(child, transform_at(3.0)));
        let before = weights(&puppet);
        assert!(puppet.set_node_base_transform(child, transform_at(8.0)));

        assert_ne!(weights(&puppet), before);
    }

    #[test]
    fn replacing_node_kind_rebuilds_kind_registries() {
        use crate::{deform::DeformStack, PartData};

        let mut puppet = Puppet::new();
        let id = puppet.insert_child(puppet.root(), Node::default(), None);
        assert!(puppet.iter_deform_nodes().all(|(node_id, _)| node_id != id));
        assert!(!puppet.has_simple_physics());

        assert!(puppet.set_node_kind(
            id,
            NodeKind::Part(Box::new(PartData {
                deform_stack: DeformStack::new(1),
                ..Default::default()
            }))
        ));
        assert!(puppet.iter_deform_nodes().any(|(node_id, _)| node_id == id));

        assert!(puppet.set_node_kind(id, NodeKind::SimplePhysics(Box::default())));
        assert!(puppet.iter_deform_nodes().all(|(node_id, _)| node_id != id));
        assert!(puppet.has_simple_physics());

        assert!(puppet.set_node_kind(id, NodeKind::Empty));
        assert!(!puppet.has_simple_physics());
    }

    #[test]
    fn replacing_physics_kind_retires_its_param_contribution() {
        let mut puppet = Puppet::new();
        let target_uuid = 77;
        puppet.set_param_value(target_uuid, Vec2::ZERO);
        let physics = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(crate::physics::SimplePhysicsData {
                    target_param_id: Some(target_uuid),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        assert!(puppet.contribute_param_value(target_uuid, physics, Vec2::ONE, 1.0));
        assert_eq!(puppet.param_value(target_uuid), Some(Vec2::ONE));

        assert!(puppet.set_node_kind(physics, NodeKind::Empty));

        assert_eq!(puppet.param_value(target_uuid), Some(Vec2::ZERO));
    }

    #[test]
    fn compute_transforms_hierarchy() {
        let mut puppet = Puppet::new();

        let root = puppet.root();
        if let Some(node) = puppet.get_mut(root) {
            node.transform.translation = Vec3::new(100.0, 0.0, 0.0);
        }

        let mut child_node = Node::default();
        child_node.transform.translation = Vec3::new(50.0, 0.0, 0.0);
        let child_id = puppet.insert_child(root, child_node, None);

        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);

        let child_global = transforms.get(child_id);
        let child_pos = child_global.transform_point3(Vec3::ZERO);

        assert_eq!(child_pos, Vec3::new(150.0, 0.0, 0.0));
    }

    #[test]
    fn compute_transforms_with_rotation_and_scale() {
        let mut puppet = Puppet::new();

        let root = puppet.root();
        if let Some(node) = puppet.get_mut(root) {
            node.transform.scale = Vec2::new(2.0, 2.0);
        }

        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);

        let global = transforms.get(root);
        let point = global.transform_point3(Vec3::new(10.0, 0.0, 0.0));

        assert_eq!(point.x, 20.0);
    }

    #[test]
    fn lock_to_root_skips_parent_chain() {
        use crate::Node;

        let mut puppet = Puppet::new();
        let root = puppet.root();

        let mut parent = Node::default();
        parent.transform.translation = Vec3::new(10.0, 0.0, 0.0);
        let parent_id = puppet.insert_child(root, parent, None);

        let mut child = Node::default();
        child.transform.translation = Vec3::new(0.0, 5.0, 0.0);
        child.lock_to_root = true;
        let child_id = puppet.insert_child(parent_id, child, None);

        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);

        let parent_pos = transforms.get(parent_id).transform_point3(Vec3::ZERO);
        assert_eq!(parent_pos, Vec3::new(10.0, 0.0, 0.0));

        let child_pos = transforms.get(child_id).transform_point3(Vec3::ZERO);
        assert_eq!(child_pos, Vec3::new(0.0, 5.0, 0.0));
    }

    #[test]
    fn lock_to_root_with_external_root_matrix() {
        use crate::Node;

        let mut puppet = Puppet::new();
        let root = puppet.root();

        let mut parent = Node::default();
        parent.transform.translation = Vec3::new(10.0, 0.0, 0.0);
        let parent_id = puppet.insert_child(root, parent, None);

        let mut child = Node::default();
        child.transform.translation = Vec3::new(0.0, 5.0, 0.0);
        child.lock_to_root = true;
        let child_id = puppet.insert_child(parent_id, child, None);

        let mut transforms = GlobalTransforms::new();
        let world_root = Mat4::from_translation(Vec3::new(100.0, 0.0, 0.0));
        puppet.compute_transforms_with_root(&mut transforms, world_root);

        let parent_pos = transforms.get(parent_id).transform_point3(Vec3::ZERO);
        assert_eq!(parent_pos, Vec3::new(110.0, 0.0, 0.0));

        let child_pos = transforms.get(child_id).transform_point3(Vec3::ZERO);
        assert_eq!(child_pos, Vec3::new(100.0, 5.0, 0.0));
    }

    #[test]
    fn apply_params_pushes_deform_into_target_part() {
        use crate::params::{Binding, BindingValues, DeformMatrix, InterpolateMode, Param};
        use crate::{Mesh, MeshIndices, Node, NodeKind, PartData};

        let mut puppet = Puppet::new();
        let mesh = Mesh::new(
            vec![Vec2::ZERO, Vec2::ZERO, Vec2::ZERO],
            vec![Vec2::ZERO; 3],
            MeshIndices::U16(vec![0, 1, 2]),
            Vec2::ZERO,
        );
        let part = PartData {
            deform_stack: crate::deform::DeformStack::new(3),
            mesh,
            ..Default::default()
        };
        let node = Node {
            kind: NodeKind::Part(Box::new(part)),
            ..Default::default()
        };
        let part_id = puppet.insert_child(puppet.root(), node, Some(42));

        // Matrix (2x1): at x=0, deform=(0,0); at x=1, deform=(10,0) per vertex.
        let zero = vec![Vec2::ZERO; 3];
        let shifted = vec![Vec2::new(10.0, 0.0); 3];
        let matrix = DeformMatrix::from_cells(2, 1, vec![zero, shifted]).unwrap();
        let param = Param {
            id: 1,
            name: "Shift".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::new(1.0, 1.0),
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                part_id,
                InterpolateMode::Linear,
                BindingValues::Deform(matrix),
            )],
        };
        puppet.set_params(vec![param]);
        puppet.set_param_value(1, Vec2::new(0.5, 0.0));

        puppet.reset_deforms();
        puppet.apply_params();
        puppet.combine_deforms();

        let combined = if let NodeKind::Part(p) = &puppet.get(part_id).unwrap().kind {
            p.deform_stack.combined().to_vec()
        } else {
            panic!("node isn't a Part");
        };
        for v in &combined {
            assert!((v.x - 5.0).abs() < 1e-5, "expected x=5.0, got {}", v.x);
            assert!(v.y.abs() < 1e-5, "expected y=0, got {}", v.y);
        }
    }

    #[test]
    fn animation_lane_writes_param_value_over_time() {
        use crate::animation::{Animation, AnimationLane, Keyframe};
        use crate::params::InterpolateMode;

        let mut puppet = Puppet::new();

        // Lane: axis 0 (x) of param 5, keyframes (0, 0.0) -> (60, 1.0)
        // at 60 fps timestep (1s total). After 0.5s, expect 0.5 on x.
        let lane = AnimationLane {
            param_id: 5,
            axis: crate::params::ParamAxis::X,
            keyframes: vec![
                Keyframe {
                    frame: 0,
                    value: 0.0,
                },
                Keyframe {
                    frame: 60,
                    value: 1.0,
                },
            ],
            interpolation: InterpolateMode::Linear,
        };
        let anim = Animation {
            name: "glide".into(),
            timestep: 1.0 / 60.0,
            length: 60,
            lanes: vec![lane],
            ..Default::default()
        };
        puppet.set_animations(vec![anim]);
        assert!(puppet.play_animation("glide"));

        puppet.tick_animations(0.5);
        let v = puppet.param_value(5).expect("lane didn't write");
        assert!(
            (v.x - 0.5).abs() < 1e-4,
            "expected 0.5 at mid-lane, got {}",
            v.x
        );
        assert!(v.y.abs() < 1e-6, "axis 1 should stay 0, got {}", v.y);

        // (0.5 + 0.6)s passes the end; looping snaps back to the loop
        // region's start (frame 0 — no lead-in) rather than carrying the
        // 0.1s overshoot.
        puppet.tick_animations(0.6);
        let v = puppet.param_value(5).unwrap();
        assert!(
            v.x.abs() < 1e-4,
            "expected loop snap to frame 0, got {}",
            v.x
        );
    }

    #[test]
    fn non_finite_animation_delta_preserves_playback_state() {
        use crate::animation::{Animation, AnimationLane, Keyframe};
        use crate::params::InterpolateMode;

        let mut puppet = Puppet::new();
        puppet.set_animations(vec![Animation {
            name: "glide".into(),
            timestep: 1.0,
            length: 10,
            lanes: vec![AnimationLane {
                param_id: 5,
                axis: crate::params::ParamAxis::X,
                keyframes: vec![
                    Keyframe {
                        frame: 0,
                        value: 0.0,
                    },
                    Keyframe {
                        frame: 10,
                        value: 10.0,
                    },
                ],
                interpolation: InterpolateMode::Linear,
            }],
            ..Default::default()
        }]);
        assert!(puppet.play_animation("glide"));
        assert!(puppet.tick_animations(2.0));
        let time = puppet.play_state.unwrap().time;
        let value = puppet.param_value(5);

        assert!(!puppet.tick_animations(f32::NAN));
        assert!(!puppet.tick_animations(f32::INFINITY));
        assert_eq!(puppet.play_state.unwrap().time, time);
        assert_eq!(puppet.param_value(5), value);
    }

    #[test]
    fn looping_wraps_over_lead_in_lead_out_region() {
        use crate::animation::{Animation, AnimationLane, Keyframe};
        use crate::params::InterpolateMode;

        let mut puppet = Puppet::new();
        // Identity ramp 0..60 so the param value reads back the frame
        // (value = frame / 60).
        let anim = Animation {
            name: "loop".into(),
            timestep: 1.0 / 60.0,
            length: 60,
            lead_in: 10,
            lead_out: 50,
            lanes: vec![AnimationLane {
                param_id: 1,
                axis: crate::params::ParamAxis::X,
                keyframes: vec![
                    Keyframe {
                        frame: 0,
                        value: 0.0,
                    },
                    Keyframe {
                        frame: 60,
                        value: 1.0,
                    },
                ],
                interpolation: InterpolateMode::Linear,
            }],
        };
        puppet.set_animations(vec![anim]);
        assert!(puppet.play_animation("loop"));

        // First pass plays the lead-in once: frame 5 < lead_in.
        puppet.tick_animations(5.0 / 60.0);
        let v = puppet.param_value(1).unwrap();
        assert!(
            (v.x - 5.0 / 60.0).abs() < 1e-4,
            "lead-in must play, got {}",
            v.x
        );

        // Reaching the lead-out (frame 50) wraps to the lead-in frame
        // (10), not to 0 and not past the lead-out.
        puppet.tick_animations(45.0 / 60.0);
        let v = puppet.param_value(1).unwrap();
        assert!(
            (v.x - 10.0 / 60.0).abs() < 1e-4,
            "expected wrap to lead_in frame 10, got frame {}",
            v.x * 60.0
        );
    }

    #[test]
    fn shrinking_animations_clears_stale_play_state() {
        use crate::animation::Animation;

        let mut puppet = Puppet::new();
        puppet.set_animations(vec![Animation {
            name: "a".into(),
            ..Default::default()
        }]);
        assert!(puppet.play_animation("a"));
        assert!(puppet.has_playing_animation());

        puppet.set_animations(vec![]);
        assert!(
            !puppet.has_playing_animation(),
            "set_animations must drop play state pointing at the old list"
        );
        assert!(!puppet.tick_animations(0.1));
    }

    #[test]
    fn animation_lane_writing_axis_1_leaves_axis_0_alone() {
        use crate::animation::{Animation, AnimationLane, Keyframe};
        use crate::params::InterpolateMode;

        let mut puppet = Puppet::new();
        let anim = Animation {
            name: "y".into(),
            timestep: 1.0 / 60.0,
            length: 60,
            lanes: vec![AnimationLane {
                param_id: 1,
                axis: crate::params::ParamAxis::Y,
                keyframes: vec![
                    Keyframe {
                        frame: 0,
                        value: 0.0,
                    },
                    Keyframe {
                        frame: 60,
                        value: 2.0,
                    },
                ],
                interpolation: InterpolateMode::Linear,
            }],
            ..Default::default()
        };
        puppet.set_animations(vec![anim]);
        puppet.set_param_value(1, Vec2::new(7.0, 0.0));
        puppet.play_animation("y");
        puppet.tick_animations(0.5);
        let v = puppet.param_value(1).unwrap();
        assert!((v.x - 7.0).abs() < 1e-6, "axis 0 must be preserved");
        assert!((v.y - 1.0).abs() < 1e-4, "axis 1 mid-lane should be 1.0");
    }

    /// `bracket` returns `(last - 1, last)` for everything from the second-to-
    /// last axis point onward, so a Stepped binding that always takes the lower
    /// index can never reach its final cell — a 3-keypoint stepped selector
    /// would only ever produce 2 of its 3 states.
    #[test]
    fn stepped_binding_reaches_its_last_axis_cell() {
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::Node;

        let mut puppet = Puppet::new();
        let node_id = puppet.insert_child(puppet.root(), Node::default(), Some(5));

        let param = Param {
            id: 1,
            name: "step".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                node_id,
                InterpolateMode::Stepped,
                BindingValues::TransformTX(Matrix {
                    width: 3,
                    height: 1,
                    data: vec![0.0, 10.0, 20.0],
                }),
            )],
        };
        puppet.set_params(vec![param]);

        let tx_at = |puppet: &mut Puppet, v: f32| {
            puppet.set_param_value(1, Vec2::new(v, 0.0));
            puppet.reset_dynamic_state();
            puppet.apply_params();
            puppet.get(node_id).unwrap().transform.translation.x
        };

        // Holds the lower cell strictly inside a bracket ...
        assert!((tx_at(&mut puppet, 0.0) - 0.0).abs() < 1e-5);
        assert!((tx_at(&mut puppet, 0.75) - 10.0).abs() < 1e-5);
        // ... and the final cell at the top of the range.
        assert!(
            (tx_at(&mut puppet, 1.0) - 20.0).abs() < 1e-5,
            "the last axis point's cell must be reachable"
        );
    }

    #[test]
    fn apply_pose_overlay_resets_unmentioned_params() {
        use crate::params::Param;
        use crate::Node;

        let mut puppet = Puppet::new();
        puppet.insert_child(puppet.root(), Node::default(), None);
        puppet.set_params(vec![
            Param {
                id: 1,
                name: "a".into(),
                is_vec2: false,
                min: Vec2::ZERO,
                max: Vec2::ONE,
                defaults: Vec2::ZERO,
                axis_points_x: vec![0.0, 1.0],
                axis_points_y: vec![0.0],
                bindings: vec![],
            },
            Param {
                id: 2,
                name: "b".into(),
                is_vec2: false,
                min: Vec2::ZERO,
                max: Vec2::ONE,
                defaults: Vec2::ZERO,
                axis_points_x: vec![0.0, 1.0],
                axis_points_y: vec![0.0],
                bindings: vec![],
            },
        ]);
        puppet.set_param_value_by_name("a", Vec2::new(1.0, 0.0));
        puppet.apply_pose_overlay(&[("b", Vec2::new(0.5, 0.0))]);
        assert_eq!(puppet.param_value(1), Some(Vec2::ZERO));
        assert_eq!(puppet.param_value(2), Some(Vec2::new(0.5, 0.0)));
    }

    #[test]
    fn apply_params_drives_scalar_bindings_additively() {
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::Node;

        let mut puppet = Puppet::new();
        let mut node = Node::default();
        node.transform.translation = Vec3::new(100.0, 0.0, 0.0);
        node.transform.scale = Vec2::new(1.0, 1.0);
        node.z_order = 2.0;
        node.base_transform = node.transform;
        node.base_z_order = node.z_order;
        let node_id = puppet.insert_child(puppet.root(), node, Some(5));

        fn m(data: Vec<f32>) -> Matrix<f32> {
            Matrix {
                width: 2,
                height: 1,
                data,
            }
        }

        // Three bindings at param=0.5 drive: +5 on tx, *2 on sx, +10 on z.
        let param = Param {
            id: 1,
            name: "mix".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![
                Binding::new(
                    node_id,
                    InterpolateMode::Linear,
                    BindingValues::TransformTX(m(vec![0.0, 10.0])),
                ),
                Binding::new(
                    node_id,
                    InterpolateMode::Linear,
                    BindingValues::TransformSX(m(vec![1.0, 3.0])),
                ),
                Binding::new(
                    node_id,
                    InterpolateMode::Linear,
                    BindingValues::ZOrder(m(vec![0.0, 20.0])),
                ),
            ],
        };
        puppet.set_params(vec![param]);
        puppet.set_param_value(1, Vec2::new(0.5, 0.0));

        puppet.reset_dynamic_state();
        puppet.apply_params();

        let n = puppet.get(node_id).unwrap();
        // tx: base 100 + 0.5*10 = 105
        assert!((n.transform.translation.x - 105.0).abs() < 1e-5);
        // sx: base 1 * (1 + 0.5*(3-1)) = 2
        assert!((n.transform.scale.x - 2.0).abs() < 1e-5);
        // z: base 2 + 0.5*20 = 12
        assert!((n.z_order - 12.0).abs() < 1e-5);

        // A second apply without reset must NOT accumulate — reset
        // baselines + re-apply = idempotent per-frame behavior.
        puppet.reset_dynamic_state();
        puppet.apply_params();
        let n = puppet.get(node_id).unwrap();
        assert!((n.transform.translation.x - 105.0).abs() < 1e-5);
    }

    #[test]
    fn apply_params_opacity_binding_multiplies_part_opacity() {
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::{Node, NodeKind, PartData};

        let mut puppet = Puppet::new();
        let part = PartData {
            opacity: 1.0,
            base_opacity: 1.0,
            ..Default::default()
        };
        let node = Node {
            kind: NodeKind::Part(Box::new(part)),
            ..Default::default()
        };
        let part_id = puppet.insert_child(puppet.root(), node, Some(7));

        let matrix = Matrix {
            width: 2,
            height: 2,
            data: vec![0.5, 0.5, 0.5, 0.5],
        };
        let param = Param {
            id: 1,
            name: "fade".into(),
            is_vec2: true,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![Binding::new(
                part_id,
                InterpolateMode::Linear,
                BindingValues::Opacity(matrix),
            )],
        };
        puppet.set_params(vec![param]);
        puppet.set_param_value(1, Vec2::new(0.5, 0.5));

        puppet.reset_dynamic_state();
        puppet.apply_params();

        let op = match &puppet.get(part_id).unwrap().kind {
            NodeKind::Part(p) => p.opacity,
            _ => panic!("not a part"),
        };
        assert!((op - 0.5).abs() < 1e-6, "expected 0.5, got {}", op);

        // Re-apply after reset must be idempotent — no accumulation.
        puppet.reset_dynamic_state();
        puppet.apply_params();
        let op2 = match &puppet.get(part_id).unwrap().kind {
            NodeKind::Part(p) => p.opacity,
            _ => unreachable!(),
        };
        assert!(
            (op2 - 0.5).abs() < 1e-6,
            "expected 0.5 after second apply, got {}",
            op2
        );
    }

    #[test]
    fn tick_physics_writes_target_param_value() {
        use crate::params::{Binding, BindingValues, DeformMatrix, InterpolateMode, Param};
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Mesh, MeshIndices, Node, NodeKind, PartData};

        let mut puppet = Puppet::new();

        // Target part: 2 vertices, DeformStack of length 2.
        let mesh = Mesh::new(
            vec![Vec2::ZERO; 2],
            vec![Vec2::ZERO; 2],
            MeshIndices::U16(vec![]),
            Vec2::ZERO,
        );
        let part = PartData {
            deform_stack: crate::deform::DeformStack::new(2),
            mesh,
            ..Default::default()
        };
        let part_id = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Part(Box::new(part)),
                ..Default::default()
            },
            Some(7),
        );

        // Physics: perturbed rigid pendulum, map_mode XY so param x
        // tracks horizontal offset directly.
        let phys = SimplePhysicsData {
            map_mode: PhysicsParamMapMode::XY,
            target_param_id: Some(99),
            gravity: 981.0,
            length: 100.0,
            angle_damping: 0.0,
            bob: Vec2::new(60.0, 80.0),
            anchor_initialized: true,
            ..Default::default()
        };
        let phys_id = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(phys)),
                ..Default::default()
            },
            None,
        );

        // Param 99: 1D, axis_points_x = [0, 1], Deform binding matrix
        // (2x1): left cell = (0, 0) per vertex, right cell = (10, 0)
        // per vertex. So param.x = 0.6 should produce x-shift of 6.0.
        let matrix = DeformMatrix::from_cells(
            2,
            1,
            vec![vec![Vec2::ZERO; 2], vec![Vec2::new(10.0, 0.0); 2]],
        )
        .unwrap();
        let param = Param {
            id: 99,
            name: "phys".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                part_id,
                InterpolateMode::Linear,
                BindingValues::Deform(matrix),
            )],
        };
        puppet.set_params(vec![param]);

        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);
        puppet.tick_physics(&transforms, 0.0); // dt=0 just writes current param value

        let val = puppet.param_value(99).expect("target param value missing");
        // With bob=(60,80) and length=100, dir=(0.6, 0.8), XY maps to
        // (local.x, 1 - local.y) = (0.6, 0.2).
        assert!(
            (val.x - 0.6).abs() < 1e-4 && (val.y - 0.2).abs() < 1e-4,
            "physics didn't drive target param: got {:?}",
            val,
        );

        puppet.reset_deforms();
        puppet.apply_params();
        puppet.combine_deforms();

        if let NodeKind::Part(p) = &puppet.get(part_id).unwrap().kind {
            for v in p.deform_stack.combined() {
                // x should be 0.6 * 10 = 6.0
                assert!(
                    (v.x - 6.0).abs() < 1e-3,
                    "expected 6.0 x-shift, got {:?}",
                    v,
                );
            }
        } else {
            panic!("part vanished");
        }

        if let Some(node) = puppet.get_mut(phys_id) {
            node.transform.scale.x = 0.0;
        }
        puppet.mark_transform_dirty(phys_id);
        puppet.compute_transforms(&mut transforms);
        puppet.tick_physics(&transforms, 0.0);

        assert_eq!(puppet.param_value(99), Some(Vec2::ZERO));
    }

    #[test]
    fn output_scale_binding_scales_physics_driven_param() {
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        // Pendulum frozen mid-tilt (dt=0 ticks don't integrate): bob
        // (60, 80) on the rest circle maps through XY to (0.6, 0.2).
        // Param 1 multiplies outputScale.x by 1 -> 2 across its range.
        let mut puppet = Puppet::new();
        let phys_id = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::XY,
                    target_param_id: Some(99),
                    length: 100.0,
                    bob: Vec2::new(60.0, 80.0),
                    anchor_initialized: true,
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let param = Param {
            id: 1,
            name: "scale".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                phys_id,
                InterpolateMode::Linear,
                BindingValues::OutputScaleX(Matrix {
                    width: 2,
                    height: 1,
                    data: vec![1.0, 2.0],
                }),
            )],
        };
        puppet.set_params(vec![param]);

        let mut transforms = GlobalTransforms::new();
        puppet.set_param_value(1, Vec2::ZERO);
        puppet.tick(&mut transforms, Mat4::IDENTITY, 0.0);
        let base = puppet.param_value(99).expect("physics wrote target");
        assert!((base.x - 0.6).abs() < 1e-4, "identity offset: {:?}", base);

        puppet.set_param_value(1, Vec2::new(1.0, 0.0));
        puppet.tick(&mut transforms, Mat4::IDENTITY, 0.0);
        let scaled = puppet.param_value(99).expect("physics wrote target");
        assert!(
            (scaled.x - 1.2).abs() < 1e-4,
            "outputScale.x binding must double the mapped x: {:?}",
            scaled
        );
        assert!(
            (scaled.y - base.y).abs() < 1e-4,
            "y axis must stay unscaled: {:?} vs {:?}",
            scaled,
            base
        );
    }

    #[test]
    fn tick_physics_samples_current_param_pose_as_anchor() {
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        let mut puppet = Puppet::new();
        let parent = puppet.insert_child(puppet.root(), Node::default(), None);
        let phys_id = puppet.insert_child(
            parent,
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::XY,
                    target_param_id: Some(99),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let param = Param {
            id: 1,
            name: "Head".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                parent,
                InterpolateMode::Linear,
                BindingValues::TransformTX(Matrix {
                    width: 2,
                    height: 1,
                    data: vec![0.0, 100.0],
                }),
            )],
        };
        puppet.set_params(vec![param]);
        puppet.set_param_value(1, Vec2::new(1.0, 0.0));

        let mut transforms = GlobalTransforms::new();
        puppet.tick(&mut transforms, Mat4::IDENTITY, 0.0);

        let NodeKind::SimplePhysics(p) = &puppet.get(phys_id).unwrap().kind else {
            panic!("node wasn't SimplePhysics");
        };
        assert!(
            (p.anchor.x - 100.0).abs() < 1e-4,
            "physics used a stale anchor: {:?}",
            p.anchor,
        );
        assert!(p.anchor.y.abs() < 1e-4);
    }

    #[test]
    fn tick_chained_physics_driver_moves_dependent_anchor() {
        // a driver's output param transform-binding another
        // driver's anchor must move that anchor (chained physics — the reference rig's
        // segmented hair). The phase-1 anchor pose applies driver params at
        // their last-frame values, so the upstream driver's output drives
        // `dependent`'s anchor away from 0.
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        let mut puppet = Puppet::new();
        // `upstream` makes uuid 100 a *driver* param (a physics target),
        // included in the anchor pose at its last-frame value.
        puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    target_param_id: Some(100),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let dependent = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::XY,
                    target_param_id: None,
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        // uuid 100 (the upstream driver's output) translates `dependent`.
        let param = Param {
            id: 100,
            name: "Physics - upstream".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                dependent,
                InterpolateMode::Linear,
                BindingValues::TransformTX(Matrix {
                    width: 2,
                    height: 1,
                    data: vec![0.0, 100.0],
                }),
            )],
        };
        puppet.set_params(vec![param]);
        // Seed last-frame driver output; persists in param_values.
        puppet.set_param_value(100, Vec2::new(1.0, 0.0));

        let mut transforms = GlobalTransforms::new();
        puppet.tick(&mut transforms, Mat4::IDENTITY, 0.0);

        let NodeKind::SimplePhysics(p) = &puppet.get(dependent).unwrap().kind else {
            panic!("node wasn't SimplePhysics");
        };
        assert!(
            (p.anchor.x - 100.0).abs() < 1e-4,
            "dependent driver's anchor ignored the upstream driver output: {:?}",
            p.anchor,
        );
    }

    /// One param (uuid 1) whose committed base is `base`, ready for
    /// contributions from arbitrary sources.
    fn contribution_puppet(base: Vec2) -> Puppet {
        use crate::params::Param;
        let mut puppet = Puppet::new();
        puppet.set_params(vec![Param {
            id: 1,
            name: "p".into(),
            is_vec2: true,
            min: Vec2::splat(-10.0),
            max: Vec2::splat(10.0),
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![],
        }]);
        puppet.set_param_value(1, base);
        puppet
    }

    #[test]
    fn contributions_resolve_as_a_weighted_mean_against_the_base() {
        let (a, b) = (NodeIdx::new(1), NodeIdx::new(2));
        let base = Vec2::new(10.0, 0.0);

        let p = contribution_puppet(base);
        assert_eq!(p.param_value(1), Some(base), "no contribution -> base");

        let mut p = contribution_puppet(base);
        p.contribute_param_value(1, a, Vec2::new(20.0, 0.0), 1.0);
        assert_eq!(
            p.param_value(1),
            Some(Vec2::new(20.0, 0.0)),
            "one at w=1 -> that value, identical to a plain set",
        );

        let mut p = contribution_puppet(base);
        p.contribute_param_value(1, a, Vec2::new(20.0, 0.0), 0.5);
        assert_eq!(
            p.param_value(1),
            Some(Vec2::new(15.0, 0.0)),
            "one at w=0.5 -> halfway between base and the value",
        );

        let mut p = contribution_puppet(base);
        p.contribute_param_value(1, a, Vec2::new(20.0, 0.0), 1.0);
        p.contribute_param_value(1, b, Vec2::new(40.0, 0.0), 1.0);
        assert_eq!(
            p.param_value(1),
            Some(Vec2::new(30.0, 0.0)),
            "two at w=1 -> midpoint, not last-writer-wins",
        );

        let mut p = contribution_puppet(base);
        p.contribute_param_value(1, a, Vec2::new(20.0, 0.0), 1.0);
        p.contribute_param_value(1, b, Vec2::new(40.0, 0.0), 3.0);
        assert_eq!(
            p.param_value(1),
            Some(Vec2::new(35.0, 0.0)),
            "w=1 and w=3 -> weighted 1:3 toward the second",
        );
    }

    #[test]
    fn contribution_order_does_not_change_the_result() {
        // The property that makes a mean the right rule: catchlight has no
        // principled ordering between a host write and a driver write, so
        // the resolution must not encode one.
        let (a, b) = (NodeIdx::new(1), NodeIdx::new(2));
        let mut forward = contribution_puppet(Vec2::new(3.0, -1.0));
        forward.contribute_param_value(1, a, Vec2::new(20.0, 5.0), 0.25);
        forward.contribute_param_value(1, b, Vec2::new(-4.0, 8.0), 0.5);

        let mut reverse = contribution_puppet(Vec2::new(3.0, -1.0));
        reverse.contribute_param_value(1, b, Vec2::new(-4.0, 8.0), 0.5);
        reverse.contribute_param_value(1, a, Vec2::new(20.0, 5.0), 0.25);

        assert_eq!(forward.param_value(1), reverse.param_value(1));
    }

    #[test]
    fn a_source_replaces_its_own_contribution_rather_than_stacking() {
        // Drivers re-contribute every frame; if that appended instead of
        // replacing, the table would grow without bound and the mean would
        // drift toward whatever the driver used to say.
        let a = NodeIdx::new(1);
        let mut p = contribution_puppet(Vec2::ZERO);
        for i in 1..=5 {
            p.contribute_param_value(1, a, Vec2::new(i as f32, 0.0), 1.0);
        }
        assert_eq!(p.param_value(1), Some(Vec2::new(5.0, 0.0)));
    }

    #[test]
    fn a_retargeted_driver_stops_claiming_its_old_param() {
        // Resolution is a mean, so an abandoned contribution doesn't just
        // linger — it keeps full authority and drags the parameter halfway
        // to a frozen value. Retargeting must retire the old claim.
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        let mut puppet = Puppet::new();
        let phys = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::XY,
                    target_param_id: Some(100),
                    bob: Vec2::new(60.0, 80.0),
                    anchor_initialized: true,
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);
        puppet.tick_physics(&transforms, 0.0);
        assert!(
            puppet.param_value(100).is_some(),
            "driver never claimed 100"
        );

        // Retarget onto a different param and re-run.
        if let Some(NodeKind::SimplePhysics(p)) = puppet.get_mut(phys).map(|n| &mut n.kind) {
            p.target_param_id = Some(101);
        }
        puppet.set_param_value(100, Vec2::new(0.5, 0.5));
        puppet.tick_physics(&transforms, 0.0);

        assert_eq!(
            puppet.param_value(100),
            Some(Vec2::new(0.5, 0.5)),
            "the abandoned claim still blended into param 100",
        );
    }

    #[test]
    fn settling_a_frozen_puppet_leaves_the_authored_pose_alone() {
        // Freezing physics promises the authored pose wins. Settling writes
        // driver outputs, and a frozen puppet never ticks physics again to
        // retire them, so they would override that pose permanently.
        use crate::physics::SimplePhysicsData;
        use crate::{Node, NodeKind};

        let mut puppet = Puppet::new();
        puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    target_param_id: Some(100),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        puppet.set_physics_enabled(false);
        puppet.set_param_value(100, Vec2::new(0.25, 0.75));
        puppet.settle_physics();

        assert_eq!(
            puppet.param_value(100),
            Some(Vec2::new(0.25, 0.75)),
            "settling overrode the authored pose of a frozen puppet",
        );
    }

    #[test]
    fn settled_chained_physics_is_a_fixed_point_of_tick() {
        // If settling really put the rig at its analytic rest state, then
        // simulating from there with no input change is a no-op — including
        // for the dependent driver, which only lands correctly once the
        // upstream driver's settled output has propagated. Needs no
        // baseline: the invariance *is* the assertion.
        //
        // Two choices here are load-bearing, and the obvious alternatives
        // make the test pass without exercising propagation at all:
        //
        // - `LengthAngle` upstream, because its rest output is `(1, 0)`.
        //   `XY` rests at exactly `(0, 0)`, which is also the param's
        //   default, so its binding would evaluate at axis point 0 and
        //   drive nothing.
        // - A *rotation* binding, because `param_value` is anchor-relative
        //   and `settle_to_rest` always parks the bob at `anchor + (0, L)`.
        //   A driver's settled output is therefore invariant to where its
        //   anchor was translated to; only a rotation, which turns the
        //   `world_inverse` the displacement is mapped through, reaches the
        //   dependent's output.
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        let mut puppet = Puppet::new();
        puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::LengthAngle,
                    target_param_id: Some(100),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let dependent = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::AngleLength,
                    target_param_id: Some(101),
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        let param = Param {
            id: 100,
            name: "Physics - upstream".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                dependent,
                InterpolateMode::Linear,
                BindingValues::TransformRZ(Matrix {
                    width: 2,
                    height: 1,
                    data: vec![0.0, 0.5],
                }),
            )],
        };
        puppet.set_params(vec![param]);

        // The upstream's output starts at the param default, so the first
        // settle pass poses the dependent through an unrotated frame and
        // maps its angle to exactly 0. Only a second pass — after the
        // upstream's rest output has been folded back into the pose — turns
        // that frame and moves the angle off zero. Nothing primes param 100
        // beforehand, or pass 1 would already see the settled rotation and
        // a single-pass settle would look correct.
        puppet.settle_physics();
        let settled = (
            puppet.param_value(100).expect("upstream output"),
            puppet.param_value(101).expect("dependent output"),
        );
        assert!(
            settled.1.x.abs() > 1e-3,
            "the upstream driver's settled output never reached the dependent \
             (angle {}) — settling resolved in one pass",
            settled.1.x,
        );

        let mut transforms = GlobalTransforms::new();
        for _ in 0..240 {
            puppet.tick(&mut transforms, Mat4::IDENTITY, 1.0 / 60.0);
        }

        let after = (
            puppet.param_value(100).expect("upstream output"),
            puppet.param_value(101).expect("dependent output"),
        );
        assert!(
            (after.0 - settled.0).length() < 1e-4 && (after.1 - settled.1).length() < 1e-4,
            "settled state drifted under simulation: {settled:?} -> {after:?}",
        );
    }

    #[test]
    fn tick_physics_output_invariant_to_root_scale() {
        // Regression: bevy demo set `with_scale(0.1, -0.1, 0.1)` on the
        // puppet's entity Transform. That scale, folded into `root`,
        // shrank physics anchor displacements by 10x while
        // SimplePhysicsData.length stayed in puppet-local units —
        // suppressing pendulum response by ~25-30x. Physics now runs in
        // puppet-local coords; the host's root should leave output
        // identical.
        use crate::physics::{PendulumKind, PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        fn build() -> (Puppet, u32) {
            let mut puppet = Puppet::new();
            let target_uuid = 77;
            let data = SimplePhysicsData {
                model: PendulumKind::SpringPendulum,
                map_mode: PhysicsParamMapMode::XY,
                gravity: 9.8 * 100.0,
                length: 100.0,
                frequency: 1.0,
                angle_damping: 0.05,
                target_param_id: Some(target_uuid),
                // Perturb the bob so the pendulum is mid-swing, not at rest.
                bob: Vec2::new(40.0, 70.0),
                anchor_initialized: true,
                anchor: Vec2::new(50.0, 0.0),
                ..Default::default()
            };
            let base = crate::Transform {
                translation: Vec3::new(50.0, 0.0, 0.0),
                ..Default::default()
            };
            let node = Node {
                transform: base,
                base_transform: base,
                kind: NodeKind::SimplePhysics(Box::new(data)),
                ..Default::default()
            };
            puppet.insert_child(puppet.root(), node, None);
            (puppet, target_uuid)
        }

        let (mut puppet_a, target) = build();
        let mut tx_a = GlobalTransforms::new();
        for _ in 0..20 {
            puppet_a.tick(&mut tx_a, Mat4::IDENTITY, 1.0 / 60.0);
        }
        let val_a = puppet_a.param_value(target).unwrap();

        let (mut puppet_b, _) = build();
        let mut tx_b = GlobalTransforms::new();
        let scaled = Mat4::from_scale(Vec3::new(0.1, -0.1, 0.1));
        for _ in 0..20 {
            puppet_b.tick(&mut tx_b, scaled, 1.0 / 60.0);
        }
        let val_b = puppet_b.param_value(target).unwrap();

        let diff = (val_a - val_b).length();
        assert!(
            diff < 1e-4,
            "physics output diverged under scaled root: identity={:?}, scaled={:?}, |diff|={}",
            val_a,
            val_b,
            diff,
        );
        // Sanity: the pendulum should actually be moving, otherwise the
        // test is trivially passing.
        assert!(
            val_a.length() > 0.05,
            "test setup produced near-zero physics output: {:?}",
            val_a,
        );
    }

    #[test]
    fn tick_physics_drives_pendulum_from_perturbed_state_toward_rest() {
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        let mut puppet = Puppet::new();
        let anchor = Vec2::new(0.0, 0.0);
        let length = 100.0;
        let data = SimplePhysicsData {
            map_mode: PhysicsParamMapMode::AngleLength,
            gravity: 981.0,
            length,
            angle_damping: 0.15,
            bob: Vec2::new(80.0, 60.0),
            anchor,
            ..Default::default()
        };
        let node = Node {
            kind: NodeKind::SimplePhysics(Box::new(data)),
            ..Default::default()
        };
        let id = puppet.insert_child(puppet.root(), node, None);

        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);

        for _ in 0..1200 {
            puppet.tick_physics(&transforms, 1.0 / 60.0);
        }

        if let NodeKind::SimplePhysics(p) = &puppet.get(id).unwrap().kind {
            let final_bob = p.bob;
            let rest = Vec2::new(0.0, length);
            assert!(
                final_bob.distance(rest) < 5.0,
                "physics didn't settle: final bob = {:?}, rest = {:?}",
                final_bob,
                rest
            );
        } else {
            panic!("node wasn't SimplePhysics anymore");
        }
    }

    #[test]
    fn tick_physics_local_only_uses_local_anchor() {
        // Parent at world (1000, 0). SimplePhysics child at local (50, 0).
        // With local_only=true the integrator must see the parent-frame
        // anchor (50, 0), not the world (1050, 0).
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind, Transform};

        fn build(local_only: bool) -> (Puppet, NodeIdx) {
            let mut puppet = Puppet::new();
            let parent_t = Transform {
                translation: Vec3::new(1000.0, 0.0, 0.0),
                ..Default::default()
            };
            let parent = puppet.insert_child(
                puppet.root(),
                Node {
                    transform: parent_t,
                    base_transform: parent_t,
                    ..Default::default()
                },
                None,
            );
            let child_t = Transform {
                translation: Vec3::new(50.0, 0.0, 0.0),
                ..Default::default()
            };
            let data = SimplePhysicsData {
                map_mode: PhysicsParamMapMode::AngleLength,
                local_only,
                gravity: 981.0,
                length: 100.0,
                angle_damping: 0.0,
                bob: Vec2::new(0.0, 100.0),
                anchor_initialized: true,
                ..Default::default()
            };
            let id = puppet.insert_child(
                parent,
                Node {
                    transform: child_t,
                    base_transform: child_t,
                    kind: NodeKind::SimplePhysics(Box::new(data)),
                    ..Default::default()
                },
                None,
            );
            (puppet, id)
        }

        for (local_only, expected_x) in [(true, 50.0_f32), (false, 1050.0_f32)] {
            let (mut puppet, id) = build(local_only);
            let mut tx = GlobalTransforms::new();
            puppet.compute_transforms(&mut tx);
            puppet.tick_physics(&tx, 1.0 / 60.0);
            let NodeKind::SimplePhysics(p) = &puppet.get(id).unwrap().kind else {
                panic!("node wasn't SimplePhysics");
            };
            assert!(
                (p.anchor.x - expected_x).abs() < 1e-3,
                "local_only={}: expected anchor.x={}, got {}",
                local_only,
                expected_x,
                p.anchor.x,
            );
            assert!(p.anchor.y.abs() < 1e-3);
        }
    }

    // Build a puppet whose SimplePhysics driver settles at rest and whose
    // target param drives both a Deform and an Opacity binding on a Part.
    // Returns (puppet, part_id).
    fn settling_physics_puppet() -> (Puppet, NodeIdx) {
        use crate::params::{Binding, BindingValues, DeformMatrix, InterpolateMode, Matrix, Param};
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Mesh, MeshIndices, Node, NodeKind, PartData};

        let mut puppet = Puppet::new();
        let mesh = Mesh::new(
            vec![Vec2::ZERO; 2],
            vec![Vec2::ZERO; 2],
            MeshIndices::U16(vec![]),
            Vec2::ZERO,
        );
        let part = PartData {
            deform_stack: crate::deform::DeformStack::new(2),
            mesh,
            opacity: 1.0,
            base_opacity: 1.0,
            ..Default::default()
        };
        let part_id = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Part(Box::new(part)),
                ..Default::default()
            },
            Some(7),
        );
        // World-anchored pendulum at the origin: `anchor_initialized:false`
        // snaps the bob to rest on the first tick, an exact f32 fixed point.
        puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::XY,
                    target_param_id: Some(99),
                    length: 100.0,
                    gravity: 981.0,
                    angle_damping: 0.5,
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        // Param 99 (physics target): Deform stays non-zero across its range
        // (so a reset-without-reapply would zero it), Opacity holds 0.5 (so
        // a reset-without-reapply would leave it at base 1.0).
        let deform = DeformMatrix::from_cells(
            2,
            1,
            vec![vec![Vec2::new(5.0, 0.0); 2], vec![Vec2::new(15.0, 0.0); 2]],
        )
        .unwrap();
        let opacity = Matrix {
            width: 2,
            height: 1,
            data: vec![0.5, 0.5],
        };
        let param = Param {
            id: 99,
            name: "phys".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![
                Binding::new(
                    part_id,
                    InterpolateMode::Linear,
                    BindingValues::Deform(deform),
                ),
                Binding::new(
                    part_id,
                    InterpolateMode::Linear,
                    BindingValues::Opacity(opacity),
                ),
            ],
        };
        puppet.set_params(vec![param]);
        (puppet, part_id)
    }

    fn part_deform(puppet: &Puppet, id: NodeIdx) -> (Vec<Vec2>, u64) {
        match &puppet.get(id).unwrap().kind {
            crate::NodeKind::Part(p) => (
                p.deform_stack.combined().to_vec(),
                p.deform_stack.generation(),
            ),
            _ => panic!("node isn't a Part"),
        }
    }

    fn part_opacity(puppet: &Puppet, id: NodeIdx) -> f32 {
        match &puppet.get(id).unwrap().kind {
            crate::NodeKind::Part(p) => p.opacity,
            _ => panic!("node isn't a Part"),
        }
    }

    #[test]
    fn settled_physics_skips_pipeline_yet_holds_posed_state() {
        // Once the driver settles, `tick` takes the skip path (no pre-pass,
        // no final apply). The observable pose — rendered transform, deform
        // combined()/generation(), opacity, and param_generation — must stay
        // bit-identical to the posed frame, not drift and not revert to the
        // unposed baseline.
        let (mut puppet, part_id) = settling_physics_puppet();
        let mut out = GlobalTransforms::new();
        let dt = 1.0 / 60.0;

        // Reach the skip regime (the rest pendulum settles on frame 1).
        for _ in 0..10 {
            puppet.tick(&mut out, Mat4::IDENTITY, dt);
        }
        let ref_gen = puppet.param_generation;
        let (ref_combined, ref_deform_gen) = part_deform(&puppet, part_id);
        let ref_opacity = part_opacity(&puppet, part_id);
        let ref_tf = out.get(part_id);
        assert!(
            ref_combined.iter().any(|v| v.length() > 1e-3),
            "settled deform must be non-zero to be a meaningful guard: {:?}",
            ref_combined,
        );
        assert!(
            (ref_opacity - 0.5).abs() < 1e-6,
            "opacity binding must be applied while posed, got {ref_opacity}",
        );

        // Skip frames must reproduce the posed state exactly (no drift).
        for _ in 0..60 {
            puppet.tick(&mut out, Mat4::IDENTITY, dt);
            assert_eq!(
                puppet.param_generation, ref_gen,
                "param_generation drifted on a skip frame",
            );
            let (combined, gen) = part_deform(&puppet, part_id);
            assert_eq!(
                gen, ref_deform_gen,
                "deform generation changed on a skip frame"
            );
            assert_eq!(
                combined, ref_combined,
                "deform combined changed on a skip frame"
            );
            assert_eq!(
                out.get(part_id),
                ref_tf,
                "transform changed on a skip frame"
            );
            assert!(
                (part_opacity(&puppet, part_id) - ref_opacity).abs() < 1e-6,
                "opacity drifted on a skip frame",
            );
        }

        // Invalidating the cached anchor pose (as an external
        // reset_dynamic_state caller would) forces the next tick to rerun
        // the pre-pass + final apply; it must reproduce the identical posed
        // state, not leave the puppet reset/unposed.
        puppet.last_anchor_pose_generation = None;
        puppet.tick(&mut out, Mat4::IDENTITY, dt);
        assert!(
            (part_opacity(&puppet, part_id) - ref_opacity).abs() < 1e-6,
            "rebuilding after a stale anchor pose left opacity {} != {ref_opacity}",
            part_opacity(&puppet, part_id),
        );
        assert_eq!(part_deform(&puppet, part_id).0, ref_combined);
        assert_eq!(out.get(part_id), ref_tf);
    }

    #[test]
    fn settled_physics_reactivates_when_anchor_moves() {
        // After settling, moving the anchor (a param that transform-binds the
        // physics node's parent) must invalidate the cached anchor pose so
        // the driver output responds within a frame or two.
        use crate::params::{Binding, BindingValues, InterpolateMode, Matrix, Param};
        use crate::physics::{PhysicsParamMapMode, SimplePhysicsData};
        use crate::{Node, NodeKind};

        let mut puppet = Puppet::new();
        let parent = puppet.insert_child(puppet.root(), Node::default(), None);
        // World-anchored so the parent's translation feeds the world anchor.
        puppet.insert_child(
            parent,
            Node {
                kind: NodeKind::SimplePhysics(Box::new(SimplePhysicsData {
                    map_mode: PhysicsParamMapMode::XY,
                    target_param_id: Some(99),
                    length: 100.0,
                    gravity: 981.0,
                    angle_damping: 0.2,
                    ..Default::default()
                })),
                ..Default::default()
            },
            None,
        );
        // Param 1 shifts the parent (and thus the anchor) by +200 in x.
        let param = Param {
            id: 1,
            name: "anchor".into(),
            is_vec2: false,
            min: Vec2::ZERO,
            max: Vec2::ONE,
            defaults: Vec2::ZERO,
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![Binding::new(
                parent,
                InterpolateMode::Linear,
                BindingValues::TransformTX(Matrix {
                    width: 2,
                    height: 1,
                    data: vec![0.0, 200.0],
                }),
            )],
        };
        puppet.set_params(vec![param]);

        let mut out = GlobalTransforms::new();
        let dt = 1.0 / 60.0;
        for _ in 0..80 {
            puppet.tick(&mut out, Mat4::IDENTITY, dt);
        }
        let settled = puppet.param_value(99).expect("driver wrote target");

        // Move the anchor and let the driver react.
        puppet.set_param_value(1, Vec2::new(1.0, 0.0));
        for _ in 0..3 {
            puppet.tick(&mut out, Mat4::IDENTITY, dt);
        }
        let reacted = puppet.param_value(99).expect("driver wrote target");
        assert!(
            (reacted - settled).length() > 0.01,
            "driver output ignored the moved anchor: settled={settled:?}, reacted={reacted:?}",
        );
    }
}
