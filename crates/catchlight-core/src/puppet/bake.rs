//! Compiling a [`Model`] into what a puppet evaluates.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use glam::{Vec2, Vec3};

use crate::components::{
    CompositeData, Mask, Mesh, MeshGroupData, MeshIndices, Node, NodeIdx, NodeKind, PartData,
    TextureIdx, Transform,
};
use crate::deform::DeformStack;
use crate::formats::clm::{ClmIndices, ClmMesh};
use crate::id::{NodeId, ParamId};
use crate::interpolate::InterpolateMode;
use crate::model::{BindingKey, BindingTarget, DenseGrid, Model, ModelNodeKind};

use super::arena::Arena;

/// Each baked binding owns its own bracketing positions.
#[derive(Clone)]
pub(super) struct BakedParam {
    pub(super) id: ParamId,
    pub(super) min: f32,
    pub(super) max: f32,
    pub(super) default: f32,
}

#[derive(Clone)]
pub(super) struct BakedBinding {
    pub(super) key_positions: Vec<Vec<f32>>,
    pub(super) node: NodeIdx,
    pub(super) target: BindingTarget,
    /// Second slot present when the binding spans two params.
    pub(super) x: u32,
    pub(super) y: Option<u32>,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) mode: InterpolateMode,
    /// Shared with [`Model::binding_dense`]'s memo.
    pub(super) grid: Arc<DenseGrid>,
    /// Unique per binding.
    pub(super) source: crate::deform::DeformSource,
    /// Per-cell: whether the cell holds the target's identity value.
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

#[derive(Clone)]
pub(super) struct Baked {
    pub(super) arena: Arena,
    pub(super) node_of_id: Arc<HashMap<NodeId, NodeIdx>>,
    pub(super) id_of_node: Arc<Vec<NodeId>>,
    pub(super) params: Arc<Vec<BakedParam>>,
    pub(super) slot_of_param: Arc<HashMap<ParamId, u32>>,
    pub(super) bindings: Arc<Vec<BakedBinding>>,
    pub(super) spine_targets: Arc<Vec<Vec<Option<u32>>>>,
}

/// A model's bake, held by the model it was baked from. [`Model::bump`]
/// clears it; a clone of the model carries it.
#[derive(Clone, Default)]
pub(crate) struct BakeMemo(OnceLock<Arc<Baked>>);

impl BakeMemo {
    pub(crate) fn clear(&mut self) {
        self.0.take();
    }

    /// `self` must be `model`'s own memo.
    pub(super) fn get_or_bake(&self, model: &Model) -> Baked {
        Baked::clone(self.0.get_or_init(|| Arc::new(bake(model))))
    }
}

impl std::fmt::Debug for BakeMemo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0.get() {
            Some(_) => "BakeMemo(baked)",
            None => "BakeMemo(empty)",
        })
    }
}

fn bake(model: &Model) -> Baked {
    let mut arena = Arena::new();
    let mut node_of_id = HashMap::with_capacity(model.node_count());
    let mut id_of_node = Vec::with_capacity(model.node_count());

    // The root is arena slot 0 already. An addon fragment has no single root,
    // so it bakes to a puppet holding only that empty slot.
    let Some(root_id) = model.root().cloned() else {
        return Baked {
            arena,
            node_of_id: Arc::new(node_of_id),
            id_of_node: Arc::new(id_of_node),
            params: Arc::default(),
            slot_of_param: Arc::default(),
            bindings: Arc::default(),
            spine_targets: Arc::default(),
        };
    };
    if let Some(root) = model.node(&root_id) {
        arena.nodes[0] = build_node(model, root);
        Arc::make_mut(&mut arena.base_local_matrix)[0] = arena.nodes[0].base_transform.to_matrix();
    }
    node_of_id.insert(root_id.clone(), arena.root());
    id_of_node.push(root_id.clone());

    // Pre-order, so a node's parent always has a slot before it does.
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
        let idx = arena.insert_child(parent, build_node(model, node));
        node_of_id.insert(id.clone(), idx);
        id_of_node.push(id.clone());
        push_children(&mut stack, node, idx);
    }
    arena.rebuild_kind_registries();

    // Last, because a mask may name a node after its target in the walk.
    for (slot, id) in id_of_node.iter().enumerate() {
        let Some(model_node) = model.node(id) else {
            continue;
        };
        let sources: Vec<Mask> = model_masks(model_node)
            .iter()
            .filter_map(|m| {
                node_of_id.get(m.source()).map(|&idx| Mask {
                    source: idx,
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

    // A slot not refilled since the part was re-meshed drops out rather than
    // pulling a vertex that now means something else.
    arena.welds = Arc::new(
        model
            .welds()
            .iter()
            .filter_map(|w| {
                Some(crate::weld::Weld {
                    a: *node_of_id.get(w.a())?,
                    b: *node_of_id.get(w.b())?,
                    pairs: w
                        .resolve(model)
                        .into_iter()
                        .map(|p| crate::weld::WeldPair {
                            a_vert: p.a_vert,
                            b_vert: p.b_vert,
                            weight: p.weight,
                        })
                        .collect(),
                })
            })
            .collect(),
    );

    arena.rebuild_all_mesh_group_pins();
    arena.rebuild_spine_rest_state();

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
        });
    }

    let mut bindings: Vec<BakedBinding> = Vec::new();
    for b in model.bindings() {
        let index = bindings.len();
        if let Some(baked) = bake_binding(model, b.key(), index, &node_of_id, &slot_of_param) {
            bindings.push(baked);
        }
    }

    let spine_targets: Vec<Vec<Option<u32>>> = arena
        .spine_node_ids
        .iter()
        .map(|&idx| {
            let id = &id_of_node[idx.0 as usize];
            match model.node(id).map(|n| &n.kind) {
                Some(ModelNodeKind::Spine(spine)) => spine
                    .targets()
                    .iter()
                    .map(|p| p.as_ref().and_then(|p| slot_of_param.get(p).copied()))
                    .collect(),
                _ => Vec::new(),
            }
        })
        .collect();

    Baked {
        arena,
        node_of_id: Arc::new(node_of_id),
        id_of_node: Arc::new(id_of_node),
        params: Arc::new(params),
        slot_of_param: Arc::new(slot_of_param),
        bindings: Arc::new(bindings),
        spine_targets: Arc::new(spine_targets),
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
        key_positions: model.binding(key)?.key_positions().to_vec(),
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

fn build_node(model: &Model, node: &crate::model::ModelNode) -> Node {
    let transform = Transform {
        translation: Vec3::from_array(node.transform.translation),
        rotation: Vec3::from_array(node.transform.rotation),
        scale: Vec2::from_array(node.transform.scale),
    };
    let kind = match &node.kind {
        ModelNodeKind::MotionResponse(_) => NodeKind::MotionResponse,
        ModelNodeKind::Group => NodeKind::Group,
        ModelNodeKind::Part(p) => {
            let mesh = Arc::new(build_mesh(p.mesh()));
            let deform_stack = DeformStack::new(mesh.vertices.len());
            NodeKind::Part(Box::new(PartData {
                mesh,
                albedo_texture: p
                    .albedo()
                    .and_then(|t| model.texture_ids().iter().position(|x| x == t))
                    .map(|i| TextureIdx(i as u32)),
                opacity: p.opacity,
                base_opacity: p.opacity,
                tint: Vec3::from_array(p.tint),
                base_tint: Vec3::from_array(p.tint),
                screen_tint: Vec3::from_array(p.screen_tint),
                base_screen_tint: Vec3::from_array(p.screen_tint),
                blend_mode: p.blend_mode,
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
            let mesh = Arc::new(build_mesh(mg.mesh()));
            let deform_stack = DeformStack::new(mesh.vertices.len());
            NodeKind::MeshGroup(Box::new(MeshGroupData {
                mesh,
                translate_children: mg.translate_children,
                deform_stack,
                pins: Default::default(),
                bins: None,
            }))
        }
        // Needs every descendant's rest globals; `Arena::rebuild_spine_rest_state`
        // fills it.
        ModelNodeKind::Spine(spine) => {
            let data = crate::spine::SpineData::new(
                spine
                    .joints()
                    .iter()
                    .map(|j| Vec2::new(j[0], j[1]))
                    .collect(),
            );
            NodeKind::Spine(Box::new(data))
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

fn build_mesh(m: &ClmMesh) -> Mesh {
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
        ClmIndices::U16(v) => MeshIndices::U16(v.clone()),
        ClmIndices::U32(v) => MeshIndices::U32(v.clone()),
    };
    Mesh::new(vertices, uvs, indices, Vec2::from_array(m.origin))
}
