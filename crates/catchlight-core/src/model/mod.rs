//! The authored model: the character as a person made it.
//!
//! [`Model`] holds what a model file holds — the node tree, params and their
//! bindings, textures, welds and physics settings — and nothing that animating
//! it produces. A puppet animates a Model; a render cache is prepared from
//! one. Both read it, neither owns it.
//!
//! Invariants this module enforces:
//!
//! - **Nothing mutates a Model except its own methods.** Every field is
//!   private and every mutating method bumps [`Model::generation`], so a
//!   derived object (a puppet, a render cache) can hold the last generation it
//!   baked against and rebake when it moved. A mutation path that forgets to
//!   bump is the bug class `generation_bumps_on_every_mutating_method` exists
//!   to catch.
//! - **The tree is always valid.** One root, no cycles, no dangling
//!   cross-reference: deleting a node also drops every mask and binding that
//!   pointed into the removed subtree, and deleting a param or texture nulls
//!   out whatever referenced it. Cross-references (a part's albedo, a mask's
//!   source, a physics target, a weld's ends) are private and only reachable
//!   through methods that check them, which is what makes [`Model::flatten`]
//!   total.
//! - **Keys are stable, indices are not.** Nodes, params and textures are
//!   addressed by slotmap keys ([`NodeKey`], [`ParamKey`], [`TexKey`]) that
//!   survive insert, reparent and delete. Array indices exist only at the file
//!   edge: [`Model::flatten`] assigns them in topological order on save and
//!   [`Model::from_clp_file`] recovers keys on open.
//! - **Sibling order is document state.** It is the draw order for equal z
//!   order, so [`Model::reorder`] is an edit, not view state.
//! - **Textures stay source-encoded.** A [`ModelTexture`] keeps the author's
//!   bytes verbatim; decoding is the render cache's job.
//! - **Heavy leaves are shared.** Meshes, binding cell grids and texture bytes
//!   sit behind `Arc` and are edited through `Arc::make_mut`, so cloning a
//!   Model for undo is a shallow copy of small structs plus refcount bumps.
//!   Measured by `bench::clone_and_edit_a_five_hundred_node_model` (release,
//!   `--ignored`): a 500-node, 500-binding, 3.0 MiB model clones in **55 µs**
//!   and clones-then-authors-one-deform-cell in **57 µs**. Undo pushes one
//!   snapshot per edit, so that is the whole per-edit cost of the history.
//!
//! Pure and wasm-safe: no GPU, no async, no filesystem.

mod binding;
mod check;
mod flatten;

pub use binding::{
    deform_cells, mask_mode_name, param_range_is_valid, scalar_cells, target_of, BindingKey,
    BindingTarget, ScalarTarget,
};
pub use check::CheckWarning;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;

use slotmap::{new_key_type, SlotMap};

use crate::components::{BlendMode, MaskMode};
use crate::formats::clp::{
    self as clp, ClpBindingValues, ClpMesh, ClpMeshGroup, ClpPhysics, ClpTransform, TextureAlpha,
    TextureEncoding,
};
use crate::params::InterpolateMode;
use crate::physics::{PendulumKind, PhysicsParamMapMode};

/// Why an edit to a [`Model`] — or a read of a `.clp` file into one — could
/// not be carried out. Every mutating method leaves the model untouched when
/// it returns one of these.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("unknown node id")]
    UnknownNode,
    #[error("unknown param id")]
    UnknownParam,
    #[error("unknown texture id")]
    UnknownTexture,
    #[error(".clp node arena must contain exactly one root at index 0")]
    InvalidClpRoot,
    #[error(".clp node {node} parent index {parent} must name a preceding node")]
    InvalidClpParent { node: usize, parent: u32 },
    #[error("cannot reparent a node under itself or a descendant")]
    Cycle,
    #[error("the root node cannot be {0}")]
    Root(&'static str),
    #[error("binding cell is outside the param's axis grid")]
    CellOutOfRange,
    #[error("no such binding")]
    UnknownBinding,
    #[error("node is not a part")]
    NotAPart,
    #[error("node carries no mesh")]
    NotMeshed,
    #[error("node cannot have masks")]
    NotMaskable,
    #[error("a node cannot mask itself")]
    SelfMask,
    #[error("node is not a simple physics node")]
    NotPhysics,
    #[error("a colour binding cannot target a mesh group, which is never drawn")]
    ColorOnMeshGroup,
    #[error("binding target does not match the operation")]
    WrongTarget,
    #[error("index out of range")]
    IndexOutOfRange,
    #[error("constraint edges may not cross")]
    ConstraintCross,
    #[error("mesh is malformed: {0}")]
    MalformedMesh(&'static str),
    #[error(".clp codec: {0}")]
    Clp(#[from] crate::formats::clp::ClpError),
    #[error(transparent)]
    LoadLimit(#[from] crate::LoadLimitError),
}

new_key_type! {
    pub struct NodeKey;
    pub struct ParamKey;
    pub struct TexKey;
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
ffi_id!(NodeKey);
ffi_id!(ParamKey);
ffi_id!(TexKey);

/// A source-encoded texture (verbatim PNG/TGA bytes), shared via `Arc` so model
/// snapshots are cheap to clone even when the structure churns.
#[derive(Debug, Clone)]
pub struct ModelTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: Arc<Vec<u8>>,
}

/// The authored model: a tree of nodes by stable key, ordered params and
/// textures, the bindings params drive nodes through, and authored physics.
/// The tree is always valid (single root, no cycles, no dangling
/// cross-references), so [`Model::flatten`] is total.
#[derive(Debug, Clone)]
pub struct Model {
    generation: u64,
    physics: ClpPhysics,
    welds: Vec<ModelWeld>,
    nodes: SlotMap<NodeKey, ModelNode>,
    root: NodeKey,
    params: SlotMap<ParamKey, ModelParam>,
    param_order: Vec<ParamKey>,
    textures: SlotMap<TexKey, ModelTexture>,
    texture_order: Vec<TexKey>,
    bindings: Vec<ModelBinding>,
}

/// A welded part pair: two parts whose vertex pairs are pulled together after
/// every other deformation.
#[derive(Debug, Clone)]
pub struct ModelWeld {
    a: NodeKey,
    b: NodeKey,
    pairs: Arc<Vec<clp::ClpWeldPair>>,
}

impl ModelWeld {
    pub fn new(a: NodeKey, b: NodeKey, pairs: Vec<clp::ClpWeldPair>) -> Self {
        Self {
            a,
            b,
            pairs: Arc::new(pairs),
        }
    }

    pub fn a(&self) -> NodeKey {
        self.a
    }

    pub fn b(&self) -> NodeKey {
        self.b
    }

    pub fn pairs(&self) -> &[clp::ClpWeldPair] {
        &self.pairs
    }
}

#[derive(Debug, Clone)]
pub struct ModelNode {
    pub name: String,
    pub enabled: bool,
    pub z_order: f32,
    pub transform: ClpTransform,
    pub lock_to_root: bool,
    pub kind: ModelNodeKind,
    parent: Option<NodeKey>,
    children: Vec<NodeKey>,
}

#[derive(Debug, Clone)]
pub enum ModelNodeKind {
    Group,
    Part(ModelPart),
    Composite(ModelComposite),
    MeshGroup(ModelMeshGroup),
    SimplePhysics(ModelPhysics),
}

impl ModelNodeKind {
    /// The wire/UI name of the kind.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Part(_) => "part",
            Self::Composite(_) => "composite",
            Self::MeshGroup(_) => "mesh_group",
            Self::SimplePhysics(_) => "physics",
        }
    }
}

/// A mesh behind an `Arc`: cloning a Model shares it, and the first edit
/// through [`Model`] copies it out.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelMesh(Arc<ClpMesh>);

impl ModelMesh {
    pub fn to_clp(&self) -> ClpMesh {
        (*self.0).clone()
    }
}

impl From<ClpMesh> for ModelMesh {
    fn from(mesh: ClpMesh) -> Self {
        Self(Arc::new(mesh))
    }
}

impl Deref for ModelMesh {
    type Target = ClpMesh;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A mesh group deforms what is beneath it and is never drawn, so it has no
/// colour to edit: no opacity, blend mode, tint or screen tint.
#[derive(Debug, Clone)]
pub struct ModelMeshGroup {
    pub dynamic: bool,
    pub translate_children: bool,
    mesh: ModelMesh,
}

impl ModelMeshGroup {
    /// A mesh group over `mesh`: static, leaving meshless descendants in place.
    pub fn new(mesh: impl Into<ModelMesh>) -> Self {
        Self {
            dynamic: false,
            translate_children: false,
            mesh: mesh.into(),
        }
    }

    pub fn mesh(&self) -> &ClpMesh {
        &self.mesh
    }

    pub fn from_clp(group: &ClpMeshGroup) -> Self {
        Self {
            dynamic: group.dynamic,
            translate_children: group.translate_children,
            mesh: group.mesh.clone().into(),
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
pub struct ModelPart {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub mask_threshold: f32,
    mesh: ModelMesh,
    /// Albedo texture, or `None` for an unmapped part (the renderer culls it).
    albedo: Option<TexKey>,
    masks: Vec<ModelMask>,
}

impl ModelPart {
    /// An opaque, unmasked, untextured part drawing `mesh`.
    pub fn new(mesh: impl Into<ModelMesh>) -> Self {
        Self {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            mask_threshold: 0.5,
            mesh: mesh.into(),
            albedo: None,
            masks: Vec::new(),
        }
    }

    pub fn mesh(&self) -> &ClpMesh {
        &self.mesh
    }

    pub fn albedo(&self) -> Option<TexKey> {
        self.albedo
    }

    pub fn masks(&self) -> &[ModelMask] {
        &self.masks
    }
}

#[derive(Debug, Clone)]
pub struct ModelComposite {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub mask_threshold: f32,
    pub propagate_meshgroup: bool,
    masks: Vec<ModelMask>,
}

impl ModelComposite {
    /// An opaque, unmasked composite that a mesh group above it does not reach.
    pub fn new() -> Self {
        Self {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            mask_threshold: 0.5,
            propagate_meshgroup: false,
            masks: Vec::new(),
        }
    }

    pub fn masks(&self) -> &[ModelMask] {
        &self.masks
    }
}

impl Default for ModelComposite {
    fn default() -> Self {
        Self::new()
    }
}

/// One drawable's clipping rule: whose shape clips it, and whether what that
/// shape covers is kept or cut away. Only [`Model::mask_add`] builds one, so a
/// mask's source is always a live part.
#[derive(Debug, Clone, Copy)]
pub struct ModelMask {
    source: NodeKey,
    mode: MaskMode,
}

impl ModelMask {
    pub fn source(&self) -> NodeKey {
        self.source
    }

    pub fn mode(&self) -> MaskMode {
        self.mode
    }
}

#[derive(Debug, Clone)]
pub struct ModelPhysics {
    pub kind: PendulumKind,
    pub map_mode: PhysicsParamMapMode,
    pub local_only: bool,
    pub gravity: f32,
    pub length: f32,
    pub frequency: f32,
    pub angle_damping: f32,
    pub length_damping: f32,
    pub output_scale: [f32; 2],
    target_param: Option<ParamKey>,
}

impl ModelPhysics {
    /// A pendulum at the reference defaults, driving no param yet.
    pub fn new(kind: PendulumKind) -> Self {
        Self {
            kind,
            map_mode: PhysicsParamMapMode::default(),
            local_only: false,
            gravity: 9.8,
            length: 100.0,
            frequency: 1.0,
            angle_damping: 0.5,
            length_damping: 0.5,
            output_scale: [1.0, 1.0],
            target_param: None,
        }
    }

    /// The param the pendulum's swing is written into, if any.
    pub fn target_param(&self) -> Option<ParamKey> {
        self.target_param
    }
}

#[derive(Debug, Clone)]
pub struct ModelParam {
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
}

/// One param's control over one property of one node. Bindings live on the
/// model, not on the param, and are addressed by their [`BindingKey`].
#[derive(Debug, Clone)]
pub struct ModelBinding {
    key: BindingKey,
    interpolate_mode: InterpolateMode,
    values: ModelBindingValues,
}

impl ModelBinding {
    pub fn key(&self) -> &BindingKey {
        &self.key
    }

    pub fn param(&self) -> ParamKey {
        self.key.param
    }

    pub fn node(&self) -> NodeKey {
        self.key.node
    }

    pub fn target(&self) -> BindingTarget {
        self.key.target
    }

    pub fn interpolate_mode(&self) -> InterpolateMode {
        self.interpolate_mode
    }

    pub fn values(&self) -> &ClpBindingValues {
        &self.values
    }
}

/// A binding's authored cell grid behind an `Arc`, shared by every snapshot
/// until one of them edits it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelBindingValues(Arc<ClpBindingValues>);

impl ModelBindingValues {
    pub fn to_clp(&self) -> ClpBindingValues {
        (*self.0).clone()
    }

    /// Copy-on-write access, `pub(crate)` so every cell rewrite goes through a
    /// [`Model`] method that can bump the generation.
    pub(crate) fn make_mut(&mut self) -> &mut ClpBindingValues {
        Arc::make_mut(&mut self.0)
    }
}

impl From<ClpBindingValues> for ModelBindingValues {
    fn from(values: ClpBindingValues) -> Self {
        Self(Arc::new(values))
    }
}

impl Deref for ModelBindingValues {
    type Target = ClpBindingValues;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ModelNode {
    /// A node at the identity transform (unit scale), enabled, z order 0.
    pub fn new(name: impl Into<String>, kind: ModelNodeKind) -> Self {
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

    pub fn parent(&self) -> Option<NodeKey> {
        self.parent
    }

    pub fn children(&self) -> &[NodeKey] {
        &self.children
    }

    /// The node's mesh, for the two kinds that carry one.
    pub fn mesh(&self) -> Option<&ClpMesh> {
        match &self.kind {
            ModelNodeKind::Part(p) => Some(p.mesh()),
            ModelNodeKind::MeshGroup(mg) => Some(mg.mesh()),
            _ => None,
        }
    }

    fn masks(&self) -> Option<&[ModelMask]> {
        match &self.kind {
            ModelNodeKind::Part(p) => Some(&p.masks),
            ModelNodeKind::Composite(c) => Some(&c.masks),
            _ => None,
        }
    }

    fn masks_mut(&mut self) -> Option<&mut Vec<ModelMask>> {
        match &mut self.kind {
            ModelNodeKind::Part(p) => Some(&mut p.masks),
            ModelNodeKind::Composite(c) => Some(&mut c.masks),
            _ => None,
        }
    }

    fn mesh_mut(&mut self) -> Option<&mut ModelMesh> {
        match &mut self.kind {
            ModelNodeKind::Part(p) => Some(&mut p.mesh),
            ModelNodeKind::MeshGroup(mg) => Some(&mut mg.mesh),
            _ => None,
        }
    }
}

impl Model {
    /// A new model with a single `Group` root named "Root".
    pub fn new() -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(ModelNode::new("Root", ModelNodeKind::Group));
        Self {
            generation: 0,
            physics: ClpPhysics::default(),
            welds: Vec::new(),
            nodes,
            root,
            params: SlotMap::with_key(),
            param_order: Vec::new(),
            textures: SlotMap::with_key(),
            texture_order: Vec::new(),
            bindings: Vec::new(),
        }
    }

    /// Bumped by every mutating method. A puppet or a render cache remembers
    /// the generation it baked against and rebakes when this moved; nothing
    /// reads the number itself.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The one place a mutation is recorded. Every `&mut self` method calls it
    /// on success — and only on success, so a rejected edit leaves derived
    /// objects alone.
    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    // ---- tree ----

    pub fn root(&self) -> NodeKey {
        self.root
    }

    pub fn node(&self, id: NodeKey) -> Option<&ModelNode> {
        self.nodes.get(id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Edit one node's own properties: name, transform, z order, enabled,
    /// lock-to-root and the plain values on its kind. Cross-references (the
    /// albedo, masks, the physics target) and the mesh have their own methods,
    /// because they have to be checked against the rest of the model.
    pub fn update_node<R>(
        &mut self,
        id: NodeKey,
        f: impl FnOnce(&mut ModelNode) -> R,
    ) -> Result<R, ModelError> {
        let node = self.nodes.get_mut(id).ok_or(ModelError::UnknownNode)?;
        let out = f(node);
        self.bump();
        Ok(out)
    }

    /// Nodes in topological pre-order from the root, each parent before its
    /// children, following sibling order. This is the order [`Self::flatten`]
    /// snapshots into the arena.
    pub fn nodes_in_order(&self) -> Vec<NodeKey> {
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
    pub fn add_node(
        &mut self,
        parent: NodeKey,
        mut node: ModelNode,
    ) -> Result<NodeKey, ModelError> {
        if !self.nodes.contains_key(parent) {
            return Err(ModelError::UnknownNode);
        }
        // A node cloned out of another model carries that model's keys.
        self.check_node_refs(&node)?;
        node.parent = Some(parent);
        node.children.clear();
        let id = self.nodes.insert(node);
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(id);
        }
        self.bump();
        Ok(id)
    }

    /// Remove a node and its whole subtree, then drop every mask and binding
    /// that pointed into the removed set so the model stays referentially valid.
    pub fn delete_node(&mut self, id: NodeKey) -> Result<(), ModelError> {
        if id == self.root {
            return Err(ModelError::Root("deleted"));
        }
        if !self.nodes.contains_key(id) {
            return Err(ModelError::UnknownNode);
        }
        let removed: Vec<NodeKey> = self.subtree(id);
        let removed_set: HashSet<NodeKey> = removed.iter().copied().collect();
        if let Some(parent) = self.nodes.get(id).and_then(|n| n.parent) {
            if let Some(p) = self.nodes.get_mut(parent) {
                p.children.retain(|&c| c != id);
            }
        }
        for r in &removed {
            self.nodes.remove(*r);
        }
        for node in self.nodes.values_mut() {
            if let Some(masks) = node.masks_mut() {
                masks.retain(|m| !removed_set.contains(&m.source));
            }
        }
        self.bindings.retain(|b| !removed_set.contains(&b.key.node));
        self.welds
            .retain(|w| !removed_set.contains(&w.a) && !removed_set.contains(&w.b));
        self.bump();
        Ok(())
    }

    /// Move `id` (and its subtree) under `new_parent`. Rejects moving the root
    /// or creating a cycle.
    pub fn reparent(&mut self, id: NodeKey, new_parent: NodeKey) -> Result<(), ModelError> {
        if id == self.root {
            return Err(ModelError::Root("reparented"));
        }
        if !self.nodes.contains_key(id) || !self.nodes.contains_key(new_parent) {
            return Err(ModelError::UnknownNode);
        }
        if self.is_self_or_descendant(new_parent, id) {
            return Err(ModelError::Cycle);
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
        self.bump();
        Ok(())
    }

    /// Move `id` to `index` within its parent's children (clamped to the end).
    /// Sibling order is draw-list order for equal z order, so this is a document
    /// edit, not view state.
    pub fn reorder(&mut self, id: NodeKey, index: usize) -> Result<(), ModelError> {
        if id == self.root {
            return Err(ModelError::Root("reordered"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent)
            .ok_or(ModelError::UnknownNode)?;
        let p = self.nodes.get_mut(parent).ok_or(ModelError::UnknownNode)?;
        let cur = p
            .children
            .iter()
            .position(|&c| c == id)
            .ok_or(ModelError::UnknownNode)?;
        p.children.remove(cur);
        let index = index.min(p.children.len());
        p.children.insert(index, id);
        self.bump();
        Ok(())
    }

    /// Deep-copy `id`'s subtree as its next sibling. Mask references inside the
    /// subtree point at the copies; external ones stay shared. Each copied node
    /// also copies its param bindings, so the duplicate deforms like the
    /// original. The copy's root is renamed "<name> copy".
    pub fn duplicate_subtree(&mut self, id: NodeKey) -> Result<NodeKey, ModelError> {
        if id == self.root {
            return Err(ModelError::Root("duplicated"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent)
            .ok_or(ModelError::UnknownNode)?;

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

        let mut map: HashMap<NodeKey, NodeKey> = HashMap::new();
        for &old in &order {
            let mut copy = self.nodes.get(old).ok_or(ModelError::UnknownNode)?.clone();
            let new_parent = match copy.parent {
                Some(p) if old != id => *map.get(&p).ok_or(ModelError::UnknownNode)?,
                _ => parent,
            };
            if old == id {
                copy.name = format!("{} copy", copy.name);
            }
            copy.parent = Some(new_parent);
            copy.children.clear();
            let new_id = self.nodes.insert(copy);
            if let Some(p) = self.nodes.get_mut(new_parent) {
                p.children.push(new_id);
            }
            map.insert(old, new_id);
        }

        for &new_id in map.values() {
            if let Some(masks) = self.nodes.get_mut(new_id).and_then(ModelNode::masks_mut) {
                for m in masks.iter_mut() {
                    if let Some(&mapped) = map.get(&m.source) {
                        m.source = mapped;
                    }
                }
            }
        }

        let copied: Vec<ModelBinding> = self
            .bindings
            .iter()
            .filter_map(|b| {
                map.get(&b.key.node).map(|&node| ModelBinding {
                    key: BindingKey { node, ..b.key },
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.clone(),
                })
            })
            .collect();
        self.bindings.extend(copied);

        let new_root = *map.get(&id).ok_or(ModelError::UnknownNode)?;
        let pos = self
            .nodes
            .get(parent)
            .and_then(|p| p.children.iter().position(|&c| c == id))
            .ok_or(ModelError::UnknownNode)?;
        // reorder bumps the generation for the whole duplicate.
        self.reorder(new_root, pos + 1)?;
        Ok(new_root)
    }

    // ---- node cross-references ----

    /// Point a part at a texture, or unmap it (the renderer culls an unmapped
    /// part).
    pub fn set_part_albedo(
        &mut self,
        id: NodeKey,
        albedo: Option<TexKey>,
    ) -> Result<(), ModelError> {
        if let Some(t) = albedo {
            if !self.textures.contains_key(t) {
                return Err(ModelError::UnknownTexture);
            }
        }
        match self.nodes.get_mut(id).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Part(p)) => p.albedo = albedo,
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode),
        }
        self.bump();
        Ok(())
    }

    /// Aim a simple physics node at a param, or at nothing.
    pub fn set_physics_target(
        &mut self,
        id: NodeKey,
        param: Option<ParamKey>,
    ) -> Result<(), ModelError> {
        if let Some(p) = param {
            if !self.params.contains_key(p) {
                return Err(ModelError::UnknownParam);
            }
        }
        match self.nodes.get_mut(id).map(|n| &mut n.kind) {
            Some(ModelNodeKind::SimplePhysics(ph)) => ph.target_param = param,
            Some(_) => return Err(ModelError::NotPhysics),
            None => return Err(ModelError::UnknownNode),
        }
        self.bump();
        Ok(())
    }

    /// Replace a meshed node's mesh, mapping every authored deform cell on it
    /// through `refit(old_mesh, new_mesh, offsets)`. The mesh and the cells
    /// that are sized to it move in one step, so no reader ever sees offsets
    /// fitted to a mesh that is gone.
    pub fn set_node_mesh_with(
        &mut self,
        id: NodeKey,
        mesh: ClpMesh,
        mut refit: impl FnMut(&ClpMesh, &ClpMesh, &[f32]) -> Vec<f32>,
    ) -> Result<(), ModelError> {
        validate_mesh(&mesh)?;
        let old = match self.nodes.get(id).and_then(ModelNode::mesh) {
            Some(m) => m.clone(),
            None => {
                return Err(if self.nodes.contains_key(id) {
                    ModelError::NotMeshed
                } else {
                    ModelError::UnknownNode
                })
            }
        };
        for b in &mut self.bindings {
            if b.key.node != id {
                continue;
            }
            if let ClpBindingValues::Deform(cells) = b.values.make_mut() {
                for cell in &mut cells.cells {
                    cell.value = refit(&old, &mesh, &cell.value);
                }
            }
        }
        if let Some(slot) = self.nodes.get_mut(id).and_then(ModelNode::mesh_mut) {
            *slot = mesh.into();
        }
        self.bump();
        Ok(())
    }

    /// Replace a meshed node's mesh, resizing every authored deform cell on it
    /// to the new vertex count (new vertices start at zero offset).
    pub fn set_node_mesh(&mut self, id: NodeKey, mesh: ClpMesh) -> Result<(), ModelError> {
        self.set_node_mesh_with(id, mesh, |_, new, offsets| {
            let mut out = offsets.to_vec();
            out.resize(new.verts.len(), 0.0);
            out
        })
    }

    // ---- masks ----

    fn masks_mut(&mut self, id: NodeKey) -> Result<&mut Vec<ModelMask>, ModelError> {
        match self.nodes.get_mut(id) {
            Some(n) => n.masks_mut().ok_or(ModelError::NotMaskable),
            None => Err(ModelError::UnknownNode),
        }
    }

    /// Append a mask source. Sources must be parts (the renderer rasterizes a
    /// source's own mesh + texture into the mask).
    pub fn mask_add(
        &mut self,
        id: NodeKey,
        source: NodeKey,
        mode: MaskMode,
    ) -> Result<(), ModelError> {
        if source == id {
            return Err(ModelError::SelfMask);
        }
        match self.nodes.get(source).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(_)) => {}
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode),
        }
        self.masks_mut(id)?.push(ModelMask { source, mode });
        self.bump();
        Ok(())
    }

    pub fn mask_set_mode(
        &mut self,
        id: NodeKey,
        index: usize,
        mode: MaskMode,
    ) -> Result<(), ModelError> {
        let masks = self.masks_mut(id)?;
        let m = masks.get_mut(index).ok_or(ModelError::IndexOutOfRange)?;
        m.mode = mode;
        self.bump();
        Ok(())
    }

    /// Move the mask at `index` to `to` (clamped). Mask order is evaluation
    /// order, so this is a document edit.
    pub fn mask_reorder(&mut self, id: NodeKey, index: usize, to: usize) -> Result<(), ModelError> {
        let masks = self.masks_mut(id)?;
        if index >= masks.len() {
            return Err(ModelError::IndexOutOfRange);
        }
        let m = masks.remove(index);
        let to = to.min(masks.len());
        masks.insert(to, m);
        self.bump();
        Ok(())
    }

    pub fn mask_delete(&mut self, id: NodeKey, index: usize) -> Result<(), ModelError> {
        let masks = self.masks_mut(id)?;
        if index >= masks.len() {
            return Err(ModelError::IndexOutOfRange);
        }
        masks.remove(index);
        self.bump();
        Ok(())
    }

    // ---- params ----

    pub fn param_ids(&self) -> &[ParamKey] {
        &self.param_order
    }

    pub fn param(&self, id: ParamKey) -> Option<&ModelParam> {
        self.params.get(id)
    }

    pub fn add_param(&mut self, param: ModelParam) -> ParamKey {
        let id = self.params.insert(param);
        self.param_order.push(id);
        self.bump();
        id
    }

    /// Remove a param, its bindings, and any physics node that drove it.
    pub fn delete_param(&mut self, id: ParamKey) -> Result<(), ModelError> {
        if self.params.remove(id).is_none() {
            return Err(ModelError::UnknownParam);
        }
        self.param_order.retain(|&p| p != id);
        self.bindings.retain(|b| b.key.param != id);
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::SimplePhysics(ph) = &mut node.kind {
                if ph.target_param == Some(id) {
                    ph.target_param = None;
                }
            }
        }
        self.bump();
        Ok(())
    }

    // ---- textures ----

    pub fn texture_ids(&self) -> &[TexKey] {
        &self.texture_order
    }

    pub fn texture(&self, id: TexKey) -> Option<&ModelTexture> {
        self.textures.get(id)
    }

    pub fn add_texture(&mut self, texture: ModelTexture) -> TexKey {
        let id = self.textures.insert(texture);
        self.texture_order.push(id);
        self.bump();
        id
    }

    /// Remove a texture and unmap any part that referenced it.
    pub fn delete_texture(&mut self, id: TexKey) -> Result<(), ModelError> {
        if self.textures.remove(id).is_none() {
            return Err(ModelError::UnknownTexture);
        }
        self.texture_order.retain(|&t| t != id);
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::Part(p) = &mut node.kind {
                if p.albedo == Some(id) {
                    p.albedo = None;
                }
            }
        }
        self.bump();
        Ok(())
    }

    // ---- physics and welds ----

    pub fn physics(&self) -> &ClpPhysics {
        &self.physics
    }

    pub fn set_physics(&mut self, physics: ClpPhysics) {
        self.physics = physics;
        self.bump();
    }

    pub fn welds(&self) -> &[ModelWeld] {
        &self.welds
    }

    /// Replace the weld list. Every end must name a live node.
    pub fn set_welds(&mut self, welds: Vec<ModelWeld>) -> Result<(), ModelError> {
        if welds
            .iter()
            .any(|w| !self.nodes.contains_key(w.a) || !self.nodes.contains_key(w.b))
        {
            return Err(ModelError::UnknownNode);
        }
        self.welds = welds;
        self.bump();
        Ok(())
    }

    // ---- accounting ----

    pub fn estimated_size_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(
                self.nodes
                    .len()
                    .saturating_mul(std::mem::size_of::<ModelNode>()),
            )
            .saturating_add(
                self.params
                    .len()
                    .saturating_mul(std::mem::size_of::<ModelParam>()),
            )
            .saturating_add(
                self.textures
                    .len()
                    .saturating_mul(std::mem::size_of::<ModelTexture>()),
            )
            .saturating_add(
                self.param_order
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ParamKey>()),
            )
            .saturating_add(
                self.texture_order
                    .capacity()
                    .saturating_mul(std::mem::size_of::<TexKey>()),
            )
            .saturating_add(
                self.welds
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ModelWeld>()),
            )
            .saturating_add(
                self.bindings
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ModelBinding>()),
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
                    .saturating_mul(std::mem::size_of::<NodeKey>()),
            );
            if let Some(mesh) = node.mesh() {
                bytes = bytes.saturating_add(mesh_size(mesh));
            }
            if let Some(masks) = node.masks() {
                bytes = bytes
                    .saturating_add(masks.len().saturating_mul(std::mem::size_of::<ModelMask>()));
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
                );
        }
        for binding in &self.bindings {
            bytes = bytes.saturating_add(binding_values_size(&binding.values));
        }
        for texture in self.textures.values() {
            bytes = bytes.saturating_add(texture.data.capacity());
        }
        bytes
    }

    /// The node's mesh, for the two kinds that carry one.
    pub fn node_mesh(&self, id: NodeKey) -> Option<&ClpMesh> {
        self.node(id)?.mesh()
    }

    /// Every cross-reference a node about to join the model carries has to
    /// name something this model actually has.
    fn check_node_refs(&self, node: &ModelNode) -> Result<(), ModelError> {
        if let ModelNodeKind::Part(p) = &node.kind {
            if p.albedo.is_some_and(|t| !self.textures.contains_key(t)) {
                return Err(ModelError::UnknownTexture);
            }
        }
        if let ModelNodeKind::SimplePhysics(ph) = &node.kind {
            if ph
                .target_param
                .is_some_and(|p| !self.params.contains_key(p))
            {
                return Err(ModelError::UnknownParam);
            }
        }
        if let Some(masks) = node.masks() {
            for m in masks {
                match self.nodes.get(m.source).map(|n| &n.kind) {
                    Some(ModelNodeKind::Part(_)) => {}
                    Some(_) => return Err(ModelError::NotAPart),
                    None => return Err(ModelError::UnknownNode),
                }
            }
        }
        Ok(())
    }

    fn subtree(&self, root: NodeKey) -> Vec<NodeKey> {
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

    fn is_self_or_descendant(&self, candidate: NodeKey, ancestor: NodeKey) -> bool {
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

/// Vertices come in pairs, uvs match them, and every index names a vertex.
fn validate_mesh(mesh: &ClpMesh) -> Result<(), ModelError> {
    if !mesh.verts.len().is_multiple_of(2) {
        return Err(ModelError::MalformedMesh(
            "vertex array is not [x, y] pairs",
        ));
    }
    if !mesh.uvs.is_empty() && mesh.uvs.len() != mesh.verts.len() {
        return Err(ModelError::MalformedMesh(
            "uv count does not match vertices",
        ));
    }
    let vcount = mesh.verts.len() / 2;
    let max_index = match &mesh.indices {
        clp::ClpIndices::U16(v) => v.iter().map(|&i| i as usize).max(),
        clp::ClpIndices::U32(v) => v.iter().map(|&i| i as usize).max(),
    };
    if max_index.is_some_and(|m| m >= vcount) {
        return Err(ModelError::MalformedMesh("index names a missing vertex"));
    }
    Ok(())
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

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::clp::{ClpIndices, ClpWeldPair};

    fn quad() -> ClpMesh {
        ClpMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0; 8],
            indices: ClpIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    fn part_kind() -> ModelNodeKind {
        ModelNodeKind::Part(ModelPart::new(quad()))
    }

    /// One model carrying every kind of thing a mutating method can reach.
    struct Rig {
        model: Model,
        root: NodeKey,
        part: NodeKey,
        other: NodeKey,
        composite: NodeKey,
        physics: NodeKey,
        param: ParamKey,
        tex: TexKey,
    }

    fn rig() -> Rig {
        let mut model = Model::new();
        let root = model.root();
        let tex = model.add_texture(ModelTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: Arc::new(vec![0x89, b'P', b'N', b'G']),
        });
        let part = model
            .add_node(root, ModelNode::new("part", part_kind()))
            .unwrap();
        let other = model
            .add_node(root, ModelNode::new("other", part_kind()))
            .unwrap();
        let composite = model
            .add_node(
                root,
                ModelNode::new("composite", ModelNodeKind::Composite(ModelComposite::new())),
            )
            .unwrap();
        let physics = model
            .add_node(
                root,
                ModelNode::new(
                    "physics",
                    ModelNodeKind::SimplePhysics(ModelPhysics::new(PendulumKind::RigidPendulum)),
                ),
            )
            .unwrap();
        let param = model.add_param(ModelParam {
            name: "p".into(),
            is_vec2: false,
            min: [-1.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0],
        });
        model.mask_add(composite, part, MaskMode::Mask).unwrap();
        Rig {
            model,
            root,
            part,
            other,
            composite,
            physics,
            param,
            tex,
        }
    }

    fn scalar_key(r: &Rig) -> BindingKey {
        BindingKey::new(r.param, r.part, BindingTarget::Scalar(ScalarTarget::Tx))
    }

    fn deform_key(r: &Rig) -> BindingKey {
        BindingKey::new(r.param, r.part, BindingTarget::Deform)
    }

    /// Every derived object (puppet, render cache) rebakes off `generation`, so
    /// a mutating method that forgets to bump it is a stale-cache bug that no
    /// other test would see. This list is the API surface: a new mutating
    /// method belongs in it.
    #[test]
    fn generation_bumps_on_every_mutating_method() {
        #[allow(clippy::type_complexity)]
        let edits: Vec<(&str, Box<dyn Fn(&mut Rig)>)> = vec![
            (
                "update_node",
                Box::new(|r| {
                    r.model.update_node(r.part, |n| n.z_order = 3.0).unwrap();
                }),
            ),
            (
                "add_node",
                Box::new(|r| {
                    r.model
                        .add_node(r.root, ModelNode::new("new", ModelNodeKind::Group))
                        .unwrap();
                }),
            ),
            (
                "delete_node",
                Box::new(|r| r.model.delete_node(r.other).unwrap()),
            ),
            (
                "reparent",
                Box::new(|r| r.model.reparent(r.part, r.composite).unwrap()),
            ),
            ("reorder", Box::new(|r| r.model.reorder(r.part, 0).unwrap())),
            (
                "duplicate_subtree",
                Box::new(|r| {
                    r.model.duplicate_subtree(r.part).unwrap();
                }),
            ),
            (
                "set_part_albedo",
                Box::new(|r| r.model.set_part_albedo(r.part, Some(r.tex)).unwrap()),
            ),
            (
                "set_physics_target",
                Box::new(|r| {
                    r.model
                        .set_physics_target(r.physics, Some(r.param))
                        .unwrap()
                }),
            ),
            (
                "set_node_mesh",
                Box::new(|r| r.model.set_node_mesh(r.part, quad()).unwrap()),
            ),
            (
                "set_node_mesh_with",
                Box::new(|r| {
                    r.model
                        .set_node_mesh_with(r.part, quad(), |_, _, v| v.to_vec())
                        .unwrap()
                }),
            ),
            (
                "mask_add",
                Box::new(|r| {
                    r.model
                        .mask_add(r.composite, r.other, MaskMode::Mask)
                        .unwrap()
                }),
            ),
            (
                "mask_set_mode",
                Box::new(|r| {
                    r.model
                        .mask_set_mode(r.composite, 0, MaskMode::DodgeMask)
                        .unwrap()
                }),
            ),
            (
                "mask_reorder",
                Box::new(|r| r.model.mask_reorder(r.composite, 0, 0).unwrap()),
            ),
            (
                "mask_delete",
                Box::new(|r| r.model.mask_delete(r.composite, 0).unwrap()),
            ),
            (
                "add_param",
                Box::new(|r| {
                    r.model.add_param(ModelParam {
                        name: "q".into(),
                        is_vec2: false,
                        min: [0.0, 0.0],
                        max: [1.0, 0.0],
                        defaults: [0.0, 0.0],
                        axis_points_x: vec![0.0, 1.0],
                        axis_points_y: vec![0.0],
                    });
                }),
            ),
            (
                "delete_param",
                Box::new(|r| r.model.delete_param(r.param).unwrap()),
            ),
            (
                "add_texture",
                Box::new(|r| {
                    r.model.add_texture(ModelTexture {
                        encoding: TextureEncoding::Png,
                        alpha: TextureAlpha::Straight,
                        data: Arc::new(Vec::new()),
                    });
                }),
            ),
            (
                "delete_texture",
                Box::new(|r| r.model.delete_texture(r.tex).unwrap()),
            ),
            (
                "set_physics",
                Box::new(|r| r.model.set_physics(ClpPhysics::default())),
            ),
            (
                "set_welds",
                Box::new(|r| {
                    r.model
                        .set_welds(vec![ModelWeld::new(
                            r.part,
                            r.other,
                            vec![ClpWeldPair {
                                a_vert: 0,
                                b_vert: 0,
                                weight: 1.0,
                            }],
                        )])
                        .unwrap()
                }),
            ),
            (
                "add_binding",
                Box::new(|r| r.model.add_binding(&scalar_key(r)).unwrap()),
            ),
            (
                "set_binding_key",
                Box::new(|r| {
                    r.model
                        .set_binding_key(&scalar_key(r), [1, 0], 5.0)
                        .unwrap()
                }),
            ),
            (
                "unset_binding_key",
                Box::new(|r| {
                    let key = scalar_key(r);
                    r.model.set_binding_key(&key, [1, 0], 5.0).unwrap();
                    r.model.unset_binding_key(&key, [1, 0]).unwrap();
                }),
            ),
            (
                "reset_binding_key",
                Box::new(|r| {
                    let key = scalar_key(r);
                    r.model.add_binding(&key).unwrap();
                    r.model.reset_binding_key(&key, [1, 0]).unwrap();
                }),
            ),
            (
                "delete_binding",
                Box::new(|r| {
                    let key = scalar_key(r);
                    r.model.add_binding(&key).unwrap();
                    r.model.delete_binding(&key).unwrap();
                }),
            ),
            (
                "set_binding_interpolate",
                Box::new(|r| {
                    let key = scalar_key(r);
                    r.model.add_binding(&key).unwrap();
                    r.model
                        .set_binding_interpolate(&key, InterpolateMode::Cubic)
                        .unwrap();
                }),
            ),
            (
                "invert_binding",
                Box::new(|r| {
                    let key = scalar_key(r);
                    r.model.set_binding_key(&key, [2, 0], 5.0).unwrap();
                    r.model.invert_binding(&key).unwrap();
                }),
            ),
            (
                "copy_binding_key",
                Box::new(|r| {
                    let key = scalar_key(r);
                    r.model.set_binding_key(&key, [0, 0], 5.0).unwrap();
                    r.model.copy_binding_key(&key, [0, 0], [2, 0]).unwrap();
                }),
            ),
            (
                "set_deform_vertices",
                Box::new(|r| {
                    r.model
                        .set_deform_vertices(&deform_key(r), [1, 0], vec![1.0; 8])
                        .unwrap()
                }),
            ),
            (
                "set_deform_from_transform",
                Box::new(|r| {
                    r.model
                        .set_deform_from_transform(
                            &deform_key(r),
                            [1, 0],
                            [1.0, 0.0],
                            0.0,
                            [1.0, 1.0],
                        )
                        .unwrap()
                }),
            ),
            (
                "set_param_name",
                Box::new(|r| r.model.set_param_name(r.param, "renamed".into()).unwrap()),
            ),
            (
                "set_param_defaults",
                Box::new(|r| r.model.set_param_defaults(r.param, [0.5, 0.0]).unwrap()),
            ),
            (
                "set_param_range",
                Box::new(|r| {
                    r.model
                        .set_param_range(r.param, [0.0, 0.0], [2.0, 0.0])
                        .unwrap()
                }),
            ),
            (
                "axis_insert",
                Box::new(|r| {
                    r.model.axis_insert(r.param, 0, 0.25).unwrap();
                }),
            ),
            (
                "axis_delete",
                Box::new(|r| r.model.axis_delete(r.param, 0, 1).unwrap()),
            ),
            (
                "axis_move",
                Box::new(|r| r.model.axis_move(r.param, 0, 1, 0.6).unwrap()),
            ),
            (
                "param_flip",
                Box::new(|r| r.model.param_flip(r.param, 0).unwrap()),
            ),
        ];

        for (name, edit) in edits {
            let mut r = rig();
            let before = r.model.generation();
            edit(&mut r);
            assert!(
                r.model.generation() > before,
                "{name} did not bump the generation",
            );
        }
    }

    /// A rejected edit must leave every derived object alone, so it must not
    /// move the generation either.
    #[test]
    fn a_rejected_edit_leaves_the_generation_alone() {
        let mut r = rig();
        let before = r.model.generation();
        assert!(r.model.delete_node(r.root).is_err());
        assert!(r.model.reparent(r.root, r.part).is_err());
        assert!(r.model.mask_add(r.part, r.part, MaskMode::Mask).is_err());
        assert!(r.model.mask_delete(r.composite, 7).is_err());
        assert!(r
            .model
            .set_binding_key(&scalar_key(&r), [9, 0], 1.0)
            .is_err());
        assert_eq!(r.model.generation(), before);
    }

    #[test]
    fn reorder_moves_within_siblings_and_clamps() {
        let mut m = Model::new();
        let root = m.root();
        let a = m
            .add_node(root, ModelNode::new("a", ModelNodeKind::Group))
            .unwrap();
        let b = m
            .add_node(root, ModelNode::new("b", ModelNodeKind::Group))
            .unwrap();
        let c = m
            .add_node(root, ModelNode::new("c", ModelNodeKind::Group))
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

    #[test]
    fn model_snapshots_share_large_payloads_until_mutated() {
        let mut model = Model::new();
        let node = model
            .add_node(model.root(), ModelNode::new("mesh", part_kind()))
            .unwrap();
        let param = model.add_param(ModelParam {
            name: "deform".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
        });
        let key = BindingKey::new(param, node, BindingTarget::Deform);
        model
            .set_deform_vertices(&key, [1, 0], vec![1.0; 8])
            .unwrap();

        let mut edited = model.clone();
        let mesh_arc = |m: &Model| match &m.node(node).unwrap().kind {
            ModelNodeKind::Part(part) => part.mesh.0.clone(),
            _ => unreachable!(),
        };
        let cells_arc = |m: &Model| m.binding(&key).unwrap().values.0.clone();
        let original_mesh = mesh_arc(&model);
        let original_cells = cells_arc(&model);
        assert!(Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));
        assert!(Arc::ptr_eq(&original_cells, &cells_arc(&edited)));

        // An unrelated edit still shares.
        edited
            .update_node(node, |n| n.name = "renamed".into())
            .unwrap();
        assert!(Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));

        let mut moved = quad();
        moved.verts[0] = 7.0;
        edited.set_node_mesh(node, moved).unwrap();
        edited
            .set_deform_vertices(&key, [1, 0], vec![2.0; 8])
            .unwrap();

        assert!(!Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));
        assert!(!Arc::ptr_eq(&original_cells, &cells_arc(&edited)));
        // The snapshot taken before the edits is byte-identical afterwards.
        assert_eq!(model.node_mesh(node).unwrap().verts, quad().verts);
        assert_eq!(
            deform_cells(model.binding(&key).unwrap().values())
                .unwrap()
                .iter()
                .map(|c| (c.x, c.y, c.value.clone()))
                .collect::<Vec<_>>(),
            vec![(0, 0, vec![0.0; 8]), (1, 0, vec![1.0; 8])],
        );
    }

    #[test]
    fn duplicate_copies_bindings_and_remaps_internal_masks() {
        let mut m = Model::new();
        let root = m.root();
        let group = m
            .add_node(root, ModelNode::new("g", ModelNodeKind::Group))
            .unwrap();
        let mask_src = m
            .add_node(group, ModelNode::new("mask", part_kind()))
            .unwrap();
        let masked = m
            .add_node(group, ModelNode::new("masked", part_kind()))
            .unwrap();
        m.mask_add(masked, mask_src, MaskMode::Mask).unwrap();

        let param = m.add_param(ModelParam {
            name: "x".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
        });
        let key = BindingKey::new(param, masked, BindingTarget::Scalar(ScalarTarget::Tx));
        m.set_binding_key(&key, [1, 0], 5.0).unwrap();

        let copy = m.duplicate_subtree(group).unwrap();
        // the copy lands right after the original.
        assert_eq!(m.node(root).unwrap().children(), &[group, copy]);
        assert_eq!(m.node(copy).unwrap().name, "g copy");

        let copy_children = m.node(copy).unwrap().children().to_vec();
        assert_eq!(copy_children.len(), 2);
        let copy_masked = copy_children[1];
        // internal mask reference points at the copied source, not the original.
        if let ModelNodeKind::Part(p) = &m.node(copy_masked).unwrap().kind {
            assert_eq!(p.masks().len(), 1);
            assert_eq!(p.masks()[0].source(), copy_children[0]);
        } else {
            panic!("expected part");
        }
        // the copied node has its own binding.
        assert_eq!(m.bindings_of_param(param).count(), 2);
        assert!(m.bindings().any(|b| b.node() == copy_masked));
        assert!(m.to_clp_bytes().is_ok());
    }

    #[test]
    fn mask_ops_validate_and_reorder() {
        let mut m = Model::new();
        let root = m.root();
        let a = m.add_node(root, ModelNode::new("a", part_kind())).unwrap();
        let b = m.add_node(root, ModelNode::new("b", part_kind())).unwrap();
        let c = m.add_node(root, ModelNode::new("c", part_kind())).unwrap();
        let target = m.add_node(root, ModelNode::new("t", part_kind())).unwrap();

        assert!(m.mask_add(target, target, MaskMode::Mask).is_err());
        assert!(m
            .mask_add(a, root, MaskMode::Mask)
            .is_err_and(|e| matches!(e, ModelError::NotAPart)));

        m.mask_add(target, a, MaskMode::Mask).unwrap();
        m.mask_add(target, b, MaskMode::DodgeMask).unwrap();
        m.mask_add(target, c, MaskMode::Mask).unwrap();
        m.mask_reorder(target, 2, 0).unwrap();
        m.mask_set_mode(target, 1, MaskMode::DodgeMask).unwrap();
        m.mask_delete(target, 2).unwrap();
        if let ModelNodeKind::Part(p) = &m.node(target).unwrap().kind {
            assert_eq!(p.masks().len(), 2);
            assert_eq!(p.masks()[0].source(), c);
            assert!(matches!(p.masks()[1].mode(), MaskMode::DodgeMask));
        } else {
            panic!("expected part");
        }
        assert!(m.mask_delete(target, 5).is_err());
    }

    #[test]
    fn cross_references_are_checked_before_they_land() {
        let mut r = rig();
        // A texture, param and node this model no longer has.
        let orphan_tex = r.model.add_texture(ModelTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: Arc::new(Vec::new()),
        });
        r.model.delete_texture(orphan_tex).unwrap();
        assert!(matches!(
            r.model.set_part_albedo(r.part, Some(orphan_tex)),
            Err(ModelError::UnknownTexture)
        ));
        assert!(matches!(
            r.model.set_part_albedo(r.composite, Some(r.tex)),
            Err(ModelError::NotAPart)
        ));
        let orphan_param = r.model.add_param(ModelParam {
            name: "gone".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
        });
        r.model.delete_param(orphan_param).unwrap();
        let orphan_node = r
            .model
            .add_node(r.root, ModelNode::new("gone", ModelNodeKind::Group))
            .unwrap();
        r.model.delete_node(orphan_node).unwrap();
        assert!(matches!(
            r.model.set_physics_target(r.physics, Some(orphan_param)),
            Err(ModelError::UnknownParam)
        ));
        assert!(matches!(
            r.model
                .set_welds(vec![ModelWeld::new(r.part, orphan_node, Vec::new())]),
            Err(ModelError::UnknownNode)
        ));
        // A node cloned out of another model cannot smuggle its keys in.
        let mut foreign = ModelPart::new(quad());
        foreign.albedo = Some(orphan_tex);
        assert!(matches!(
            r.model.add_node(
                r.root,
                ModelNode::new("smuggler", ModelNodeKind::Part(foreign))
            ),
            Err(ModelError::UnknownTexture)
        ));
        assert!(r.model.to_clp_bytes().is_ok());
    }
}

#[cfg(test)]
mod bench {
    use super::tests_support::dense_model;
    use std::time::Instant;

    /// Undo is a stack of `Model` snapshots, so a snapshot has to cost about
    /// nothing next to the edit that follows it. This is the measurement the
    /// number in the module doc comes from; run it with
    /// `cargo test -p catchlight-core --lib -- --ignored --nocapture bench`.
    #[test]
    #[ignore = "a timing measurement, not a pass/fail check"]
    fn clone_and_edit_a_five_hundred_node_model() {
        let (model, key) = dense_model(500, 64, 3);
        let bytes = model.estimated_size_bytes();

        let runs = 200;
        let start = Instant::now();
        let mut sink = 0usize;
        for _ in 0..runs {
            let snapshot = model.clone();
            sink += snapshot.node_count();
            std::hint::black_box(snapshot);
        }
        let clone_ns = start.elapsed().as_nanos() / runs;

        let start = Instant::now();
        for _ in 0..runs {
            let mut snapshot = model.clone();
            snapshot
                .set_deform_vertices(&key, [1, 1], vec![1.0; 128])
                .unwrap();
            std::hint::black_box(snapshot);
        }
        let edit_ns = start.elapsed().as_nanos() / runs;

        println!(
            "500 nodes / {} bindings / {:.1} MiB: clone {:.1} us, clone + one deform cell {:.1} us ({sink})",
            model.bindings().count(),
            bytes as f64 / (1024.0 * 1024.0),
            clone_ns as f64 / 1000.0,
            edit_ns as f64 / 1000.0,
        );
    }
}

/// Model builders shared by the tests and the benchmark.
#[cfg(test)]
mod tests_support {
    use super::*;
    use crate::formats::clp::ClpIndices;

    /// A model with `nodes` parts, each carrying a `verts`-vertex mesh and a
    /// deform binding whose `keys` x `keys` grid is fully authored.
    pub(super) fn dense_model(nodes: usize, verts: usize, keys: usize) -> (Model, BindingKey) {
        let positions: Vec<f32> = (0..keys).map(|i| i as f32 / (keys - 1) as f32).collect();
        let mut model = Model::new();
        let root = model.root();
        let param = model.add_param(ModelParam {
            name: "sweep".into(),
            is_vec2: true,
            min: [-1.0, -1.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_points_x: positions.clone(),
            axis_points_y: positions,
        });
        let mesh = ClpMesh {
            verts: (0..verts * 2).map(|i| i as f32).collect(),
            uvs: vec![0.0; verts * 2],
            indices: ClpIndices::U16(Vec::new()),
            origin: [0.0, 0.0],
        };
        let mut last = None;
        for i in 0..nodes {
            let node = model
                .add_node(
                    root,
                    ModelNode::new(
                        format!("part-{i}"),
                        ModelNodeKind::Part(ModelPart::new(mesh.clone())),
                    ),
                )
                .unwrap();
            let key = BindingKey::new(param, node, BindingTarget::Deform);
            for y in 0..keys as u32 {
                for x in 0..keys as u32 {
                    model
                        .set_deform_vertices(&key, [x, y], vec![x as f32; verts * 2])
                        .unwrap();
                }
            }
            last = Some(key);
        }
        let key = last.expect("at least one node");
        (model, key)
    }
}
