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
//! - **An Id is unique within the model and never changes on its own.** This
//!   is where the uniqueness [`crate::id`] cannot check is enforced: a
//!   generated Id is re-drawn until it is free, an author-chosen one is
//!   refused if it is taken, and [`Model::rename_node_id`] and its two
//!   siblings are the only way an Id ever changes — each rewrites every
//!   reference to it. The root's Id starts out `root`; a model read from a
//!   `.clp`, which stores no Ids, gets `node-<arena index>` for the rest.
//! - **Nothing is addressed by name.** A [`Name`] is a label: free to change,
//!   free to repeat, and never a key.
//! - **Every param is a scalar.** Joint control over two params is a property
//!   of the binding — a [`BindingKey`] names one or two params and its grid is
//!   the product of their key positions — so a pose is a plain map
//!   `ParamId -> f32` and nothing else carries a second dimension.
//! - **Sibling order is document state.** It is the draw order for equal z
//!   order, so [`Model::reorder`] is an edit, not view state.
//! - **Textures stay source-encoded.** A [`ModelTexture`] keeps the author's
//!   bytes verbatim; decoding is the render cache's job.
//! - **Derived values are memoized, never stored.** A binding's dense grid —
//!   what `crate::fill` derives from its authored cells — is built on first
//!   read and dropped the moment the cells, the key positions or the mesh it
//!   was derived from move. Nothing else on a Model is derived.
//! - **Heavy leaves are shared.** Meshes, binding cell grids and texture bytes
//!   sit behind `Arc` and are edited through `Arc::make_mut`, so cloning a
//!   Model for undo is a shallow copy of small structs plus refcount bumps.
//!   Measured by `bench::clone_and_edit_a_five_hundred_node_model` (release,
//!   `--ignored`): a 500-node, 500-binding, 3.1 MiB model clones in **~50 µs**
//!   and clones-then-authors-one-deform-cell in **~62 µs** — the difference is
//!   the one 3x3x128-float grid `Arc::make_mut` has to copy out. Undo pushes
//!   one snapshot per edit, so that is the whole per-edit cost of the history.
//!
//! Pure and wasm-safe: no GPU, no async, no filesystem.

mod binding;
mod check;
mod eval;
mod flatten;

pub use binding::{
    deform_cells, mask_mode_name, param_range_is_valid, scalar_cells, target_of, BindingKey,
    BindingParams, BindingTarget, DenseGrid, ScalarTarget,
};
pub use check::CheckWarning;
pub use eval::Pose;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

use crate::components::{BlendMode, MaskMode};
use crate::formats::clp::{
    self as clp, ClpBindingValues, ClpMesh, ClpMeshGroup, ClpPhysics, ClpTransform, TextureAlpha,
    TextureEncoding,
};
use crate::id::{HexSource, IdError, Name, NodeId, NodeIdKind, ParamId, TexId};
use crate::params::InterpolateMode;
use crate::physics::{PendulumKind, PhysicsParamMapMode};

/// How many times a generated Id is re-drawn before the model gives up. The
/// 32 bits collide by birthday around 2^16 siblings, so a handful of retries
/// covers any real model and the cap only fires on a wedged [`HexSource`].
const ID_MINT_ATTEMPTS: usize = 64;

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
    #[error("id {0:?} is already used in this model")]
    DuplicateId(String),
    #[error("could not draw a free id in {ID_MINT_ATTEMPTS} attempts")]
    IdExhausted,
    #[error(transparent)]
    Id(#[from] IdError),
    #[error(".clp node arena must contain exactly one root at index 0")]
    InvalidClpRoot,
    #[error(".clp node {node} parent index {parent} must name a preceding node")]
    InvalidClpParent { node: usize, parent: u32 },
    #[error("cannot reparent a node under itself or a descendant")]
    Cycle,
    #[error("the root node cannot be {0}")]
    Root(&'static str),
    #[error("binding cell is outside the param's key grid")]
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
    #[error("a two-param binding needs two different params")]
    SelfPairedBinding,
    #[error(
        "the two-param binding on node {node} ({target}) cannot be written to .clp v0: its \
         params are not an adjacent `<name>.x` / `<name>.y` pair"
    )]
    UnpairableBinding { node: String, target: &'static str },
    #[error(
        "physics node {0} drives two params that are not an adjacent `<name>.x` / `<name>.y` \
         pair, which .clp v0 cannot express"
    )]
    UnpairablePhysicsTarget(String),
    #[error("binding target does not match the operation")]
    WrongTarget,
    #[error("index out of range")]
    IndexOutOfRange,
    #[error("mesh is malformed: {0}")]
    MalformedMesh(&'static str),
    #[error(".clp codec: {0}")]
    Clp(#[from] crate::formats::clp::ClpError),
    #[error(transparent)]
    LoadLimit(#[from] crate::LoadLimitError),
}

/// A source-encoded texture (verbatim PNG/TGA bytes), shared via `Arc` so model
/// snapshots are cheap to clone even when the structure churns.
#[derive(Debug, Clone)]
pub struct ModelTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: Arc<Vec<u8>>,
}

/// The authored model: a tree of nodes by Id, ordered params and textures, the
/// bindings params drive nodes through, and authored physics. The tree is
/// always valid (single root, no cycles, no dangling cross-references), so
/// [`Model::flatten`] is total.
#[derive(Debug, Clone)]
pub struct Model {
    generation: u64,
    physics: ClpPhysics,
    welds: Vec<ModelWeld>,
    nodes: HashMap<NodeId, ModelNode>,
    root: NodeId,
    params: HashMap<ParamId, ModelParam>,
    param_order: Vec<ParamId>,
    textures: HashMap<TexId, ModelTexture>,
    texture_order: Vec<TexId>,
    bindings: Vec<ModelBinding>,
}

/// A welded part pair: two parts whose vertex pairs are pulled together after
/// every other deformation.
#[derive(Debug, Clone)]
pub struct ModelWeld {
    a: NodeId,
    b: NodeId,
    pairs: Arc<Vec<clp::ClpWeldPair>>,
}

impl ModelWeld {
    pub fn new(a: NodeId, b: NodeId, pairs: Vec<clp::ClpWeldPair>) -> Self {
        Self {
            a,
            b,
            pairs: Arc::new(pairs),
        }
    }

    pub fn a(&self) -> &NodeId {
        &self.a
    }

    pub fn b(&self) -> &NodeId {
        &self.b
    }

    pub fn pairs(&self) -> &[clp::ClpWeldPair] {
        &self.pairs
    }
}

#[derive(Debug, Clone)]
pub struct ModelNode {
    pub name: Name,
    pub enabled: bool,
    pub z_order: f32,
    pub transform: ClpTransform,
    pub lock_to_root: bool,
    pub kind: ModelNodeKind,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
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

    /// The segment a generated Id records the node's kind as.
    fn id_kind(&self) -> NodeIdKind {
        match self {
            Self::Group => NodeIdKind::Group,
            Self::Part(_) => NodeIdKind::Part,
            Self::Composite(_) => NodeIdKind::Composite,
            Self::MeshGroup(_) => NodeIdKind::MeshGroup,
            Self::SimplePhysics(_) => NodeIdKind::SimplePhysics,
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
    albedo: Option<TexId>,
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

    pub fn albedo(&self) -> Option<&TexId> {
        self.albedo.as_ref()
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
#[derive(Debug, Clone)]
pub struct ModelMask {
    source: NodeId,
    mode: MaskMode,
}

impl ModelMask {
    pub fn source(&self) -> &NodeId {
        &self.source
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
    target_params: [Option<ParamId>; 2],
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
            target_params: [None, None],
        }
    }

    /// The two params the pendulum's swing is written into, in the order the
    /// map mode produces them. Either may be unset — a pendulum aimed at one
    /// param writes only the first.
    pub fn target_params(&self) -> &[Option<ParamId>; 2] {
        &self.target_params
    }

    /// Whether the pendulum writes `param`.
    pub fn drives(&self, param: &ParamId) -> bool {
        self.target_params.iter().flatten().any(|p| p == param)
    }
}

/// A named scalar the author exposes for posing. Joint control over two
/// params is a property of the *binding*, not of the param: see
/// [`BindingParams`].
#[derive(Debug, Clone)]
pub struct ModelParam {
    pub name: Name,
    pub min: f32,
    pub max: f32,
    /// Rest value, in param-value space (unlike the key positions).
    pub default: f32,
    /// The values along the param at which a binding may hold authored cells,
    /// normalized 0..1 across `[min, max]` — the same convention `.clp` stores
    /// and the runtime interpolates in.
    pub key_positions: Vec<f32>,
}

impl ModelParam {
    /// A param over `[min, max]` resting at `default`, with a key position at
    /// each end.
    pub fn new(name: Name, min: f32, max: f32, default: f32) -> Self {
        Self {
            name,
            min,
            max,
            default,
            key_positions: vec![0.0, 1.0],
        }
    }
}

/// One or two params' control over one property of one node. Bindings live on
/// the model, not on the param, and are addressed by their [`BindingKey`].
#[derive(Debug, Clone)]
pub struct ModelBinding {
    key: BindingKey,
    interpolate_mode: InterpolateMode,
    values: ModelBindingValues,
    /// The dense grid derived from `values`, built on the first read. Shared
    /// by every snapshot that shares the cells, and dropped by
    /// [`Self::values_mut`] — the only way the cells change.
    dense: OnceLock<Arc<DenseGrid>>,
}

impl ModelBinding {
    pub fn key(&self) -> &BindingKey {
        &self.key
    }

    /// The one or two params that drive it.
    pub fn params(&self) -> &BindingParams {
        &self.key.params
    }

    pub fn node(&self) -> &NodeId {
        &self.key.node
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

    /// Copy-on-write access to the cells, dropping the dense grid derived from
    /// them. This is the only way a binding's cells change, which is what
    /// makes the memo safe to hand out from a `&self` method.
    fn values_mut(&mut self) -> &mut ClpBindingValues {
        self.dense.take();
        Arc::make_mut(&mut self.values.0)
    }

    /// Drop the derived grid without touching the cells — for a change to the
    /// key positions or to the mesh the deform identity is sized to.
    fn invalidate_dense(&mut self) {
        self.dense.take();
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
    /// A node at the identity transform (unit scale), enabled, z order 0. The
    /// label is truncated rather than rejected — a name is not a key.
    pub fn new(name: impl AsRef<str>, kind: ModelNodeKind) -> Self {
        Self {
            name: Name::truncated(name),
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

    pub fn parent(&self) -> Option<&NodeId> {
        self.parent.as_ref()
    }

    pub fn children(&self) -> &[NodeId] {
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

/// The Id a fresh model's root carries until an author renames it.
const DEFAULT_ROOT_ID: &str = "root";

impl Model {
    /// A new model with a single `Group` root named "Root", at Id `root`.
    pub fn new() -> Self {
        let root = NodeId::from_generated(DEFAULT_ROOT_ID.to_string());
        let mut nodes = HashMap::new();
        nodes.insert(root.clone(), ModelNode::new("Root", ModelNodeKind::Group));
        Self {
            generation: 0,
            physics: ClpPhysics::default(),
            welds: Vec::new(),
            nodes,
            root,
            params: HashMap::new(),
            param_order: Vec::new(),
            textures: HashMap::new(),
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

    pub fn root(&self) -> &NodeId {
        &self.root
    }

    pub fn node(&self, id: &NodeId) -> Option<&ModelNode> {
        self.nodes.get(id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn node_ids(&self) -> impl Iterator<Item = &NodeId> {
        self.nodes.keys()
    }

    /// Edit one node's own properties: name, transform, z order, enabled,
    /// lock-to-root and the plain values on its kind. Cross-references (the
    /// albedo, masks, the physics target) and the mesh have their own methods,
    /// because they have to be checked against the rest of the model.
    pub fn update_node<R>(
        &mut self,
        id: &NodeId,
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
    pub fn nodes_in_order(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack = vec![self.root.clone()];
        while let Some(id) = stack.pop() {
            if let Some(n) = self.nodes.get(&id) {
                for child in n.children.iter().rev() {
                    stack.push(child.clone());
                }
            }
            out.push(id);
        }
        out
    }

    /// Insert `node` as the last child of `parent` under a generated Id
    /// (`<parent>/<kind>-<8 hex>`, re-drawn until it is free).
    pub fn add_node(
        &mut self,
        parent: &NodeId,
        node: ModelNode,
        hex: &mut impl HexSource,
    ) -> Result<NodeId, ModelError> {
        let kind = node.kind.id_kind();
        let id = self.mint(
            |h| NodeId::generate(parent, kind, h),
            |m, id| m.nodes.contains_key(id),
            hex,
        )?;
        self.add_node_with_id(id.clone(), parent, node)?;
        Ok(id)
    }

    /// Insert `node` under an Id the author chose. Fails if the Id is taken.
    pub fn add_node_with_id(
        &mut self,
        id: NodeId,
        parent: &NodeId,
        mut node: ModelNode,
    ) -> Result<(), ModelError> {
        if !self.nodes.contains_key(parent) {
            return Err(ModelError::UnknownNode);
        }
        if self.nodes.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.to_string()));
        }
        // A node cloned out of another model carries that model's Ids.
        self.check_node_refs(&node)?;
        node.parent = Some(parent.clone());
        node.children.clear();
        self.nodes.insert(id.clone(), node);
        if let Some(p) = self.nodes.get_mut(parent) {
            p.children.push(id);
        }
        self.bump();
        Ok(())
    }

    /// Remove a node and its whole subtree, then drop every mask and binding
    /// that pointed into the removed set so the model stays referentially valid.
    pub fn delete_node(&mut self, id: &NodeId) -> Result<(), ModelError> {
        if id == &self.root {
            return Err(ModelError::Root("deleted"));
        }
        if !self.nodes.contains_key(id) {
            return Err(ModelError::UnknownNode);
        }
        let removed: HashSet<NodeId> = self.subtree(id).into_iter().collect();
        if let Some(parent) = self.nodes.get(id).and_then(|n| n.parent.clone()) {
            if let Some(p) = self.nodes.get_mut(&parent) {
                p.children.retain(|c| c != id);
            }
        }
        for r in &removed {
            self.nodes.remove(r);
        }
        for node in self.nodes.values_mut() {
            if let Some(masks) = node.masks_mut() {
                masks.retain(|m| !removed.contains(&m.source));
            }
        }
        self.bindings.retain(|b| !removed.contains(&b.key.node));
        self.welds
            .retain(|w| !removed.contains(&w.a) && !removed.contains(&w.b));
        self.bump();
        Ok(())
    }

    /// Move `id` (and its subtree) under `new_parent`. Rejects moving the root
    /// or creating a cycle. The node keeps its Id, parent prefix and all: the
    /// prefix records where a node was created, not where it lives.
    pub fn reparent(&mut self, id: &NodeId, new_parent: &NodeId) -> Result<(), ModelError> {
        if id == &self.root {
            return Err(ModelError::Root("reparented"));
        }
        if !self.nodes.contains_key(id) || !self.nodes.contains_key(new_parent) {
            return Err(ModelError::UnknownNode);
        }
        if self.is_self_or_descendant(new_parent, id) {
            return Err(ModelError::Cycle);
        }
        let old_parent = self.nodes.get(id).and_then(|n| n.parent.clone());
        if let Some(op) = old_parent {
            if let Some(p) = self.nodes.get_mut(&op) {
                p.children.retain(|c| c != id);
            }
        }
        if let Some(p) = self.nodes.get_mut(new_parent) {
            p.children.push(id.clone());
        }
        if let Some(n) = self.nodes.get_mut(id) {
            n.parent = Some(new_parent.clone());
        }
        self.bump();
        Ok(())
    }

    /// Move `id` to `index` within its parent's children (clamped to the end).
    /// Sibling order is draw-list order for equal z order, so this is a document
    /// edit, not view state.
    pub fn reorder(&mut self, id: &NodeId, index: usize) -> Result<(), ModelError> {
        if id == &self.root {
            return Err(ModelError::Root("reordered"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent.clone())
            .ok_or(ModelError::UnknownNode)?;
        let p = self.nodes.get_mut(&parent).ok_or(ModelError::UnknownNode)?;
        let cur = p
            .children
            .iter()
            .position(|c| c == id)
            .ok_or(ModelError::UnknownNode)?;
        let moved = p.children.remove(cur);
        let index = index.min(p.children.len());
        p.children.insert(index, moved);
        self.bump();
        Ok(())
    }

    /// Deep-copy `id`'s subtree as its next sibling, under fresh Ids. Mask
    /// references inside the subtree point at the copies; external ones stay
    /// shared. Each copied node also copies its param bindings, so the
    /// duplicate deforms like the original. The copy's root is renamed
    /// "<name> copy".
    pub fn duplicate_subtree(
        &mut self,
        id: &NodeId,
        hex: &mut impl HexSource,
    ) -> Result<NodeId, ModelError> {
        if id == &self.root {
            return Err(ModelError::Root("duplicated"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent.clone())
            .ok_or(ModelError::UnknownNode)?;

        // Pre-order with sibling order preserved (parent precedes children).
        let mut order = Vec::new();
        let mut stack = vec![id.clone()];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.get(&n) {
                for c in node.children.iter().rev() {
                    stack.push(c.clone());
                }
            }
            order.push(n);
        }

        let mut map: HashMap<NodeId, NodeId> = HashMap::new();
        for old in &order {
            let mut copy = self.nodes.get(old).ok_or(ModelError::UnknownNode)?.clone();
            let new_parent = match &copy.parent {
                Some(p) if old != id => map.get(p).ok_or(ModelError::UnknownNode)?.clone(),
                _ => parent.clone(),
            };
            if old == id {
                copy.name = Name::truncated(format!("{} copy", copy.name));
            }
            copy.parent = Some(new_parent.clone());
            copy.children.clear();
            let kind = copy.kind.id_kind();
            let new_id = self.mint(
                |h| NodeId::generate(&new_parent, kind, h),
                |m, id| m.nodes.contains_key(id),
                hex,
            )?;
            self.nodes.insert(new_id.clone(), copy);
            if let Some(p) = self.nodes.get_mut(&new_parent) {
                p.children.push(new_id.clone());
            }
            map.insert(old.clone(), new_id);
        }

        for new_id in map.values() {
            if let Some(masks) = self.nodes.get_mut(new_id).and_then(ModelNode::masks_mut) {
                for m in masks.iter_mut() {
                    if let Some(mapped) = map.get(&m.source) {
                        m.source = mapped.clone();
                    }
                }
            }
        }

        let copied: Vec<ModelBinding> = self
            .bindings
            .iter()
            .filter_map(|b| {
                map.get(&b.key.node).map(|node| ModelBinding {
                    key: BindingKey {
                        params: b.key.params.clone(),
                        node: node.clone(),
                        target: b.key.target,
                    },
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.clone(),
                    dense: b.dense.clone(),
                })
            })
            .collect();
        self.bindings.extend(copied);

        let new_root = map.get(id).ok_or(ModelError::UnknownNode)?.clone();
        let pos = self
            .nodes
            .get(&parent)
            .and_then(|p| p.children.iter().position(|c| c == id))
            .ok_or(ModelError::UnknownNode)?;
        // reorder bumps the generation for the whole duplicate.
        self.reorder(&new_root, pos + 1)?;
        Ok(new_root)
    }

    // ---- renaming ----

    /// Give a node a different Id, rewriting every reference to it: its
    /// parent's child list, its children's parent, masks that name it as a
    /// source, bindings that drive it, welds that end on it, and the root
    /// itself. Renaming is a breaking change for any addon that referenced the
    /// old Id; there are no aliases.
    pub fn rename_node_id(&mut self, old: &NodeId, new: NodeId) -> Result<(), ModelError> {
        if old == &new {
            return Ok(());
        }
        if !self.nodes.contains_key(old) {
            return Err(ModelError::UnknownNode);
        }
        if self.nodes.contains_key(&new) {
            return Err(ModelError::DuplicateId(new.to_string()));
        }
        let node = self.nodes.remove(old).ok_or(ModelError::UnknownNode)?;
        let parent = node.parent.clone();
        let children = node.children.clone();
        self.nodes.insert(new.clone(), node);
        if let Some(p) = parent.and_then(|p| self.nodes.get_mut(&p)) {
            for c in &mut p.children {
                if c == old {
                    *c = new.clone();
                }
            }
        }
        for child in children {
            if let Some(c) = self.nodes.get_mut(&child) {
                c.parent = Some(new.clone());
            }
        }
        for node in self.nodes.values_mut() {
            if let Some(masks) = node.masks_mut() {
                for m in masks.iter_mut() {
                    if &m.source == old {
                        m.source = new.clone();
                    }
                }
            }
        }
        for b in &mut self.bindings {
            if &b.key.node == old {
                b.key.node = new.clone();
            }
        }
        for w in &mut self.welds {
            if &w.a == old {
                w.a = new.clone();
            }
            if &w.b == old {
                w.b = new.clone();
            }
        }
        if &self.root == old {
            self.root = new;
        }
        self.bump();
        Ok(())
    }

    /// Give a param a different Id, rewriting the bindings it drives and any
    /// physics node aimed at it.
    pub fn rename_param_id(&mut self, old: &ParamId, new: ParamId) -> Result<(), ModelError> {
        if old == &new {
            return Ok(());
        }
        if !self.params.contains_key(old) {
            return Err(ModelError::UnknownParam);
        }
        if self.params.contains_key(&new) {
            return Err(ModelError::DuplicateId(new.to_string()));
        }
        let param = self.params.remove(old).ok_or(ModelError::UnknownParam)?;
        self.params.insert(new.clone(), param);
        for p in &mut self.param_order {
            if p == old {
                *p = new.clone();
            }
        }
        for b in &mut self.bindings {
            match &mut b.key.params {
                BindingParams::One(p) => {
                    if p == old {
                        *p = new.clone();
                    }
                }
                BindingParams::Two(x, y) => {
                    if x == old {
                        *x = new.clone();
                    }
                    if y == old {
                        *y = new.clone();
                    }
                }
            }
        }
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::SimplePhysics(ph) = &mut node.kind {
                for t in &mut ph.target_params {
                    if t.as_ref() == Some(old) {
                        *t = Some(new.clone());
                    }
                }
            }
        }
        self.bump();
        Ok(())
    }

    /// Give a texture a different Id, rewriting every part that draws with it.
    pub fn rename_tex_id(&mut self, old: &TexId, new: TexId) -> Result<(), ModelError> {
        if old == &new {
            return Ok(());
        }
        if !self.textures.contains_key(old) {
            return Err(ModelError::UnknownTexture);
        }
        if self.textures.contains_key(&new) {
            return Err(ModelError::DuplicateId(new.to_string()));
        }
        let texture = self
            .textures
            .remove(old)
            .ok_or(ModelError::UnknownTexture)?;
        self.textures.insert(new.clone(), texture);
        for t in &mut self.texture_order {
            if t == old {
                *t = new.clone();
            }
        }
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::Part(p) = &mut node.kind {
                if p.albedo.as_ref() == Some(old) {
                    p.albedo = Some(new.clone());
                }
            }
        }
        self.bump();
        Ok(())
    }

    // ---- node cross-references ----

    /// Point a part at a texture, or unmap it (the renderer culls an unmapped
    /// part).
    pub fn set_part_albedo(
        &mut self,
        id: &NodeId,
        albedo: Option<TexId>,
    ) -> Result<(), ModelError> {
        if albedo
            .as_ref()
            .is_some_and(|t| !self.textures.contains_key(t))
        {
            return Err(ModelError::UnknownTexture);
        }
        match self.nodes.get_mut(id).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Part(p)) => p.albedo = albedo,
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode),
        }
        self.bump();
        Ok(())
    }

    /// Aim a simple physics node at up to two params — the pendulum writes
    /// one value into each — or at nothing.
    pub fn set_physics_targets(
        &mut self,
        id: &NodeId,
        targets: [Option<ParamId>; 2],
    ) -> Result<(), ModelError> {
        if targets
            .iter()
            .flatten()
            .any(|p| !self.params.contains_key(p))
        {
            return Err(ModelError::UnknownParam);
        }
        match self.nodes.get_mut(id).map(|n| &mut n.kind) {
            Some(ModelNodeKind::SimplePhysics(ph)) => ph.target_params = targets,
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
        id: &NodeId,
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
            if &b.key.node != id {
                continue;
            }
            // A deform's identity is sized to the mesh, so every binding on
            // the node loses its derived grid, authored cells or not.
            b.invalidate_dense();
            if let ClpBindingValues::Deform(cells) = b.values_mut() {
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
    pub fn set_node_mesh(&mut self, id: &NodeId, mesh: ClpMesh) -> Result<(), ModelError> {
        self.set_node_mesh_with(id, mesh, |_, new, offsets| {
            let mut out = offsets.to_vec();
            out.resize(new.verts.len(), 0.0);
            out
        })
    }

    // ---- masks ----

    fn masks_mut(&mut self, id: &NodeId) -> Result<&mut Vec<ModelMask>, ModelError> {
        match self.nodes.get_mut(id) {
            Some(n) => n.masks_mut().ok_or(ModelError::NotMaskable),
            None => Err(ModelError::UnknownNode),
        }
    }

    /// Append a mask source. Sources must be parts (the renderer rasterizes a
    /// source's own mesh + texture into the mask).
    pub fn mask_add(
        &mut self,
        id: &NodeId,
        source: &NodeId,
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
        let source = source.clone();
        self.masks_mut(id)?.push(ModelMask { source, mode });
        self.bump();
        Ok(())
    }

    pub fn mask_set_mode(
        &mut self,
        id: &NodeId,
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
    pub fn mask_reorder(&mut self, id: &NodeId, index: usize, to: usize) -> Result<(), ModelError> {
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

    pub fn mask_delete(&mut self, id: &NodeId, index: usize) -> Result<(), ModelError> {
        let masks = self.masks_mut(id)?;
        if index >= masks.len() {
            return Err(ModelError::IndexOutOfRange);
        }
        masks.remove(index);
        self.bump();
        Ok(())
    }

    // ---- params ----

    pub fn param_ids(&self) -> &[ParamId] {
        &self.param_order
    }

    pub fn param(&self, id: &ParamId) -> Option<&ModelParam> {
        self.params.get(id)
    }

    /// Add a param under a generated Id (`param-<8 hex>`).
    pub fn add_param(
        &mut self,
        param: ModelParam,
        hex: &mut impl HexSource,
    ) -> Result<ParamId, ModelError> {
        let id = self.mint(ParamId::generate, |m, id| m.params.contains_key(id), hex)?;
        self.add_param_with_id(id.clone(), param)?;
        Ok(id)
    }

    /// Add a param under an Id the author chose. Fails if the Id is taken.
    pub fn add_param_with_id(&mut self, id: ParamId, param: ModelParam) -> Result<(), ModelError> {
        if self.params.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.to_string()));
        }
        self.params.insert(id.clone(), param);
        self.param_order.push(id);
        self.bump();
        Ok(())
    }

    /// Remove a param, every binding that named it (a two-param binding loses
    /// half its grid, so it goes too), and any physics node that drove it.
    pub fn delete_param(&mut self, id: &ParamId) -> Result<(), ModelError> {
        if self.params.remove(id).is_none() {
            return Err(ModelError::UnknownParam);
        }
        self.param_order.retain(|p| p != id);
        self.bindings.retain(|b| !b.key.params.contains(id));
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::SimplePhysics(ph) = &mut node.kind {
                for t in &mut ph.target_params {
                    if t.as_ref() == Some(id) {
                        *t = None;
                    }
                }
            }
        }
        self.bump();
        Ok(())
    }

    // ---- textures ----

    pub fn texture_ids(&self) -> &[TexId] {
        &self.texture_order
    }

    pub fn texture(&self, id: &TexId) -> Option<&ModelTexture> {
        self.textures.get(id)
    }

    /// Add a texture under a generated Id (`tex-<8 hex>`).
    pub fn add_texture(
        &mut self,
        texture: ModelTexture,
        hex: &mut impl HexSource,
    ) -> Result<TexId, ModelError> {
        let id = self.mint(TexId::generate, |m, id| m.textures.contains_key(id), hex)?;
        self.add_texture_with_id(id.clone(), texture)?;
        Ok(id)
    }

    /// Add a texture under an Id the author chose. Fails if the Id is taken.
    pub fn add_texture_with_id(
        &mut self,
        id: TexId,
        texture: ModelTexture,
    ) -> Result<(), ModelError> {
        if self.textures.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.to_string()));
        }
        self.textures.insert(id.clone(), texture);
        self.texture_order.push(id);
        self.bump();
        Ok(())
    }

    /// Remove a texture and unmap any part that referenced it.
    pub fn delete_texture(&mut self, id: &TexId) -> Result<(), ModelError> {
        if self.textures.remove(id).is_none() {
            return Err(ModelError::UnknownTexture);
        }
        self.texture_order.retain(|t| t != id);
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::Part(p) = &mut node.kind {
                if p.albedo.as_ref() == Some(id) {
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
            .any(|w| !self.nodes.contains_key(&w.a) || !self.nodes.contains_key(&w.b))
        {
            return Err(ModelError::UnknownNode);
        }
        self.welds = welds;
        self.bump();
        Ok(())
    }

    // ---- accounting ----

    pub fn estimated_size_bytes(&self) -> usize {
        let mut bytes =
            std::mem::size_of::<Self>()
                .saturating_add(self.nodes.len().saturating_mul(
                    std::mem::size_of::<ModelNode>() + std::mem::size_of::<NodeId>(),
                ))
                .saturating_add(self.params.len().saturating_mul(
                    std::mem::size_of::<ModelParam>() + std::mem::size_of::<ParamId>(),
                ))
                .saturating_add(self.textures.len().saturating_mul(
                    std::mem::size_of::<ModelTexture>() + std::mem::size_of::<TexId>(),
                ))
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
        for (id, node) in &self.nodes {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(node.name.as_str().len())
                .saturating_add(
                    node.children
                        .capacity()
                        .saturating_mul(std::mem::size_of::<NodeId>()),
                );
            if let Some(mesh) = node.mesh() {
                bytes = bytes.saturating_add(mesh_size(mesh));
            }
            if let Some(masks) = node.masks() {
                bytes = bytes
                    .saturating_add(masks.len().saturating_mul(std::mem::size_of::<ModelMask>()));
            }
        }
        for (id, param) in &self.params {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(param.name.as_str().len())
                .saturating_add(
                    param
                        .key_positions
                        .capacity()
                        .saturating_mul(std::mem::size_of::<f32>()),
                );
        }
        for binding in &self.bindings {
            bytes = bytes.saturating_add(binding_values_size(&binding.values));
        }
        for (id, texture) in &self.textures {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(texture.data.capacity());
        }
        bytes
    }

    /// The node's mesh, for the two kinds that carry one.
    pub fn node_mesh(&self, id: &NodeId) -> Option<&ClpMesh> {
        self.node(id)?.mesh()
    }

    /// Draw a generated Id until `taken` says it is free. 32 bits collide
    /// eventually, and [`crate::id`] cannot check uniqueness — this is where
    /// the model does.
    fn mint<T, H: HexSource>(
        &self,
        mut generate: impl FnMut(&mut H) -> T,
        taken: impl Fn(&Self, &T) -> bool,
        hex: &mut H,
    ) -> Result<T, ModelError> {
        for _ in 0..ID_MINT_ATTEMPTS {
            let id = generate(hex);
            if !taken(self, &id) {
                return Ok(id);
            }
        }
        Err(ModelError::IdExhausted)
    }

    /// Every cross-reference a node about to join the model carries has to
    /// name something this model actually has.
    fn check_node_refs(&self, node: &ModelNode) -> Result<(), ModelError> {
        if let ModelNodeKind::Part(p) = &node.kind {
            if p.albedo
                .as_ref()
                .is_some_and(|t| !self.textures.contains_key(t))
            {
                return Err(ModelError::UnknownTexture);
            }
        }
        if let ModelNodeKind::SimplePhysics(ph) = &node.kind {
            if ph
                .target_params
                .iter()
                .flatten()
                .any(|p| !self.params.contains_key(p))
            {
                return Err(ModelError::UnknownParam);
            }
        }
        if let Some(masks) = node.masks() {
            for m in masks {
                match self.nodes.get(&m.source).map(|n| &n.kind) {
                    Some(ModelNodeKind::Part(_)) => {}
                    Some(_) => return Err(ModelError::NotAPart),
                    None => return Err(ModelError::UnknownNode),
                }
            }
        }
        Ok(())
    }

    fn subtree(&self, root: &NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(id) = stack.pop() {
            if let Some(n) = self.nodes.get(&id) {
                stack.extend(n.children.iter().cloned());
            }
            out.push(id);
        }
        out
    }

    fn is_self_or_descendant(&self, candidate: &NodeId, ancestor: &NodeId) -> bool {
        let mut cur = Some(candidate);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            cur = self.nodes.get(c).and_then(|n| n.parent.as_ref());
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
    use crate::id::SeededHex;

    /// One model carrying every kind of thing a mutating method can reach.
    pub(super) struct Rig {
        pub model: Model,
        pub root: NodeId,
        pub part: NodeId,
        pub other: NodeId,
        pub composite: NodeId,
        pub physics: NodeId,
        pub param: ParamId,
        pub tex: TexId,
        pub hex: SeededHex,
    }

    impl Rig {
        fn node(&mut self, name: &str, kind: ModelNodeKind) -> NodeId {
            let root = self.root.clone();
            self.model
                .add_node(&root, ModelNode::new(name, kind), &mut self.hex)
                .unwrap()
        }
    }

    fn rig() -> Rig {
        let mut hex = SeededHex::new(7);
        let mut model = Model::new();
        let root = model.root().clone();
        let tex = model
            .add_texture(
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: Arc::new(vec![0x89, b'P', b'N', b'G']),
                },
                &mut hex,
            )
            .unwrap();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("p"),
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                    key_positions: vec![0.0, 0.5, 1.0],
                },
                &mut hex,
            )
            .unwrap();
        let mut rig = Rig {
            model,
            root,
            part: NodeId::new("placeholder").unwrap(),
            other: NodeId::new("placeholder").unwrap(),
            composite: NodeId::new("placeholder").unwrap(),
            physics: NodeId::new("placeholder").unwrap(),
            param,
            tex,
            hex,
        };
        rig.part = rig.node("part", part_kind());
        rig.other = rig.node("other", part_kind());
        rig.composite = rig.node("composite", ModelNodeKind::Composite(ModelComposite::new()));
        rig.physics = rig.node(
            "physics",
            ModelNodeKind::SimplePhysics(ModelPhysics::new(PendulumKind::RigidPendulum)),
        );
        let (composite, part) = (rig.composite.clone(), rig.part.clone());
        rig.model
            .mask_add(&composite, &part, MaskMode::Mask)
            .unwrap();
        rig
    }

    fn scalar_key(r: &Rig) -> BindingKey {
        BindingKey::new(
            r.param.clone(),
            r.part.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        )
    }

    fn deform_key(r: &Rig) -> BindingKey {
        BindingKey::new(r.param.clone(), r.part.clone(), BindingTarget::Deform)
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
                    r.model.update_node(&r.part, |n| n.z_order = 3.0).unwrap();
                }),
            ),
            (
                "add_node",
                Box::new(|r| {
                    r.node("new", ModelNodeKind::Group);
                }),
            ),
            (
                "add_node_with_id",
                Box::new(|r| {
                    r.model
                        .add_node_with_id(
                            NodeId::new("chosen").unwrap(),
                            &r.root,
                            ModelNode::new("new", ModelNodeKind::Group),
                        )
                        .unwrap();
                }),
            ),
            (
                "delete_node",
                Box::new(|r| r.model.delete_node(&r.other).unwrap()),
            ),
            (
                "reparent",
                Box::new(|r| r.model.reparent(&r.part, &r.composite).unwrap()),
            ),
            (
                "reorder",
                Box::new(|r| r.model.reorder(&r.part, 0).unwrap()),
            ),
            (
                "duplicate_subtree",
                Box::new(|r| {
                    let part = r.part.clone();
                    r.model.duplicate_subtree(&part, &mut r.hex).unwrap();
                }),
            ),
            (
                "rename_node_id",
                Box::new(|r| {
                    r.model
                        .rename_node_id(&r.part, NodeId::new("renamed").unwrap())
                        .unwrap()
                }),
            ),
            (
                "rename_param_id",
                Box::new(|r| {
                    r.model
                        .rename_param_id(&r.param, ParamId::new("renamed").unwrap())
                        .unwrap()
                }),
            ),
            (
                "rename_tex_id",
                Box::new(|r| {
                    r.model
                        .rename_tex_id(&r.tex, TexId::new("renamed").unwrap())
                        .unwrap()
                }),
            ),
            (
                "set_part_albedo",
                Box::new(|r| {
                    r.model
                        .set_part_albedo(&r.part, Some(r.tex.clone()))
                        .unwrap()
                }),
            ),
            (
                "set_physics_target",
                Box::new(|r| {
                    r.model
                        .set_physics_targets(&r.physics, [Some(r.param.clone()), None])
                        .unwrap()
                }),
            ),
            (
                "set_node_mesh",
                Box::new(|r| r.model.set_node_mesh(&r.part, quad()).unwrap()),
            ),
            (
                "set_node_mesh_with",
                Box::new(|r| {
                    r.model
                        .set_node_mesh_with(&r.part, quad(), |_, _, v| v.to_vec())
                        .unwrap()
                }),
            ),
            (
                "mask_add",
                Box::new(|r| {
                    r.model
                        .mask_add(&r.composite, &r.other, MaskMode::Mask)
                        .unwrap()
                }),
            ),
            (
                "mask_set_mode",
                Box::new(|r| {
                    r.model
                        .mask_set_mode(&r.composite, 0, MaskMode::DodgeMask)
                        .unwrap()
                }),
            ),
            (
                "mask_reorder",
                Box::new(|r| r.model.mask_reorder(&r.composite, 0, 0).unwrap()),
            ),
            (
                "mask_delete",
                Box::new(|r| r.model.mask_delete(&r.composite, 0).unwrap()),
            ),
            (
                "add_param",
                Box::new(|r| {
                    let p = param("q");
                    r.model.add_param(p, &mut r.hex).unwrap();
                }),
            ),
            (
                "add_param_with_id",
                Box::new(|r| {
                    r.model
                        .add_param_with_id(ParamId::new("chosen").unwrap(), param("q"))
                        .unwrap()
                }),
            ),
            (
                "delete_param",
                Box::new(|r| r.model.delete_param(&r.param).unwrap()),
            ),
            (
                "add_texture",
                Box::new(|r| {
                    let t = texture();
                    r.model.add_texture(t, &mut r.hex).unwrap();
                }),
            ),
            (
                "add_texture_with_id",
                Box::new(|r| {
                    r.model
                        .add_texture_with_id(TexId::new("chosen").unwrap(), texture())
                        .unwrap()
                }),
            ),
            (
                "delete_texture",
                Box::new(|r| r.model.delete_texture(&r.tex).unwrap()),
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
                            r.part.clone(),
                            r.other.clone(),
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
                Box::new(|r| {
                    r.model
                        .set_param_name(&r.param, Name::truncated("renamed"))
                        .unwrap()
                }),
            ),
            (
                "set_param_default",
                Box::new(|r| r.model.set_param_default(&r.param, 0.5).unwrap()),
            ),
            (
                "set_param_range",
                Box::new(|r| r.model.set_param_range(&r.param, 0.0, 2.0).unwrap()),
            ),
            (
                "key_insert",
                Box::new(|r| {
                    r.model.key_insert(&r.param, 0.25).unwrap();
                }),
            ),
            (
                "key_delete",
                Box::new(|r| r.model.key_delete(&r.param, 1).unwrap()),
            ),
            (
                "key_move",
                Box::new(|r| r.model.key_move(&r.param, 1, 0.6).unwrap()),
            ),
            (
                "param_flip",
                Box::new(|r| r.model.param_flip(&r.param).unwrap()),
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
        let root = r.root.clone();
        assert!(r.model.delete_node(&root).is_err());
        assert!(r.model.reparent(&root, &r.part).is_err());
        assert!(r.model.mask_add(&r.part, &r.part, MaskMode::Mask).is_err());
        assert!(r.model.mask_delete(&r.composite, 7).is_err());
        assert!(r
            .model
            .set_binding_key(&scalar_key(&r), [9, 0], 1.0)
            .is_err());
        assert_eq!(r.model.generation(), before);
    }

    /// An Id is what an addon reaches into a model by, so a rename has to be a
    /// whole-model rewrite or it leaves a dangling reference behind.
    #[test]
    fn renaming_rewrites_every_reference_to_the_id() {
        let mut r = rig();
        let (part, other, composite, physics) = (
            r.part.clone(),
            r.other.clone(),
            r.composite.clone(),
            r.physics.clone(),
        );
        let (param, tex) = (r.param.clone(), r.tex.clone());
        let child = r
            .model
            .add_node(&part, ModelNode::new("child", part_kind()), &mut r.hex)
            .unwrap();
        r.model.set_part_albedo(&part, Some(tex.clone())).unwrap();
        r.model
            .set_physics_targets(&physics, [Some(param.clone()), None])
            .unwrap();
        r.model
            .set_welds(vec![ModelWeld::new(part.clone(), other, Vec::new())])
            .unwrap();
        let key = BindingKey::new(param.clone(), part.clone(), BindingTarget::Deform);
        r.model
            .set_deform_vertices(&key, [1, 0], vec![1.0; 8])
            .unwrap();

        let new_node = NodeId::new("head/part-renamed").unwrap();
        r.model.rename_node_id(&part, new_node.clone()).unwrap();

        assert!(r.model.node(&part).is_none());
        assert!(r.model.node(&new_node).is_some());
        // parent's child list, and the child's parent
        assert!(r
            .model
            .node(&r.root)
            .unwrap()
            .children()
            .contains(&new_node));
        assert_eq!(r.model.node(&child).unwrap().parent(), Some(&new_node));
        // the mask that named it as a source
        assert_eq!(
            r.model.node(&composite).unwrap().masks().unwrap()[0].source(),
            &new_node
        );
        // the binding that drives it, and the weld that ends on it
        assert_eq!(r.model.bindings().next().unwrap().node(), &new_node);
        assert_eq!(r.model.welds()[0].a(), &new_node);
        assert!(r.model.to_clp_bytes().is_ok());

        let new_param = ParamId::new("head-turn").unwrap();
        r.model.rename_param_id(&param, new_param.clone()).unwrap();
        assert_eq!(r.model.param_ids(), std::slice::from_ref(&new_param));
        assert_eq!(
            r.model.bindings().next().unwrap().params(),
            &BindingParams::One(new_param.clone())
        );
        assert_eq!(
            physics_target(&r.model, &physics),
            Some(new_param),
            "the physics node follows its param",
        );

        let new_tex = TexId::new("skin").unwrap();
        r.model.rename_tex_id(&tex, new_tex.clone()).unwrap();
        assert_eq!(r.model.texture_ids(), std::slice::from_ref(&new_tex));
        assert_eq!(albedo(&r.model, &new_node), Some(new_tex));
        assert!(r.model.to_clp_bytes().is_ok());

        // Renaming the root moves the root handle too.
        let new_root = NodeId::new("body").unwrap();
        let old_root = r.root.clone();
        r.model.rename_node_id(&old_root, new_root.clone()).unwrap();
        assert_eq!(r.model.root(), &new_root);
        assert!(r.model.to_clp_bytes().is_ok());
    }

    #[test]
    fn renaming_onto_a_taken_id_is_refused() {
        let mut r = rig();
        let (part, other) = (r.part.clone(), r.other.clone());
        assert!(matches!(
            r.model.rename_node_id(&part, other),
            Err(ModelError::DuplicateId(_))
        ));
        // Renaming to the same Id is a no-op, not a conflict.
        r.model.rename_node_id(&part, part.clone()).unwrap();
        assert!(r.model.node(&part).is_some());
    }

    /// A generated Id records the parent a node was *created* under; it is a
    /// reading aid, not a path, so moving the node must not touch it.
    #[test]
    fn reparenting_leaves_the_generated_id_alone() {
        let mut r = rig();
        let part = r.part.clone();
        assert!(
            part.as_str().starts_with("root/part-"),
            "generated under the root: {part}"
        );
        r.model.reparent(&part, &r.composite.clone()).unwrap();
        assert_eq!(
            r.model.node(&part).unwrap().parent(),
            Some(&r.composite),
            "the node moved",
        );
        assert!(
            r.model.node(&part).is_some(),
            "and kept the Id it was created with: {part}",
        );
        assert!(part.as_str().starts_with("root/part-"));
    }

    /// 32 bits collide eventually and `id.rs` cannot check uniqueness, so the
    /// model re-draws until the Id is free.
    #[test]
    fn a_colliding_generated_id_is_redrawn() {
        // Hands out 0xaaaaaaaa twice, then 0xbbbbbbbb forever.
        let mut draws = 0u32;
        let mut hex = move || {
            draws += 1;
            if draws <= 2 {
                0xaaaa_aaaa
            } else {
                0xbbbb_bbbb
            }
        };
        let mut model = Model::new();
        let root = model.root().clone();
        let a = model
            .add_node(&root, ModelNode::new("a", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let b = model
            .add_node(&root, ModelNode::new("b", ModelNodeKind::Group), &mut hex)
            .unwrap();
        assert_eq!(a.as_str(), "root/group-aaaaaaaa");
        assert_eq!(
            b.as_str(),
            "root/group-bbbbbbbb",
            "the collision was redrawn"
        );
        assert_eq!(model.node_count(), 3);

        // A source that never moves runs the attempts out rather than looping.
        let mut stuck = || 0xaaaa_aaaa_u32;
        assert!(matches!(
            model.add_node(&root, ModelNode::new("c", ModelNodeKind::Group), &mut stuck),
            Err(ModelError::IdExhausted)
        ));
    }

    #[test]
    fn an_author_chosen_id_cannot_collide() {
        let mut model = Model::new();
        let root = model.root().clone();
        let chosen = NodeId::new("head").unwrap();
        model
            .add_node_with_id(
                chosen.clone(),
                &root,
                ModelNode::new("Head", ModelNodeKind::Group),
            )
            .unwrap();
        assert!(matches!(
            model.add_node_with_id(
                chosen,
                &root,
                ModelNode::new("Head again", ModelNodeKind::Group)
            ),
            Err(ModelError::DuplicateId(_))
        ));
    }

    #[test]
    fn reorder_moves_within_siblings_and_clamps() {
        let mut hex = SeededHex::new(1);
        let mut m = Model::new();
        let root = m.root().clone();
        let mut add = |m: &mut Model, name: &str| {
            m.add_node(&root, ModelNode::new(name, ModelNodeKind::Group), &mut hex)
                .unwrap()
        };
        let a = add(&mut m, "a");
        let b = add(&mut m, "b");
        let c = add(&mut m, "c");
        let root = m.root().clone();

        m.reorder(&c, 0).unwrap();
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[c.clone(), a.clone(), b.clone()]
        );

        // an out-of-range index clamps to the end.
        m.reorder(&c, 99).unwrap();
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[a.clone(), b.clone(), c.clone()]
        );

        m.reorder(&b, 1).unwrap();
        assert_eq!(m.node(&root).unwrap().children(), &[a, b, c]);

        assert!(m.reorder(&root, 0).is_err());
    }

    #[test]
    fn model_snapshots_share_large_payloads_until_mutated() {
        let mut r = rig();
        let node = r.part.clone();
        let key = deform_key(&r);
        let model = &mut r.model;
        model
            .set_deform_vertices(&key, [2, 0], vec![1.0; 8])
            .unwrap();

        let model = r.model;
        let mut edited = model.clone();
        let mesh_arc = |m: &Model| match &m.node(&node).unwrap().kind {
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
            .update_node(&node, |n| n.name = Name::truncated("renamed"))
            .unwrap();
        assert!(Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));

        let mut moved = quad();
        moved.verts[0] = 7.0;
        edited.set_node_mesh(&node, moved).unwrap();
        edited
            .set_deform_vertices(&key, [2, 0], vec![2.0; 8])
            .unwrap();

        assert!(!Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));
        assert!(!Arc::ptr_eq(&original_cells, &cells_arc(&edited)));
        // The snapshot taken before the edits is byte-identical afterwards.
        assert_eq!(model.node_mesh(&node).unwrap().verts, quad().verts);
        assert_eq!(
            deform_cells(model.binding(&key).unwrap().values())
                .unwrap()
                .iter()
                .map(|c| (c.x, c.y, c.value.clone()))
                .collect::<Vec<_>>(),
            vec![(1, 0, vec![0.0; 8]), (2, 0, vec![1.0; 8])],
        );
    }

    #[test]
    fn duplicate_copies_bindings_and_remaps_internal_masks() {
        let mut hex = SeededHex::new(3);
        let mut m = Model::new();
        let root = m.root().clone();
        let group = m
            .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let mask_src = m
            .add_node(&group, ModelNode::new("mask", part_kind()), &mut hex)
            .unwrap();
        let masked = m
            .add_node(&group, ModelNode::new("masked", part_kind()), &mut hex)
            .unwrap();
        m.mask_add(&masked, &mask_src, MaskMode::Mask).unwrap();

        let param = m.add_param(param("x"), &mut hex).unwrap();
        let key = BindingKey::new(
            param.clone(),
            masked,
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        m.set_binding_key(&key, [1, 0], 5.0).unwrap();

        let copy = m.duplicate_subtree(&group, &mut hex).unwrap();
        // the copy lands right after the original, under its own Id.
        assert_ne!(copy, group);
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[group.clone(), copy.clone()]
        );
        assert_eq!(m.node(&copy).unwrap().name.as_str(), "g copy");

        let copy_children = m.node(&copy).unwrap().children().to_vec();
        assert_eq!(copy_children.len(), 2);
        let copy_masked = &copy_children[1];
        // internal mask reference points at the copied source, not the original.
        let masks = m.node(copy_masked).unwrap().masks().unwrap();
        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0].source(), &copy_children[0]);
        // the copied node has its own binding.
        assert_eq!(m.bindings_of_param(&param).count(), 2);
        assert!(m.bindings().any(|b| b.node() == copy_masked));
        assert!(m.to_clp_bytes().is_ok());
    }

    #[test]
    fn mask_ops_validate_and_reorder() {
        let mut r = rig();
        let (a, b, target) = (r.part.clone(), r.other.clone(), r.composite.clone());
        let c = r.node("c", part_kind());
        let root = r.root.clone();
        let m = &mut r.model;

        m.mask_delete(&target, 0).unwrap();
        assert!(m.mask_add(&a, &a, MaskMode::Mask).is_err());
        assert!(m
            .mask_add(&a, &root, MaskMode::Mask)
            .is_err_and(|e| matches!(e, ModelError::NotAPart)));

        m.mask_add(&target, &a, MaskMode::Mask).unwrap();
        m.mask_add(&target, &b, MaskMode::DodgeMask).unwrap();
        m.mask_add(&target, &c, MaskMode::Mask).unwrap();
        m.mask_reorder(&target, 2, 0).unwrap();
        m.mask_set_mode(&target, 1, MaskMode::DodgeMask).unwrap();
        m.mask_delete(&target, 2).unwrap();
        let masks = m.node(&target).unwrap().masks().unwrap();
        assert_eq!(masks.len(), 2);
        assert_eq!(masks[0].source(), &c);
        assert!(matches!(masks[1].mode(), MaskMode::DodgeMask));
        assert!(m.mask_delete(&target, 5).is_err());
    }

    #[test]
    fn cross_references_are_checked_before_they_land() {
        let mut r = rig();
        // A texture, param and node this model no longer has.
        let orphan_tex = r.model.add_texture(texture(), &mut r.hex).unwrap();
        r.model.delete_texture(&orphan_tex).unwrap();
        let orphan_param = r.model.add_param(param("gone"), &mut r.hex).unwrap();
        r.model.delete_param(&orphan_param).unwrap();
        let orphan_node = r.node("gone", ModelNodeKind::Group);
        r.model.delete_node(&orphan_node).unwrap();

        assert!(matches!(
            r.model.set_part_albedo(&r.part, Some(orphan_tex.clone())),
            Err(ModelError::UnknownTexture)
        ));
        assert!(matches!(
            r.model.set_part_albedo(&r.composite, Some(r.tex.clone())),
            Err(ModelError::NotAPart)
        ));
        assert!(matches!(
            r.model
                .set_physics_targets(&r.physics, [Some(orphan_param), None]),
            Err(ModelError::UnknownParam)
        ));
        assert!(matches!(
            r.model.set_welds(vec![ModelWeld::new(
                r.part.clone(),
                orphan_node,
                Vec::new()
            )]),
            Err(ModelError::UnknownNode)
        ));
        // A node cloned out of another model cannot smuggle its Ids in.
        let mut foreign = ModelPart::new(quad());
        foreign.albedo = Some(orphan_tex);
        let root = r.root.clone();
        assert!(matches!(
            r.model.add_node(
                &root,
                ModelNode::new("smuggler", ModelNodeKind::Part(foreign)),
                &mut r.hex,
            ),
            Err(ModelError::UnknownTexture)
        ));
        assert!(r.model.to_clp_bytes().is_ok());
    }

    fn physics_target(m: &Model, node: &NodeId) -> Option<ParamId> {
        match &m.node(node)?.kind {
            ModelNodeKind::SimplePhysics(ph) => ph.target_params()[0].clone(),
            _ => None,
        }
    }

    fn albedo(m: &Model, node: &NodeId) -> Option<TexId> {
        match &m.node(node)?.kind {
            ModelNodeKind::Part(p) => p.albedo().cloned(),
            _ => None,
        }
    }

    fn texture() -> ModelTexture {
        ModelTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: Arc::new(Vec::new()),
        }
    }

    pub(super) fn quad() -> ClpMesh {
        ClpMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0; 8],
            indices: ClpIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    pub(super) fn part_kind() -> ModelNodeKind {
        ModelNodeKind::Part(ModelPart::new(quad()))
    }

    pub(super) fn param(name: &str) -> ModelParam {
        ModelParam::new(Name::truncated(name), 0.0, 1.0, 0.0)
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
    use crate::id::SeededHex;

    /// A model with `nodes` parts, each carrying a `verts`-vertex mesh and a
    /// two-param deform binding whose `keys` x `keys` grid is fully authored.
    pub(super) fn dense_model(nodes: usize, verts: usize, keys: usize) -> (Model, BindingKey) {
        let positions: Vec<f32> = (0..keys).map(|i| i as f32 / (keys - 1) as f32).collect();
        let mut hex = SeededHex::new(11);
        let mut model = Model::new();
        let root = model.root().clone();
        let mut param = |name: &str, model: &mut Model| {
            model
                .add_param(
                    ModelParam {
                        name: Name::truncated(name),
                        min: -1.0,
                        max: 1.0,
                        default: 0.0,
                        key_positions: positions.clone(),
                    },
                    &mut hex,
                )
                .unwrap()
        };
        let (param_x, param_y) = (param("sweep.x", &mut model), param("sweep.y", &mut model));
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
                    &root,
                    ModelNode::new(
                        format!("part-{i}"),
                        ModelNodeKind::Part(ModelPart::new(mesh.clone())),
                    ),
                    &mut hex,
                )
                .unwrap();
            let key = BindingKey::pair(
                param_x.clone(),
                param_y.clone(),
                node,
                BindingTarget::Deform,
            );
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
