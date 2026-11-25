//! The authored model: the character as a person made it.

pub mod addon;
mod binding;
mod check;
mod correspondence;
mod equality;
mod eval;
mod extensions;
mod file;
mod response;
mod validate;

pub use addon::{InstallError, Installed, Required, Requirement, Requirements};
pub use binding::{
    deform_cells, mask_mode_name, param_range_is_valid, scalar_cells, target_of, BindingKey,
    BindingParams, BindingTarget, DenseGrid, ScalarTarget,
};
pub use check::{CheckWarning, ExtensionWarning};
pub use correspondence::{DeformMapping, MeshMappingError, VertexWeight, MAX_MAPPING_ENTRIES};
pub use eval::Pose;
pub use file::ClmLoadError;
pub use validate::SpineJointError;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::components::{BlendMode, MaskMode};
use crate::extension::{Extensions, IdRef, RawExtension};
use crate::formats::clm::{
    self as clm, ClmAnimation, ClmBindingValues, ClmMesh, ClmTransform, TextureAlpha,
    TextureEncoding,
};
use crate::id::{
    ExtensionKey, HexSource, IdError, Name, NodeId, NodeIdKind, ParamId, SlotId, TexId,
};
use crate::interpolate::InterpolateMode;

/// 32-bit Ids collide by birthday around 2^16 siblings; the cap only fires on
/// a wedged [`HexSource`].
const ID_MINT_ATTEMPTS: usize = 64;

/// Every mutating method leaves the model untouched when it returns one of these.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("invalid motion response {node}: {reason}")]
    InvalidResponse { node: NodeId, reason: String },
    #[error("{target} binding on {node}: {reason}")]
    InvalidResponseBinding {
        node: NodeId,
        target: &'static str,
        reason: String,
    },
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
    #[error("cannot reparent a node under itself or a descendant")]
    Cycle,
    #[error("this needs a complete model; an addon fragment has no single root")]
    Fragment,
    #[error("the root node cannot be {0}")]
    Root(&'static str),
    #[error("an in-place kind edit cannot change {0}")]
    KindEdit(&'static str),
    #[error("invalid binding key positions or cell coordinates")]
    CellOutOfRange,
    #[error("a param's range must be finite and increasing")]
    InvalidParamRange,
    #[error("{0} must be finite")]
    NonFinite(&'static str),
    #[error("a deform holds {got} offsets, but the node's mesh takes {expected}")]
    DeformLength { expected: usize, got: usize },
    #[error("no such binding")]
    UnknownBinding,
    #[error("node is not a part")]
    NotAPart,
    #[error("a mask source must be a part or a composite")]
    NotAMaskSource,
    #[error("part carries no such slot")]
    UnknownSlot,
    #[error("slot {0:?} is already on this part")]
    DuplicateSlot(String),
    #[error("a weld pairs two different parts, never a part with itself")]
    WeldSelfPaired,
    #[error("a weld's end is not a part")]
    WeldEndNotAPart,
    #[error("those two parts are already welded; one weld records a part pair")]
    DuplicateWeld,
    #[error("a weld pairs slot {0:?}, which the part on that side does not carry")]
    WeldUnknownSlot(String),
    #[error("a weld pairs slot {0:?} twice")]
    WeldSlotPairedTwice(String),
    #[error("no weld pairs those two parts")]
    UnknownWeld,
    #[error("a slot weight is a share of a meeting point, so it must be within 0..=1")]
    WeldWeightOutOfRange,
    #[error("node carries no mesh")]
    NotMeshed,
    #[error("node cannot have masks")]
    NotMaskable,
    #[error("a node cannot mask itself")]
    SelfMask,
    #[error("node is not a spine node")]
    NotSpine,
    #[error("node is not a motion response")]
    NotMotionResponse,
    #[error("a spine reads one param per link: {targets} targets for {joints} joints")]
    SpineTargetArity { targets: usize, joints: usize },
    #[error(transparent)]
    SpineJoints(#[from] SpineJointError),
    #[error("a colour binding cannot target a mesh group, which is never drawn")]
    ColorOnMeshGroup,
    #[error("a two-param binding needs two different params")]
    SelfPairedBinding,
    #[error("binding target does not match the operation")]
    WrongTarget,
    #[error("index out of range")]
    IndexOutOfRange,
    #[error("this model carries no extension {0:?}")]
    UnknownExtension(String),
    #[error(
        "extension key {0:?} is catchlight's own to define; a vendor key names its vendor first"
    )]
    ReservedExtension(String),
    #[error("extension {key:?} carries {size} bytes, over the {max}-byte cap")]
    ExtensionTooLarge {
        key: String,
        size: usize,
        max: usize,
    },
    #[error("mesh is malformed: {0}")]
    MalformedMesh(&'static str),
    #[error("an animation lane's keyframes must be in frame order")]
    UnsortedLane,
    #[error(".clm codec: {0}")]
    Clm(#[from] crate::formats::clm::ClmError),
    #[error("invalid .clm: {0}")]
    InvalidClm(#[from] ClmLoadError),
    #[error(transparent)]
    LoadLimit(#[from] crate::LoadLimitError),
}

/// Source-encoded texture bytes (PNG/TGA), immutable under its Id. `Arc<[u8]>`
/// matches [`crate::texture::EncodedTexture`], so decoding shares the allocation.
#[derive(Debug, Clone)]
pub struct ModelTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: Arc<[u8]>,
}

impl From<&ModelTexture> for crate::texture::EncodedTexture {
    fn from(tex: &ModelTexture) -> Self {
        crate::texture::EncodedTexture {
            format: match tex.encoding {
                TextureEncoding::Png => crate::texture::TextureFormat::Png,
                TextureEncoding::Tga => crate::texture::TextureFormat::Tga,
            },
            data: Arc::clone(&tex.data),
            premultiplied: tex.alpha == TextureAlpha::PremultipliedSrgb,
        }
    }
}

/// An extension as stored. JSON values travel inline with the structure;
/// bytes live in their own section and travel by hash.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionValue {
    /// A plain annotation: carried, never maintained.
    Json(serde_json::Value),
    /// A plain annotation of at most [`clm::MAX_EXTENSION_BYTES`].
    Bytes(Arc<[u8]>),
    Maintained(RawExtension),
}

impl ExtensionValue {
    /// The bytes this value keeps in the `Extensions` section, a maintained
    /// extension's byte payload among them.
    pub fn bytes(&self) -> Option<&Arc<[u8]>> {
        match self {
            Self::Bytes(data) => Some(data),
            Self::Maintained(raw) => raw.payload.bytes(),
            Self::Json(_) => None,
        }
    }

    /// The JSON this value carries inline, a maintained extension's JSON
    /// payload among them.
    pub fn json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(value) => Some(value),
            Self::Maintained(raw) => raw.payload.json(),
            Self::Bytes(_) => None,
        }
    }

    /// Checked by the writer too, so any value a model takes can be saved.
    fn check_size(&self, key: &ExtensionKey) -> Result<(), ModelError> {
        let (size, max) = match (self.bytes(), self.json()) {
            (Some(data), _) => (data.len(), clm::MAX_EXTENSION_BYTES),
            (None, Some(json)) => (clm::extension_json_len(json), clm::MAX_EXTENSION_JSON_BYTES),
            (None, None) => return Ok(()),
        };
        if size > max {
            return Err(ModelError::ExtensionTooLarge {
                key: key.to_string(),
                size,
                max,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Model {
    identity: u64,
    generation: u64,
    welds: Vec<ModelWeld>,
    nodes: HashMap<NodeId, ModelNode>,
    /// Avoid scanning a model without responses for response ownership on every edit.
    response_count: usize,
    response_index: OnceLock<Arc<response::ResponseIndex>>,
    /// Nodes whose parent this model does not carry: exactly one (parentless) in
    /// a complete model, one or more in a fragment.
    roots: Vec<NodeId>,
    params: HashMap<ParamId, ModelParam>,
    param_order: Vec<ParamId>,
    textures: HashMap<TexId, ModelTexture>,
    texture_order: Vec<TexId>,
    bindings: Vec<ModelBinding>,
    animations: Vec<ClmAnimation>,
    extensions: Extensions,
    /// Unrecognised file sections, written back unchanged on save.
    unknown_sections: Vec<clm::ClmUnknownSection>,
    /// Derived; dropped by [`Model::bump`].
    binding_index: OnceLock<HashMap<BindingKey, usize>>,
    /// Derived; dropped by [`Model::bump`].
    bake: crate::puppet::BakeMemo,
}

pub const DEFAULT_SLOT_WEIGHT: f32 = 0.5;

/// A named handle on a part, filled by one of its vertices or unfilled until
/// the part's author refills it after a re-mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    id: SlotId,
    vertex: Option<u32>,
}

impl Slot {
    pub fn id(&self) -> &SlotId {
        &self.id
    }

    pub fn vertex(&self) -> Option<u32> {
        self.vertex
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotPair {
    pub a: SlotId,
    pub b: SlotId,
    /// A's share of the meeting point, `0..=1`: 1.0 pins A, 0.5 meets midway.
    pub weight: f32,
}

/// Pairs are pulled to a weighted meeting point after every other deformation.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelWeld {
    a: NodeId,
    b: NodeId,
    pairs: Arc<Vec<SlotPair>>,
}

impl ModelWeld {
    pub fn new(a: NodeId, b: NodeId, pairs: Vec<SlotPair>) -> Self {
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

    pub fn pairs(&self) -> &[SlotPair] {
        &self.pairs
    }

    /// Pairs with an unfilled slot on either end are skipped.
    pub fn resolve(&self, model: &Model) -> Vec<ModelWeldPair> {
        let (Some(a), Some(b)) = (model.slots(&self.a), model.slots(&self.b)) else {
            return Vec::new();
        };
        let find = |slots: &[Slot], id: &SlotId| slots.iter().find(|s| &s.id == id)?.vertex;
        self.pairs
            .iter()
            .filter_map(|pair| {
                Some(ModelWeldPair {
                    a_vert: find(a, &pair.a)?,
                    b_vert: find(b, &pair.b)?,
                    weight: pair.weight,
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelWeldPair {
    /// Vertex indices into each part's mesh.
    pub a_vert: u32,
    pub b_vert: u32,
    /// A's share of the meeting point, `0..=1`: 1.0 pins A, 0.5 meets midway.
    pub weight: f32,
}

#[derive(Debug, Clone)]
pub struct NodeProperties {
    pub name: Name,
    pub enabled: bool,
    pub z_order: f32,
    pub transform: ClmTransform,
    pub lock_to_root: bool,
}

#[derive(Debug, Clone)]
pub struct ModelNode {
    properties: NodeProperties,
    pub kind: ModelNodeKind,
    parent: Option<NodeId>,
    // Like meshes, node-owned lists are shared across snapshots and edit rollback.
    children: Arc<Vec<NodeId>>,
}

#[derive(Debug, Clone)]
pub enum ModelNodeKind {
    Group,
    Part(ModelPart),
    Composite(ModelComposite),
    MeshGroup(ModelMeshGroup),
    MotionResponse(crate::MotionResponse),
    Spine(ModelSpine),
}

impl ModelNodeKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Part(_) => "part",
            Self::Composite(_) => "composite",
            Self::MeshGroup(_) => "mesh_group",
            Self::MotionResponse(_) => "motion_response",
            Self::Spine(_) => "spine",
        }
    }

    fn id_kind(&self) -> NodeIdKind {
        match self {
            Self::Group => NodeIdKind::Group,
            Self::Part(_) => NodeIdKind::Part,
            Self::Composite(_) => NodeIdKind::Composite,
            Self::MeshGroup(_) => NodeIdKind::MeshGroup,
            Self::MotionResponse(_) => NodeIdKind::MotionResponse,
            Self::Spine(_) => NodeIdKind::Spine,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMesh(Arc<ClmMesh>);

impl ModelMesh {
    pub fn to_clm(&self) -> ClmMesh {
        (*self.0).clone()
    }
}

impl From<ClmMesh> for ModelMesh {
    fn from(mesh: ClmMesh) -> Self {
        Self(Arc::new(mesh))
    }
}

impl Deref for ModelMesh {
    type Target = ClmMesh;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Never drawn, so it has no colour fields.
#[derive(Debug, Clone)]
pub struct ModelMeshGroup {
    pub translate_children: bool,
    mesh: ModelMesh,
}

impl ModelMeshGroup {
    pub fn new(mesh: impl Into<ModelMesh>) -> Self {
        Self {
            translate_children: true,
            mesh: mesh.into(),
        }
    }

    pub fn mesh(&self) -> &ClmMesh {
        &self.mesh
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
    /// `None` for an unmapped part, which the renderer culls.
    albedo: Option<TexId>,
    masks: Arc<Vec<ModelMask>>,
    /// Built only by [`Model`]'s slot methods, so a slot is in mesh range or unfilled.
    slots: Arc<Vec<Slot>>,
}

impl ModelPart {
    pub fn new(mesh: impl Into<ModelMesh>) -> Self {
        Self {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            mask_threshold: 0.5,
            mesh: mesh.into(),
            albedo: None,
            masks: Arc::default(),
            slots: Arc::default(),
        }
    }

    pub fn mesh(&self) -> &ClmMesh {
        &self.mesh
    }

    /// A cheap immutable snapshot, sharing storage until the mesh is replaced.
    pub fn shared_mesh(&self) -> ModelMesh {
        self.mesh.clone()
    }

    pub fn albedo(&self) -> Option<&TexId> {
        self.albedo.as_ref()
    }

    pub fn masks(&self) -> &[ModelMask] {
        &self.masks
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
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
    masks: Arc<Vec<ModelMask>>,
}

impl ModelComposite {
    pub fn new() -> Self {
        Self {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            mask_threshold: 0.5,
            propagate_meshgroup: false,
            masks: Arc::default(),
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

/// The source is a part or composite other than the node it clips; in a
/// fragment it may be a base node, which [`Model::install`] checks.
#[derive(Debug, Clone, PartialEq)]
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

/// `targets` is always as long as `joints`; `None` marks a rigid link.
/// `joints[i]` is the far end of link `i`; `targets[i]` is link `i`'s bend,
/// the turn at the joint above it (the node's origin for link 0).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSpine {
    joints: Arc<Vec<[f32; 2]>>,
    targets: Arc<Vec<Option<ParamId>>>,
}

impl ModelSpine {
    /// Unchecked until it joins a model; see [`Self::check_joints`].
    pub fn new(joints: Vec<[f32; 2]>) -> Self {
        let targets = vec![None; joints.len()];
        Self {
            joints: joints.into(),
            targets: targets.into(),
        }
    }

    /// At least one joint, all finite, and no joint repeating the point above it
    /// (the node's origin for the first).
    pub fn check_joints(joints: &[[f32; 2]]) -> Result<(), SpineJointError> {
        validate::spine_joints(joints)
    }

    /// In the node's own space.
    pub fn joints(&self) -> &[[f32; 2]] {
        &self.joints
    }

    pub fn targets(&self) -> &[Option<ParamId>] {
        &self.targets
    }

    pub fn reads(&self, param: &ParamId) -> bool {
        self.targets.iter().flatten().any(|p| p == param)
    }

    fn set_joints(&mut self, joints: Vec<[f32; 2]>) {
        self.joints = joints.into();
        Arc::make_mut(&mut self.targets).resize(self.joints.len(), None);
    }
}

#[derive(Debug, Clone)]
pub struct ModelParam {
    pub name: Name,
    pub min: f32,
    pub max: f32,
    /// In param-value space.
    pub default: f32,
}

impl ModelParam {
    pub fn new(name: Name, min: f32, max: f32, default: f32) -> Self {
        Self {
            name,
            min,
            max,
            default,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelBinding {
    key: BindingKey,
    key_positions: Vec<Vec<f32>>,
    interpolate_mode: InterpolateMode,
    values: ModelBindingValues,
    /// Memo of `values`; dropped by [`Self::values_mut`].
    dense: OnceLock<Arc<DenseGrid>>,
}

impl ModelBinding {
    /// Normalized key positions, one axis per driving param, in param order.
    pub fn key_positions(&self) -> &[Vec<f32>] {
        &self.key_positions
    }

    pub fn key(&self) -> &BindingKey {
        &self.key
    }

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

    pub fn values(&self) -> &ClmBindingValues {
        &self.values
    }

    /// Copy-on-write; the only way cells change, which keeps the dense memo valid.
    fn values_mut(&mut self) -> &mut ClmBindingValues {
        self.dense.take();
        Arc::make_mut(&mut self.values.0)
    }

    fn invalidate_dense(&mut self) {
        self.dense.take();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelBindingValues(Arc<ClmBindingValues>);

impl ModelBindingValues {
    pub fn to_clm(&self) -> ClmBindingValues {
        (*self.0).clone()
    }
}

impl From<ClmBindingValues> for ModelBindingValues {
    fn from(values: ClmBindingValues) -> Self {
        Self(Arc::new(values))
    }
}

impl Deref for ModelBindingValues {
    type Target = ClmBindingValues;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for ModelNode {
    type Target = NodeProperties;
    fn deref(&self) -> &NodeProperties {
        &self.properties
    }
}
impl std::ops::DerefMut for ModelNode {
    fn deref_mut(&mut self) -> &mut NodeProperties {
        &mut self.properties
    }
}

impl ModelNode {
    /// The label is truncated rather than rejected.
    pub fn new(name: impl AsRef<str>, kind: ModelNodeKind) -> Self {
        Self {
            properties: NodeProperties {
                name: Name::truncated(name),
                enabled: true,
                z_order: 0.0,
                transform: ClmTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0, 1.0],
                },
                lock_to_root: false,
            },
            kind,
            parent: None,
            children: Arc::default(),
        }
    }

    pub fn parent(&self) -> Option<&NodeId> {
        self.parent.as_ref()
    }

    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    pub fn mesh(&self) -> Option<&ClmMesh> {
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

    fn remove_masks(&mut self, removed: impl Fn(&NodeId) -> bool) {
        if self
            .masks()
            .is_some_and(|masks| masks.iter().any(|m| removed(&m.source)))
        {
            if let Some(masks) = self.masks_mut() {
                masks.retain(|m| !removed(&m.source));
            }
        }
    }

    fn masks_mut(&mut self) -> Option<&mut Vec<ModelMask>> {
        match &mut self.kind {
            ModelNodeKind::Part(p) => Some(Arc::make_mut(&mut p.masks)),
            ModelNodeKind::Composite(c) => Some(Arc::make_mut(&mut c.masks)),
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

const DEFAULT_ROOT_ID: &str = "root";

/// Starts at 1 so 0 is free to mean "no model".
static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);

fn next_identity() -> u64 {
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

/// Process-wide because undo restores older models: a per-model counter would
/// reissue a generation a derived object already baked against.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

impl Model {
    pub fn new() -> Self {
        let root = NodeId::from_generated(DEFAULT_ROOT_ID.to_string());
        let mut nodes = HashMap::new();
        nodes.insert(root.clone(), ModelNode::new("Root", ModelNodeKind::Group));
        Self {
            identity: next_identity(),
            generation: 0,
            welds: Vec::new(),
            nodes,
            response_count: 0,
            response_index: OnceLock::new(),
            roots: vec![root],
            params: HashMap::new(),
            param_order: Vec::new(),
            textures: HashMap::new(),
            texture_order: Vec::new(),
            bindings: Vec::new(),
            animations: Vec::new(),
            extensions: Extensions::default(),
            unknown_sections: Vec::new(),
            binding_index: OnceLock::new(),
            bake: Default::default(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Compared beside [`Self::generation`], which every loader starts at 0.
    /// A clone keeps it: an undo snapshot is the same model.
    pub fn identity(&self) -> u64 {
        self.identity
    }

    /// Called only on success, so a rejected edit leaves derived objects alone.
    fn bump(&mut self) {
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        self.binding_index.take();
        self.bake.clear();
    }

    pub(crate) fn bake_memo(&self) -> &crate::puppet::BakeMemo {
        &self.bake
    }

    pub fn root(&self) -> Option<&NodeId> {
        match self.roots.as_slice() {
            [only] if self.nodes.get(only).is_some_and(|n| n.parent.is_none()) => Some(only),
            _ => None,
        }
    }

    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    pub fn is_fragment(&self) -> bool {
        self.root().is_none()
    }

    fn siblings_of(&self, id: &NodeId) -> Option<&[NodeId]> {
        match self.nodes.get(id)?.parent.as_ref() {
            Some(p) if self.nodes.contains_key(p) => Some(&self.nodes.get(p)?.children),
            _ => Some(&self.roots),
        }
    }

    fn siblings_mut(&mut self, id: &NodeId) -> Result<&mut Vec<NodeId>, ModelError> {
        let parent = self
            .nodes
            .get(id)
            .ok_or(ModelError::UnknownNode)?
            .parent
            .clone();
        match parent {
            Some(p) if self.nodes.contains_key(&p) => self
                .nodes
                .get_mut(&p)
                .map(|n| Arc::make_mut(&mut n.children))
                .ok_or(ModelError::UnknownNode),
            _ => Ok(&mut self.roots),
        }
    }

    fn attach_child(&mut self, parent: &NodeId, child: NodeId) {
        match self.nodes.get_mut(parent) {
            Some(p) => Arc::make_mut(&mut p.children).push(child),
            None => self.roots.push(child),
        }
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

    /// Cannot change response ownership or inputs. Z-order and transform must stay
    /// finite, or the edit is undone with [`ModelError::NonFinite`].
    pub fn update_node<R>(
        &mut self,
        id: &NodeId,
        f: impl FnOnce(&mut NodeProperties) -> R,
    ) -> Result<R, ModelError> {
        let node = self.nodes.get_mut(id).ok_or(ModelError::UnknownNode)?;
        let previous = node.properties.clone();
        let out = f(&mut node.properties);
        if let Err(fault) = validate::node_placement(node.z_order, &node.transform) {
            node.properties = previous;
            return Err(fault.into());
        }
        self.bump();
        Ok(out)
    }

    /// Edits only the fields a kind exposes. Swapping the kind, or changing a
    /// mesh, albedo, mask list, slot list or spine, is undone with
    /// [`ModelError::KindEdit`]: each has its own method that keeps dependents in
    /// step.
    pub fn update_node_kind<R>(
        &mut self,
        id: &NodeId,
        f: impl FnOnce(&mut ModelNodeKind) -> R,
    ) -> Result<R, ModelError> {
        let node = self.nodes.get_mut(id).ok_or(ModelError::UnknownNode)?;
        let previous = node.kind.clone();
        let out = f(&mut node.kind);
        if let Some(what) = kind_edit_fault(&previous, &node.kind) {
            node.kind = previous;
            return Err(ModelError::KindEdit(what));
        }
        let response_changed = matches!(
            (&previous, &node.kind),
            (ModelNodeKind::MotionResponse(a), ModelNodeKind::MotionResponse(b)) if a != b
        );
        if response_changed {
            self.response_index.take();
            if let Err(error) = self.validate_responses() {
                self.nodes.get_mut(id).ok_or(ModelError::UnknownNode)?.kind = previous;
                self.response_index.take();
                return Err(error);
            }
        }
        self.bump();
        Ok(out)
    }

    /// Pre-order, following sibling order; `.clm` and the arena bridge rely on it.
    pub fn nodes_in_order(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self.roots.iter().rev().cloned().collect();
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

    /// Generated Id: `<parent>/<kind>-<8 hex>`, re-drawn until free.
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
        validate::node_placement(node.z_order, &node.transform)?;
        if let Some(mesh) = node.mesh() {
            validate_mesh(mesh)?;
        }
        if let ModelNodeKind::Spine(spine) = &node.kind {
            validate::spine_joints(spine.joints())?;
        }
        // A node cloned out of another model carries that model's Ids.
        self.check_node_refs(&node)?;
        node.parent = Some(parent.clone());
        node.children = Arc::default();
        let added = usize::from(matches!(node.kind, ModelNodeKind::MotionResponse(_)));
        self.response_count += added;
        if added != 0 {
            self.response_index.take();
        }
        // In a fragment the new node can resolve a formerly external input.
        let check = added != 0 || self.resolves_response_node(&id);
        self.nodes.insert(id.clone(), node);
        if check {
            if let Err(error) = self.validate_responses() {
                self.response_count -= added;
                self.response_index.take();
                self.nodes.remove(&id);
                return Err(error);
            }
        }
        self.attach_child(parent, id);
        self.bump();
        Ok(())
    }

    /// Also drops the textures only the subtree drew.
    pub fn delete_node(&mut self, id: &NodeId) -> Result<(), ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("deleted"));
        }
        if !self.nodes.contains_key(id) {
            return Err(ModelError::UnknownNode);
        }
        let removed: HashSet<NodeId> = self.subtree(id).into_iter().collect();
        self.remove_nodes(&removed);
        self.bump();
        Ok(())
    }

    /// `removed` must be closed under descent. Does not bump; the caller does.
    fn remove_nodes(&mut self, removed: &HashSet<NodeId>) {
        self.roots.retain(|r| !removed.contains(r));
        for id in removed {
            self.remove_counted_node(id);
        }
        for node in self.nodes.values_mut() {
            if node.children.iter().any(|c| removed.contains(c)) {
                Arc::make_mut(&mut node.children).retain(|c| !removed.contains(c));
            }
            node.remove_masks(|id| removed.contains(id));
            validate::drop_slot_inputs(&mut node.kind, |part, _| removed.contains(part));
        }
        self.bindings.retain(|b| !removed.contains(&b.key.node));
        self.welds
            .retain(|w| !removed.contains(&w.a) && !removed.contains(&w.b));
        self.gc_textures();
        self.remove_in_extensions(|r| match r {
            IdRef::Node(id) | IdRef::Slot(id, _) => removed.contains(id),
            _ => false,
        });
    }

    /// The node keeps its Id: the prefix records where it was created.
    pub fn reparent(&mut self, id: &NodeId, new_parent: &NodeId) -> Result<(), ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("reparented"));
        }
        if !self.nodes.contains_key(id) || !self.nodes.contains_key(new_parent) {
            return Err(ModelError::UnknownNode);
        }
        if self.is_self_or_descendant(new_parent, id) {
            return Err(ModelError::Cycle);
        }
        if matches!(&self.nodes[id].kind, ModelNodeKind::MotionResponse(r) if r.inputs.iter().any(|t| matches!(t.source,crate::ResponseSource::Parent)))
            && !matches!(
                self.nodes[new_parent].kind,
                ModelNodeKind::MotionResponse(_)
            )
        {
            return Err(ModelError::InvalidResponse {
                node: id.clone(),
                reason: "Parent needs a motion response parent".into(),
            });
        }
        let old_parent = self.nodes.get(id).and_then(|n| n.parent.clone());
        match old_parent.and_then(|op| self.nodes.get_mut(&op)) {
            Some(p) => Arc::make_mut(&mut p.children).retain(|c| c != id),
            // A fragment root: its parent is not here to detach it from.
            None => self.roots.retain(|r| r != id),
        }
        self.attach_child(new_parent, id.clone());
        if let Some(n) = self.nodes.get_mut(id) {
            n.parent = Some(new_parent.clone());
        }
        self.bump();
        Ok(())
    }

    /// `index` clamps to the end. Sibling order breaks z-order ties when drawing.
    pub fn reorder(&mut self, id: &NodeId, index: usize) -> Result<(), ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("reordered"));
        }
        let siblings = self.siblings_mut(id)?;
        let cur = siblings
            .iter()
            .position(|c| c == id)
            .ok_or(ModelError::UnknownNode)?;
        let moved = siblings.remove(cur);
        let index = index.min(siblings.len());
        siblings.insert(index, moved);
        self.bump();
        Ok(())
    }

    /// Copies bindings, masks into the subtree, and welds between copied parts;
    /// a weld crossing the subtree edge is not copied.
    pub fn duplicate_subtree(
        &mut self,
        id: &NodeId,
        hex: &mut impl HexSource,
    ) -> Result<NodeId, ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("duplicated"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent.clone())
            .ok_or(ModelError::UnknownNode)?;

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

        // Reserve every Id first so exhaustion leaves no partial copy.
        let mut map = HashMap::new();
        let mut reserved = HashSet::new();
        for old in &order {
            let node = self.node(old).ok_or(ModelError::UnknownNode)?;
            let parent_id = node.parent().and_then(|p| map.get(p)).unwrap_or(&parent);
            let kind = node.kind.id_kind();
            let new_id = self.mint(
                |h| NodeId::generate(parent_id, kind, h),
                |m, id| {
                    reserved.contains(id)
                        || m.nodes.contains_key(id)
                        || m.resolves_response_node(id)
                },
                hex,
            )?;
            reserved.insert(new_id.clone());
            map.insert(old.clone(), new_id);
        }
        let previous_response_count = self.response_count;
        for old in &order {
            let mut copy = self.node(old).ok_or(ModelError::UnknownNode)?.clone();
            let new_id = map.get(old).ok_or(ModelError::UnknownNode)?;
            let new_parent = copy.parent().and_then(|p| map.get(p)).unwrap_or(&parent);
            if old == id {
                copy.name = Name::truncated(format!("{} copy", copy.name));
            }
            copy.parent = Some(new_parent.clone());
            copy.children = Arc::default();
            self.response_count +=
                usize::from(matches!(copy.kind, ModelNodeKind::MotionResponse(_)));
            self.nodes.insert(new_id.clone(), copy);
            self.attach_child(new_parent, new_id.clone());
        }
        if self.response_count != previous_response_count {
            self.response_index.take();
        }

        for new_id in map.values() {
            if let Some(ModelNodeKind::MotionResponse(r)) =
                self.nodes.get_mut(new_id).map(|n| &mut n.kind)
            {
                r.output_param = None;
                for t in &mut r.inputs {
                    if let crate::ResponseSource::PartSlot { part, .. } = &mut t.source {
                        if let Some(mapped) = map.get(part) {
                            *part = mapped.clone();
                        }
                    }
                }
            }
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
                    key_positions: b.key_positions.clone(),
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

        // A weld crossing the subtree edge stays with the original; a second weld
        // onto the same slots would pull it two ways.
        let welded: Vec<ModelWeld> = self
            .welds
            .iter()
            .filter_map(|w| {
                Some(ModelWeld {
                    a: map.get(&w.a)?.clone(),
                    b: map.get(&w.b)?.clone(),
                    pairs: w.pairs.clone(),
                })
            })
            .collect();
        self.welds.extend(welded);

        let new_root = map.get(id).ok_or(ModelError::UnknownNode)?.clone();
        let pos = self
            .siblings_of(id)
            .and_then(|s| s.iter().position(|c| c == id))
            .ok_or(ModelError::UnknownNode)?;
        // reorder bumps the generation for the whole duplicate.
        self.reorder(&new_root, pos + 1)?;
        Ok(new_root)
    }

    /// Rewrites every reference. No alias is kept, so addons naming the old Id break.
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
        if self.resolves_response_node(&new) {
            self.validate_response_rename(Some((old, &new)), None)?;
        }
        let node = self.nodes.remove(old).ok_or(ModelError::UnknownNode)?;
        let parent = node.parent.clone();
        let children = node.children.clone();
        self.nodes.insert(new.clone(), node);
        self.response_index.take();
        if let Some(p) = parent.and_then(|p| self.nodes.get_mut(&p)) {
            for c in Arc::make_mut(&mut p.children) {
                if c == old {
                    *c = new.clone();
                }
            }
        }
        for child in children.iter() {
            if let Some(c) = self.nodes.get_mut(child) {
                c.parent = Some(new.clone());
            }
        }
        for node in self.nodes.values_mut() {
            if !node
                .masks()
                .is_some_and(|masks| masks.iter().any(|m| &m.source == old))
            {
                continue;
            }
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
        for r in &mut self.roots {
            if r == old {
                *r = new.clone();
            }
        }
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::MotionResponse(r) = &mut node.kind {
                for t in &mut r.inputs {
                    if let crate::ResponseSource::PartSlot { part, .. } = &mut t.source {
                        if part == old {
                            *part = new.clone();
                        }
                    }
                }
            }
        }
        self.extensions.rewrite(|r| match r {
            IdRef::Node(id) if id == old => Some(IdRef::Node(new.clone())),
            IdRef::Slot(id, slot) if id == old => Some(IdRef::Slot(new.clone(), slot.clone())),
            _ => None,
        });
        self.bump();
        Ok(())
    }

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
        if self.is_fragment()
            && (self.response_owner(old).is_some() || self.response_owner(&new).is_some())
        {
            self.validate_response_rename(None, Some((old, &new)))?;
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
        for lane in self.animations.iter_mut().flat_map(|a| &mut a.lanes) {
            if &lane.param == old {
                lane.param = new.clone();
            }
        }
        self.extensions.rewrite(|r| match r {
            IdRef::Param(id) if id == old => Some(IdRef::Param(new.clone())),
            _ => None,
        });
        for node in self.nodes.values_mut() {
            match &mut node.kind {
                ModelNodeKind::Spine(spine) if spine.reads(old) => {
                    for t in Arc::make_mut(&mut spine.targets) {
                        if t.as_ref() == Some(old) {
                            *t = Some(new.clone());
                        }
                    }
                }
                ModelNodeKind::MotionResponse(r) => {
                    if r.output_param.as_ref() == Some(old) {
                        r.output_param = Some(new.clone());
                    }
                    for t in &mut r.inputs {
                        if let crate::ResponseSource::PoseParam { param: p } = &mut t.source {
                            if p == old {
                                *p = new.clone();
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.response_index.take();
        self.bump();
        Ok(())
    }

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
        self.extensions.rewrite(|r| match r {
            IdRef::Texture(id) if id == old => Some(IdRef::Texture(new.clone())),
            _ => None,
        });
        self.bump();
        Ok(())
    }

    /// Deletes the texture left behind if this part was its last user.
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
        match self.nodes.get(id).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(_)) => {}
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode),
        }
        self.point_albedo(id, albedo);
        self.bump();
        Ok(())
    }

    /// `targets` must match the joint count; [`Model::set_spine_joints`] changes it.
    pub fn set_spine_targets(
        &mut self,
        node: &NodeId,
        targets: Vec<Option<ParamId>>,
    ) -> Result<(), ModelError> {
        if targets
            .iter()
            .flatten()
            .any(|p| !self.params.contains_key(p))
        {
            return Err(ModelError::UnknownParam);
        }
        match self.nodes.get_mut(node).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Spine(spine)) => {
                if targets.len() != spine.joints.len() {
                    return Err(ModelError::SpineTargetArity {
                        targets: targets.len(),
                        joints: spine.joints.len(),
                    });
                }
                spine.targets = targets.into();
            }
            Some(_) => return Err(ModelError::NotSpine),
            None => return Err(ModelError::UnknownNode),
        }
        self.bump();
        Ok(())
    }

    /// Links past the new end lose their param; new links arrive rigid.
    pub fn set_spine_joints(
        &mut self,
        node: &NodeId,
        joints: Vec<[f32; 2]>,
    ) -> Result<(), ModelError> {
        match self.nodes.get_mut(node).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Spine(spine)) => {
                validate::spine_joints(&joints)?;
                spine.set_joints(joints);
            }
            Some(_) => return Err(ModelError::NotSpine),
            None => return Err(ModelError::UnknownNode),
        }
        self.bump();
        Ok(())
    }

    /// `refit` must return one finite `[dx, dy]` per new vertex, or the edit is
    /// refused untouched. Slots are not refitted: every slot on the part is
    /// emptied and returned for its author to refill.
    pub fn set_node_mesh_with(
        &mut self,
        id: &NodeId,
        mesh: ClmMesh,
        mut refit: impl FnMut(&ClmMesh, &ClmMesh, &[f32]) -> Vec<f32>,
    ) -> Result<Vec<SlotId>, ModelError> {
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
        let expected = mesh.verts.len();
        let mut refitted: Vec<(usize, Vec<Vec<f32>>)> = Vec::new();
        for (index, binding) in self
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.node() == id)
        {
            let mut values = Vec::new();
            if let ClmBindingValues::Deform(cells) = binding.values() {
                values.reserve(cells.cells.len());
                for cell in &cells.cells {
                    let value = refit(&old, &mesh, &cell.value);
                    validate::deform_offsets(&value, expected)?;
                    values.push(value);
                }
            }
            refitted.push((index, values));
        }
        let apply = |b: &mut ModelBinding, values: Vec<Vec<f32>>| {
            // The deform identity is sized to the mesh, so every grid goes stale.
            b.invalidate_dense();
            if let ClmBindingValues::Deform(cells) = b.values_mut() {
                for (cell, value) in cells.cells.iter_mut().zip(values) {
                    cell.value = value;
                }
            }
        };
        // A custom mesh refit also edits binding cells. Stage this node's
        // bindings together if one of them protects a response's rest pose.
        if self.bindings_of_node(id).any(|b| {
            b.target() == BindingTarget::Deform
                && b.params().iter().any(|p| self.response_owner(p).is_some())
        }) {
            let mut edits = Vec::with_capacity(refitted.len());
            for (index, values) in refitted {
                let mut candidate = self.bindings[index].clone();
                apply(&mut candidate, values);
                self.validate_response_binding(&candidate)?;
                edits.push((index, candidate));
            }
            for (index, binding) in edits {
                self.bindings[index] = binding;
            }
        } else {
            for (index, values) in refitted {
                apply(&mut self.bindings[index], values);
            }
        }
        if let Some(slot) = self.nodes.get_mut(id).and_then(ModelNode::mesh_mut) {
            *slot = mesh.into();
        }
        let mut emptied = Vec::new();
        if let Some(ModelNodeKind::Part(part)) = self.nodes.get_mut(id).map(|n| &mut n.kind) {
            for slot in Arc::make_mut(&mut part.slots) {
                slot.vertex = None;
                emptied.push(slot.id.clone());
            }
        }
        self.bump();
        Ok(emptied)
    }

    /// New vertices start at zero offset; slots are emptied as in
    /// [`Self::set_node_mesh_with`].
    pub fn set_node_mesh(&mut self, id: &NodeId, mesh: ClmMesh) -> Result<Vec<SlotId>, ModelError> {
        self.set_node_mesh_with(id, mesh, |_, new, offsets| {
            let mut out = offsets.to_vec();
            out.resize(new.verts.len(), 0.0);
            out
        })
    }

    fn masks_mut(&mut self, id: &NodeId) -> Result<&mut Vec<ModelMask>, ModelError> {
        match self.nodes.get_mut(id) {
            Some(n) => n.masks_mut().ok_or(ModelError::NotMaskable),
            None => Err(ModelError::UnknownNode),
        }
    }

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
            Some(k) if is_mask_source(k) => {}
            Some(_) => return Err(ModelError::NotAMaskSource),
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

    /// `to` clamps. Mask order is evaluation order.
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

    pub fn slots(&self, id: &NodeId) -> Option<&[Slot]> {
        match &self.node(id)?.kind {
            ModelNodeKind::Part(p) => Some(p.slots()),
            _ => None,
        }
    }

    pub fn slot(&self, id: &NodeId, slot: &SlotId) -> Option<&Slot> {
        self.slots(id)?.iter().find(|s| &s.id == slot)
    }

    pub fn unfilled_slots(&self) -> Vec<(NodeId, SlotId)> {
        let mut out = Vec::new();
        for id in self.nodes_in_order() {
            let Some(slots) = self.slots(&id) else {
                continue;
            };
            for slot in slots {
                if slot.vertex.is_none() {
                    out.push((id.clone(), slot.id.clone()));
                }
            }
        }
        out
    }

    pub fn slot_add(&mut self, id: &NodeId, slot: SlotId) -> Result<(), ModelError> {
        let part = self.part_mut(id)?;
        if part.slots.iter().any(|s| s.id == slot) {
            return Err(ModelError::DuplicateSlot(slot.to_string()));
        }
        Arc::make_mut(&mut part.slots).push(Slot {
            id: slot,
            vertex: None,
        });
        self.bump();
        Ok(())
    }

    /// Generated Id: `slot-<8 hex>`, re-drawn until free on this part.
    pub fn slot_add_generated(
        &mut self,
        id: &NodeId,
        hex: &mut impl HexSource,
    ) -> Result<SlotId, ModelError> {
        self.part_mut(id)?;
        let slot = self.mint(SlotId::generate, |m, s| m.slot(id, s).is_some(), hex)?;
        self.slot_add(id, slot.clone())?;
        Ok(slot)
    }

    /// Rewrites every weld pair naming the slot; no alias is kept.
    pub fn rename_slot(
        &mut self,
        id: &NodeId,
        from: &SlotId,
        to: SlotId,
    ) -> Result<(), ModelError> {
        let part = self.part_mut(id)?;
        let Some(at) = part.slots.iter().position(|s| &s.id == from) else {
            return Err(ModelError::UnknownSlot);
        };
        if &to != from && part.slots.iter().any(|s| s.id == to) {
            return Err(ModelError::DuplicateSlot(to.to_string()));
        }
        Arc::make_mut(&mut part.slots)[at].id = to.clone();
        for w in &mut self.welds {
            let (renames_a, renames_b) = (&w.a == id, &w.b == id);
            if !renames_a && !renames_b {
                continue;
            }
            for pair in Arc::make_mut(&mut w.pairs) {
                if renames_a && &pair.a == from {
                    pair.a = to.clone();
                }
                if renames_b && &pair.b == from {
                    pair.b = to.clone();
                }
            }
        }
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::MotionResponse(r) = &mut node.kind {
                for t in &mut r.inputs {
                    if let crate::ResponseSource::PartSlot { part, slot, .. } = &mut t.source {
                        if part == id && slot == from {
                            *slot = to.clone();
                        }
                    }
                }
            }
        }
        self.extensions.rewrite(|r| match r {
            IdRef::Slot(part, slot) if part == id && slot == from => {
                Some(IdRef::Slot(part.clone(), to.clone()))
            }
            _ => None,
        });
        self.bump();
        Ok(())
    }

    pub fn slot_fill(&mut self, id: &NodeId, slot: &SlotId, vertex: u32) -> Result<(), ModelError> {
        let vertices = match self.node(id).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(p)) => p.mesh().vertex_count(),
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode),
        };
        if vertex as usize >= vertices {
            return Err(ModelError::IndexOutOfRange);
        }
        self.slot_mut(id, slot)?.vertex = Some(vertex);
        self.bump();
        Ok(())
    }

    pub fn slot_clear(&mut self, id: &NodeId, slot: &SlotId) -> Result<(), ModelError> {
        self.slot_mut(id, slot)?.vertex = None;
        self.bump();
        Ok(())
    }

    /// Drops the weld pairs naming it; a weld left with no pairs stays.
    pub fn slot_delete(&mut self, id: &NodeId, slot: &SlotId) -> Result<(), ModelError> {
        let part = self.part_mut(id)?;
        let Some(at) = part.slots.iter().position(|s| &s.id == slot) else {
            return Err(ModelError::UnknownSlot);
        };
        Arc::make_mut(&mut part.slots).remove(at);
        for w in &mut self.welds {
            let (drops_a, drops_b) = (&w.a == id, &w.b == id);
            if !drops_a && !drops_b {
                continue;
            }
            Arc::make_mut(&mut w.pairs)
                .retain(|p| !(drops_a && &p.a == slot) && !(drops_b && &p.b == slot));
        }
        for node in self.nodes.values_mut() {
            validate::drop_slot_inputs(&mut node.kind, |part, s| part == id && s == slot);
        }
        self.remove_in_extensions(|r| matches!(r, IdRef::Slot(part, s) if part == id && s == slot));
        self.bump();
        Ok(())
    }

    fn part_mut(&mut self, id: &NodeId) -> Result<&mut ModelPart, ModelError> {
        match self.nodes.get_mut(id) {
            Some(n) => match &mut n.kind {
                ModelNodeKind::Part(p) => Ok(p),
                _ => Err(ModelError::NotAPart),
            },
            None => Err(ModelError::UnknownNode),
        }
    }

    fn slot_mut(&mut self, id: &NodeId, slot: &SlotId) -> Result<&mut Slot, ModelError> {
        Arc::make_mut(&mut self.part_mut(id)?.slots)
            .iter_mut()
            .find(|s| &s.id == slot)
            .ok_or(ModelError::UnknownSlot)
    }

    pub fn param_ids(&self) -> &[ParamId] {
        &self.param_order
    }

    pub fn param(&self, id: &ParamId) -> Option<&ModelParam> {
        self.params.get(id)
    }

    pub fn add_param(
        &mut self,
        param: ModelParam,
        hex: &mut impl HexSource,
    ) -> Result<ParamId, ModelError> {
        let id = self.mint(ParamId::generate, |m, id| m.params.contains_key(id), hex)?;
        self.add_param_with_id(id.clone(), param)?;
        Ok(id)
    }

    pub fn add_param_with_id(&mut self, id: ParamId, param: ModelParam) -> Result<(), ModelError> {
        if !param_range_is_valid(param.min, param.max) {
            return Err(ModelError::InvalidParamRange);
        }
        if !param.default.is_finite() {
            return Err(ModelError::NonFinite("a param's default"));
        }
        if self.params.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.to_string()));
        }
        self.params.insert(id.clone(), param);
        if self.is_fragment() && self.response_owner(&id).is_some() {
            if let Err(error) = self.validate_responses() {
                self.params.remove(&id);
                return Err(error);
            }
        }
        self.param_order.push(id);
        self.bump();
        Ok(())
    }

    /// A two-param binding naming it is removed whole.
    pub fn delete_param(&mut self, id: &ParamId) -> Result<(), ModelError> {
        if self.params.remove(id).is_none() {
            return Err(ModelError::UnknownParam);
        }
        self.param_order.retain(|p| p != id);
        self.bindings.retain(|b| !b.key.params.contains(id));
        for animation in &mut self.animations {
            animation.lanes.retain(|lane| &lane.param != id);
        }
        for node in self.nodes.values_mut() {
            match &mut node.kind {
                ModelNodeKind::MotionResponse(r) => {
                    r.inputs.retain(
                        |t| !matches!(&t.source,crate::ResponseSource::PoseParam { param: p } if p==id),
                    );
                    if r.output_param.as_ref() == Some(id) {
                        r.output_param = None;
                    }
                }
                ModelNodeKind::Spine(spine) if spine.reads(id) => {
                    for t in Arc::make_mut(&mut spine.targets) {
                        if t.as_ref() == Some(id) {
                            *t = None;
                        }
                    }
                }
                _ => {}
            }
        }
        self.response_index.take();
        self.remove_in_extensions(|r| matches!(r, IdRef::Param(p) if p == id));
        self.bump();
        Ok(())
    }

    pub fn texture_ids(&self) -> &[TexId] {
        &self.texture_order
    }

    pub fn texture(&self, id: &TexId) -> Option<&ModelTexture> {
        self.textures.get(id)
    }

    /// Displaces `part`'s old albedo as [`Self::set_part_albedo`] does.
    pub fn add_texture(
        &mut self,
        part: &NodeId,
        texture: ModelTexture,
        hex: &mut impl HexSource,
    ) -> Result<TexId, ModelError> {
        let id = self.mint(TexId::generate, |m, id| m.textures.contains_key(id), hex)?;
        self.add_texture_with_id(id.clone(), part, texture)?;
        Ok(id)
    }

    pub fn add_texture_with_id(
        &mut self,
        id: TexId,
        part: &NodeId,
        texture: ModelTexture,
    ) -> Result<(), ModelError> {
        if self.textures.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.to_string()));
        }
        match self.nodes.get(part).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(_)) => {}
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode),
        }
        self.textures.insert(id.clone(), texture);
        self.texture_order.push(id.clone());
        self.point_albedo(part, Some(id));
        self.bump();
        Ok(())
    }

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
        self.remove_in_extensions(|r| matches!(r, IdRef::Texture(t) if t == id));
        self.bump();
        Ok(())
    }

    /// The textures [`Self::delete_node`] would drop, in model texture order.
    pub fn textures_dropped_by_deleting(&self, id: &NodeId) -> Vec<TexId> {
        let removed: HashSet<NodeId> = self.subtree(id).into_iter().collect();
        let kept: HashSet<&TexId> = self
            .nodes
            .iter()
            .filter(|(id, _)| !removed.contains(*id))
            .filter_map(|(_, n)| albedo_of(n))
            .collect();
        let going: HashSet<&TexId> = removed
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .filter_map(albedo_of)
            .collect();
        self.texture_order
            .iter()
            .filter(|t| going.contains(t) && !kept.contains(t))
            .cloned()
            .collect()
    }

    /// The texture [`Self::set_part_albedo`] or [`Self::add_texture`] would drop.
    pub fn texture_dropped_by_repointing(
        &self,
        part: &NodeId,
        albedo: Option<&TexId>,
    ) -> Option<TexId> {
        let old = self.nodes.get(part).and_then(albedo_of)?;
        if Some(old) == albedo {
            return None;
        }
        self.nodes
            .iter()
            .all(|(id, n)| id == part || albedo_of(n) != Some(old))
            .then(|| old.clone())
    }

    /// The caller checks both Ids and bumps.
    fn point_albedo(&mut self, part: &NodeId, albedo: Option<TexId>) {
        let old = match self.nodes.get_mut(part).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Part(p)) => std::mem::replace(&mut p.albedo, albedo),
            _ => return,
        };
        if let Some(old) = old {
            if !self.nodes.values().any(|n| albedo_of(n) == Some(&old)) {
                self.textures.remove(&old);
                self.texture_order.retain(|t| t != &old);
                self.remove_in_extensions(|r| matches!(r, IdRef::Texture(t) if t == &old));
            }
        }
    }

    fn gc_textures(&mut self) {
        let drawn: HashSet<TexId> = self.nodes.values().filter_map(albedo_of).cloned().collect();
        let dropped: HashSet<TexId> = self
            .texture_order
            .iter()
            .filter(|id| !drawn.contains(*id))
            .cloned()
            .collect();
        self.textures.retain(|id, _| drawn.contains(id));
        self.texture_order.retain(|id| drawn.contains(id));
        if !dropped.is_empty() {
            self.remove_in_extensions(|r| matches!(r, IdRef::Texture(t) if dropped.contains(t)));
        }
    }

    pub fn animations(&self) -> &[ClmAnimation] {
        &self.animations
    }

    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    pub fn unknown_sections(&self) -> &[clm::ClmUnknownSection] {
        &self.unknown_sections
    }

    /// `catchlight.` keys are reserved and refused. Size caps match the reader's,
    /// so any value taken can be saved.
    pub fn set_extension(
        &mut self,
        key: ExtensionKey,
        value: ExtensionValue,
    ) -> Result<(), ModelError> {
        if key.is_reserved() {
            return Err(ModelError::ReservedExtension(key.to_string()));
        }
        value.check_size(&key)?;
        self.extensions
            .map
            .insert(key, crate::extension::Stored::value(value));
        self.bump();
        Ok(())
    }

    /// A missing key is an error, so a typo is not silently ignored.
    pub fn delete_extension(&mut self, key: &ExtensionKey) -> Result<(), ModelError> {
        if key.is_reserved() {
            return Err(ModelError::ReservedExtension(key.to_string()));
        }
        if self.extensions.map.remove(key).is_none() {
            return Err(ModelError::UnknownExtension(key.to_string()));
        }
        self.bump();
        Ok(())
    }

    /// Lanes must name live params and keep frames in order (playback
    /// binary-searches them).
    pub fn set_animations(&mut self, animations: Vec<ClmAnimation>) -> Result<(), ModelError> {
        if animations
            .iter()
            .flat_map(|a| &a.lanes)
            .any(|lane| !self.params.contains_key(&lane.param))
        {
            return Err(ModelError::UnknownParam);
        }
        if animations
            .iter()
            .flat_map(|a| &a.lanes)
            .any(|lane| lane.keyframes.windows(2).any(|w| w[0].frame > w[1].frame))
        {
            return Err(ModelError::UnsortedLane);
        }
        for lane in animations.iter().flat_map(|a| &a.lanes) {
            self.check_response_writer(&lane.param)?;
        }
        self.animations = animations;
        self.bump();
        Ok(())
    }

    pub fn welds(&self) -> &[ModelWeld] {
        &self.welds
    }

    /// `slot` is on `a`, and `weight` is `a`'s share whichever way the weld is
    /// stored (naming the parts reversed stores `1 - weight`), so it must be in
    /// `0..=1`.
    pub fn set_weld_slot_weight(
        &mut self,
        a: &NodeId,
        b: &NodeId,
        slot: &SlotId,
        weight: f32,
    ) -> Result<(), ModelError> {
        if !(0.0..=1.0).contains(&weight) {
            return Err(ModelError::WeldWeightOutOfRange);
        }
        let weld = self
            .welds
            .iter_mut()
            .find(|w| (&w.a == a && &w.b == b) || (&w.a == b && &w.b == a))
            .ok_or(ModelError::UnknownWeld)?;
        let forward = &weld.a == a;
        let share = if forward { weight } else { 1.0 - weight };
        let pairs = Arc::make_mut(&mut weld.pairs);
        let at = pairs
            .iter()
            .position(|p| if forward { &p.a } else { &p.b } == slot)
            .ok_or(ModelError::UnknownSlot)?;
        pairs[at].weight = share;
        self.bump();
        Ok(())
    }

    /// Unfilled slots are fine. In a fragment, an end the fragment does not carry
    /// is a weld into the base, checked by [`Model::install`].
    pub fn set_welds(&mut self, welds: Vec<ModelWeld>) -> Result<(), ModelError> {
        let fragment = self.is_fragment();
        let mut welded = validate::Welded::with_capacity(welds.len());
        for w in &welds {
            validate::weld(
                &w.a,
                &w.b,
                w.pairs.iter().map(|pair| (&pair.a, &pair.b, pair.weight)),
                &mut welded,
                |end| self.weld_end(end, fragment),
                |part, slot| self.slot(part, slot).is_some(),
            )
            .map_err(|fault| match fault {
                validate::WeldFault::SelfPaired => ModelError::WeldSelfPaired,
                validate::WeldFault::EndNotAPart { .. } => ModelError::WeldEndNotAPart,
                validate::WeldFault::EndMissing { .. } => ModelError::UnknownNode,
                validate::WeldFault::Duplicate => ModelError::DuplicateWeld,
                validate::WeldFault::WeightOutOfRange { .. } => ModelError::WeldWeightOutOfRange,
                validate::WeldFault::UnknownSlot { slot, .. } => {
                    ModelError::WeldUnknownSlot(slot.to_string())
                }
                validate::WeldFault::SlotPairedTwice { slot, .. } => {
                    ModelError::WeldSlotPairedTwice(slot.to_string())
                }
            })?;
        }
        self.welds = welds;
        self.bump();
        Ok(())
    }

    fn weld_end(&self, end: &NodeId, fragment: bool) -> validate::WeldEnd {
        validate::WeldEnd::of(self.node(end).map(|n| &n.kind), fragment)
    }

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
                )
                .saturating_add(
                    self.animations
                        .capacity()
                        .saturating_mul(std::mem::size_of::<ClmAnimation>()),
                );
        for lane in self.animations.iter().flat_map(|a| &a.lanes) {
            bytes = bytes.saturating_add(
                lane.keyframes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<clm::ClmKeyframe>()),
            );
        }
        for weld in &self.welds {
            bytes = bytes
                .saturating_add(weld.a.as_str().len())
                .saturating_add(weld.b.as_str().len())
                .saturating_add(
                    weld.pairs
                        .capacity()
                        .saturating_mul(std::mem::size_of::<SlotPair>()),
                );
            for pair in weld.pairs.iter() {
                bytes = bytes
                    .saturating_add(pair.a.as_str().len())
                    .saturating_add(pair.b.as_str().len());
            }
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
            if let ModelNodeKind::Part(part) = &node.kind {
                bytes = bytes.saturating_add(
                    part.slots
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Slot>()),
                );
                for slot in part.slots.iter() {
                    bytes = bytes.saturating_add(slot.id.as_str().len());
                }
            }
        }
        for (id, param) in &self.params {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(param.name.as_str().len());
        }
        for binding in &self.bindings {
            bytes = bytes.saturating_add(binding_values_size(&binding.values));
            bytes = bytes
                .saturating_add(binding.key_positions.capacity() * std::mem::size_of::<Vec<f32>>());
            for axis in &binding.key_positions {
                bytes = bytes.saturating_add(axis.capacity() * std::mem::size_of::<f32>());
            }
        }
        for (id, texture) in &self.textures {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(texture.data.len());
        }
        bytes
    }

    pub fn node_mesh(&self, id: &NodeId) -> Option<&ClmMesh> {
        self.node(id)?.mesh()
    }

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

    fn check_node_refs(&self, node: &ModelNode) -> Result<(), ModelError> {
        // Checked in either shape: an addon carries its own textures, so an
        // albedo is the one cross-reference that never dangles into a base.
        if let ModelNodeKind::Part(p) = &node.kind {
            if p.albedo
                .as_ref()
                .is_some_and(|t| !self.textures.contains_key(t))
            {
                return Err(ModelError::UnknownTexture);
            }
        }
        // A fragment's other cross-references are supposed to dangle into a
        // base; `Model::install` is where they are resolved.
        if self.is_fragment() {
            return Ok(());
        }
        if let ModelNodeKind::Spine(spine) = &node.kind {
            if spine
                .targets
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
                    Some(k) if is_mask_source(k) => {}
                    Some(_) => return Err(ModelError::NotAMaskSource),
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

/// Pointer-equal leaves are equal without being read.
fn same<T: PartialEq + ?Sized>(a: &Arc<T>, b: &Arc<T>) -> bool {
    Arc::ptr_eq(a, b) || a == b
}

fn kind_edit_fault(before: &ModelNodeKind, after: &ModelNodeKind) -> Option<&'static str> {
    use ModelNodeKind as Kind;
    match (before, after) {
        (Kind::Group, Kind::Group) | (Kind::MotionResponse(_), Kind::MotionResponse(_)) => None,
        (Kind::Part(a), Kind::Part(b)) => {
            if !same(&a.mesh.0, &b.mesh.0) {
                Some("a part's mesh")
            } else if a.albedo != b.albedo {
                Some("a part's albedo")
            } else if !same(&a.masks, &b.masks) {
                Some("a part's masks")
            } else if !same(&a.slots, &b.slots) {
                Some("a part's slots")
            } else {
                None
            }
        }
        (Kind::Composite(a), Kind::Composite(b)) => {
            (!same(&a.masks, &b.masks)).then_some("a composite's masks")
        }
        (Kind::MeshGroup(a), Kind::MeshGroup(b)) => {
            (!same(&a.mesh.0, &b.mesh.0)).then_some("a mesh group's mesh")
        }
        (Kind::Spine(a), Kind::Spine(b)) => (a != b).then_some("a spine's joints or targets"),
        _ => Some("a node's kind"),
    }
}

pub(crate) fn is_mask_source(kind: &ModelNodeKind) -> bool {
    matches!(kind, ModelNodeKind::Part(_) | ModelNodeKind::Composite(_))
}

/// A part's albedo is a texture's only user.
pub(crate) fn albedo_of(node: &ModelNode) -> Option<&TexId> {
    match &node.kind {
        ModelNodeKind::Part(p) => p.albedo.as_ref(),
        _ => None,
    }
}

/// Vertices come in pairs, uvs match them, and every index names a vertex.
fn validate_mesh(mesh: &ClmMesh) -> Result<(), ModelError> {
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
        clm::ClmIndices::U16(v) => v.iter().map(|&i| i as usize).max(),
        clm::ClmIndices::U32(v) => v.iter().map(|&i| i as usize).max(),
    };
    if max_index.is_some_and(|m| m >= vcount) {
        return Err(ModelError::MalformedMesh("index names a missing vertex"));
    }
    Ok(())
}

fn mesh_size(mesh: &ClmMesh) -> usize {
    let indices = match &mesh.indices {
        clm::ClmIndices::U16(indices) => indices
            .capacity()
            .saturating_mul(std::mem::size_of::<u16>()),
        clm::ClmIndices::U32(indices) => indices
            .capacity()
            .saturating_mul(std::mem::size_of::<u32>()),
    };
    mesh.verts
        .capacity()
        .saturating_add(mesh.uvs.capacity())
        .saturating_mul(std::mem::size_of::<f32>())
        .saturating_add(indices)
}

fn binding_values_size(values: &ClmBindingValues) -> usize {
    use ClmBindingValues as V;
    match values {
        V::Deform(cells) => cells
            .cells
            .capacity()
            .saturating_mul(std::mem::size_of::<clm::ClmCell<Vec<f32>>>())
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
        | V::TransformTZ(cells)
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
        | V::ScreenTintB(cells) => cells
            .cells
            .capacity()
            .saturating_mul(std::mem::size_of::<clm::ClmCell<f32>>()),
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
    use crate::formats::clm::ClmIndices;
    use crate::id::SeededHex;

    pub(super) struct Fixture {
        pub model: Model,
        pub root: NodeId,
        pub part: NodeId,
        pub other: NodeId,
        pub composite: NodeId,
        pub response: NodeId,
        pub param: ParamId,
        pub tex: TexId,
        pub hex: SeededHex,
    }

    impl Fixture {
        fn node(&mut self, name: &str, kind: ModelNodeKind) -> NodeId {
            let root = self.root.clone();
            self.model
                .add_node(&root, ModelNode::new(name, kind), &mut self.hex)
                .unwrap()
        }

        fn spine(&mut self) -> NodeId {
            self.node(
                "spine",
                ModelNodeKind::Spine(ModelSpine::new(vec![[0.0, -50.0], [0.0, -100.0]])),
            )
        }
    }

    fn fixture() -> Fixture {
        let mut hex = SeededHex::new(7);
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("p"),
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .unwrap();
        let mut fixture = Fixture {
            model,
            root,
            part: NodeId::new("placeholder").unwrap(),
            other: NodeId::new("placeholder").unwrap(),
            composite: NodeId::new("placeholder").unwrap(),
            response: NodeId::new("placeholder").unwrap(),
            param,
            tex: TexId::new("placeholder").unwrap(),
            hex,
        };
        fixture.part = fixture.node("part", part_kind());
        fixture.other = fixture.node("other", part_kind());
        // `other` draws the texture; `part` is left unmapped.
        let drawn_by = fixture.other.clone();
        fixture.tex = fixture
            .model
            .add_texture(
                &drawn_by,
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: [0x89, b'P', b'N', b'G'][..].into(),
                },
                &mut fixture.hex,
            )
            .unwrap();
        fixture.composite =
            fixture.node("composite", ModelNodeKind::Composite(ModelComposite::new()));
        fixture.response = fixture.node(
            "response",
            ModelNodeKind::MotionResponse(Default::default()),
        );
        let (composite, part) = (fixture.composite.clone(), fixture.part.clone());
        fixture
            .model
            .mask_add(&composite, &part, MaskMode::Mask)
            .unwrap();
        fixture
    }

    fn slot(id: &str) -> SlotId {
        SlotId::new(id).unwrap()
    }

    fn pair(id: &str, weight: f32) -> SlotPair {
        SlotPair {
            a: slot(id),
            b: slot(id),
            weight,
        }
    }

    fn scalar_key(r: &Fixture) -> BindingKey {
        BindingKey::new(
            r.param.clone(),
            r.part.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        )
    }

    fn deform_key(r: &Fixture) -> BindingKey {
        BindingKey::new(r.param.clone(), r.part.clone(), BindingTarget::Deform)
    }

    fn one_lane_animation(param: &ParamId) -> ClmAnimation {
        ClmAnimation {
            name: "blink".into(),
            length: 10,
            lanes: vec![crate::formats::clm::ClmLane {
                param: param.clone(),
                interpolation: InterpolateMode::Linear,
                keyframes: vec![crate::formats::clm::ClmKeyframe {
                    frame: 0,
                    value: 1.0,
                }],
            }],
            ..ClmAnimation::default()
        }
    }

    #[test]
    fn an_animation_lane_must_name_a_live_param() {
        let mut r = fixture();
        let stranger = ParamId::new("nobody").unwrap();

        assert!(matches!(
            r.model.set_animations(vec![one_lane_animation(&stranger)]),
            Err(ModelError::UnknownParam)
        ));
        assert!(r.model.animations().is_empty());
    }

    #[test]
    fn an_animation_lane_must_be_in_frame_order() {
        let mut r = fixture();
        let param = r.param.clone();
        let mut animation = one_lane_animation(&param);
        animation.lanes[0].keyframes = vec![
            crate::formats::clm::ClmKeyframe {
                frame: 8,
                value: 1.0,
            },
            crate::formats::clm::ClmKeyframe {
                frame: 2,
                value: 0.0,
            },
        ];

        assert!(matches!(
            r.model.set_animations(vec![animation]),
            Err(ModelError::UnsortedLane)
        ));
        assert!(r.model.animations().is_empty());
    }

    #[test]
    fn deleting_a_param_drops_its_animation_lanes() {
        let mut r = fixture();
        let param = r.param.clone();
        r.model
            .set_animations(vec![one_lane_animation(&param)])
            .unwrap();

        r.model.delete_param(&param).unwrap();

        assert_eq!(r.model.animations().len(), 1, "the animation survives");
        assert!(
            r.model.animations()[0].lanes.is_empty(),
            "its lane over the deleted param does not"
        );
    }

    #[test]
    fn every_constructed_model_gets_its_own_identity() {
        let a = Model::new();
        let b = Model::new();
        assert_eq!(a.generation(), b.generation(), "both start at generation 0");
        assert_ne!(a.identity(), b.identity());
    }

    fn spine_targets(m: &Model, node: &NodeId) -> Vec<Option<ParamId>> {
        match m.node(node).map(|n| &n.kind) {
            Some(ModelNodeKind::Spine(sp)) => sp.targets().to_vec(),
            other => panic!("not a spine: {other:?}"),
        }
    }

    #[test]
    fn deleting_a_param_leaves_a_rigid_link_behind() {
        let mut r = fixture();
        let spine = r.spine();
        let param = r.param.clone();
        r.model
            .set_spine_targets(&spine, vec![Some(param.clone()), None])
            .unwrap();

        r.model.delete_param(&param).unwrap();
        assert_eq!(spine_targets(&r.model, &spine), vec![None, None]);
    }

    #[test]
    fn a_spine_no_reader_would_take_cannot_be_authored() {
        let mut r = fixture();
        let spine = r.spine();
        let root = r.root.clone();
        let before = r.model.generation();

        for (joints, expected) in [
            (Vec::new(), SpineJointError::NoJoints),
            (vec![[0.0, f32::NAN]], SpineJointError::NonFinite(0)),
            (vec![[0.0, 0.0]], SpineJointError::Coincident(0)),
            (
                vec![[0.0, -5.0], [0.0, -5.0]],
                SpineJointError::Coincident(1),
            ),
        ] {
            assert!(
                matches!(
                    r.model.set_spine_joints(&spine, joints.clone()),
                    Err(ModelError::SpineJoints(e)) if e == expected
                ),
                "set_spine_joints took {joints:?}"
            );
            let node = ModelNode::new("bad", ModelNodeKind::Spine(ModelSpine::new(joints.clone())));
            assert!(
                matches!(
                    r.model.add_node(&root, node, &mut r.hex),
                    Err(ModelError::SpineJoints(e)) if e == expected
                ),
                "add_node took {joints:?}"
            );
        }
        assert_eq!(r.model.generation(), before, "a refused edit is no edit");
        match &r.model.node(&spine).unwrap().kind {
            ModelNodeKind::Spine(sp) => {
                assert_eq!(sp.joints(), [[0.0, -50.0], [0.0, -100.0]])
            }
            other => panic!("not a spine: {other:?}"),
        }

        r.model
            .set_spine_joints(&spine, vec![[0.0, -10.0]])
            .unwrap();
        Model::from_clm_bytes(&r.model.to_clm_bytes().unwrap()).unwrap();
    }

    #[test]
    fn a_value_that_is_not_a_number_is_refused_by_every_mutator_that_writes_one() {
        let mut r = fixture();
        let (part, root, param) = (r.part.clone(), r.root.clone(), r.param.clone());
        let before = r.model.generation();

        // The closure's other writes are undone along with the bad one.
        let got = r.model.update_node(&part, |n| {
            n.name = Name::truncated("renamed");
            n.enabled = false;
            n.z_order = f32::NAN;
        });
        assert!(
            matches!(got, Err(ModelError::NonFinite("a node's z-order"))),
            "{got:?}"
        );
        let got = r
            .model
            .update_node(&part, |n| n.transform.scale[0] = f32::INFINITY);
        assert!(
            matches!(got, Err(ModelError::NonFinite("a node's transform"))),
            "{got:?}"
        );
        let node = r.model.node(&part).unwrap();
        assert_eq!(node.name.as_str(), "part");
        assert!(node.enabled);
        assert_eq!(node.z_order, 0.0);
        assert_eq!(node.transform, ClmTransform::default());

        let mut adrift = ModelNode::new("adrift", ModelNodeKind::Group);
        adrift.transform.translation[1] = f32::NAN;
        let got = r.model.add_node(&root, adrift, &mut r.hex);
        assert!(
            matches!(got, Err(ModelError::NonFinite("a node's transform"))),
            "{got:?}"
        );

        let got = r.model.add_param_with_id(
            ParamId::new("nan").unwrap(),
            ModelParam::new(Name::truncated("nan"), 0.0, 1.0, f32::NAN),
        );
        assert!(
            matches!(got, Err(ModelError::NonFinite("a param's default"))),
            "{got:?}"
        );
        let got = r.model.set_param_default(&param, f32::INFINITY);
        assert!(
            matches!(got, Err(ModelError::NonFinite("a param's default"))),
            "{got:?}"
        );
        assert_eq!(r.model.param(&param).unwrap().default, 0.0);

        assert_eq!(r.model.generation(), before, "a refused edit is no edit");
        // A finite value outside the range is still the author's business.
        r.model.set_param_default(&param, 7.0).unwrap();
    }

    #[test]
    fn a_refit_that_returns_a_bad_cell_refuses_the_whole_mesh_edit() {
        let mut r = fixture();
        let part = r.part.clone();
        let key = deform_key(&r);
        r.model
            .set_deform_vertices(&key, [0, 0], vec![0.5; 8])
            .unwrap();
        r.model
            .set_deform_vertices(&key, [1, 0], vec![0.25; 8])
            .unwrap();
        let mut bigger = quad();
        bigger.verts.extend([0.0, 0.0]);
        bigger.uvs.extend([0.0, 0.0]);
        let resized = |new: &ClmMesh, offsets: &[f32]| {
            let mut out = offsets.to_vec();
            out.resize(new.verts.len(), 0.0);
            out
        };
        let before = r.model.generation();
        let bytes = r.model.to_clm_bytes().unwrap();

        // The first cell refits fine; the second does not. Nothing may stick.
        let mut cell = 0;
        let got = r
            .model
            .set_node_mesh_with(&part, bigger.clone(), |_, new, offsets| {
                cell += 1;
                if cell == 2 {
                    offsets.to_vec()
                } else {
                    resized(new, offsets)
                }
            });
        assert!(
            matches!(
                got,
                Err(ModelError::DeformLength {
                    expected: 10,
                    got: 8
                })
            ),
            "{got:?}"
        );
        let got = r
            .model
            .set_node_mesh_with(&part, bigger.clone(), |_, new, _| {
                vec![f32::NAN; new.verts.len()]
            });
        assert!(
            matches!(got, Err(ModelError::NonFinite("a deform offset"))),
            "{got:?}"
        );
        assert_eq!(r.model.generation(), before, "a refused edit is no edit");
        assert_eq!(r.model.to_clm_bytes().unwrap(), bytes);

        r.model
            .set_node_mesh_with(&part, bigger, |_, new, offsets| resized(new, offsets))
            .unwrap();
        Model::from_clm_bytes(&r.model.to_clm_bytes().unwrap()).unwrap();
    }

    #[test]
    fn an_in_place_kind_edit_changes_only_the_fields_a_kind_exposes() {
        let mut r = fixture();
        let spine = r.spine();
        let group = r.node(
            "bend",
            ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad())),
        );
        let (part, other, composite) = (r.part.clone(), r.other.clone(), r.composite.clone());
        // `part` as it is before it gains a slot, and `other`, which draws a
        // texture `part` does not.
        let slotless = r.model.node(&part).unwrap().kind.clone();
        let textured = r.model.node(&other).unwrap().kind.clone();
        r.model.slot_add(&part, slot("s")).unwrap();
        let mut moved = quad();
        moved.verts[0] += 1.0;

        let before = r.model.generation();
        let bytes = r.model.to_clm_bytes().unwrap();
        let swaps: Vec<(&NodeId, ModelNodeKind, &str)> = vec![
            (&part, ModelNodeKind::Group, "a node's kind"),
            (
                &part,
                ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad())),
                "a node's kind",
            ),
            (
                &other,
                ModelNodeKind::Part(ModelPart::new(moved.clone())),
                "a part's mesh",
            ),
            (&part, textured, "a part's albedo"),
            (&part, slotless, "a part's slots"),
            (
                &composite,
                ModelNodeKind::Composite(ModelComposite::new()),
                "a composite's masks",
            ),
            (
                &group,
                ModelNodeKind::MeshGroup(ModelMeshGroup::new(moved)),
                "a mesh group's mesh",
            ),
            (
                &spine,
                ModelNodeKind::Spine(ModelSpine::new(vec![[0.0, 0.0]])),
                "a spine's joints or targets",
            ),
        ];
        for (id, replacement, what) in swaps {
            let got = r.model.update_node_kind(id, |kind| *kind = replacement);
            assert!(
                matches!(got, Err(ModelError::KindEdit(named)) if named == what),
                "expected a refusal naming {what}, got {got:?}"
            );
        }
        assert_eq!(r.model.generation(), before, "a refused edit is no edit");
        assert_eq!(
            r.model.to_clm_bytes().unwrap(),
            bytes,
            "every refused edit put the kind back"
        );

        let was = r
            .model
            .update_node_kind(&part, |kind| match kind {
                ModelNodeKind::Part(p) => std::mem::replace(&mut p.opacity, 0.25),
                _ => f32::NAN,
            })
            .unwrap();
        assert_eq!(was, 1.0);
        r.model
            .update_node_kind(&composite, |kind| {
                if let ModelNodeKind::Composite(c) = kind {
                    c.blend_mode = BlendMode::Multiply;
                }
            })
            .unwrap();
        r.model
            .update_node_kind(&group, |kind| {
                if let ModelNodeKind::MeshGroup(mg) = kind {
                    mg.translate_children = false;
                }
            })
            .unwrap();
        assert_ne!(r.model.generation(), before);
        match &r.model.node(&part).unwrap().kind {
            ModelNodeKind::Part(p) => {
                assert_eq!(p.opacity, 0.25);
                assert_eq!(p.slots().len(), 1, "the slot is still there");
            }
            other => panic!("not a part: {other:?}"),
        }
    }

    #[test]
    fn a_clone_keeps_the_identity() {
        let model = Model::new();
        let snapshot = model.clone();
        assert_eq!(model.identity(), snapshot.identity());
    }

    #[test]
    fn editing_forward_from_a_restored_snapshot_never_reuses_a_generation() {
        let mut model = Model::new();
        let snapshot = model.clone();

        model.set_animations(Vec::new()).unwrap();
        let undone = model.generation();

        model = snapshot;
        assert_ne!(model.generation(), undone);

        model.set_animations(Vec::new()).unwrap();
        assert_ne!(
            model.generation(),
            undone,
            "a state that was undone must not get its generation back",
        );
    }

    /// A new mutating method belongs in this list.
    #[test]
    fn generation_bumps_on_every_mutating_method() {
        #[allow(clippy::type_complexity)]
        let edits: Vec<(&str, Box<dyn Fn(&mut Fixture)>)> = vec![
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
                "set_motion_response",
                Box::new(|r| {
                    r.model
                        .set_motion_response(
                            &r.response,
                            crate::MotionResponse {
                                inputs: vec![crate::ResponseInput {
                                    source: crate::ResponseSource::PoseParam {
                                        param: r.param.clone(),
                                    },
                                    gain: 1.0,
                                }],
                                output_param: None,
                                ..Default::default()
                            },
                        )
                        .unwrap()
                }),
            ),
            (
                "set_spine_targets",
                Box::new(|r| {
                    let spine = r.spine();
                    r.model
                        .set_spine_targets(&spine, vec![Some(r.param.clone()), None])
                        .unwrap()
                }),
            ),
            (
                "set_spine_joints",
                Box::new(|r| {
                    let spine = r.spine();
                    r.model
                        .set_spine_joints(&spine, vec![[0.0, -10.0]])
                        .unwrap()
                }),
            ),
            (
                "set_node_mesh",
                Box::new(|r| {
                    r.model.set_node_mesh(&r.part, quad()).unwrap();
                }),
            ),
            (
                "set_node_mesh_with",
                Box::new(|r| {
                    r.model
                        .set_node_mesh_with(&r.part, quad(), |_, _, v| v.to_vec())
                        .unwrap();
                }),
            ),
            (
                "slot_add",
                Box::new(|r| r.model.slot_add(&r.part, slot("s")).unwrap()),
            ),
            (
                "rename_slot",
                Box::new(|r| {
                    r.model.slot_add(&r.part, slot("s")).unwrap();
                    r.model.rename_slot(&r.part, &slot("s"), slot("t")).unwrap();
                }),
            ),
            (
                "slot_fill",
                Box::new(|r| {
                    r.model.slot_add(&r.part, slot("s")).unwrap();
                    r.model.slot_fill(&r.part, &slot("s"), 2).unwrap();
                }),
            ),
            (
                "slot_clear",
                Box::new(|r| {
                    r.model.slot_add(&r.part, slot("s")).unwrap();
                    r.model.slot_clear(&r.part, &slot("s")).unwrap();
                }),
            ),
            (
                "slot_delete",
                Box::new(|r| {
                    r.model.slot_add(&r.part, slot("s")).unwrap();
                    r.model.slot_delete(&r.part, &slot("s")).unwrap();
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
                    r.model.add_texture(&r.part, t, &mut r.hex).unwrap();
                }),
            ),
            (
                "add_texture_with_id",
                Box::new(|r| {
                    r.model
                        .add_texture_with_id(TexId::new("chosen").unwrap(), &r.part, texture())
                        .unwrap()
                }),
            ),
            (
                "delete_texture",
                Box::new(|r| r.model.delete_texture(&r.tex).unwrap()),
            ),
            (
                "set_animations",
                Box::new(|r| {
                    let animation = one_lane_animation(&r.param);
                    r.model.set_animations(vec![animation]).unwrap()
                }),
            ),
            (
                "set_welds",
                Box::new(|r| {
                    r.model
                        .set_welds(vec![ModelWeld::new(
                            r.part.clone(),
                            r.other.clone(),
                            Vec::new(),
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
                    r.model.key_insert(&scalar_key(r), &r.param, 0.25).unwrap();
                }),
            ),
            (
                "key_delete",
                Box::new(|r| r.model.key_delete(&scalar_key(r), &r.param, 1).unwrap()),
            ),
            (
                "key_move",
                Box::new(|r| r.model.key_move(&scalar_key(r), &r.param, 1, 0.6).unwrap()),
            ),
        ];

        for (name, edit) in edits {
            let mut r = fixture();
            if matches!(
                name,
                "key_insert" | "key_delete" | "key_move" | "invert_binding" | "copy_binding_key"
            ) {
                r.model
                    .add_binding_with_positions(&scalar_key(&r), vec![vec![0.0, 0.5, 1.0]])
                    .unwrap();
            }
            let before = r.model.generation();
            edit(&mut r);
            assert!(
                r.model.generation() > before,
                "{name} did not bump the generation",
            );
        }
    }

    fn welded_fixture() -> Fixture {
        let mut r = fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        for (node, verts) in [(&part, [0u32, 1]), (&other, [2, 3])] {
            for (id, vertex) in [(slot("l"), verts[0]), (slot("r"), verts[1])] {
                r.model.slot_add(node, id.clone()).unwrap();
                r.model.slot_fill(node, &id, vertex).unwrap();
            }
        }
        r.model
            .set_welds(vec![ModelWeld::new(
                part,
                other,
                vec![pair("l", 1.0), pair("r", 0.25)],
            )])
            .unwrap();
        r
    }

    #[test]
    fn a_generated_slot_id_is_redrawn_until_it_is_free() {
        let mut r = fixture();
        let part = r.part.clone();
        // Pin the draw, then take its first two results by hand so the mint
        // has something to collide with.
        let taken = {
            let mut hex = SeededHex::new(11);
            [SlotId::generate(&mut hex), SlotId::generate(&mut hex)]
        };
        for id in &taken {
            r.model.slot_add(&part, id.clone()).unwrap();
        }

        let mut hex = SeededHex::new(11);
        let minted = r.model.slot_add_generated(&part, &mut hex).unwrap();

        assert!(!taken.contains(&minted), "{minted} collides");
        assert!(minted.as_str().starts_with("slot-"), "{minted}");
        assert!(r.model.slot(&part, &minted).is_some());
        // A node that cannot carry a slot is refused before anything is drawn.
        let root = r.root.clone();
        assert!(matches!(
            r.model.slot_add_generated(&root, &mut hex),
            Err(ModelError::NotAPart)
        ));
    }

    #[test]
    fn a_slot_id_is_free_on_every_other_part() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());

        r.model.slot_add(&part, slot("m")).unwrap();
        assert!(
            r.model.slot(&other, &slot("m")).is_none(),
            "an edit to one part's slots reaches no other part",
        );
        assert_eq!(
            r.model.welds()[0].pairs().len(),
            2,
            "and pairs nothing on its own",
        );
        r.model.slot_add(&other, slot("m")).unwrap();
        assert!(r.model.slot(&other, &slot("m")).is_some());
    }

    #[test]
    fn renaming_a_slot_carries_the_weld_pairs_that_named_it() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        let before = r.model.welds()[0].resolve(&r.model);

        r.model
            .rename_slot(&part, &slot("l"), slot("neck"))
            .unwrap();

        assert!(r.model.slot(&part, &slot("l")).is_none());
        assert!(r.model.slot(&part, &slot("neck")).is_some());
        assert_eq!(r.model.welds()[0].pairs()[0].a, slot("neck"));
        assert_eq!(
            r.model.welds()[0].pairs()[0].b,
            slot("l"),
            "the far part's slot is not this rename's business",
        );
        assert!(
            r.model.slot(&other, &slot("l")).is_some(),
            "and the far part keeps the slot it had",
        );
        assert_eq!(
            r.model.welds()[0].resolve(&r.model),
            before,
            "the weld solves exactly what it did before",
        );

        // The refusals: a slot that is not there, and a name that is.
        assert!(matches!(
            r.model.rename_slot(&part, &slot("l"), slot("x")),
            Err(ModelError::UnknownSlot)
        ));
        assert!(matches!(
            r.model.rename_slot(&part, &slot("neck"), slot("r")),
            Err(ModelError::DuplicateSlot(id)) if id == "r"
        ));
        r.model
            .rename_slot(&part, &slot("neck"), slot("neck"))
            .unwrap();
    }

    #[test]
    fn a_slot_weight_is_a_share_of_the_end_it_is_named_against() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());

        r.model
            .set_weld_slot_weight(&part, &other, &slot("l"), 0.25)
            .unwrap();
        assert_eq!(r.model.welds()[0].pairs()[0].weight, 0.25);
        assert_eq!(r.model.welds()[0].pairs()[1].weight, 0.25);

        // Named the other way round, 0.25 is B's share, so A's is 0.75 — and
        // `slot` is read on the part named first, which is now `other`.
        r.model
            .set_weld_slot_weight(&other, &part, &slot("l"), 0.25)
            .unwrap();
        assert_eq!(r.model.welds()[0].pairs()[0].weight, 0.75);

        assert!(matches!(
            r.model
                .set_weld_slot_weight(&part, &other, &slot("gone"), 0.5),
            Err(ModelError::UnknownSlot)
        ));
        let root = r.root.clone();
        assert!(matches!(
            r.model.set_weld_slot_weight(&part, &root, &slot("l"), 0.5),
            Err(ModelError::UnknownWeld)
        ));
        assert!(
            matches!(
                r.model.set_weld_slot_weight(&part, &other, &slot("l"), 1.5),
                Err(ModelError::WeldWeightOutOfRange)
            ),
            "a share outside 0..=1 has no meaning to flip",
        );
    }

    #[test]
    fn re_meshing_a_part_empties_its_slots_and_reports_them() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        assert_eq!(
            r.model.welds()[0].resolve(&r.model),
            vec![
                ModelWeldPair {
                    a_vert: 0,
                    b_vert: 2,
                    weight: 1.0
                },
                ModelWeldPair {
                    a_vert: 1,
                    b_vert: 3,
                    weight: 0.25
                },
            ]
        );

        let emptied = r.model.set_node_mesh(&part, quad()).unwrap();
        assert_eq!(emptied, vec![slot("l"), slot("r")]);
        assert_eq!(
            r.model.unfilled_slots(),
            vec![(part.clone(), slot("l")), (part.clone(), slot("r"))],
        );
        assert!(
            r.model
                .slots(&other)
                .unwrap()
                .iter()
                .all(|s| s.vertex().is_some()),
            "the far part's slots are none of this edit's business",
        );

        let weld = &r.model.welds()[0];
        assert_eq!(weld.pairs().len(), 2, "the weld itself is untouched");
        assert!(
            weld.resolve(&r.model).is_empty(),
            "an unfilled slot is skipped, not guessed at",
        );

        r.model.slot_fill(&part, &slot("r"), 3).unwrap();
        assert_eq!(
            r.model.welds()[0].resolve(&r.model),
            vec![ModelWeldPair {
                a_vert: 3,
                b_vert: 3,
                weight: 0.25
            }],
            "a half-repaired part solves the pairs it has",
        );
        assert!(r.model.to_clm_bytes().is_ok());
    }

    #[test]
    fn deleting_a_slot_drops_the_pairs_that_named_it() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());

        r.model.slot_delete(&part, &slot("l")).unwrap();

        assert!(r.model.slot(&part, &slot("l")).is_none());
        assert!(
            r.model.slot(&other, &slot("l")).is_some(),
            "the far part keeps its own slot",
        );
        let pairs = r.model.welds()[0].pairs();
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            (pairs[0].a.clone(), pairs[0].b.clone()),
            (slot("r"), slot("r"))
        );
        assert!(r.model.to_clm_bytes().is_ok());

        r.model.slot_delete(&other, &slot("r")).unwrap();
        assert_eq!(r.model.welds().len(), 1, "a weld with no pairs stays");
        assert!(r.model.welds()[0].pairs().is_empty());
    }

    #[test]
    fn slot_edits_are_checked_before_they_land() {
        let mut r = fixture();
        let (part, composite) = (r.part.clone(), r.composite.clone());
        let ghost = NodeId::new("ghost").unwrap();

        assert!(matches!(
            r.model.slot_add(&composite, slot("l")),
            Err(ModelError::NotAPart)
        ));
        assert!(matches!(
            r.model.slot_add(&ghost, slot("l")),
            Err(ModelError::UnknownNode)
        ));

        r.model.slot_add(&part, slot("l")).unwrap();
        assert!(matches!(
            r.model.slot_add(&part, slot("l")),
            Err(ModelError::DuplicateSlot(id)) if id == "l"
        ));
        assert!(matches!(
            r.model.slot_fill(&part, &slot("ghost"), 0),
            Err(ModelError::UnknownSlot)
        ));
        assert!(
            matches!(
                r.model.slot_fill(&part, &slot("l"), 4),
                Err(ModelError::IndexOutOfRange)
            ),
            "the fixture's part draws a quad, so vertex 4 is off the end",
        );

        r.model.slot_fill(&part, &slot("l"), 3).unwrap();
        let filled = |m: &Model| m.slot(&part, &slot("l")).unwrap().vertex();
        assert_eq!(filled(&r.model), Some(3));
        r.model.slot_clear(&part, &slot("l")).unwrap();
        assert_eq!(filled(&r.model), None);

        assert!(matches!(
            r.model.slot_delete(&part, &slot("ghost")),
            Err(ModelError::UnknownSlot)
        ));
        assert!(
            r.model.slots(&composite).is_none(),
            "only a part carries slots",
        );
    }

    #[test]
    fn a_weld_joins_two_parts_and_pairs_slots_they_carry() {
        let mut r = welded_fixture();
        let (part, other, composite) = (r.part.clone(), r.other.clone(), r.composite.clone());
        let weld = |pairs: Vec<SlotPair>| vec![ModelWeld::new(part.clone(), other.clone(), pairs)];

        assert!(
            matches!(
                r.model
                    .set_welds(vec![ModelWeld::new(part.clone(), part.clone(), Vec::new())]),
                Err(ModelError::WeldSelfPaired)
            ),
            "a part is not welded to itself",
        );
        assert!(
            matches!(
                r.model
                    .set_welds(vec![ModelWeld::new(part.clone(), composite, Vec::new())]),
                Err(ModelError::WeldEndNotAPart)
            ),
            "only a part carries slots to pair",
        );
        assert!(matches!(
            r.model.set_welds(vec![ModelWeld::new(
                part.clone(),
                NodeId::new("ghost").unwrap(),
                Vec::new()
            )]),
            Err(ModelError::UnknownNode)
        ));
        assert!(
            matches!(
                r.model.set_welds(vec![
                    ModelWeld::new(part.clone(), other.clone(), Vec::new()),
                    ModelWeld::new(other.clone(), part.clone(), Vec::new()),
                ]),
                Err(ModelError::DuplicateWeld)
            ),
            "one record per unordered part pair, found in either order",
        );
        assert!(
            matches!(
                r.model.set_welds(weld(vec![pair("ghost", 0.5)])),
                Err(ModelError::WeldUnknownSlot(id)) if id == "ghost"
            ),
            "a pair naming a slot its part does not carry",
        );
        assert!(
            matches!(
                r.model.set_welds(weld(vec![pair("l", 0.5), pair("l", 0.25)])),
                Err(ModelError::WeldSlotPairedTwice(id)) if id == "l"
            ),
            "one slot, two pairs",
        );
        assert!(
            matches!(
                r.model.set_welds(weld(vec![pair("l", 1.5)])),
                Err(ModelError::WeldWeightOutOfRange)
            ),
            "a weight is a share of a meeting point",
        );
        assert_eq!(
            r.model.welds().len(),
            1,
            "a refused weld list leaves the old one in place",
        );
        assert_eq!(r.model.welds()[0].pairs().len(), 2);

        // A weld may pair slots with different Ids on the two sides, and one
        // side's slot may be left out of the pairing altogether.
        r.model.slot_add(&part, slot("m")).unwrap();
        r.model
            .set_welds(weld(vec![SlotPair {
                a: slot("m"),
                b: slot("r"),
                weight: 0.5,
            }]))
            .unwrap();
        assert!(r.model.to_clm_bytes().is_ok());

        let other_clone = other.clone();
        r.model.delete_node(&other_clone).unwrap();
        assert!(
            r.model.welds().is_empty(),
            "deleting a node takes the welds that named it, the way it takes \
             the masks that named it",
        );
    }

    #[test]
    fn what_the_reader_refuses_cannot_be_authored() {
        let mut r = fixture();
        let root = r.root.clone();
        let before = r.model.generation();
        let mut hex = SeededHex::new(3);

        let mut odd = quad();
        odd.verts.push(0.5);
        assert!(matches!(
            r.model.add_node(
                &root,
                ModelNode::new("odd", ModelNodeKind::Part(ModelPart::new(odd))),
                &mut hex,
            ),
            Err(ModelError::MalformedMesh(_))
        ));
        assert!(matches!(
            r.model.add_param_with_id(
                ParamId::new("flat").unwrap(),
                ModelParam::new(Name::truncated("flat"), 1.0, 1.0, 0.0),
            ),
            Err(ModelError::InvalidParamRange)
        ));
        assert_eq!(r.model.generation(), before, "neither edit landed");
        assert!(r.model.to_clm_bytes().is_ok());
    }

    #[test]
    fn a_rejected_edit_leaves_the_generation_alone() {
        let mut r = fixture();
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

    #[test]
    fn adding_a_texture_points_a_part_at_it_in_one_edit() {
        let mut r = fixture();
        let (part, before) = (r.part.clone(), r.model.generation());

        let tex = r.model.add_texture(&part, texture(), &mut r.hex).unwrap();

        assert_eq!(albedo(&r.model, &part), Some(tex.clone()));
        assert!(r.model.texture(&tex).is_some());
        assert!(
            r.model.generation() > before,
            "one edit, and it moved the clock"
        );
    }

    #[test]
    fn a_texture_needs_a_part_to_draw_it() {
        let mut r = fixture();
        let (composite, stranger) = (r.composite.clone(), NodeId::new("gone").unwrap());
        let before = r.model.generation();

        assert!(matches!(
            r.model.add_texture(&composite, texture(), &mut r.hex),
            Err(ModelError::NotAPart)
        ));
        assert!(matches!(
            r.model.add_texture(&stranger, texture(), &mut r.hex),
            Err(ModelError::UnknownNode)
        ));
        assert!(matches!(
            r.model
                .add_texture_with_id(TexId::new("chosen").unwrap(), &composite, texture()),
            Err(ModelError::NotAPart)
        ));
        assert_eq!(r.model.texture_ids().len(), 1, "nothing landed");
        assert_eq!(r.model.generation(), before, "and nothing moved the clock");
    }

    #[test]
    fn repointing_the_last_part_drawing_a_texture_deletes_it() {
        for albedo_after in [None, Some(())] {
            let mut r = fixture();
            let (part, other, old) = (r.part.clone(), r.other.clone(), r.tex.clone());
            // `other` draws `old`; hand it to `part` as well, then take it
            // back off `other` — two users, so nothing goes.
            r.model.set_part_albedo(&part, Some(old.clone())).unwrap();
            r.model.set_part_albedo(&other, None).unwrap();
            assert!(
                r.model.texture(&old).is_some(),
                "a texture a part still draws stays"
            );

            let next = albedo_after.map(|()| {
                r.model
                    .add_texture(&other, texture(), &mut r.hex)
                    .expect("a second texture")
            });
            assert_eq!(
                r.model.texture_dropped_by_repointing(&part, next.as_ref()),
                Some(old.clone()),
                "the query and the edit have to agree"
            );
            let before = r.model.generation();
            r.model.set_part_albedo(&part, next.clone()).unwrap();

            assert!(r.model.texture(&old).is_none(), "the last user let go");
            assert!(
                !r.model.texture_ids().contains(&old),
                "and the order with it"
            );
            assert_eq!(albedo(&r.model, &part), next);
            assert!(r.model.generation() > before, "one edit, one bump");
        }
    }

    #[test]
    fn an_upload_takes_the_texture_it_displaces() {
        let mut r = fixture();
        let (other, old) = (r.other.clone(), r.tex.clone());

        assert_eq!(
            r.model.texture_dropped_by_repointing(&other, None),
            Some(old.clone())
        );
        let new = r.model.add_texture(&other, texture(), &mut r.hex).unwrap();

        assert_eq!(r.model.texture_ids(), std::slice::from_ref(&new));
        assert_eq!(albedo(&r.model, &other), Some(new));
    }

    #[test]
    fn a_snapshot_taken_before_the_cascade_still_has_the_texture() {
        let mut r = fixture();
        let (other, old) = (r.other.clone(), r.tex.clone());
        let snapshot = r.model.clone();

        r.model.set_part_albedo(&other, None).unwrap();
        assert!(r.model.texture(&old).is_none());

        assert!(snapshot.texture(&old).is_some());
        assert_eq!(snapshot.texture_ids(), std::slice::from_ref(&old));
    }

    #[test]
    fn deleting_a_subtree_takes_only_the_textures_nothing_else_draws() {
        let mut r = fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        let shared = r.tex.clone();
        // `other` draws `shared` and stays behind. Under `part`: a child
        // drawing `shared` too, and one with a texture of its own.
        let sharer = r
            .model
            .add_node(&part, ModelNode::new("sharer", part_kind()), &mut r.hex)
            .unwrap();
        r.model
            .set_part_albedo(&sharer, Some(shared.clone()))
            .unwrap();
        let owner = r
            .model
            .add_node(&part, ModelNode::new("owner", part_kind()), &mut r.hex)
            .unwrap();
        let exclusive = r.model.add_texture(&owner, texture(), &mut r.hex).unwrap();

        assert_eq!(
            r.model.textures_dropped_by_deleting(&part),
            vec![exclusive.clone()],
            "the query and the edit have to agree"
        );
        r.model.delete_node(&part).unwrap();

        assert!(
            r.model.texture(&exclusive).is_none(),
            "nothing draws it now"
        );
        assert!(
            r.model.texture(&shared).is_some(),
            "a part outside the subtree still draws it"
        );
        assert_eq!(r.model.texture_ids(), std::slice::from_ref(&shared));
        assert_eq!(albedo(&r.model, &other), Some(shared));
    }

    #[test]
    fn no_edit_leaves_a_texture_with_no_part_drawing_it() {
        #[allow(clippy::type_complexity)]
        let edits: Vec<(&str, Box<dyn Fn(&mut Fixture)>)> = vec![
            (
                "set_part_albedo(None)",
                Box::new(|r| r.model.set_part_albedo(&r.other, None).unwrap()),
            ),
            (
                "add_texture over the old one",
                Box::new(|r| {
                    let t = texture();
                    r.model.add_texture(&r.other, t, &mut r.hex).unwrap();
                }),
            ),
            (
                "delete_node",
                Box::new(|r| r.model.delete_node(&r.other).unwrap()),
            ),
            (
                "delete_texture",
                Box::new(|r| r.model.delete_texture(&r.tex).unwrap()),
            ),
        ];
        for (name, edit) in edits {
            let mut r = fixture();
            edit(&mut r);
            let drawn: HashSet<&TexId> = r.model.nodes.values().filter_map(albedo_of).collect();
            for id in r.model.texture_ids() {
                assert!(
                    drawn.contains(id),
                    "{name} left {id} with no part drawing it"
                );
            }
        }
    }

    #[test]
    fn renaming_rewrites_every_reference_to_the_id() {
        let mut r = fixture();
        let (part, other, composite, response) = (
            r.part.clone(),
            r.other.clone(),
            r.composite.clone(),
            r.response.clone(),
        );
        let (param, tex) = (r.param.clone(), r.tex.clone());
        let child = r
            .model
            .add_node(&part, ModelNode::new("child", part_kind()), &mut r.hex)
            .unwrap();
        r.model.set_part_albedo(&part, Some(tex.clone())).unwrap();
        r.model
            .set_motion_response(
                &response,
                crate::MotionResponse {
                    inputs: vec![crate::ResponseInput {
                        source: crate::ResponseSource::PoseParam {
                            param: param.clone(),
                        },
                        gain: 1.0,
                    }],
                    output_param: None,
                    ..Default::default()
                },
            )
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
        assert!(r
            .model
            .node(&r.root)
            .unwrap()
            .children()
            .contains(&new_node));
        assert_eq!(r.model.node(&child).unwrap().parent(), Some(&new_node));
        assert_eq!(
            r.model.node(&composite).unwrap().masks().unwrap()[0].source(),
            &new_node
        );
        assert_eq!(r.model.bindings().next().unwrap().node(), &new_node);
        assert_eq!(r.model.welds()[0].a(), &new_node);
        assert!(r.model.to_clm_bytes().is_ok());

        r.model
            .set_animations(vec![one_lane_animation(&param)])
            .unwrap();

        let new_param = ParamId::new("head-turn").unwrap();
        r.model.rename_param_id(&param, new_param.clone()).unwrap();
        assert_eq!(
            r.model.animations()[0].lanes[0].param,
            new_param,
            "the animation lane follows its param",
        );
        assert_eq!(r.model.param_ids(), std::slice::from_ref(&new_param));
        assert_eq!(
            r.model.bindings().next().unwrap().params(),
            &BindingParams::One(new_param.clone())
        );
        assert_eq!(
            response_input(&r.model, &response),
            Some(new_param),
            "the response node follows its param",
        );

        let new_tex = TexId::new("skin").unwrap();
        r.model.rename_tex_id(&tex, new_tex.clone()).unwrap();
        assert_eq!(r.model.texture_ids(), std::slice::from_ref(&new_tex));
        assert_eq!(albedo(&r.model, &new_node), Some(new_tex));
        assert!(r.model.to_clm_bytes().is_ok());

        let new_root = NodeId::new("body").unwrap();
        let old_root = r.root.clone();
        r.model.rename_node_id(&old_root, new_root.clone()).unwrap();
        assert_eq!(r.model.root(), Some(&new_root));
        assert!(r.model.to_clm_bytes().is_ok());
    }

    #[test]
    fn renaming_onto_a_taken_id_is_refused() {
        let mut r = fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        assert!(matches!(
            r.model.rename_node_id(&part, other),
            Err(ModelError::DuplicateId(_))
        ));
        // Renaming to the same Id is a no-op, not a conflict.
        r.model.rename_node_id(&part, part.clone()).unwrap();
        assert!(r.model.node(&part).is_some());
    }

    #[test]
    fn reparenting_leaves_the_generated_id_alone() {
        let mut r = fixture();
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
        let root = model.root().unwrap().clone();
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
        let root = model.root().unwrap().clone();
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
        let root = m.root().unwrap().clone();
        let mut add = |m: &mut Model, name: &str| {
            m.add_node(&root, ModelNode::new(name, ModelNodeKind::Group), &mut hex)
                .unwrap()
        };
        let a = add(&mut m, "a");
        let b = add(&mut m, "b");
        let c = add(&mut m, "c");
        let root = m.root().unwrap().clone();

        m.reorder(&c, 0).unwrap();
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[c.clone(), a.clone(), b.clone()]
        );

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
        let mut r = fixture();
        let node = r.part.clone();
        let key = deform_key(&r);
        let model = &mut r.model;
        model
            .add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        model
            .set_deform_vertices(&key, [2, 0], vec![1.0; 8])
            .unwrap();

        let model = r.model;
        let mut edited = model.clone();
        let mesh_arc = |m: &Model| match &m.node(&node).unwrap().kind {
            ModelNodeKind::Part(part) => part.shared_mesh().0,
            _ => unreachable!(),
        };
        let cells_arc = |m: &Model| m.binding(&key).unwrap().values.0.clone();
        let original_mesh = mesh_arc(&model);
        let original_cells = cells_arc(&model);
        assert!(Arc::ptr_eq(&original_mesh, &mesh_arc(&edited)));
        assert!(Arc::ptr_eq(&original_cells, &cells_arc(&edited)));

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
            vec![(2, 0, vec![1.0; 8])],
        );
    }

    #[test]
    fn duplicate_copies_bindings_and_remaps_internal_masks() {
        let mut hex = SeededHex::new(3);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
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
        assert_ne!(copy, group);
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[group.clone(), copy.clone()]
        );
        assert_eq!(m.node(&copy).unwrap().name.as_str(), "g copy");

        let copy_children = m.node(&copy).unwrap().children().to_vec();
        assert_eq!(copy_children.len(), 2);
        let copy_masked = &copy_children[1];
        let masks = m.node(copy_masked).unwrap().masks().unwrap();
        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0].source(), &copy_children[0]);
        assert_eq!(m.bindings_of_param(&param).count(), 2);
        assert!(m.bindings().any(|b| b.node() == copy_masked));
        assert!(m.to_clm_bytes().is_ok());
    }

    #[test]
    fn duplicate_copies_the_welds_between_the_parts_it_copies() {
        let mut hex = SeededHex::new(3);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let group = m
            .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let upper = m
            .add_node(&group, ModelNode::new("upper", part_kind()), &mut hex)
            .unwrap();
        let lower = m
            .add_node(&group, ModelNode::new("lower", part_kind()), &mut hex)
            .unwrap();
        let outside = m
            .add_node(&root, ModelNode::new("outside", part_kind()), &mut hex)
            .unwrap();
        for part in [&upper, &lower, &outside] {
            m.slot_add(part, slot("seam")).unwrap();
            m.slot_fill(part, &slot("seam"), 1).unwrap();
        }
        m.set_welds(vec![
            ModelWeld::new(upper.clone(), lower.clone(), vec![pair("seam", 0.25)]),
            ModelWeld::new(upper.clone(), outside.clone(), vec![pair("seam", 0.5)]),
        ])
        .unwrap();

        let copy = m.duplicate_subtree(&group, &mut hex).unwrap();
        let copies = m.node(&copy).unwrap().children().to_vec();
        let (upper_copy, lower_copy) = (&copies[0], &copies[1]);

        assert_eq!(m.welds().len(), 3, "one weld came along, one stayed");
        let weld = m
            .welds()
            .iter()
            .find(|w| w.a() == upper_copy && w.b() == lower_copy)
            .expect("the weld inside the subtree is copied between the copies");
        assert_eq!(weld.pairs(), [pair("seam", 0.25)]);
        assert_eq!(weld.resolve(&m).len(), 1, "and it resolves on their slots");
        assert!(
            !m.welds()
                .iter()
                .any(|w| copies.contains(w.a()) && w.b() == &outside),
            "a weld that leaves the subtree stays with the original"
        );
        Model::from_clm_bytes(&m.to_clm_bytes().unwrap()).expect("the copy saves and loads");
    }

    #[test]
    fn mask_ops_validate_and_reorder() {
        let mut r = fixture();
        let (a, b, target) = (r.part.clone(), r.other.clone(), r.composite.clone());
        let c = r.node("c", part_kind());
        let root = r.root.clone();
        let m = &mut r.model;

        m.mask_delete(&target, 0).unwrap();
        assert!(m.mask_add(&a, &a, MaskMode::Mask).is_err());
        assert!(m
            .mask_add(&a, &root, MaskMode::Mask)
            .is_err_and(|e| matches!(e, ModelError::NotAMaskSource)));
        m.mask_add(&a, &target, MaskMode::Mask).unwrap();
        m.mask_delete(&a, 0).unwrap();
        let bend = r.node(
            "bend",
            ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad())),
        );
        let m = &mut r.model;
        assert!(m
            .mask_add(&a, &bend, MaskMode::Mask)
            .is_err_and(|e| matches!(e, ModelError::NotAMaskSource)));

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
        let mut r = fixture();
        // A texture, param and node this model no longer has.
        let orphan_tex = r.model.add_texture(&r.part, texture(), &mut r.hex).unwrap();
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
            r.model.set_welds(vec![ModelWeld::new(
                r.part.clone(),
                orphan_node,
                Vec::new(),
            )]),
            Err(ModelError::UnknownNode)
        ));
        assert!(
            matches!(
                r.model.set_welds(vec![ModelWeld::new(
                    r.part.clone(),
                    r.composite.clone(),
                    Vec::new(),
                )]),
                Err(ModelError::WeldEndNotAPart)
            ),
            "a weld's end has to be a part",
        );
        assert!(
            matches!(
                r.model.set_welds(vec![ModelWeld::new(
                    r.part.clone(),
                    r.other.clone(),
                    vec![pair("ghost", 0.5)],
                )]),
                Err(ModelError::WeldUnknownSlot(_))
            ),
            "a weld's pair has to name a slot the part carries",
        );
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
        assert!(r.model.to_clm_bytes().is_ok());
    }

    #[test]
    fn a_new_mesh_group_translates_its_children() {
        assert!(ModelMeshGroup::new(quad()).translate_children);
    }

    fn response_input(m: &Model, node: &NodeId) -> Option<ParamId> {
        match &m.node(node)?.kind {
            ModelNodeKind::MotionResponse(r) => match &r.inputs.first()?.source {
                crate::ResponseSource::PoseParam { param: p } => Some(p.clone()),
                _ => None,
            },
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
            data: [][..].into(),
        }
    }

    pub(super) fn quad() -> ClmMesh {
        ClmMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0; 8],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
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

    /// Undo-snapshot cost; run with
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

#[cfg(test)]
mod tests_support {
    use super::*;
    use crate::formats::clm::ClmIndices;
    use crate::id::SeededHex;

    pub(super) fn dense_model(nodes: usize, verts: usize, keys: usize) -> (Model, BindingKey) {
        let positions: Vec<f32> = (0..keys).map(|i| i as f32 / (keys - 1) as f32).collect();
        let mut hex = SeededHex::new(11);
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let mut param = |name: &str, model: &mut Model| {
            model
                .add_param(
                    ModelParam {
                        name: Name::truncated(name),
                        min: -1.0,
                        max: 1.0,
                        default: 0.0,
                    },
                    &mut hex,
                )
                .unwrap()
        };
        let (param_x, param_y) = (param("sweep.x", &mut model), param("sweep.y", &mut model));
        let mesh = ClmMesh {
            verts: (0..verts * 2).map(|i| i as f32).collect(),
            uvs: vec![0.0; verts * 2],
            indices: ClmIndices::U16(Vec::new()),
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
            model
                .add_binding_with_positions(&key, vec![positions.clone(), positions.clone()])
                .unwrap();
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
