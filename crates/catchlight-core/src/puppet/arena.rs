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
//! it came from: mesh-group attachments, the triangle bitmap, and the base
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
    pub(crate) mesh_group_node_ids: Vec<NodeIdx>,
    // Puppet-local (root=IDENTITY) transform scratch used when sampling
    // SimplePhysics anchors. Decouples the physics integrator from any
    // host world-scale: pendulum length and gravity are loaded in
    // puppet-local units, so anchors must be in matching units.
    pub(crate) physics_transforms: GlobalTransforms,
    /// Cached `physics_anchor_skip_allowed`. Depends only on tree
    /// structure + node flags, so invalidated on `insert_child`.
    physics_anchor_skip_cached: Option<bool>,
    /// Slots that are a SimplePhysics node or an ancestor of one — the
    /// only slots the physics pre-pass transform walk needs to fill (see
    /// `compute_physics_ancestor_transforms`). Invalidated on
    /// `insert_child`.
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
            mesh_group_node_ids: Vec::new(),
            physics_transforms: GlobalTransforms::new(),
            physics_anchor_skip_cached: None,
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

    /// Rebuild the three kind registries from the current node kinds.
    pub(crate) fn rebuild_kind_registries(&mut self) {
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
        self.invalidate_structure_caches();
    }

    /// Drop every cache that depends on tree shape or node kinds.
    pub(crate) fn invalidate_structure_caches(&mut self) {
        self.mg_pre_order_cache = None;
        self.physics_anchor_skip_cached = None;
        self.physics_ancestor_mask = None;
    }

    pub(crate) fn rebuild_all_mesh_group_attachments(&mut self) {
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

    pub(crate) fn propagate_mesh_group_deforms(&mut self, transforms: &GlobalTransforms) {
        let _span = tracing::trace_span!("propagate_mesh_group_deforms").entered();
        crate::meshgroup::propagate_mesh_group_deforms(self, transforms);
    }

    /// Solve every weld into the parts' `DeformSource::Weld` slots. Call
    /// after `propagate_mesh_group_deforms` and before `combine_deforms`.
    pub(crate) fn apply_welds(&mut self, transforms: &GlobalTransforms) {
        crate::weld::apply_welds(self, transforms);
    }

    /// Run the `translateChildren=true` MG filter on each tc=true MG's
    /// descendants without a mesh (Origin Nodes, Group Nodes, SimplePhysics
    /// nodes). Returns whether any target was shifted; when false the
    /// caller's transforms are still valid and the re-walk can be skipped.
    pub(crate) fn apply_translate_children_filter(
        &mut self,
        transforms: &GlobalTransforms,
    ) -> bool {
        let _span = tracing::trace_span!("apply_translate_children_filter").entered();
        crate::meshgroup::apply_translate_children_filter(self, transforms)
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
        // A new node changes the physics ancestor set, the tc-target guard and
        // the mesh-group pre-order.
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
                // Skip Transform::to_matrix() if (a) the fold didn't
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

    /// Anchor point for a physics driver in the driver's own **Y-down**
    /// frame (gravity toward +Y, matching the reference pendulum). The
    /// node world is Y-up, so the Y is flipped here; the driver's output
    /// conjugates `world_inverse` by the same flip to undo it.
    ///
    /// The `local_only` branch uses `node.transform.translation`, including
    /// parameter-driven offsets from the anchor pre-pass rather than the
    /// frozen load pose.
    pub(crate) fn physics_anchor(
        &self,
        transforms: &GlobalTransforms,
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
    pub(crate) fn physics_anchor_skip_allowed(&mut self) -> bool {
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

    pub(crate) fn ensure_physics_ancestor_mask(&mut self) {
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

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}
