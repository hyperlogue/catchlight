//! The legacy arena document: the shape catchlight's model file had before
//! `.clm`. It has no byte codec any more — nothing reads or writes it — and
//! `.clm` is the only thing that reaches a disk.
//!
//! **It stays until cl-0ci**, because it is still an *authoring* shape: the
//! `.inx` reader writes one (`importer::inochi2d::to_legacy`) and `cargo
//! xtask`'s fixture generators author one by hand, and both then go through
//! [`Model::from_legacy`](crate::Model::from_legacy) to become a `.clm`.
//! [`Model::to_legacy`](crate::Model::to_legacy) is the way back, which the
//! fixture-drift check and the scalar-param-split tests need. When the
//! importer moves to its own crate and the generators author a `Model`
//! directly, this module and both halves of the bridge go with them.
//!
//! The structure is an **arena**: nodes and params are flat `Vec`s and every
//! cross-reference is an index into one of them — a node's `parent`, a
//! binding's `node`, a mask's `source`, a physics target, a part's `albedo`
//! (into the texture table). There is no id space; the array position *is* the
//! identity, which is exactly what `.clm` replaced. Nodes are in topological
//! order (`parent < self`) with the root at index 0.
//!
//! Params here still carry two axes with their bindings nested underneath;
//! splitting them into scalar params is the bridge's job. It all goes away
//! with the legacy runtime and the importer's move to its own crate.

use super::clm::{
    ClmBindingValues, ClmMesh, ClmPhysics, ClmTransform, TextureAlpha, TextureEncoding,
};
use crate::components::{BlendMode, MaskMode};
use crate::interpolate::InterpolateMode;
use crate::model::ModelWeldPair;
use crate::physics::{PendulumKind, PhysicsParamMapMode};

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: Vec<u8>,
}

/// A whole legacy document: the structure plus the texture table.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyFile {
    pub doc: LegacyDocument,
    pub textures: Vec<LegacyTexture>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyDocument {
    pub physics: ClmPhysics,
    /// Node arena in topological order: index 0 is the root (`parent: None`),
    /// every other node's `parent` is a strictly smaller index.
    pub nodes: Vec<LegacyNode>,
    pub params: Vec<LegacyParam>,
    pub welds: Vec<LegacyWeld>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyNode {
    /// Index of the parent node, or `None` for the single root.
    pub parent: Option<u32>,
    pub name: String,
    pub enabled: bool,
    pub z_order: f32,
    pub transform: ClmTransform,
    pub lock_to_root: bool,
    pub kind: LegacyNodeKind,
}

/// Mirrors the live [`crate::components::NodeKind`]. Conversion maps any
/// unmodeled node kind to `Group` before serialization.
#[derive(Debug, Clone, PartialEq)]
pub enum LegacyNodeKind {
    Group,
    Part(LegacyPart),
    Composite(LegacyComposite),
    MeshGroup(LegacyMeshGroup),
    SimplePhysics(LegacySimplePhysics),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyPart {
    pub mesh: ClmMesh,
    /// Albedo texture index; `u32::MAX` is unmapped (the renderer culls it).
    pub albedo: u32,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub masks: Vec<LegacyMask>,
    pub mask_threshold: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyComposite {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub masks: Vec<LegacyMask>,
    pub mask_threshold: f32,
    pub propagate_meshgroup: bool,
}

/// A mesh group deforms what is beneath it and is never drawn, so it stores no
/// colour. Colour keys written by an older writer decode as unknown fields and
/// are ignored; a *binding* aiming a colour target at a mesh group is rejected
/// at load ([`ClmLoadError::ColorOnMeshGroup`](crate::model::ClmLoadError)).
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyMeshGroup {
    pub mesh: ClmMesh,
    pub dynamic: bool,
    pub translate_children: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacySimplePhysics {
    pub kind: PendulumKind,
    pub map_mode: PhysicsParamMapMode,
    pub local_only: bool,
    /// Driven param index, or `None`.
    pub target_param: Option<u32>,
    /// Authored, unscaled — the global g-scale fold is a build step.
    pub gravity: f32,
    pub length: f32,
    pub frequency: f32,
    pub angle_damping: f32,
    pub length_damping: f32,
    pub output_scale: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegacyMask {
    /// Node index supplying the mask shape.
    pub source: u32,
    pub mode: MaskMode,
}

/// One welded part pair. Canonical and symmetric: each unordered `{a, b}`
/// pair appears at most once, and the runtime solve moves both sides — there
/// is no owner and no mirrored record.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyWeld {
    /// Node indices of the two welded parts.
    pub a: u32,
    pub b: u32,
    pub pairs: Vec<ModelWeldPair>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyParam {
    pub name: String,
    pub is_vec2: bool,
    pub min: [f32; 2],
    pub max: [f32; 2],
    pub defaults: [f32; 2],
    pub axis_points_x: Vec<f32>,
    pub axis_points_y: Vec<f32>,
    pub bindings: Vec<LegacyBinding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyBinding {
    /// Driven node index.
    pub node: u32,
    pub interpolate_mode: InterpolateMode,
    pub values: ClmBindingValues,
}
