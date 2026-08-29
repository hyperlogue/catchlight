use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use slotmap::{new_key_type, SlotMap};

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clp::{
    self as clp, ClpBindingValues, ClpMesh, ClpMeshGroup, ClpPhysics, ClpTransform, TextureAlpha,
    TextureEncoding,
};
use catchlight_core::params::InterpolateMode;
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};

use crate::EditError;

new_key_type! {
    pub struct NodeId;
    pub struct ParamId;
    pub struct TexId;
}

/// Opaque `u64` conversions for the wire/handle layer (the server maps these
/// to/from protocol refs); the encoding is slotmap's, callers only round-trip it.
macro_rules! ffi_id {
    ($t:ty) => {
        impl $t {
            pub fn to_ffi(self) -> u64 {
                slotmap::Key::data(&self).as_ffi()
            }
            pub fn from_ffi(v: u64) -> Self {
                slotmap::KeyData::from_ffi(v).into()
            }
        }
    };
}
ffi_id!(NodeId);
ffi_id!(ParamId);
ffi_id!(TexId);

/// A source-encoded texture (verbatim PNG/TGA bytes), shared via `Arc` so model
/// snapshots are cheap to clone even when the structure churns.
#[derive(Debug, Clone)]
pub struct EditTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: Arc<Vec<u8>>,
}

/// The editable puppet: a tree of nodes by stable id, ordered params and
/// textures, and authored physics. The tree is always valid (single root, no
/// cycles, no dangling cross-references), so [`EditModel::flatten`] is total.
#[derive(Debug, Clone)]
pub struct EditModel {
    pub physics: ClpPhysics,
    pub welds: Vec<EditWeld>,
    pub(crate) nodes: SlotMap<NodeId, EditNode>,
    pub(crate) root: NodeId,
    pub(crate) params: SlotMap<ParamId, EditParam>,
    pub(crate) param_order: Vec<ParamId>,
    pub(crate) textures: SlotMap<TexId, EditTexture>,
    pub(crate) texture_order: Vec<TexId>,
}

/// A welded Part pair (`ClpWeld` with stable node ids).
#[derive(Debug, Clone)]
pub struct EditWeld {
    pub a: NodeId,
    pub b: NodeId,
    pub pairs: Arc<Vec<clp::ClpWeldPair>>,
}

#[derive(Debug, Clone)]
pub struct EditNode {
    pub name: String,
    pub enabled: bool,
    pub z_order: f32,
    pub transform: ClpTransform,
    pub lock_to_root: bool,
    pub kind: EditNodeKind,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
}

#[derive(Debug, Clone)]
pub enum EditNodeKind {
    Empty,
    Part(EditPart),
    Composite(EditComposite),
    MeshGroup(EditMeshGroup),
    SimplePhysics(EditPhysics),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditMesh(Arc<ClpMesh>);

impl EditMesh {
    pub fn to_clp(&self) -> ClpMesh {
        (*self.0).clone()
    }
}

impl From<ClpMesh> for EditMesh {
    fn from(mesh: ClpMesh) -> Self {
        Self(Arc::new(mesh))
    }
}

impl Deref for EditMesh {
    type Target = ClpMesh;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for EditMesh {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

/// A mesh group deforms what is beneath it and is never drawn, so it has no
/// colour to edit: no opacity, blend mode, tint or screen tint.
#[derive(Debug, Clone)]
pub struct EditMeshGroup {
    pub mesh: EditMesh,
    pub dynamic: bool,
    pub translate_children: bool,
}

impl EditMeshGroup {
    pub fn from_clp(group: &ClpMeshGroup) -> Self {
        Self {
            mesh: group.mesh.clone().into(),
            dynamic: group.dynamic,
            translate_children: group.translate_children,
        }
    }

    pub fn to_clp(&self) -> ClpMeshGroup {
        ClpMeshGroup {
            mesh: self.mesh.to_clp(),
            dynamic: self.dynamic,
            translate_children: self.translate_children,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EditPart {
    pub mesh: EditMesh,
    /// Albedo texture, or `None` for an unmapped part (the renderer culls it).
    pub albedo: Option<TexId>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub masks: Vec<EditMask>,
    pub mask_threshold: f32,
}

#[derive(Debug, Clone)]
pub struct EditComposite {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub masks: Vec<EditMask>,
    pub mask_threshold: f32,
    pub propagate_meshgroup: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct EditMask {
    pub source: NodeId,
    pub mode: MaskMode,
}

#[derive(Debug, Clone)]
pub struct EditPhysics {
    pub kind: PendulumKind,
    pub map_mode: PhysicsParamMapMode,
    pub local_only: bool,
    pub target_param: Option<ParamId>,
    pub gravity: f32,
    pub length: f32,
    pub frequency: f32,
    pub angle_damping: f32,
    pub length_damping: f32,
    pub output_scale: [f32; 2],
}

#[derive(Debug, Clone)]
pub struct EditParam {
    pub name: String,
    pub is_vec2: bool,
    pub min: [f32; 2],
    pub max: [f32; 2],
    /// Rest pose, in param-value space (unlike the axis points).
    pub defaults: [f32; 2],
    /// Keypoint positions, normalized 0..1 across `[min, max]` — the same
    /// convention `.clp` stores and the runtime interpolates in.
    pub axis_points_x: Vec<f32>,
    pub axis_points_y: Vec<f32>,
    pub bindings: Vec<EditBinding>,
}

#[derive(Debug, Clone)]
pub struct EditBinding {
    pub node: NodeId,
    pub interpolate_mode: InterpolateMode,
    pub values: EditBindingValues,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditBindingValues(Arc<ClpBindingValues>);

impl EditBindingValues {
    pub fn to_clp(&self) -> ClpBindingValues {
        (*self.0).clone()
    }
}

impl From<ClpBindingValues> for EditBindingValues {
    fn from(values: ClpBindingValues) -> Self {
        Self(Arc::new(values))
    }
}

impl Deref for EditBindingValues {
    type Target = ClpBindingValues;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for EditBindingValues {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl EditNode {
    /// A node at the identity transform (unit scale), enabled, z order 0.
    pub fn new(name: impl Into<String>, kind: EditNodeKind) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            z_order: 0.0,
            transform: ClpTransform {
                translation: [0.0; 3],
                rotation: [0.0; 3],
                scale: [1.0, 1.0],
            },
            lock_to_root: false,
            kind,
            parent: None,
            children: Vec::new(),
        }
    }

    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    pub fn children(&self) -> &[NodeId] {
        &self.children
    }
}

impl EditModel {
    /// A new puppet with a single `Empty` root named "Root".
    pub fn new() -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(EditNode::new("Root", EditNodeKind::Empty));
        Self {
            physics: ClpPhysics::default(),
            welds: Vec::new(),
            nodes,
            root,
            params: SlotMap::with_key(),
            param_order: Vec::new(),
            textures: SlotMap::with_key(),
            texture_order: Vec::new(),
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> Option<&EditNode> {
        self.nodes.get(id)
    }

    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut EditNode> {
        self.nodes.get_mut(id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn param_ids(&self) -> &[ParamId] {
        &self.param_order
    }

    pub fn param(&self, id: ParamId) -> Option<&EditParam> {
        self.params.get(id)
    }

    pub fn param_mut(&mut self, id: ParamId) -> Option<&mut EditParam> {
        self.params.get_mut(id)
    }

    pub fn texture_ids(&self) -> &[TexId] {
        &self.texture_order
    }

    pub fn texture(&self, id: TexId) -> Option<&EditTexture> {
        self.textures.get(id)
    }

    pub fn estimated_size_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(
                self.nodes
                    .len()
                    .saturating_mul(std::mem::size_of::<EditNode>()),
            )
            .saturating_add(
                self.params
                    .len()
                    .saturating_mul(std::mem::size_of::<EditParam>()),
            )
            .saturating_add(
                self.textures
                    .len()
                    .saturating_mul(std::mem::size_of::<EditTexture>()),
            )
            .saturating_add(
                self.param_order
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ParamId>()),
            )
            .saturating_add(
                self.texture_order
                    .capacity()
                    .saturating_mul(std::mem::size_of::<TexId>()),
            )
            .saturating_add(
                self.welds
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EditWeld>()),
            );
        for weld in &self.welds {
            bytes = bytes.saturating_add(
                weld.pairs
                    .capacity()
                    .saturating_mul(std::mem::size_of::<clp::ClpWeldPair>()),
            );
        }
        for node in self.nodes.values() {
            bytes = bytes.saturating_add(node.name.capacity()).saturating_add(
                node.children
                    .capacity()
                    .saturating_mul(std::mem::size_of::<NodeId>()),
            );
            match &node.kind {
                EditNodeKind::Part(part) => {
                    bytes = bytes.saturating_add(mesh_size(&part.mesh)).saturating_add(
                        part.masks
                            .capacity()
                            .saturating_mul(std::mem::size_of::<EditMask>()),
                    );
                }
                EditNodeKind::Composite(composite) => {
                    bytes = bytes.saturating_add(
                        composite
                            .masks
                            .capacity()
                            .saturating_mul(std::mem::size_of::<EditMask>()),
                    );
                }
                EditNodeKind::MeshGroup(group) => {
                    bytes = bytes.saturating_add(mesh_size(&group.mesh));
                }
                EditNodeKind::Empty | EditNodeKind::SimplePhysics(_) => {}
            }
        }
        for param in self.params.values() {
            bytes = bytes
                .saturating_add(param.name.capacity())
                .saturating_add(
                    param
                        .axis_points_x
                        .capacity()
                        .saturating_mul(std::mem::size_of::<f32>()),
                )
                .saturating_add(
                    param
                        .axis_points_y
                        .capacity()
                        .saturating_mul(std::mem::size_of::<f32>()),
                )
                .saturating_add(
                    param
                        .bindings
                        .capacity()
                        .saturating_mul(std::mem::size_of::<EditBinding>()),
                );
            for binding in &param.bindings {
                bytes = bytes.saturating_add(binding_values_size(&binding.values));
            }
        }
        for texture in self.textures.values() {
            bytes = bytes.saturating_add(texture.data.capacity());
        }
        bytes
    }

    /// The node's editable mesh, for the two kinds that carry one.
    pub fn node_mesh(&self, id: NodeId) -> Option<&ClpMesh> {
        match self.node(id).map(|n| &n.kind)? {
            EditNodeKind::Part(p) => Some(&p.mesh),
            EditNodeKind::MeshGroup(mg) => Some(&mg.mesh),
            _ => None,
        }
    }

    /// Nodes in topological pre-order from the root, each parent before its
    /// children, following sibling order. This is the order [`Self::flatten`]
    /// snapshots into the arena.
    pub fn nodes_in_order(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(n) = self.nodes.get(id) {
                for &child in n.children.iter().rev() {
                    stack.push(child);
                }
            }
        }
        out
    }

    /// Insert `node` as the last child of `parent`. The node's own parent/child
    /// links are set here regardless of how it was constructed.
    pub fn add_node(&mut self, parent: NodeId, mut node: EditNode) -> Result<NodeId, EditError> {
        if !self.nodes.contains_key(parent) {
            return Err(EditError::UnknownNode);
        }
        node.parent = Some(parent);
        node.children.clear();
        let id = self.nodes.insert(node);
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(id);
        }
        Ok(id)
    }

    /// Remove a node and its whole subtree, then drop every mask and binding
    /// that pointed into the removed set so the model stays referentially valid.
    pub fn delete_node(&mut self, id: NodeId) -> Result<(), EditError> {
        if id == self.root {
            return Err(EditError::Root("deleted"));
        }
        if !self.nodes.contains_key(id) {
            return Err(EditError::UnknownNode);
        }
        let removed: Vec<NodeId> = self.subtree(id);
        let removed_set: HashSet<NodeId> = removed.iter().copied().collect();
        if let Some(parent) = self.nodes.get(id).and_then(|n| n.parent) {
            if let Some(p) = self.nodes.get_mut(parent) {
                p.children.retain(|&c| c != id);
            }
        }
        for r in &removed {
            self.nodes.remove(*r);
        }
        for node in self.nodes.values_mut() {
            match &mut node.kind {
                EditNodeKind::Part(p) => p.masks.retain(|m| !removed_set.contains(&m.source)),
                EditNodeKind::Composite(c) => c.masks.retain(|m| !removed_set.contains(&m.source)),
                _ => {}
            }
        }
        for param in self.params.values_mut() {
            param.bindings.retain(|b| !removed_set.contains(&b.node));
        }
        self.welds
            .retain(|w| !removed_set.contains(&w.a) && !removed_set.contains(&w.b));
        Ok(())
    }

    /// Move `id` (and its subtree) under `new_parent`. Rejects moving the root
    /// or creating a cycle.
    pub fn reparent(&mut self, id: NodeId, new_parent: NodeId) -> Result<(), EditError> {
        if id == self.root {
            return Err(EditError::Root("reparented"));
        }
        if !self.nodes.contains_key(id) || !self.nodes.contains_key(new_parent) {
            return Err(EditError::UnknownNode);
        }
        if self.is_self_or_descendant(new_parent, id) {
            return Err(EditError::Cycle);
        }
        let old_parent = self.nodes.get(id).and_then(|n| n.parent);
        if let Some(op) = old_parent {
            if let Some(p) = self.nodes.get_mut(op) {
                p.children.retain(|&c| c != id);
            }
        }
        if let Some(p) = self.nodes.get_mut(new_parent) {
            p.children.push(id);
        }
        if let Some(n) = self.nodes.get_mut(id) {
            n.parent = Some(new_parent);
        }
        Ok(())
    }

    /// Move `id` to `index` within its parent's children (clamped to the end).
    /// Sibling order is draw-list order for equal z order, so this is a document
    /// edit, not view state.
    pub fn reorder(&mut self, id: NodeId, index: usize) -> Result<(), EditError> {
        if id == self.root {
            return Err(EditError::Root("reordered"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent)
            .ok_or(EditError::UnknownNode)?;
        let p = self.nodes.get_mut(parent).ok_or(EditError::UnknownNode)?;
        let cur = p
            .children
            .iter()
            .position(|&c| c == id)
            .ok_or(EditError::UnknownNode)?;
        p.children.remove(cur);
        let index = index.min(p.children.len());
        p.children.insert(index, id);
        Ok(())
    }

    /// Deep-copy `id`'s subtree as its next sibling. Mask references inside the
    /// subtree point at the copies; external ones stay shared. Each copied node
    /// also copies its param bindings, so the duplicate deforms like the
    /// original. The copy's root is renamed "<name> copy".
    pub fn duplicate_subtree(&mut self, id: NodeId) -> Result<NodeId, EditError> {
        if id == self.root {
            return Err(EditError::Root("duplicated"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent)
            .ok_or(EditError::UnknownNode)?;

        // Pre-order with sibling order preserved (parent precedes children).
        let mut order = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            order.push(n);
            if let Some(node) = self.nodes.get(n) {
                for &c in node.children.iter().rev() {
                    stack.push(c);
                }
            }
        }

        let mut map: HashMap<NodeId, NodeId> = HashMap::new();
        for &old in &order {
            let mut copy = self.nodes.get(old).ok_or(EditError::UnknownNode)?.clone();
            let new_parent = match copy.parent {
                Some(p) if old != id => *map.get(&p).ok_or(EditError::UnknownNode)?,
                _ => parent,
            };
            if old == id {
                copy.name = format!("{} copy", copy.name);
            }
            let new_id = self.add_node(new_parent, copy)?;
            map.insert(old, new_id);
        }

        for &new_id in map.values() {
            if let Some(node) = self.nodes.get_mut(new_id) {
                let masks = match &mut node.kind {
                    EditNodeKind::Part(p) => Some(&mut p.masks),
                    EditNodeKind::Composite(c) => Some(&mut c.masks),
                    _ => None,
                };
                if let Some(masks) = masks {
                    for m in masks.iter_mut() {
                        if let Some(&mapped) = map.get(&m.source) {
                            m.source = mapped;
                        }
                    }
                }
            }
        }

        for param in self.params.values_mut() {
            let copied: Vec<EditBinding> = param
                .bindings
                .iter()
                .filter_map(|b| {
                    map.get(&b.node).map(|&new_node| EditBinding {
                        node: new_node,
                        interpolate_mode: b.interpolate_mode,
                        values: b.values.clone(),
                    })
                })
                .collect();
            param.bindings.extend(copied);
        }

        let new_root = *map.get(&id).ok_or(EditError::UnknownNode)?;
        let pos = self
            .nodes
            .get(parent)
            .and_then(|p| p.children.iter().position(|&c| c == id))
            .ok_or(EditError::UnknownNode)?;
        self.reorder(new_root, pos + 1)?;
        Ok(new_root)
    }

    fn masks_mut(&mut self, id: NodeId) -> Result<&mut Vec<EditMask>, EditError> {
        match self.nodes.get_mut(id).map(|n| &mut n.kind) {
            Some(EditNodeKind::Part(p)) => Ok(&mut p.masks),
            Some(EditNodeKind::Composite(c)) => Ok(&mut c.masks),
            Some(_) => Err(EditError::NotMaskable),
            None => Err(EditError::UnknownNode),
        }
    }

    /// Append a mask source. Sources must be Parts (the renderer rasterizes a
    /// source's own mesh + texture into the mask).
    pub fn mask_add(
        &mut self,
        id: NodeId,
        source: NodeId,
        mode: MaskMode,
    ) -> Result<(), EditError> {
        if source == id {
            return Err(EditError::SelfMask);
        }
        match self.nodes.get(source).map(|n| &n.kind) {
            Some(EditNodeKind::Part(_)) => {}
            Some(_) => return Err(EditError::NotAPart),
            None => return Err(EditError::UnknownNode),
        }
        self.masks_mut(id)?.push(EditMask { source, mode });
        Ok(())
    }

    pub fn mask_set_mode(
        &mut self,
        id: NodeId,
        index: usize,
        mode: MaskMode,
    ) -> Result<(), EditError> {
        let masks = self.masks_mut(id)?;
        let m = masks.get_mut(index).ok_or(EditError::IndexOutOfRange)?;
        m.mode = mode;
        Ok(())
    }

    /// Move the mask at `index` to `to` (clamped). Mask order is evaluation
    /// order, so this is a document edit.
    pub fn mask_reorder(&mut self, id: NodeId, index: usize, to: usize) -> Result<(), EditError> {
        let masks = self.masks_mut(id)?;
        if index >= masks.len() {
            return Err(EditError::IndexOutOfRange);
        }
        let m = masks.remove(index);
        let to = to.min(masks.len());
        masks.insert(to, m);
        Ok(())
    }

    pub fn mask_delete(&mut self, id: NodeId, index: usize) -> Result<(), EditError> {
        let masks = self.masks_mut(id)?;
        if index >= masks.len() {
            return Err(EditError::IndexOutOfRange);
        }
        masks.remove(index);
        Ok(())
    }

    pub fn add_param(&mut self, param: EditParam) -> ParamId {
        let id = self.params.insert(param);
        self.param_order.push(id);
        id
    }

    /// Remove a param and null out any physics node that drove it.
    pub fn delete_param(&mut self, id: ParamId) -> Result<(), EditError> {
        if self.params.remove(id).is_none() {
            return Err(EditError::UnknownParam);
        }
        self.param_order.retain(|&p| p != id);
        for node in self.nodes.values_mut() {
            if let EditNodeKind::SimplePhysics(ph) = &mut node.kind {
                if ph.target_param == Some(id) {
                    ph.target_param = None;
                }
            }
        }
        Ok(())
    }

    pub fn add_texture(&mut self, texture: EditTexture) -> TexId {
        let id = self.textures.insert(texture);
        self.texture_order.push(id);
        id
    }

    /// Remove a texture and unmap any part that referenced it.
    pub fn delete_texture(&mut self, id: TexId) -> Result<(), EditError> {
        if self.textures.remove(id).is_none() {
            return Err(EditError::UnknownTexture);
        }
        self.texture_order.retain(|&t| t != id);
        for node in self.nodes.values_mut() {
            if let EditNodeKind::Part(p) = &mut node.kind {
                if p.albedo == Some(id) {
                    p.albedo = None;
                }
            }
        }
        Ok(())
    }

    fn subtree(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(n) = self.nodes.get(id) {
                stack.extend(n.children.iter().copied());
            }
        }
        out
    }

    fn is_self_or_descendant(&self, candidate: NodeId, ancestor: NodeId) -> bool {
        let mut cur = Some(candidate);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            cur = self.nodes.get(c).and_then(|n| n.parent);
        }
        false
    }
}

fn mesh_size(mesh: &ClpMesh) -> usize {
    let indices = match &mesh.indices {
        clp::ClpIndices::U16(indices) => indices
            .capacity()
            .saturating_mul(std::mem::size_of::<u16>()),
        clp::ClpIndices::U32(indices) => indices
            .capacity()
            .saturating_mul(std::mem::size_of::<u32>()),
    };
    mesh.verts
        .capacity()
        .saturating_add(mesh.uvs.capacity())
        .saturating_mul(std::mem::size_of::<f32>())
        .saturating_add(indices)
}

fn binding_values_size(values: &ClpBindingValues) -> usize {
    use ClpBindingValues as V;
    match values {
        V::Deform(cells) => cells
            .cells
            .capacity()
            .saturating_mul(std::mem::size_of::<clp::ClpCell<Vec<f32>>>())
            .saturating_add(cells.cells.iter().fold(0usize, |bytes, cell| {
                bytes.saturating_add(
                    cell.value
                        .capacity()
                        .saturating_mul(std::mem::size_of::<f32>()),
                )
            })),
        V::ZOrder(cells)
        | V::TransformTX(cells)
        | V::TransformTY(cells)
        | V::TransformSX(cells)
        | V::TransformSY(cells)
        | V::TransformRX(cells)
        | V::TransformRY(cells)
        | V::TransformRZ(cells)
        | V::Opacity(cells)
        | V::TintR(cells)
        | V::TintG(cells)
        | V::TintB(cells)
        | V::ScreenTintR(cells)
        | V::ScreenTintG(cells)
        | V::ScreenTintB(cells)
        | V::OutputScaleX(cells)
        | V::OutputScaleY(cells) => cells
            .cells
            .capacity()
            .saturating_mul(std::mem::size_of::<clp::ClpCell<f32>>()),
    }
}

impl Default for EditModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_moves_within_siblings_and_clamps() {
        let mut m = EditModel::new();
        let root = m.root();
        let a = m
            .add_node(root, EditNode::new("a", EditNodeKind::Empty))
            .unwrap();
        let b = m
            .add_node(root, EditNode::new("b", EditNodeKind::Empty))
            .unwrap();
        let c = m
            .add_node(root, EditNode::new("c", EditNodeKind::Empty))
            .unwrap();

        m.reorder(c, 0).unwrap();
        assert_eq!(m.node(root).unwrap().children(), &[c, a, b]);

        // an out-of-range index clamps to the end.
        m.reorder(c, 99).unwrap();
        assert_eq!(m.node(root).unwrap().children(), &[a, b, c]);

        m.reorder(b, 1).unwrap();
        assert_eq!(m.node(root).unwrap().children(), &[a, b, c]);

        assert!(m.reorder(root, 0).is_err());
    }

    fn part() -> EditNodeKind {
        EditNodeKind::Part(EditPart {
            mesh: ClpMesh::default().into(),
            albedo: None,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            masks: Vec::new(),
            mask_threshold: 0.5,
        })
    }

    #[test]
    fn model_snapshots_share_large_payloads_until_mutated() {
        let mut model = EditModel::new();
        let node = model
            .add_node(
                model.root(),
                EditNode::new(
                    "mesh",
                    EditNodeKind::Part(EditPart {
                        mesh: ClpMesh {
                            verts: vec![-1.0, 0.0, 1.0, 0.0],
                            uvs: vec![0.0; 4],
                            indices: clp::ClpIndices::U16(Vec::new()),
                            origin: [0.0; 2],
                        }
                        .into(),
                        albedo: None,
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        tint: [1.0; 3],
                        screen_tint: [0.0; 3],
                        masks: Vec::new(),
                        mask_threshold: 0.5,
                    }),
                ),
            )
            .unwrap();
        let param = model.add_param(EditParam {
            name: "deform".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: Vec::new(),
        });
        model
            .set_deform_vertices(param, node, [1, 0], vec![1.0; 4])
            .unwrap();

        let mut edited = model.clone();
        let original_mesh = match &model.node(node).unwrap().kind {
            EditNodeKind::Part(part) => &part.mesh,
            _ => unreachable!(),
        };
        let edited_mesh = match &edited.node(node).unwrap().kind {
            EditNodeKind::Part(part) => &part.mesh,
            _ => unreachable!(),
        };
        assert!(Arc::ptr_eq(&original_mesh.0, &edited_mesh.0));
        assert!(Arc::ptr_eq(
            &model.param(param).unwrap().bindings[0].values.0,
            &edited.param(param).unwrap().bindings[0].values.0,
        ));

        edited.node_mut(node).unwrap().name = "renamed".into();
        let edited_mesh = match &edited.node(node).unwrap().kind {
            EditNodeKind::Part(part) => &part.mesh,
            _ => unreachable!(),
        };
        assert!(Arc::ptr_eq(&original_mesh.0, &edited_mesh.0));

        if let EditNodeKind::Part(part) = &mut edited.node_mut(node).unwrap().kind {
            part.mesh.verts[0] = 7.0;
        }
        edited
            .set_deform_vertices(param, node, [1, 0], vec![2.0; 4])
            .unwrap();

        let edited_mesh = match &edited.node(node).unwrap().kind {
            EditNodeKind::Part(part) => &part.mesh,
            _ => unreachable!(),
        };
        assert!(!Arc::ptr_eq(&original_mesh.0, &edited_mesh.0));
        assert!(!Arc::ptr_eq(
            &model.param(param).unwrap().bindings[0].values.0,
            &edited.param(param).unwrap().bindings[0].values.0,
        ));
        assert_eq!(original_mesh.verts[0], -1.0);
        assert_eq!(
            crate::deform_cells(&model.param(param).unwrap().bindings[0].values).unwrap()[0].value,
            vec![0.0; 4],
        );
    }

    #[test]
    fn duplicate_copies_bindings_and_remaps_internal_masks() {
        let mut m = EditModel::new();
        let root = m.root();
        let group = m
            .add_node(root, EditNode::new("g", EditNodeKind::Empty))
            .unwrap();
        let mask_src = m.add_node(group, EditNode::new("mask", part())).unwrap();
        let masked = m.add_node(group, EditNode::new("masked", part())).unwrap();
        m.mask_add(masked, mask_src, MaskMode::Mask).unwrap();

        let param = m.add_param(EditParam {
            name: "x".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: Vec::new(),
        });
        m.set_binding_key(param, masked, crate::ScalarTarget::Tx, 1, 0, 5.0)
            .unwrap();

        let copy = m.duplicate_subtree(group).unwrap();
        // the copy lands right after the original.
        assert_eq!(m.node(root).unwrap().children(), &[group, copy]);
        assert_eq!(m.node(copy).unwrap().name, "g copy");

        let copy_children = m.node(copy).unwrap().children().to_vec();
        assert_eq!(copy_children.len(), 2);
        let copy_masked = copy_children[1];
        // internal mask reference points at the copied source, not the original.
        if let EditNodeKind::Part(p) = &m.node(copy_masked).unwrap().kind {
            assert_eq!(p.masks.len(), 1);
            assert_eq!(p.masks[0].source, copy_children[0]);
        } else {
            panic!("expected part");
        }
        // the copied node has its own binding.
        assert_eq!(m.param(param).unwrap().bindings.len(), 2);
        assert!(m
            .param(param)
            .unwrap()
            .bindings
            .iter()
            .any(|b| b.node == copy_masked));
        assert!(m.to_clp_bytes().is_ok());
    }

    #[test]
    fn mask_ops_validate_and_reorder() {
        let mut m = EditModel::new();
        let root = m.root();
        let a = m.add_node(root, EditNode::new("a", part())).unwrap();
        let b = m.add_node(root, EditNode::new("b", part())).unwrap();
        let c = m.add_node(root, EditNode::new("c", part())).unwrap();
        let target = m.add_node(root, EditNode::new("t", part())).unwrap();

        assert!(m.mask_add(target, target, MaskMode::Mask).is_err());
        assert!(m
            .mask_add(a, root, MaskMode::Mask)
            .is_err_and(|e| matches!(e, EditError::NotAPart)));

        m.mask_add(target, a, MaskMode::Mask).unwrap();
        m.mask_add(target, b, MaskMode::DodgeMask).unwrap();
        m.mask_add(target, c, MaskMode::Mask).unwrap();
        m.mask_reorder(target, 2, 0).unwrap();
        m.mask_set_mode(target, 1, MaskMode::DodgeMask).unwrap();
        m.mask_delete(target, 2).unwrap();
        if let EditNodeKind::Part(p) = &m.node(target).unwrap().kind {
            assert_eq!(p.masks.len(), 2);
            assert_eq!(p.masks[0].source, c);
            assert_eq!(p.masks[1].source, a);
            assert!(matches!(p.masks[1].mode, MaskMode::DodgeMask));
        } else {
            panic!("expected part");
        }
        assert!(m.mask_delete(target, 5).is_err());
    }
}
