//! `.clp` — the editable, source-of-truth catchlight puppet format.
//!
//! A [`super::container`] file (magic `b"NYANPASU"`) with two sections:
//! `Structure` (one CBOR document) and `Textures` (verbatim source-encoded
//! bytes, never decoded or cropped).
//!
//! The structure is an **arena**: nodes and params are flat `Vec`s and every
//! cross-reference is an index into one of them — a node's `parent`, a binding's
//! `node`, a mask's `source`, a physics target, a part's `albedo` (into the
//! texture table). There is no separate id space; the array position *is* the
//! identity. The file is a compacted snapshot (nodes in topological order,
//! `parent < self`), so the positional indices are stable for the file's
//! lifetime — handle-stability under live editing is the in-memory arena's job,
//! not the file's. Stable ids live in memory, never in the file:
//! `catchlight-editor-core`'s `EditModel` assigns indices only at the file edge
//! (`crates/catchlight-editor-core/src/flatten.rs`).
//!
//! The wire mirrors catchlight's *authored* `Puppet` model — no derived caches,
//! no runtime state, nothing the runtime doesn't model (metadata, groups,
//! automation, animations, cameras, emissive/bump, …). CBOR maps keyed by field
//! name give additive evolution: a future field returns as `#[serde(default)]`
//! and old/new readers interoperate. **Never** add `deny_unknown_fields`; a
//! breaking change bumps [`FORMAT_VERSION`], which is **0** today and the only
//! version `decode_structure` accepts. There is no migration path and no code
//! that reads an older one.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::container::{self, ContainerError, Section};
use crate::components::{BlendMode, MaskMode};
use crate::params::InterpolateMode;
use crate::physics::{PhysicsModel, PhysicsParamMapMode};

pub const MAGIC: [u8; 8] = *b"NYANPASU";
/// Larger cumulative `z_order` values render in front.
pub const FORMAT_VERSION: u16 = 0;

const SECTION_STRUCTURE: u32 = 0;
const SECTION_TEXTURES: u32 = 1;

#[derive(Debug, Error)]
pub enum ClpError {
    #[error("container framing: {0}")]
    Container(#[from] ContainerError),
    #[error("CBOR encode: {0}")]
    Encode(String),
    #[error("CBOR decode: {0}")]
    Decode(String),
    #[error("missing {0} section")]
    MissingSection(&'static str),
    #[error("unsupported .clp format_version {0}")]
    UnsupportedVersion(u16),
    #[error(transparent)]
    LoadLimit(#[from] crate::load_budget::LoadLimitError),
}

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// The whole decoded `.clp`: the structure document plus the texture table.
#[derive(Debug, Clone, PartialEq)]
pub struct ClpFile {
    pub version: u16,
    pub doc: ClpDocument,
    pub textures: Vec<ClpTexture>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpDocument {
    #[serde(default)]
    pub physics: ClpPhysics,
    /// Node arena in topological order: index 0 is the root (`parent: None`),
    /// every other node's `parent` is a strictly smaller index.
    pub nodes: Vec<ClpNode>,
    #[serde(default)]
    pub params: Vec<ClpParam>,
    #[serde(default)]
    pub welds: Vec<ClpWeld>,
}

/// Authored global physics; the build folds `pixels_per_meter * gravity` into
/// each SimplePhysics node's gravity, so the authored values stay editable here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClpPhysics {
    pub pixels_per_meter: f32,
    pub gravity: f32,
}

impl Default for ClpPhysics {
    fn default() -> Self {
        Self {
            pixels_per_meter: 1000.0,
            gravity: 9.8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ClpTransform {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpNode {
    /// Index of the parent node, or `None` for the single root.
    #[serde(default)]
    pub parent: Option<u32>,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub z_order: f32,
    #[serde(default)]
    pub transform: ClpTransform,
    #[serde(default)]
    pub lock_to_root: bool,
    pub kind: ClpNodeKind,
}

fn default_true() -> bool {
    true
}

/// Mirrors the live [`crate::components::NodeKind`]. Conversion maps any
/// unmodeled node kind to `Empty` before serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClpNodeKind {
    Empty,
    Part(ClpPart),
    Composite(ClpComposite),
    MeshGroup(ClpMeshGroup),
    SimplePhysics(ClpSimplePhysics),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpPart {
    pub mesh: ClpMesh,
    /// Albedo texture index; `u32::MAX` is unmapped (the renderer culls it).
    pub albedo: u32,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    #[serde(default)]
    pub masks: Vec<ClpMask>,
    pub mask_threshold: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpComposite {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    #[serde(default)]
    pub masks: Vec<ClpMask>,
    pub mask_threshold: f32,
    pub propagate_meshgroup: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpMeshGroup {
    pub mesh: ClpMesh,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub dynamic: bool,
    pub translate_children: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpSimplePhysics {
    pub model: PhysicsModel,
    pub map_mode: PhysicsParamMapMode,
    pub local_only: bool,
    /// Driven param index, or `None`.
    #[serde(default)]
    pub target_param: Option<u32>,
    /// Authored, unscaled — the global g-scale fold is a build step.
    pub gravity: f32,
    pub length: f32,
    pub frequency: f32,
    pub angle_damping: f32,
    pub length_damping: f32,
    pub output_scale: [f32; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClpMask {
    /// Node index supplying the mask shape.
    pub source: u32,
    pub mode: MaskMode,
}

/// One welded Part pair. Canonical and symmetric: each unordered `{a, b}`
/// pair appears at most once, and the runtime solve moves both sides — there
/// is no owner and no mirrored record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpWeld {
    /// Node indices of the two welded Parts.
    pub a: u32,
    pub b: u32,
    #[serde(default)]
    pub pairs: Vec<ClpWeldPair>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClpWeldPair {
    /// Vertex indices into each Part's mesh.
    pub a_vert: u32,
    pub b_vert: u32,
    /// A's share of the meeting point in `[0, 1]`: 1.0 pins A and snaps B
    /// to it, 0.5 meets midway.
    pub weight: f32,
}

/// Mesh vertices and UVs as flat `[x, y, x, y, …]` arrays (not `Vec2` structs):
/// CBOR would otherwise repeat a per-element header. UVs are authored (never
/// crop-rewritten — the build crops textures and rewrites UVs).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClpMesh {
    #[serde(default)]
    pub verts: Vec<f32>,
    #[serde(default)]
    pub uvs: Vec<f32>,
    pub indices: ClpIndices,
    pub origin: [f32; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClpIndices {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

impl Default for ClpIndices {
    fn default() -> Self {
        ClpIndices::U16(Vec::new())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpParam {
    pub name: String,
    pub is_vec2: bool,
    pub min: [f32; 2],
    pub max: [f32; 2],
    pub defaults: [f32; 2],
    #[serde(default)]
    pub axis_points_x: Vec<f32>,
    #[serde(default)]
    pub axis_points_y: Vec<f32>,
    #[serde(default)]
    pub bindings: Vec<ClpBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpBinding {
    /// Driven node index.
    pub node: u32,
    pub interpolate_mode: InterpolateMode,
    pub values: ClpBindingValues,
}

/// Binding targets mirror the live [`crate::params::BindingValues`]. Binding
/// kinds the runtime doesn't fold (emissionStrength, unknown targets) are
/// dropped at import. Each variant carries only the *authored* keypoint cells;
/// unauthored cells are derived, never stored — authored = present, so the
/// editor's set/partial/unset state is the data shape itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClpBindingValues {
    /// Per cell: flat `[x, y, x, y, …]` per-vertex offsets.
    Deform(ClpCells<Vec<f32>>),
    ZOrder(ClpCells<f32>),
    TransformTX(ClpCells<f32>),
    TransformTY(ClpCells<f32>),
    TransformSX(ClpCells<f32>),
    TransformSY(ClpCells<f32>),
    TransformRX(ClpCells<f32>),
    TransformRY(ClpCells<f32>),
    TransformRZ(ClpCells<f32>),
    Opacity(ClpCells<f32>),
    TintR(ClpCells<f32>),
    TintG(ClpCells<f32>),
    TintB(ClpCells<f32>),
    ScreenTintR(ClpCells<f32>),
    ScreenTintG(ClpCells<f32>),
    ScreenTintB(ClpCells<f32>),
    OutputScaleX(ClpCells<f32>),
    OutputScaleY(ClpCells<f32>),
}

/// Sparse authored cells over the param's axis grid. Kept sorted by `(y, x)`
/// so save→save stays byte-stable regardless of authoring order.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClpCells<T> {
    #[serde(default)]
    pub cells: Vec<ClpCell<T>>,
}

/// One authored keypoint: `(x, y)` indexes the param's axis grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClpCell<T> {
    pub x: u32,
    pub y: u32,
    pub value: T,
}

fn cbor_to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, ClpError> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).map_err(|e| ClpError::Encode(e.to_string()))?;
    Ok(out)
}

fn cbor_from_slice<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ClpError> {
    ciborium::from_reader(bytes).map_err(|e| ClpError::Decode(e.to_string()))
}

/// Serialize to `.clp` bytes. Deterministic for a given input: ciborium writes
/// struct fields in declaration order and the container lays sections out in
/// order, so save→save is byte-stable.
pub fn encode(doc: &ClpDocument, textures: &[ClpTexture]) -> Result<Vec<u8>, ClpError> {
    let structure = cbor_to_vec(doc)?;
    let tex = cbor_to_vec(&textures)?;
    let sections = [
        Section {
            kind: SECTION_STRUCTURE,
            data: &structure,
        },
        Section {
            kind: SECTION_TEXTURES,
            data: &tex,
        },
    ];
    Ok(container::write(&MAGIC, FORMAT_VERSION, &sections))
}

pub fn decode(bytes: &[u8]) -> Result<ClpFile, ClpError> {
    decode_with_budget(bytes, &mut crate::load_budget::LoadBudget::default())
}

pub fn decode_with_budget(
    bytes: &[u8],
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<ClpFile, ClpError> {
    budget.charge(
        crate::load_budget::LoadResource::EncodedBytes,
        bytes.len() as u64,
    )?;
    let file = container::read(bytes, &MAGIC)?;
    let structure = file
        .section(SECTION_STRUCTURE)
        .ok_or(ClpError::MissingSection("Structure"))?;
    let doc = decode_structure(file.version, structure)?;
    let textures = match file.section(SECTION_TEXTURES) {
        Some(b) => cbor_from_slice(b)?,
        None => Vec::new(),
    };
    Ok(ClpFile {
        version: file.version,
        doc,
        textures,
    })
}

fn decode_structure(version: u16, bytes: &[u8]) -> Result<ClpDocument, ClpError> {
    if version != FORMAT_VERSION {
        return Err(ClpError::UnsupportedVersion(version));
    }
    cbor_from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value;

    fn sample() -> (ClpDocument, Vec<ClpTexture>) {
        // node 0 = root (Empty); node 1 = Part child of 0.
        let root = ClpNode {
            parent: None,
            name: "Puppet".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClpTransform::default(),
            lock_to_root: false,
            kind: ClpNodeKind::Empty,
        };
        let part = ClpNode {
            parent: Some(0),
            name: "Body".into(),
            enabled: true,
            z_order: 0.5,
            transform: ClpTransform {
                translation: [1.0, 2.0, 0.0],
                rotation: [0.0, 0.0, 0.25],
                scale: [1.0, 1.0],
            },
            lock_to_root: false,
            kind: ClpNodeKind::Part(ClpPart {
                mesh: ClpMesh {
                    verts: vec![-1.0, -1.0, 1.0, -1.0, 0.0, 1.0],
                    uvs: vec![0.0, 1.0, 1.0, 1.0, 0.5, 0.0],
                    indices: ClpIndices::U16(vec![0, 1, 2]),
                    origin: [0.0, 0.0],
                },
                albedo: 0,
                opacity: 1.0,
                blend_mode: BlendMode::Multiply,
                tint: [1.0, 1.0, 1.0],
                screen_tint: [0.0, 0.0, 0.0],
                masks: vec![ClpMask {
                    source: 0,
                    mode: MaskMode::DodgeMask,
                }],
                mask_threshold: 0.5,
            }),
        };
        let param = ClpParam {
            name: "Mouth".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![
                ClpBinding {
                    node: 1,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClpBindingValues::Deform(ClpCells {
                        cells: vec![ClpCell {
                            x: 1,
                            y: 0,
                            value: vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                        }],
                    }),
                },
                ClpBinding {
                    node: 1,
                    interpolate_mode: InterpolateMode::Cubic,
                    values: ClpBindingValues::Opacity(ClpCells {
                        cells: vec![
                            ClpCell {
                                x: 0,
                                y: 0,
                                value: 1.0,
                            },
                            ClpCell {
                                x: 1,
                                y: 0,
                                value: 0.5,
                            },
                        ],
                    }),
                },
            ],
        };
        let doc = ClpDocument {
            physics: ClpPhysics::default(),
            nodes: vec![root, part],
            params: vec![param],
            // Wire-shape coverage only; build-time validation lives in from_clp.
            welds: vec![ClpWeld {
                a: 1,
                b: 1,
                pairs: vec![ClpWeldPair {
                    a_vert: 0,
                    b_vert: 2,
                    weight: 0.5,
                }],
            }],
        };
        let textures = vec![ClpTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::PremultipliedSrgb,
            data: vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4],
        }];
        (doc, textures)
    }

    #[test]
    fn roundtrip_preserves_structure_and_textures() {
        let (doc, textures) = sample();
        let bytes = encode(&doc, &textures).unwrap();
        let file = decode(&bytes).unwrap();
        assert_eq!(file.version, FORMAT_VERSION);
        assert_eq!(file.doc, doc);
        assert_eq!(file.textures, textures);
    }

    #[test]
    fn save_is_byte_stable() {
        let (doc, textures) = sample();
        let a = encode(&doc, &textures).unwrap();
        let b = encode(&doc, &textures).unwrap();
        assert_eq!(
            a, b,
            "encode must be deterministic for editor dirty-checking"
        );
    }

    #[test]
    fn decode_rejects_an_encoded_file_over_the_aggregate_budget() {
        let (doc, textures) = sample();
        let bytes = encode(&doc, &textures).unwrap();
        let mut budget = crate::load_budget::LoadBudget::new(crate::load_budget::LoadLimits {
            encoded_bytes: bytes.len() as u64 - 1,
            ..crate::load_budget::LoadLimits::default()
        });

        let err = decode_with_budget(&bytes, &mut budget).unwrap_err();

        assert!(matches!(
            err,
            ClpError::LoadLimit(crate::load_budget::LoadLimitError {
                resource: "encoded bytes",
                ..
            })
        ));
    }

    #[test]
    fn unknown_keys_are_ignored_forward_compat() {
        let (doc, _) = sample();
        let mut value: Value = cbor_from_slice(&cbor_to_vec(&doc).unwrap()).unwrap();
        if let Value::Map(entries) = &mut value {
            entries.push((Value::Text("future_field".into()), Value::Integer(7.into())));
        } else {
            panic!("expected a CBOR map");
        }
        let back: ClpDocument = cbor_from_slice(&cbor_to_vec(&value).unwrap()).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn missing_optional_keys_default_backward_compat() {
        // A bare node map (kind only) must decode: parent→None (root), the rest
        // to their `#[serde(default)]`s.
        let value = Value::Map(vec![(
            Value::Text("kind".into()),
            Value::Text("Empty".into()),
        )]);
        let node: ClpNode = cbor_from_slice(&cbor_to_vec(&value).unwrap()).unwrap();
        assert_eq!(node.parent, None);
        assert!(node.enabled, "enabled must default to true");
        assert_eq!(node.z_order, 0.0);
        assert_eq!(node.transform, ClpTransform::default());
    }

    #[test]
    fn future_version_is_refused_not_silently_misread() {
        let (doc, textures) = sample();
        let mut bytes = encode(&doc, &textures).unwrap();
        bytes[8] = 0xFF;
        bytes[9] = 0xFF;
        match decode(&bytes) {
            Err(ClpError::UnsupportedVersion(0xFFFF)) => {}
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }
}
