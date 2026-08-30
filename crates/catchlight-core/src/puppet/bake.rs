//! Compiling a [`Model`] into what a puppet evaluates.
//!
//! Baking walks the model's tree in pre-order and fills the arena, so a
//! [`NodeIdx`] is a node's position in that walk. Everything a hot loop needs
//! is resolved here, once: Ids become slots, a binding's dense grid becomes an
//! `Arc` shared with the model's own memo, and each cell gets its identity
//! flag. Nothing here reads a pose — [`crate::puppet::Puppet`] carries the pose
//! across a rebake itself.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Vec2, Vec3};

use crate::components::{
    CompositeData, Mask, Mesh, MeshGroupData, MeshIndices, Node, NodeIdx, NodeKind, PartData,
    TextureId, Transform,
};
use crate::deform::DeformStack;
use crate::formats::clp::{ClpIndices, ClpMesh};
use crate::id::{NodeId, ParamId};
use crate::model::{BindingKey, BindingTarget, DenseGrid, Model, ModelNodeKind, ModelPhysics};
use crate::params::InterpolateMode;
use crate::physics::SimplePhysicsData;

use super::arena::Arena;

/// One param, flattened to the numbers the fold needs: where a value sits in
/// `[min, max]` and which key positions bracket it.
#[derive(Clone)]
pub(super) struct BakedParam {
    pub(super) id: ParamId,
    pub(super) min: f32,
    pub(super) max: f32,
    pub(super) default: f32,
    pub(super) key_positions: Vec<f32>,
}

/// One binding, resolved against the arena: which slot it writes, which param
/// slots index its grid, and the grid itself.
#[derive(Clone)]
pub(super) struct BakedBinding {
    pub(super) node: NodeIdx,
    pub(super) target: BindingTarget,
    /// Param slot along the grid's x axis, and along its y axis when the
    /// binding spans two params.
    pub(super) x: u32,
    pub(super) y: Option<u32>,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) mode: InterpolateMode,
    /// Shared with [`Model::binding_dense`]'s memo, so the fill is derived once
    /// however many puppets animate the model.
    pub(super) grid: Arc<DenseGrid>,
    /// The deform-stack slot this binding writes, unique per binding.
    pub(super) source: crate::deform::DeformSource,
    /// Per-cell: does the cell hold the target's identity value. The fold
    /// AND-folds the cells with non-zero interpolation weight to skip a
    /// binding whose output is guaranteed to be a no-op.
    pub(super) cell_zero: Vec<bool>,
}

impl BakedBinding {
    pub(super) fn cell_zero_at(&self, x: usize, y: usize) -> bool {
        if self.width == 0 || self.height == 0 {
            return false;
        }
        let cx = x.min(self.width - 1);
        let cy = y.min(self.height - 1);
        self.cell_zero[cy * self.width + cx]
    }
}

/// Everything [`Puppet::new`] derives from a model in one pass.
pub(super) struct Baked {
    pub(super) arena: Arena,
    pub(super) node_of_id: HashMap<NodeId, NodeIdx>,
    pub(super) id_of_node: Vec<NodeId>,
    pub(super) params: Vec<BakedParam>,
    pub(super) slot_of_param: HashMap<ParamId, u32>,
    pub(super) bindings: Vec<BakedBinding>,
    /// Parallel to `arena.physics_node_ids`: the param slots each driver
    /// writes, in the order its map mode produces them.
    pub(super) physics_targets: Vec<[Option<u32>; 2]>,
}

pub(super) fn bake(model: &Model) -> Baked {
    let mut arena = Arena::new();
    let mut node_of_id = HashMap::with_capacity(model.node_count());
    let mut id_of_node = Vec::with_capacity(model.node_count());

    // The root is arena slot 0, already there; fill it in place rather than
    // hanging a second node under it.
    let g_scale = model.physics().pixels_per_meter * model.physics().gravity;
    let root_id = model.root().clone();
    if let Some(root) = model.node(&root_id) {
        arena.nodes[0] = build_node(model, root, g_scale);
        arena.base_local_matrix[0] = arena.nodes[0].base_transform.to_matrix();
    }
    node_of_id.insert(root_id.clone(), arena.root());
    id_of_node.push(root_id.clone());

    // Pre-order, so a node's parent always has a slot before it does, and a
    // NodeIdx is the node's position in the walk.
    let mut stack: Vec<(NodeId, NodeIdx)> = Vec::new();
    let push_children =
        |stack: &mut Vec<(NodeId, NodeIdx)>, node: &crate::model::ModelNode, idx| {
            for child in node.children().iter().rev() {
                stack.push((child.clone(), idx));
            }
        };
    if let Some(root) = model.node(&root_id) {
        push_children(&mut stack, root, arena.root());
    }
    while let Some((id, parent)) = stack.pop() {
        let Some(node) = model.node(&id) else {
            continue;
        };
        let idx = arena.insert_child(parent, build_node(model, node, g_scale));
        node_of_id.insert(id.clone(), idx);
        id_of_node.push(id.clone());
        push_children(&mut stack, node, idx);
    }
    // `insert_child` registered every node but the root, whose kind was
    // written into slot 0 directly.
    arena.rebuild_kind_registries();

    // Masks name their source by NodeIdx: `Mask::source_uuid` is the legacy
    // uuid namespace, and a Model has none, so a baked mask carries the
    // source's arena slot. `Puppet::node_for_uuid` is the identity because of
    // it. The render cache task replaces the field outright.
    for (slot, id) in id_of_node.iter().enumerate() {
        let Some(model_node) = model.node(id) else {
            continue;
        };
        let sources: Vec<Mask> = model_masks(model_node)
            .iter()
            .filter_map(|m| {
                node_of_id.get(m.source()).map(|idx| Mask {
                    source_uuid: idx.0,
                    mode: m.mode(),
                })
            })
            .collect();
        match &mut arena.nodes[slot].kind {
            NodeKind::Part(p) => p.masks = sources,
            NodeKind::Composite(c) => c.masks = sources,
            _ => {}
        }
    }

    arena.welds = model
        .welds()
        .iter()
        .filter_map(|w| {
            Some(crate::weld::Weld {
                a: *node_of_id.get(w.a())?,
                b: *node_of_id.get(w.b())?,
                pairs: w
                    .pairs()
                    .iter()
                    .map(|p| crate::weld::WeldPair {
                        a_vert: p.a_vert,
                        b_vert: p.b_vert,
                        weight: p.weight,
                    })
                    .collect(),
            })
        })
        .collect();

    arena.rebuild_all_mesh_group_attachments();

    let mut params = Vec::with_capacity(model.param_ids().len());
    let mut slot_of_param = HashMap::with_capacity(model.param_ids().len());
    for id in model.param_ids() {
        let Some(p) = model.param(id) else { continue };
        slot_of_param.insert(id.clone(), params.len() as u32);
        params.push(BakedParam {
            id: id.clone(),
            min: p.min,
            max: p.max,
            default: p.default,
            key_positions: p.key_positions.clone(),
        });
    }

    let mut bindings: Vec<BakedBinding> = Vec::new();
    for b in model.bindings() {
        let index = bindings.len();
        if let Some(baked) = bake_binding(model, b.key(), index, &node_of_id, &slot_of_param) {
            bindings.push(baked);
        }
    }

    let physics_targets = arena
        .physics_node_ids
        .iter()
        .map(|&idx| {
            let id = &id_of_node[idx.0 as usize];
            match model.node(id).map(|n| &n.kind) {
                Some(ModelNodeKind::SimplePhysics(ph)) => {
                    let slot = |p: &Option<ParamId>| {
                        p.as_ref().and_then(|p| slot_of_param.get(p).copied())
                    };
                    let t = ph.target_params();
                    [slot(&t[0]), slot(&t[1])]
                }
                _ => [None, None],
            }
        })
        .collect();

    Baked {
        arena,
        node_of_id,
        id_of_node,
        params,
        slot_of_param,
        bindings,
        physics_targets,
    }
}

fn model_masks(node: &crate::model::ModelNode) -> &[crate::model::ModelMask] {
    match &node.kind {
        ModelNodeKind::Part(p) => p.masks(),
        ModelNodeKind::Composite(c) => c.masks(),
        _ => &[],
    }
}

fn bake_binding(
    model: &Model,
    key: &BindingKey,
    index: usize,
    node_of_id: &HashMap<NodeId, NodeIdx>,
    slot_of_param: &HashMap<ParamId, u32>,
) -> Option<BakedBinding> {
    let node = *node_of_id.get(&key.node)?;
    let x = *slot_of_param.get(key.params.x())?;
    let y = match key.params.y() {
        Some(p) => Some(*slot_of_param.get(p)?),
        None => None,
    };
    let (width, height) = model.binding_grid(key).ok()?;
    let (width, height) = (width as usize, height as usize);
    let grid = model.binding_dense(key)?.clone();
    let identity = match key.target {
        BindingTarget::Deform => 0.0,
        BindingTarget::Scalar(t) => t.identity(),
    };
    let cell_zero = match &*grid {
        DenseGrid::Scalar(cells) => cells.iter().map(|v| *v == identity).collect(),
        DenseGrid::Deform(cells) => cells
            .iter()
            .map(|cell| cell.iter().all(|v| *v == 0.0))
            .collect(),
    };
    Some(BakedBinding {
        node,
        target: key.target,
        x,
        y,
        width,
        height,
        mode: model.binding(key)?.interpolate_mode(),
        source: super::fold::deform_source(index),
        grid,
        cell_zero,
    })
}

fn build_node(model: &Model, node: &crate::model::ModelNode, g_scale: f32) -> Node {
    let transform = Transform {
        translation: Vec3::from_array(node.transform.translation),
        rotation: Vec3::from_array(node.transform.rotation),
        scale: Vec2::from_array(node.transform.scale),
    };
    let kind = match &node.kind {
        ModelNodeKind::Group => NodeKind::Group,
        ModelNodeKind::Part(p) => {
            let mesh = build_mesh(p.mesh());
            let deform_stack = DeformStack::new(mesh.vertices.len());
            NodeKind::Part(Box::new(PartData {
                mesh,
                albedo_texture: TextureId(
                    p.albedo()
                        .and_then(|t| model.texture_ids().iter().position(|x| x == t))
                        .map(|i| i as u32)
                        .unwrap_or(u32::MAX),
                ),
                opacity: p.opacity,
                base_opacity: p.opacity,
                tint: Vec3::from_array(p.tint),
                base_tint: Vec3::from_array(p.tint),
                screen_tint: Vec3::from_array(p.screen_tint),
                base_screen_tint: Vec3::from_array(p.screen_tint),
                blend_mode: p.blend_mode,
                // Filled once every node has a slot.
                masks: Vec::new(),
                mask_threshold: p.mask_threshold,
                deform_stack,
            }))
        }
        ModelNodeKind::Composite(c) => NodeKind::Composite(Box::new(CompositeData {
            opacity: c.opacity,
            base_opacity: c.opacity,
            tint: Vec3::from_array(c.tint),
            base_tint: Vec3::from_array(c.tint),
            screen_tint: Vec3::from_array(c.screen_tint),
            base_screen_tint: Vec3::from_array(c.screen_tint),
            blend_mode: c.blend_mode,
            masks: Vec::new(),
            propagate_mesh_group: c.propagate_meshgroup,
            mask_threshold: c.mask_threshold,
        })),
        ModelNodeKind::MeshGroup(mg) => {
            let mesh = build_mesh(mg.mesh());
            let deform_stack = DeformStack::new(mesh.vertices.len());
            NodeKind::MeshGroup(Box::new(MeshGroupData {
                mesh,
                dynamic: mg.dynamic,
                translate_children: mg.translate_children,
                deform_stack,
                attachments: Default::default(),
                bitmap: None,
            }))
        }
        ModelNodeKind::SimplePhysics(ph) => {
            NodeKind::SimplePhysics(Box::new(build_physics(ph, &transform, g_scale)))
        }
    };
    Node {
        name: node.name.as_str().to_string(),
        enabled: node.enabled,
        base_transform: transform,
        base_z_order: node.z_order,
        transform,
        z_order: node.z_order,
        lock_to_root: node.lock_to_root,
        kind,
    }
}

/// The authored driver plus a rest state. `target_param_id` stays `None`: it
/// is the legacy uuid namespace, and this runtime resolves a driver's targets
/// through [`Baked::physics_targets`] instead.
fn build_physics(ph: &ModelPhysics, transform: &Transform, g_scale: f32) -> SimplePhysicsData {
    let anchor = Vec2::new(transform.translation.x, transform.translation.y);
    SimplePhysicsData {
        kind: ph.kind,
        map_mode: ph.map_mode,
        local_only: ph.local_only,
        target_param_id: None,
        // The model stores authored, unscaled gravity; the integrator wants it
        // pre-folded with the model-level pixelsPerMeter x gravity.
        gravity: ph.gravity * g_scale,
        length: ph.length,
        frequency: ph.frequency,
        angle_damping: ph.angle_damping,
        length_damping: ph.length_damping,
        output_scale: Vec2::from_array(ph.output_scale),
        offset_output_scale: Vec2::ONE,
        bob: anchor + Vec2::new(0.0, ph.length),
        spring_vel: Vec2::ZERO,
        d_angle: 0.0,
        anchor,
        anchor_initialized: false,
    }
}

fn build_mesh(m: &ClpMesh) -> Mesh {
    let vertices: Vec<Vec2> = m
        .verts
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Vec2::new(c[0], c[1]))
        .collect();
    let uvs: Vec<Vec2> = m
        .uvs
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Vec2::new(c[0], c[1]))
        .collect();
    let indices = match &m.indices {
        ClpIndices::U16(v) => MeshIndices::U16(v.clone()),
        ClpIndices::U32(v) => MeshIndices::U32(v.clone()),
    };
    Mesh::new(vertices, uvs, indices, Vec2::from_array(m.origin))
}
