//! The compact-index node arena a puppet evaluates in.
//!
//! Both runtimes address nodes by [`NodeIdx`], a slot into one dense `Vec` —
//! never by Id — so a full-node pass is cache-linear and a point lookup is a
//! bounds check plus an index. Every `NodeIdx` handed out by `insert_child` is
//! sequential and never removed, which is what makes the slot index and the
//! handle the same number. Ids live at the edge: a [`crate::puppet::Puppet`]
//! translates them when it bakes and when a caller asks.
//!
//! The arena holds the *evaluated frame* — each node's working transform,
//! z order, colour and deform stack — plus what is baked once from the model
//! it came from: mesh-group pins, the triangle bitmap, and the base
//! matrices the transform walk reuses when a node has no delta this frame.
//! What it does not hold is a pose: a fold writes into the arena, but which
//! fold to run and at what values is the owning runtime's business. That is
//! the whole reason it is a separate struct — [`crate::puppet`] carries the
//! pose layers over one set of passes.

use glam::Mat4;

use crate::components::{Node, NodeIdx};
use crate::node::NodeTree;

/// Computed global transforms for all nodes in a puppet.
/// Vec<Mat4> indexed by NodeIdx.0; aligns with the dense node storage so
/// point lookups are a bounds check + index rather than a hash probe, and
/// the DFS walk in compute_transforms_with_root writes through contiguous
/// memory.
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

/// The dense node storage and the passes that run over it.
///
/// Field visibility is `pub(crate)` because [`crate::meshgroup`] and
/// [`crate::weld`] borrow several of these disjointly in one loop, which no
/// accessor pair can express.
#[derive(Clone)]
pub(crate) struct Arena {
    // Dense storage indexed by NodeIdx.0. Every NodeIdx allocated via
    // `allocate_id` is sequential and never removed, so NodeIdx doubles
    // as the slot index -- no HashMap<NodeIdx,_> indirection, full-node
    // passes are cache-linear, and point lookups are a bounds check + index.
    pub(crate) nodes: Vec<Node>,
    pub(crate) tree: NodeTree,
    next_id: u32,
    // Frame-persistent scratch used by propagate_mesh_group_deforms to
    // carry computed offsets between a read-borrow of the MG node and a
    // write-borrow of the child node. Sized by the largest child
    // vert_count seen so far; reused across MGs and across frames.
    pub(crate) mg_propagate_scratch: Vec<glam::Vec2>,
    // Parallel scratch used by mesh-group propagation to read the child's
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
    // Parallel to nodes: true when a fold wrote a delta into
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
    /// The spines that carry a particle chain, in the same arena order
    /// `physics_node_ids` keeps its own in. A separate list from
    /// `spine_node_ids` because most spines carry nothing and the solver pass
    /// should not walk them; the two are otherwise the same nodes.
    pub(crate) chain_node_ids: Vec<NodeIdx>,
    /// Parallel to `chain_node_ids`: each chain's index into `spine_node_ids`,
    /// so the solver reaches its spine's row of `Baked::spine_targets` without
    /// a lookup — the hot loops never search.
    pub(crate) chain_spine: Vec<usize>,
    /// Spine nodes, in arena order. `Baked::spine_targets` is parallel to it.
    pub(crate) spine_node_ids: Vec<NodeIdx>,
    // Frame-persistent scratch for the spine pass, the same pair
    // `propagate_mesh_group_deforms` keeps: the offsets computed under a read
    // borrow of the spine and written under a write borrow of the child, and
    // the child's combined-minus-Node(spine) deform read without disturbing
    // the stack memos.
    pub(crate) spine_scratch: Vec<glam::Vec2>,
    pub(crate) spine_cur_deform_scratch: Vec<glam::Vec2>,
    pub(crate) mesh_group_node_ids: Vec<NodeIdx>,
    // Puppet-local (root=IDENTITY) transform scratch used when sampling
    // SimplePhysics anchors. Decouples the physics integrator from any
    // host world-scale: pendulum length and gravity are loaded in
    // puppet-local units, so anchors must be in matching units.
    pub(crate) physics_transforms: GlobalTransforms,
    /// Per-node `translate_children` shift from the **previous** frame's
    /// mesh-group pass, in the node's own parent space; zero for a node no
    /// such group shifted. `meshgroup::shift_translate_children` overwrites
    /// every target's entry each frame — the delta it applied, or zero for a
    /// target it skipped — and the physics anchor pre-pass adds it back
    /// (`apply_previous_tc_shifts`), so a driver anchored under a shifted
    /// node samples the shifted place one frame late. A rebake builds a new
    /// arena, so this starts at zero and never outlives the tree it measured.
    tc_shift_deltas: Vec<glam::Vec2>,
    /// How many entries of `tc_shift_deltas` are non-zero, so a model with no
    /// shifted node costs nothing per frame.
    tc_shift_nonzero: usize,
    /// Whether any entry of `tc_shift_deltas` actually moved since the last
    /// `take_tc_shift_changed`. The tick consumes it to force one more anchor
    /// pre-pass, so a shift that has just appeared reaches the anchors even if
    /// the pose then stands still.
    tc_shift_changed: bool,
    /// Slots that are a driver node — SimplePhysics or particle chain — or
    /// an ancestor of one: the only slots the physics pre-pass transform walk
    /// needs to fill (see `compute_physics_ancestor_transforms`). Invalidated
    /// on `insert_child`.
    physics_ancestor_mask: Option<Vec<bool>>,
}

impl Arena {
    pub(crate) fn new() -> Self {
        let root_id = NodeIdx::new(0);
        let tree = NodeTree::new(root_id);

        // Slot 0 = root (NodeIdx::new(0)).
        let nodes = vec![Node::default()];

        Self {
            nodes,
            tree,
            next_id: 1,
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
            chain_node_ids: Vec::new(),
            chain_spine: Vec::new(),
            spine_node_ids: Vec::new(),
            spine_scratch: Vec::new(),
            spine_cur_deform_scratch: Vec::new(),
            mesh_group_node_ids: Vec::new(),
            physics_transforms: GlobalTransforms::new(),
            tc_shift_deltas: vec![glam::Vec2::ZERO],
            tc_shift_nonzero: 0,
            tc_shift_changed: false,
            physics_ancestor_mask: None,
        }
    }

    pub(crate) fn root(&self) -> NodeIdx {
        self.tree.root
    }

    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub(crate) fn get(&self, id: NodeIdx) -> Option<&Node> {
        self.nodes.get(id.0 as usize)
    }

    pub(crate) fn get_mut(&mut self, id: NodeIdx) -> Option<&mut Node> {
        self.nodes.get_mut(id.0 as usize)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(slot, node)| (NodeIdx::new(slot as u32), node))
    }

    pub(crate) fn iter_deform_nodes(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.deform_node_ids
            .iter()
            .filter_map(|&id| self.nodes.get(id.0 as usize).map(|node| (id, node)))
    }

    pub(crate) fn mark_transform_dirty(&mut self, id: NodeIdx) {
        if let Some(d) = self.node_transform_dirty.get_mut(id.0 as usize) {
            *d = true;
        }
    }

    /// Rebuild the five kind registries from the current node kinds.
    pub(crate) fn rebuild_kind_registries(&mut self) {
        self.deform_node_ids.clear();
        self.physics_node_ids.clear();
        self.chain_node_ids.clear();
        self.chain_spine.clear();
        self.spine_node_ids.clear();
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
            if matches!(&node.kind, crate::NodeKind::Spine(sp) if sp.chain.is_some()) {
                self.chain_node_ids.push(id);
                self.chain_spine.push(self.spine_node_ids.len());
            }
            if matches!(&node.kind, crate::NodeKind::Spine(_)) {
                self.spine_node_ids.push(id);
            }
            if matches!(&node.kind, crate::NodeKind::MeshGroup(_)) {
                self.mesh_group_node_ids.push(id);
            }
        }
        self.invalidate_structure_caches();
    }

    /// Drop every cache that depends on tree shape or node kinds.
    pub(crate) fn invalidate_structure_caches(&mut self) {
        self.mg_pre_order_cache = None;
        self.physics_ancestor_mask = None;
    }

    pub(crate) fn rebuild_all_mesh_group_pins(&mut self) {
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
                let pins = crate::meshgroup::bake_mesh_group_pins(self, &transforms, id);
                let bitmap = self
                    .nodes
                    .get(id.0 as usize)
                    .and_then(|node| match &node.kind {
                        crate::NodeKind::MeshGroup(mesh_group) => {
                            crate::meshgroup::MgTriangleBitmap::build(&mesh_group.mesh)
                        }
                        _ => None,
                    });
                (id, pins, bitmap)
            })
            .collect();
        for (id, pins, bitmap) in baked {
            if let Some(crate::NodeKind::MeshGroup(mesh_group)) =
                self.nodes.get_mut(id.0 as usize).map(|node| &mut node.kind)
            {
                mesh_group.pins = pins;
                mesh_group.bitmap = bitmap;
            }
        }
    }

    /// Restore every node's transform / z_order / colour to the authored
    /// baseline. Call at the start of a frame before a fold, so deltas don't
    /// accumulate across frames.
    pub(crate) fn reset_dynamic_state(&mut self) {
        let _span = tracing::trace_span!("reset_dynamic_state").entered();
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
    pub(crate) fn reset_deforms(&mut self) {
        let _span = tracing::trace_span!("reset_deforms").entered();
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

    /// Collapse each DeformStack's active sources into its combined
    /// per-vertex offset. Idempotent and cheap when nothing is dirty.
    pub(crate) fn combine_deforms(&mut self) {
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

    /// Derive every spine's rest state: the per-vertex assignment, the way
    /// [`Self::rebuild_all_mesh_group_pins`] derives a group's pins, and the
    /// spring offsets of the chain it carries.
    ///
    /// Both need the same thing and can only be had here: the transforms the
    /// art was drawn at. The assignment needs each descendant's rest place,
    /// and the spring fit needs the node's rest **rotation**, because the
    /// balance that makes the drawing an equilibrium is not the same balance
    /// at a different tilt.
    pub(crate) fn rebuild_spine_rest_state(&mut self) {
        self.reset_dynamic_state();
        self.reset_deforms();
        if self.spine_node_ids.is_empty() {
            return;
        }
        let mut transforms = GlobalTransforms::new();
        self.compute_transforms(&mut transforms);
        let baked: Vec<_> = self
            .spine_node_ids
            .iter()
            .map(|&id| {
                (
                    id,
                    crate::spine::bake_spine_pins(self, &transforms, id),
                    self.chain_orient(&transforms, id),
                )
            })
            .collect();
        for (id, pins, orient) in baked {
            let Some(crate::NodeKind::Spine(spine)) =
                self.nodes.get_mut(id.0 as usize).map(|node| &mut node.kind)
            else {
                continue;
            };
            spine.pins = pins;
            if let (Some(chain), Some(orient)) = (&mut spine.chain, orient) {
                let gravity = chain.gravity;
                for link in chain.links.iter_mut() {
                    link.spring_offset =
                        crate::physics::fitted_spring_offset(link, gravity, orient);
                }
            }
        }
    }

    /// Turn every spine's descendants by this frame's bends. Call after the
    /// transforms are computed and before the mesh groups, so a group above a
    /// spine warps art the spine has already bent.
    pub(crate) fn propagate_spine_deforms(&mut self, transforms: &GlobalTransforms) {
        let _span = tracing::trace_span!("propagate_spine_deforms").entered();
        crate::spine::propagate_spine_deforms(self, transforms);
    }

    /// Run the mesh groups in pre-order: each combines its stack, shifts its
    /// `translate_children` targets and brings the globals under them up to
    /// date in `transforms`, then pushes the combined deform to its meshed
    /// children. `root` is the same fold `compute_transforms_with_root` takes.
    pub(crate) fn propagate_mesh_group_deforms(
        &mut self,
        transforms: &mut GlobalTransforms,
        root: Mat4,
    ) {
        let _span = tracing::trace_span!("propagate_mesh_group_deforms").entered();
        crate::meshgroup::propagate_mesh_group_deforms(self, transforms, root);
    }

    /// Solve every weld into the parts' `DeformSource::Weld` slots. Call
    /// after `propagate_mesh_group_deforms` and before `combine_deforms`.
    pub(crate) fn apply_welds(&mut self, transforms: &GlobalTransforms) {
        crate::weld::apply_welds(self, transforms);
    }

    /// Allocate a new id, attach as a child of `parent`, and install `node`.
    ///
    /// This is the only supported way to grow the arena, so the dense stores
    /// and the tree stay in sync.
    pub(crate) fn insert_child(&mut self, parent: NodeIdx, node: Node) -> NodeIdx {
        let id = self.allocate_id();
        // An unresolvable parent would leave the node registered in every side
        // index (deform / mesh-group / physics) but absent from the tree, so it
        // never renders while its physics driver still perturbs the model.
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
        let is_chain = matches!(&node.kind, crate::NodeKind::Spine(sp) if sp.chain.is_some());
        let is_spine = matches!(&node.kind, crate::NodeKind::Spine(_));
        self.base_local_matrix.push(node.base_transform.to_matrix());
        self.node_transform_dirty.push(false);
        self.tc_shift_deltas.push(glam::Vec2::ZERO);
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
        if is_chain {
            self.chain_node_ids.push(id);
            self.chain_spine.push(self.spine_node_ids.len());
        }
        if is_spine {
            self.spine_node_ids.push(id);
        }
        // A new node changes the physics ancestor set and the mesh-group
        // pre-order.
        self.invalidate_structure_caches();
        id
    }

    fn allocate_id(&mut self) -> NodeIdx {
        let id = NodeIdx::new(self.next_id);
        self.next_id += 1;
        id
    }

    pub(crate) fn compute_transforms(&self, out: &mut GlobalTransforms) {
        let _span = tracing::trace_span!("compute_transforms").entered();
        self.compute_transforms_with_root(out, Mat4::IDENTITY);
    }

    /// Fold `root` into the top-level transform of the puppet so it
    /// renders at an arbitrary world position. Used by bevy integration
    /// to apply the entity's `GlobalTransform` without re-pointing the
    /// shared camera uniform between puppets (which would hit the
    /// queue.write_buffer batching hazard documented in AGENTS.md).
    pub(crate) fn compute_transforms_with_root(&self, out: &mut GlobalTransforms, root: Mat4) {
        let _span = tracing::trace_span!("compute_transforms_with_root").entered();
        // Size the Vec once at frame start; each insert below is then a
        // direct index write. DFS covers every reachable node so we
        // don't need a post-walk clear to scrub stale slots.
        out.ensure_size(self.nodes.len());

        self.tree.with_dfs_order(|order| {
            for &id in order {
                let slot = id.0 as usize;
                let node = self.nodes.get(slot);
                let local_matrix = self.local_matrix(slot, node);
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

    /// One node's local matrix. Skips `Transform::to_matrix()` if (a) the
    /// fold didn't write this frame and (b) the transform still matches base
    /// (invariant preserved when no external code has touched the Transform
    /// fields). A 36-byte equality compare is cheaper than
    /// `from_scale_rotation_translation` + `Quat::from_euler`.
    #[inline]
    fn local_matrix(&self, slot: usize, node: Option<&Node>) -> Mat4 {
        let Some(n) = node else {
            return Mat4::IDENTITY;
        };
        let dirty = self.node_transform_dirty.get(slot).copied().unwrap_or(true);
        if !dirty && n.transform == n.base_transform {
            self.base_local_matrix
                .get(slot)
                .copied()
                .unwrap_or_else(|| n.transform.to_matrix())
        } else {
            n.transform.to_matrix()
        }
    }

    /// Recompute the globals of `subtree_root` and everything under it,
    /// reading the parent's global back out of `out`. Same per-node logic as
    /// `compute_transforms_with_root`, restricted to one subtree: a mesh
    /// group's `translate_children` shift moves a whole node mid-pass, and
    /// what sits under it has to read the shifted place.
    pub(crate) fn recompute_subtree_transforms(
        &self,
        out: &mut GlobalTransforms,
        subtree_root: NodeIdx,
        root: Mat4,
    ) {
        let below = self.tree.get_all_descendants(subtree_root);
        for id in std::iter::once(subtree_root).chain(below) {
            let slot = id.0 as usize;
            let node = self.nodes.get(slot);
            let local_matrix = self.local_matrix(slot, node);
            let parent_matrix = if node.map(|n| n.lock_to_root).unwrap_or(false) {
                root
            } else {
                self.tree
                    .get_parent(id)
                    .map(|parent| out.get(parent))
                    .unwrap_or(root)
            };
            out.insert(id, parent_matrix * local_matrix);
        }
    }

    /// Anchor point for a driver — a pendulum or a particle chain — in the
    /// driver's own **Y-down** frame (gravity toward +Y). The
    /// node world is Y-up, so the Y is flipped here; the driver's output
    /// conjugates `world_inverse` by the same flip to undo it.
    ///
    /// Both kinds hang from the same point and answer to the same
    /// `local_only`, so one function serves both: the anchor is a property of
    /// the node, not of what is dangling off it.
    ///
    /// The `local_only` branch uses `node.transform.translation`, including
    /// parameter-driven offsets from the anchor pre-pass rather than the
    /// frozen load pose — and, for a node a mesh group shifts, the previous
    /// frame's shift the pre-pass replayed (`apply_previous_tc_shifts`).
    pub(crate) fn physics_anchor(
        &self,
        transforms: &GlobalTransforms,
        id: NodeIdx,
    ) -> Option<crate::Vec2> {
        let node = self.nodes.get(id.0 as usize)?;
        let local_only = match &node.kind {
            crate::NodeKind::SimplePhysics(p) => p.local_only,
            crate::NodeKind::Spine(sp) => sp.chain.as_ref()?.local_only,
            _ => return None,
        };
        let anchor = if local_only {
            crate::Vec2::new(node.transform.translation.x, node.transform.translation.y)
        } else {
            let world = transforms.get(id);
            crate::Vec2::new(world.w_axis.x, world.w_axis.y)
        };
        Some(crate::Vec2::new(anchor.x, -anchor.y))
    }

    /// The node's own rotation, as its local down taken through the world
    /// transform and the same Y flip [`Self::physics_anchor`] applies — a unit
    /// vector, because one is what names a rotation of the plane.
    ///
    /// The chain carries its drawn shape by this, so the spring and
    /// `link_bends` agree about where the art is; the node's own down is what
    /// a rotation of `(0, 1)` gives, which is why one vector is enough.
    ///
    /// The `local_only` branch integrates in the parent's frame, where the
    /// node's own rotation has not been applied, so the shape is carried by
    /// nothing.
    pub(crate) fn chain_orient(
        &self,
        transforms: &GlobalTransforms,
        id: NodeIdx,
    ) -> Option<crate::Vec2> {
        let node = self.nodes.get(id.0 as usize)?;
        let crate::NodeKind::Spine(sp) = &node.kind else {
            return None;
        };
        let chain = sp.chain.as_ref()?;
        if chain.local_only {
            return Some(crate::Vec2::new(0.0, 1.0));
        }
        // The node's local -Y in world (model space is Y-up), then flipped
        // into the physics frame.
        let world = transforms.get(id);
        let down = crate::Vec2::new(-world.y_axis.x, -world.y_axis.y);
        let down = if down.is_finite() && down.length_squared() > 1e-12 {
            down.normalize()
        } else {
            crate::Vec2::new(0.0, -1.0)
        };
        Some(crate::Vec2::new(down.x, -down.y))
    }

    /// Record what `translate_children` shift a mesh group applied to one of
    /// its targets this frame, overwriting the last one. A target the pass
    /// skipped is recorded as zero, so a stale delta never survives a frame.
    pub(crate) fn set_tc_shift_delta(&mut self, id: NodeIdx, delta: glam::Vec2) {
        let Some(slot) = self.tc_shift_deltas.get_mut(id.0 as usize) else {
            return;
        };
        let previous = *slot;
        if previous == delta {
            return;
        }
        *slot = delta;
        self.tc_shift_changed = true;
        match (previous != glam::Vec2::ZERO, delta != glam::Vec2::ZERO) {
            (false, true) => self.tc_shift_nonzero += 1,
            (true, false) => self.tc_shift_nonzero -= 1,
            _ => {}
        }
    }

    /// Whether a `translate_children` shift moved since this was last asked,
    /// clearing the flag.
    ///
    /// The anchor pre-pass replays the *stored* shift, so a frame whose
    /// mesh-group pass records a different one has left the anchors a frame
    /// behind a shift that has genuinely changed. The tick answers that by
    /// dropping its cached anchor pose, which costs one extra pre-pass while a
    /// shift is settling and nothing once it has: the shift is a function of
    /// the pose, so a still pose recomputes the same delta and stops asking.
    pub(crate) fn take_tc_shift_changed(&mut self) -> bool {
        std::mem::take(&mut self.tc_shift_changed)
    }

    /// Add the **previous** frame's `translate_children` shift back onto every
    /// node one moved. The physics anchor pre-pass calls this after folding the
    /// anchor bindings and before walking the transforms, so a driver whose
    /// ancestor chain runs through a shifted node — or which is itself a target
    /// and reads `node.transform.translation` — samples a shifted anchor rather
    /// than a pre-shift one. The anchor is therefore one frame late;
    /// running the mesh-group pass inside the
    /// pre-pass instead would need the deform bindings folded and a second
    /// transform walk every frame.
    ///
    /// `reset_dynamic_state` has restored each node's base transform first, so
    /// this adds one frame's shift and never accumulates.
    ///
    /// **Why the pre-pass may still be skipped.** It is skipped when the pose
    /// has not moved since the frame that built the cached anchor pose, and
    /// `take_tc_shift_changed` has forced one more rebuild after any frame
    /// whose shift moved. So a skipped frame is one where this ran with the
    /// same deltas the last mesh-group pass recorded. A `local_only` driver
    /// then reads `node.transform.translation` as the last final fold left it
    /// — base plus bindings plus that shift — and a world-anchored one reads
    /// the cached `physics_transforms`, built by that same pre-pass from base
    /// plus the same bindings plus the same stored deltas. Both match what a
    /// fresh pre-pass would produce, so neither pops. A `local_only` driver
    /// that is itself a shift target needs no special case of its own.
    pub(crate) fn apply_previous_tc_shifts(&mut self) {
        if self.tc_shift_nonzero == 0 {
            return;
        }
        for slot in 0..self.tc_shift_deltas.len() {
            let delta = self.tc_shift_deltas[slot];
            if delta == glam::Vec2::ZERO {
                continue;
            }
            if let Some(node) = self.nodes.get_mut(slot) {
                node.transform.translation.x += delta.x;
                node.transform.translation.y += delta.y;
            }
            if let Some(dirty) = self.node_transform_dirty.get_mut(slot) {
                *dirty = true;
            }
        }
    }

    pub(crate) fn ensure_physics_ancestor_mask(&mut self) {
        if self.physics_ancestor_mask.is_some() {
            return;
        }
        let mut mask = vec![false; self.nodes.len()];
        for &pid in self.physics_node_ids.iter().chain(&self.chain_node_ids) {
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
    /// their ancestors — the only slots the driver step reads (both the
    /// anchor and `world_inverse`). Same per-node logic as
    /// `compute_transforms_with_root` with `root = IDENTITY`; a member's
    /// parent is always a member, so parent transforms are ready when
    /// needed, and stale non-member slots are never read.
    pub(crate) fn compute_physics_ancestor_transforms(&self, out: &mut GlobalTransforms) {
        out.ensure_size(self.nodes.len());
        let mask = self.physics_ancestor_mask.as_deref().unwrap_or(&[]);
        self.tree.with_dfs_order(|order| {
            for &id in order {
                let slot = id.0 as usize;
                if !mask.get(slot).copied().unwrap_or(false) {
                    continue;
                }
                let node = self.nodes.get(slot);
                let local_matrix = self.local_matrix(slot, node);
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

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}
