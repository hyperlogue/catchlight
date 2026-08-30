//! `.clm` — the catchlight model format, and the value types a
//! [`Model`](crate::Model) is built out of.
//!
//! This module holds the leaf values that are the same in memory and on the
//! wire — meshes, transforms, authored physics, binding cells — so a Model
//! stores exactly what the file stores and neither side owns a translation of
//! the other. The document types that *frame* them live beside them once the
//! v1 structure lands; until then [`super::legacy`] still frames the file.

use serde::{Deserialize, Serialize};

/// Alpha convention of a texture's stored bytes. The compile/preview step
/// normalizes either representation to premultiplied linear color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextureAlpha {
    PremultipliedSrgb,
    Straight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextureEncoding {
    Png,
    Tga,
}

/// Authored global physics; the build folds `pixels_per_meter * gravity` into
/// each simple physics node's gravity, so the authored values stay editable
/// here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClmPhysics {
    pub pixels_per_meter: f32,
    pub gravity: f32,
}

impl Default for ClmPhysics {
    fn default() -> Self {
        Self {
            pixels_per_meter: 1000.0,
            gravity: 9.8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ClmTransform {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 2],
}

/// Mesh vertices and UVs as flat `[x, y, x, y, …]` arrays (not `Vec2` structs):
/// CBOR would otherwise repeat a per-element header. UVs are authored (never
/// crop-rewritten — the build crops textures and rewrites UVs).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClmMesh {
    #[serde(default)]
    pub verts: Vec<f32>,
    #[serde(default)]
    pub uvs: Vec<f32>,
    pub indices: ClmIndices,
    pub origin: [f32; 2],
}

impl ClmMesh {
    /// How many vertices the mesh has — `verts` is a flat `[x, y, …]`.
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / 2
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClmIndices {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

impl Default for ClmIndices {
    fn default() -> Self {
        ClmIndices::U16(Vec::new())
    }
}

/// Binding targets mirror the live [`crate::params::BindingValues`]. Binding
/// kinds the runtime doesn't fold (emissionStrength, unknown targets) are
/// dropped at import. Each variant carries only the *authored* keypoint cells;
/// unauthored cells are derived, never stored — authored = present, so the
/// editor's set/partial/unset state is the data shape itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClmBindingValues {
    /// Per cell: flat `[x, y, x, y, …]` per-vertex offsets.
    Deform(ClmCells<Vec<f32>>),
    ZOrder(ClmCells<f32>),
    TransformTX(ClmCells<f32>),
    TransformTY(ClmCells<f32>),
    TransformSX(ClmCells<f32>),
    TransformSY(ClmCells<f32>),
    TransformRX(ClmCells<f32>),
    TransformRY(ClmCells<f32>),
    TransformRZ(ClmCells<f32>),
    Opacity(ClmCells<f32>),
    TintR(ClmCells<f32>),
    TintG(ClmCells<f32>),
    TintB(ClmCells<f32>),
    ScreenTintR(ClmCells<f32>),
    ScreenTintG(ClmCells<f32>),
    ScreenTintB(ClmCells<f32>),
    OutputScaleX(ClmCells<f32>),
    OutputScaleY(ClmCells<f32>),
}

/// Sparse authored cells over the binding's key grid. Kept sorted by `(y, x)`
/// so save→save stays byte-stable regardless of authoring order.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClmCells<T> {
    #[serde(default)]
    pub cells: Vec<ClmCell<T>>,
}

/// One authored keypoint: `(x, y)` indexes the binding's key grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmCell<T> {
    pub x: u32,
    pub y: u32,
    pub value: T,
}
