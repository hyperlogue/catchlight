//! The legacy arena document: the shape catchlight's model file had before
//! `.clm`, kept as the inochi2d importer's output and the legacy runtime's
//! input.
//!
//! A [`super::container`] file (magic `b"NYANPASU"`) with two sections:
//! `Structure` (one CBOR document) and `Textures` (verbatim source-encoded
//! bytes, never decoded or cropped).
//!
//! The structure is an **arena**: nodes and params are flat `Vec`s and every
//! cross-reference is an index into one of them — a node's `parent`, a binding's
//! `node`, a mask's `source`, a physics target, a part's `albedo` (into the
//! texture table). There is no separate id space; the array position *is* the
//! identity. The document is a compacted snapshot (nodes in topological order,
//! `parent < self`), so the positional indices are stable for its lifetime.
//! Stable Ids live in [`Model`](crate::Model), which mints one per arena slot
//! at the boundary (`crates/catchlight-core/src/model/flatten.rs`).
//!
//! The wire mirrors catchlight's *authored* model — no derived caches, no
//! runtime state, nothing the runtime doesn't model (metadata, groups,
//! automation, animations, cameras, emissive/bump, …). CBOR maps keyed by field
//! name give additive evolution: a future field returns as `#[serde(default)]`
//! and old/new readers interoperate. **Never** add `deny_unknown_fields`; a
//! breaking change bumps [`FORMAT_VERSION`], which is **0** today and the only
//! version `decode_structure` accepts. There is no migration path and no code
//! that reads an older one.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::clm::{
    ClmBindingValues, ClmMesh, ClmPhysics, ClmTransform, TextureAlpha, TextureEncoding,
};
use super::container::{self, ContainerError, Section};
use crate::components::{BlendMode, MaskMode};
use crate::model::ModelWeldPair;
use crate::params::InterpolateMode;
use crate::physics::{PendulumKind, PhysicsParamMapMode};

pub const MAGIC: [u8; 8] = *b"NYANPASU";
/// Larger cumulative `z_order` values render in front.
pub const FORMAT_VERSION: u16 = 0;

const SECTION_STRUCTURE: u32 = 0;
const SECTION_TEXTURES: u32 = 1;

#[derive(Debug, Error)]
pub enum LegacyError {
    #[error("container framing: {0}")]
    Container(#[from] ContainerError),
    #[error("CBOR encode: {0}")]
    Encode(String),
    #[error("CBOR decode: {0}")]
    Decode(String),
    #[error("missing {0} section")]
    MissingSection(&'static str),
    #[error("unsupported format_version {0}")]
    UnsupportedVersion(u16),
    #[error(transparent)]
    LoadLimit(#[from] crate::load_budget::LoadLimitError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyTexture {
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// A whole legacy document: the structure plus the texture table.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyFile {
    pub version: u16,
    pub doc: LegacyDocument,
    pub textures: Vec<LegacyTexture>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyDocument {
    #[serde(default)]
    pub physics: ClmPhysics,
    /// Node arena in topological order: index 0 is the root (`parent: None`),
    /// every other node's `parent` is a strictly smaller index.
    pub nodes: Vec<LegacyNode>,
    #[serde(default)]
    pub params: Vec<LegacyParam>,
    #[serde(default)]
    pub welds: Vec<LegacyWeld>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyNode {
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
    pub transform: ClmTransform,
    #[serde(default)]
    pub lock_to_root: bool,
    pub kind: LegacyNodeKind,
}

fn default_true() -> bool {
    true
}

/// Mirrors the live [`crate::components::NodeKind`]. Conversion maps any
/// unmodeled node kind to `Group` before serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LegacyNodeKind {
    Group,
    Part(LegacyPart),
    Composite(LegacyComposite),
    MeshGroup(LegacyMeshGroup),
    SimplePhysics(LegacySimplePhysics),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyPart {
    pub mesh: ClmMesh,
    /// Albedo texture index; `u32::MAX` is unmapped (the renderer culls it).
    pub albedo: u32,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    #[serde(default)]
    pub masks: Vec<LegacyMask>,
    pub mask_threshold: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyComposite {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    #[serde(default)]
    pub masks: Vec<LegacyMask>,
    pub mask_threshold: f32,
    pub propagate_meshgroup: bool,
}

/// A mesh group deforms what is beneath it and is never drawn, so it stores no
/// colour. Colour keys written by an older writer decode as unknown fields and
/// are ignored; a *binding* aiming a colour target at a mesh group is rejected
/// at load ([`crate::params::MeshGroupColorBindingError`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyMeshGroup {
    pub mesh: ClmMesh,
    pub dynamic: bool,
    pub translate_children: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacySimplePhysics {
    pub kind: PendulumKind,
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
pub struct LegacyMask {
    /// Node index supplying the mask shape.
    pub source: u32,
    pub mode: MaskMode,
}

/// One welded part pair. Canonical and symmetric: each unordered `{a, b}`
/// pair appears at most once, and the runtime solve moves both sides — there
/// is no owner and no mirrored record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyWeld {
    /// Node indices of the two welded parts.
    pub a: u32,
    pub b: u32,
    #[serde(default)]
    pub pairs: Vec<ModelWeldPair>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyParam {
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
    pub bindings: Vec<LegacyBinding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegacyBinding {
    /// Driven node index.
    pub node: u32,
    pub interpolate_mode: InterpolateMode,
    pub values: ClmBindingValues,
}

fn cbor_to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, LegacyError> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).map_err(|e| LegacyError::Encode(e.to_string()))?;
    Ok(out)
}

fn cbor_from_slice<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, LegacyError> {
    ciborium::from_reader(bytes).map_err(|e| LegacyError::Decode(e.to_string()))
}

/// Serialize to bytes. Deterministic for a given input: ciborium writes struct
/// fields in declaration order and the container lays sections out in order,
/// so save→save is byte-stable.
pub fn encode(doc: &LegacyDocument, textures: &[LegacyTexture]) -> Result<Vec<u8>, LegacyError> {
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

pub fn decode(bytes: &[u8]) -> Result<LegacyFile, LegacyError> {
    decode_with_budget(bytes, &mut crate::load_budget::LoadBudget::default())
}

pub fn decode_with_budget(
    bytes: &[u8],
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<LegacyFile, LegacyError> {
    budget.charge(
        crate::load_budget::LoadResource::EncodedBytes,
        bytes.len() as u64,
    )?;
    let file = container::read(bytes, &MAGIC)?;
    let structure = file
        .section(SECTION_STRUCTURE)
        .ok_or(LegacyError::MissingSection("Structure"))?;
    let doc = decode_structure(file.version, structure)?;
    let textures = match file.section(SECTION_TEXTURES) {
        Some(b) => cbor_from_slice(b)?,
        None => Vec::new(),
    };
    Ok(LegacyFile {
        version: file.version,
        doc,
        textures,
    })
}

fn decode_structure(version: u16, bytes: &[u8]) -> Result<LegacyDocument, LegacyError> {
    if version != FORMAT_VERSION {
        return Err(LegacyError::UnsupportedVersion(version));
    }
    cbor_from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::clm::{ClmCell, ClmCells, ClmIndices};
    use ciborium::value::Value;

    fn sample() -> (LegacyDocument, Vec<LegacyTexture>) {
        // node 0 = root (Group); node 1 = Part child of 0.
        let root = LegacyNode {
            parent: None,
            name: "Puppet".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: LegacyNodeKind::Group,
        };
        let part = LegacyNode {
            parent: Some(0),
            name: "Body".into(),
            enabled: true,
            z_order: 0.5,
            transform: ClmTransform {
                translation: [1.0, 2.0, 0.0],
                rotation: [0.0, 0.0, 0.25],
                scale: [1.0, 1.0],
            },
            lock_to_root: false,
            kind: LegacyNodeKind::Part(LegacyPart {
                mesh: ClmMesh {
                    verts: vec![-1.0, -1.0, 1.0, -1.0, 0.0, 1.0],
                    uvs: vec![0.0, 1.0, 1.0, 1.0, 0.5, 0.0],
                    indices: ClmIndices::U16(vec![0, 1, 2]),
                    origin: [0.0, 0.0],
                },
                albedo: 0,
                opacity: 1.0,
                blend_mode: BlendMode::Multiply,
                tint: [1.0, 1.0, 1.0],
                screen_tint: [0.0, 0.0, 0.0],
                masks: vec![LegacyMask {
                    source: 0,
                    mode: MaskMode::DodgeMask,
                }],
                mask_threshold: 0.5,
            }),
        };
        let param = LegacyParam {
            name: "Mouth".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![
                LegacyBinding {
                    node: 1,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClmBindingValues::Deform(ClmCells {
                        cells: vec![ClmCell {
                            x: 1,
                            y: 0,
                            value: vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                        }],
                    }),
                },
                LegacyBinding {
                    node: 1,
                    interpolate_mode: InterpolateMode::Cubic,
                    values: ClmBindingValues::Opacity(ClmCells {
                        cells: vec![
                            ClmCell {
                                x: 0,
                                y: 0,
                                value: 1.0,
                            },
                            ClmCell {
                                x: 1,
                                y: 0,
                                value: 0.5,
                            },
                        ],
                    }),
                },
            ],
        };
        let doc = LegacyDocument {
            physics: ClmPhysics::default(),
            nodes: vec![root, part],
            params: vec![param],
            // Wire-shape coverage only; build-time validation lives in
            // `from_legacy`.
            welds: vec![LegacyWeld {
                a: 1,
                b: 1,
                pairs: vec![ModelWeldPair {
                    a_vert: 0,
                    b_vert: 2,
                    weight: 0.5,
                }],
            }],
        };
        let textures = vec![LegacyTexture {
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
            LegacyError::LoadLimit(crate::load_budget::LoadLimitError {
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
        let back: LegacyDocument = cbor_from_slice(&cbor_to_vec(&value).unwrap()).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn missing_optional_keys_default_backward_compat() {
        // A bare node map (kind only) must decode: parent→None (root), the rest
        // to their `#[serde(default)]`s.
        let value = Value::Map(vec![(
            Value::Text("kind".into()),
            Value::Text("Group".into()),
        )]);
        let node: LegacyNode = cbor_from_slice(&cbor_to_vec(&value).unwrap()).unwrap();
        assert_eq!(node.parent, None);
        assert!(node.enabled, "enabled must default to true");
        assert_eq!(node.z_order, 0.0);
        assert_eq!(node.transform, ClmTransform::default());
    }

    #[test]
    fn future_version_is_refused_not_silently_misread() {
        let (doc, textures) = sample();
        let mut bytes = encode(&doc, &textures).unwrap();
        bytes[8] = 0xFF;
        bytes[9] = 0xFF;
        match decode(&bytes) {
            Err(LegacyError::UnsupportedVersion(0xFFFF)) => {}
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }
}
