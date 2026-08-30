//! `.clm` — catchlight's model format: what a [`Model`](crate::Model) holds,
//! written down.
//!
//! A [`super::container`] file (magic `b"NYANPASU"`) with two sections:
//! `Structure` (one CBOR document) and `Textures` (verbatim source-encoded
//! bytes, never decoded or cropped).
//!
//! Invariants this module and its reader ([`Model::from_clm_bytes`]) enforce:
//!
//! - **Everything is keyed by Id.** A node's `parent`, a mask's `source`, a
//!   binding's `node` and `params`, a physics node's `target_params`, a part's
//!   `albedo`, a weld's ends: all Ids, never positions. Nodes are still
//!   written in topological order — a node's parent appears before it, and
//!   exactly one node has none — so a streaming reader can build the tree in
//!   one pass, but nothing addresses a node by where it sits. Sibling order in
//!   the file *is* the model's sibling order, which is draw order at equal z.
//! - **Byte-stability.** The same Model always writes the same bytes: node,
//!   param, texture and binding order all come from the Model's own orders,
//!   ciborium writes struct fields in declaration order, and authored cells
//!   are kept sorted by `(y, x)`. Save→save is a no-op, which is what the
//!   editor's dirty check rests on.
//! - **Params are scalar.** A binding names one or two of them and its grid is
//!   the product of their key positions; a physics node writes two. Nothing
//!   here carries a second axis.
//! - **No vertex index outside a seam.** A part carries named seams, each a
//!   set of slots filled by one of its vertices; a weld names two seams and
//!   weights their shared slots. A slot may also be *unfilled*
//!   (`vertex: null`) — what re-authoring a part's mesh leaves behind — and a
//!   weld over an unfilled slot loads, resolving to one pair fewer. A
//!   [`Model`](crate::Model) holds the same shape, so this section is a copy
//!   in both directions.
//! - **A malformed file is an error, never a panic.** Every Id that names
//!   something must find it, seam slots must be in mesh range, a weld's two
//!   seams must hold the same slots, and a binding must name one or two
//!   distinct params. The reader reports which Id in which field failed.
//! - **A binding has to be one the runtime can fold.** A colour target
//!   (`Opacity`, `Tint*`, `ScreenTint*`) on a mesh group has nowhere to land —
//!   a mesh group is never drawn — and a deform cell holds one `[dx, dy]` per
//!   mesh vertex, no more and no fewer. Both are refused on the way in, so a
//!   model that loads is one every runtime evaluates the same way; the second
//!   is refused on the way *out* too, so a bad refit is reported at save
//!   rather than at the next open.
//!
//! This module also holds the leaf value types that are the same in memory and
//! on the wire — meshes, transforms, authored physics, binding cells — so a
//! Model stores exactly what the file stores and neither side owns a
//! translation of the other.
//!
//! CBOR maps keyed by field name give additive evolution: a future field
//! returns as `#[serde(default)]` and old/new readers interoperate. **Never**
//! add `deny_unknown_fields`; a breaking change bumps [`FORMAT_VERSION`].

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::container::{self, ContainerError, Section};
use crate::components::{BlendMode, MaskMode};
use crate::id::{NodeId, ParamId, SeamId, SlotId, TexId};
use crate::params::InterpolateMode;
use crate::physics::{PendulumKind, PhysicsParamMapMode};

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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClmTransform {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 2],
}

/// The **identity** transform, not the all-zero one. `ClmNode::transform` is
/// `#[serde(default)]` and the `.inx` reader falls back here for a node with
/// no transform block, so a derived `Default` would give those nodes a zero
/// scale — a singular matrix that collapses the node and everything under it,
/// rather than the "nothing was said, so nothing moves" the absent key means.
impl Default for ClmTransform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0, 1.0],
        }
    }
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

/// A named, timed sequence of param values: a length in frames, a lead-in
/// played once, and a body that repeats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmAnimation {
    #[serde(default)]
    pub name: String,
    /// Seconds per frame.
    pub timestep: f32,
    /// Length in frames.
    pub length: i32,
    /// Frame where the lead-in ends / looping restarts. -1 (or any value
    /// outside `(0, length - 1)`) means the loop region starts at 0.
    pub lead_in: i32,
    /// Frame where the lead-out starts / looping wraps. -1 (or any value
    /// outside `(0, length - 1)`) means the loop region ends at `length`.
    pub lead_out: i32,
    #[serde(default)]
    pub lanes: Vec<ClmLane>,
}

impl Default for ClmAnimation {
    fn default() -> Self {
        Self {
            name: String::new(),
            timestep: 1.0 / 60.0,
            length: 0,
            lead_in: -1,
            lead_out: -1,
            lanes: Vec::new(),
        }
    }
}

/// One animation's track over a single param. Params are scalar, so a lane
/// names one and carries no axis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmLane {
    pub param: ParamId,
    pub interpolation: InterpolateMode,
    #[serde(default)]
    pub keyframes: Vec<ClmKeyframe>,
}

/// A value the author set on a lane at one frame. `frame` is an integer frame
/// index rather than a time; the animation's `timestep` converts it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClmKeyframe {
    pub frame: i32,
    pub value: f32,
}

// ---- the file ------------------------------------------------------------

pub const MAGIC: [u8; 8] = *b"NYANPASU";
/// Bumped for every breaking wire change. **1** is the only version
/// `decode_structure` accepts; there is no migration path and no reader for
/// the pre-public v0 arena (see [`super::legacy`], which is now memory-only).
pub const FORMAT_VERSION: u16 = 1;

const SECTION_STRUCTURE: u32 = 0;
const SECTION_TEXTURES: u32 = 1;

#[derive(Debug, Error)]
pub enum ClmError {
    #[error("container framing: {0}")]
    Container(#[from] ContainerError),
    #[error("CBOR encode: {0}")]
    Encode(String),
    #[error("CBOR decode: {0}")]
    Decode(String),
    #[error("missing {0} section")]
    MissingSection(&'static str),
    #[error("unsupported .clm format_version {0}")]
    UnsupportedVersion(u16),
    #[error(transparent)]
    LoadLimit(#[from] crate::load_budget::LoadLimitError),
}

/// The whole decoded `.clm`: the structure document plus the texture table.
#[derive(Debug, Clone, PartialEq)]
pub struct ClmFile {
    pub doc: ClmDocument,
    pub textures: Vec<ClmTexture>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClmDocument {
    #[serde(default)]
    pub physics: ClmPhysics,
    /// Every node, in topological order: a node's `parent` names one that
    /// appeared before it, and exactly one node has no parent. Order is a
    /// reading and streaming convenience — nothing addresses a node by
    /// position.
    #[serde(default)]
    pub nodes: Vec<ClmNode>,
    #[serde(default)]
    pub params: Vec<ClmParam>,
    #[serde(default)]
    pub bindings: Vec<ClmBinding>,
    #[serde(default)]
    pub welds: Vec<ClmWeld>,
    #[serde(default)]
    pub animations: Vec<ClmAnimation>,
}

/// A texture as the author supplied it: never decoded, never re-encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmTexture {
    pub id: TexId,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmNode {
    pub id: NodeId,
    /// The parent's Id, or `None` for the single root.
    #[serde(default)]
    pub parent: Option<NodeId>,
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
    pub kind: ClmNodeKind,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClmNodeKind {
    Group,
    Part(ClmPart),
    Composite(ClmComposite),
    MeshGroup(ClmMeshGroup),
    SimplePhysics(ClmSimplePhysics),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmPart {
    pub mesh: ClmMesh,
    /// The albedo texture's Id, or `None` for an unmapped part (the renderer
    /// culls it).
    #[serde(default)]
    pub albedo: Option<TexId>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    #[serde(default)]
    pub masks: Vec<ClmMask>,
    pub mask_threshold: f32,
    /// The named vertex sets welds pair, filled by whoever owns the part.
    #[serde(default)]
    pub seams: Vec<ClmSeam>,
}

/// A named set of slots on a part, each filled by one of the part's vertices,
/// so that a weld can name a vertex without naming an index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmSeam {
    pub id: SeamId,
    #[serde(default)]
    pub slots: Vec<ClmSlot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmSlot {
    pub id: SlotId,
    /// Index into the owning part's mesh vertices, or `None` for an unfilled
    /// slot — a slot whose part has been re-meshed since it was filled. A
    /// filled slot writes the bare index, so this costs nothing on the wire.
    #[serde(default)]
    pub vertex: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmComposite {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    #[serde(default)]
    pub masks: Vec<ClmMask>,
    pub mask_threshold: f32,
    pub propagate_meshgroup: bool,
}

/// A mesh group deforms what is beneath it and is never drawn, so it stores no
/// colour. A *binding* aiming a colour target at one is refused on the way in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmMeshGroup {
    pub mesh: ClmMesh,
    pub dynamic: bool,
    pub translate_children: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmSimplePhysics {
    pub kind: PendulumKind,
    pub map_mode: PhysicsParamMapMode,
    pub local_only: bool,
    /// The two params the pendulum's swing is written into, in the order the
    /// map mode produces them. Either may be unset.
    #[serde(default)]
    pub target_params: [Option<ParamId>; 2],
    /// Authored, unscaled — the global g-scale fold is a build step.
    pub gravity: f32,
    pub length: f32,
    pub frequency: f32,
    pub angle_damping: f32,
    pub length_damping: f32,
    pub output_scale: [f32; 2],
}

/// A drawable's clipping rule: whose shape clips it, and whether what that
/// shape covers is kept or cut away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmMask {
    pub source: NodeId,
    pub mode: MaskMode,
}

/// A named scalar the author exposes for posing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmParam {
    pub id: ParamId,
    #[serde(default)]
    pub name: String,
    pub min: f32,
    pub max: f32,
    /// Rest value, in param-value space (unlike the key positions).
    pub default: f32,
    /// The values along the param at which a binding may hold authored cells,
    /// normalized 0..1 across `[min, max]`.
    #[serde(default)]
    pub key_positions: Vec<f32>,
}

/// One or two params' control over one property of one node. `values` names
/// the property and carries its authored cells; the grid those cells index is
/// the product of `params`' key positions, `x` along the first param.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmBinding {
    /// One or two distinct params. Any other count is a load error.
    pub params: Vec<ParamId>,
    pub node: NodeId,
    pub interpolate_mode: InterpolateMode,
    pub values: ClmBindingValues,
}

/// Two parts' seams, paired slot by slot. Symmetric: each unordered pair of
/// ends appears at most once and the runtime solve moves both sides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmWeld {
    pub a: ClmWeldEnd,
    pub b: ClmWeldEnd,
    /// One entry per slot the two seams share; both seams must hold exactly
    /// these slots.
    #[serde(default)]
    pub weights: Vec<ClmSlotWeight>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmWeldEnd {
    pub node: NodeId,
    pub seam: SeamId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmSlotWeight {
    pub slot: SlotId,
    /// A's share of the meeting point in `[0, 1]`: 1.0 pins A and snaps B
    /// to it, 0.5 meets midway.
    pub weight: f32,
}

fn cbor_to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, ClmError> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).map_err(|e| ClmError::Encode(e.to_string()))?;
    Ok(out)
}

fn cbor_from_slice<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ClmError> {
    ciborium::from_reader(bytes).map_err(|e| ClmError::Decode(e.to_string()))
}

/// Serialize to `.clm` bytes. Deterministic for a given input: ciborium writes
/// struct fields in declaration order and the container lays sections out in
/// order, so save→save is byte-stable.
pub fn encode(doc: &ClmDocument, textures: &[ClmTexture]) -> Result<Vec<u8>, ClmError> {
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

pub fn decode(bytes: &[u8]) -> Result<ClmFile, ClmError> {
    decode_with_budget(bytes, &mut crate::load_budget::LoadBudget::default())
}

pub fn decode_with_budget(
    bytes: &[u8],
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<ClmFile, ClmError> {
    budget.charge(
        crate::load_budget::LoadResource::EncodedBytes,
        bytes.len() as u64,
    )?;
    let file = container::read(bytes, &MAGIC)?;
    let structure = file
        .section(SECTION_STRUCTURE)
        .ok_or(ClmError::MissingSection("Structure"))?;
    let doc = decode_structure(file.version, structure)?;
    let textures = match file.section(SECTION_TEXTURES) {
        Some(b) => cbor_from_slice(b)?,
        None => Vec::new(),
    };
    Ok(ClmFile { doc, textures })
}

fn decode_structure(version: u16, bytes: &[u8]) -> Result<ClmDocument, ClmError> {
    if version != FORMAT_VERSION {
        return Err(ClmError::UnsupportedVersion(version));
    }
    cbor_from_slice(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value;

    fn sample() -> (ClmDocument, Vec<ClmTexture>) {
        let root = ClmNode {
            id: NodeId::new("root").unwrap(),
            parent: None,
            name: "Puppet".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: ClmNodeKind::Group,
        };
        let body = NodeId::new("root/part-body").unwrap();
        let part = ClmNode {
            id: body.clone(),
            parent: Some(root.id.clone()),
            name: "Body".into(),
            enabled: true,
            z_order: 0.5,
            transform: ClmTransform {
                translation: [1.0, 2.0, 0.0],
                rotation: [0.0, 0.0, 0.25],
                scale: [1.0, 1.0],
            },
            lock_to_root: false,
            kind: ClmNodeKind::Part(ClmPart {
                mesh: ClmMesh {
                    verts: vec![-1.0, -1.0, 1.0, -1.0, 0.0, 1.0],
                    uvs: vec![0.0, 1.0, 1.0, 1.0, 0.5, 0.0],
                    indices: ClmIndices::U16(vec![0, 1, 2]),
                    origin: [0.0, 0.0],
                },
                albedo: Some(TexId::new("tex-skin").unwrap()),
                opacity: 1.0,
                blend_mode: BlendMode::Multiply,
                tint: [1.0, 1.0, 1.0],
                screen_tint: [0.0, 0.0, 0.0],
                masks: vec![ClmMask {
                    source: root.id.clone(),
                    mode: MaskMode::DodgeMask,
                }],
                mask_threshold: 0.5,
                seams: vec![ClmSeam {
                    id: SeamId::new("collar").unwrap(),
                    slots: vec![
                        ClmSlot {
                            id: SlotId::new("s0").unwrap(),
                            vertex: Some(2),
                        },
                        ClmSlot {
                            id: SlotId::new("s1").unwrap(),
                            vertex: None,
                        },
                    ],
                }],
            }),
        };
        let mouth = ParamId::new("param-mouth").unwrap();
        let doc = ClmDocument {
            physics: ClmPhysics::default(),
            nodes: vec![root, part],
            params: vec![ClmParam {
                id: mouth.clone(),
                name: "Mouth".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                key_positions: vec![0.0, 1.0],
            }],
            bindings: vec![ClmBinding {
                params: vec![mouth.clone()],
                node: body,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(ClmCells {
                    cells: vec![ClmCell {
                        x: 1,
                        y: 0,
                        value: vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    }],
                }),
            }],
            // Wire-shape coverage only; what a weld has to satisfy is checked
            // where a Model is built from it (`model/file.rs`).
            welds: Vec::new(),
            animations: vec![ClmAnimation {
                name: "blink".into(),
                length: 6,
                lanes: vec![ClmLane {
                    param: mouth,
                    interpolation: InterpolateMode::Stepped,
                    keyframes: vec![ClmKeyframe {
                        frame: 2,
                        value: 1.0,
                    }],
                }],
                ..ClmAnimation::default()
            }],
        };
        let textures = vec![ClmTexture {
            id: TexId::new("tex-skin").unwrap(),
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
            ClmError::LoadLimit(crate::load_budget::LoadLimitError {
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
        let back: ClmDocument = cbor_from_slice(&cbor_to_vec(&value).unwrap()).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn missing_optional_keys_default_backward_compat() {
        // A bare node map (id and kind only) must decode: parent→None (root),
        // the rest to their `#[serde(default)]`s.
        let value = Value::Map(vec![
            (Value::Text("id".into()), Value::Text("root".into())),
            (Value::Text("kind".into()), Value::Text("Group".into())),
        ]);
        let node: ClmNode = cbor_from_slice(&cbor_to_vec(&value).unwrap()).unwrap();
        assert_eq!(node.id.as_str(), "root");
        assert_eq!(node.parent, None);
        assert!(node.enabled, "enabled must default to true");
        assert_eq!(node.z_order, 0.0);
        assert_eq!(node.transform, ClmTransform::default());
    }

    /// A document with no `animations`, `bindings` or `welds` key at all is a
    /// legal model with none of them.
    #[test]
    fn an_empty_document_decodes() {
        let doc: ClmDocument = cbor_from_slice(&cbor_to_vec(&Value::Map(Vec::new())).unwrap())
            .expect("an empty map is a legal document");
        assert_eq!(doc, ClmDocument::default());
    }

    /// v0 is the pre-public arena format and nothing reads it any more, so a
    /// file carrying it has to be refused by version rather than misread as v1
    /// — the two share a magic.
    #[test]
    fn an_older_or_newer_version_is_refused_not_silently_misread() {
        let (doc, textures) = sample();
        let bytes = encode(&doc, &textures).unwrap();
        for version in [0u16, 2, 0xFFFF] {
            let mut bytes = bytes.clone();
            bytes[8..10].copy_from_slice(&version.to_le_bytes());
            match decode(&bytes) {
                Err(ClmError::UnsupportedVersion(v)) => assert_eq!(v, version),
                other => panic!("expected UnsupportedVersion({version}), got {other:?}"),
            }
        }
    }

    /// An Id is validated on the way in from a file, not just on the way in
    /// from a caller.
    #[test]
    fn an_id_off_the_charset_is_a_decode_error() {
        let value = Value::Map(vec![
            (Value::Text("id".into()), Value::Text("has space".into())),
            (Value::Text("kind".into()), Value::Text("Group".into())),
        ]);
        let bytes = cbor_to_vec(&value).unwrap();

        assert!(matches!(
            cbor_from_slice::<ClmNode>(&bytes),
            Err(ClmError::Decode(_))
        ));
    }
}
