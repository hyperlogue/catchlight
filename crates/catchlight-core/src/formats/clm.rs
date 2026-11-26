//! `.clm`: a [`super::container`] with a `Structure` (CBOR) section and a
//! `Textures` section of verbatim source-encoded bytes, plus an optional
//! `Extensions` section. A structure-only container carries `Structure` beside
//! a `TextureManifest` instead; each reader refuses the other shape.
//!
//! The same Model always writes the same bytes: the editor's dirty check rests
//! on save→save being a no-op. Never add `deny_unknown_fields`; see
//! [`crate::formats`] for how the format may change.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::container::{self, ContainerError};
use crate::components::{BlendMode, MaskMode};
use crate::extension::IdRef;
use crate::id::{ExtensionKey, NodeId, ParamId, SlotId, TexId};
use crate::interpolate::InterpolateMode;

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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClmTransform {
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 2],
}

/// Identity, not zero: an absent `transform` key must not collapse the node
/// to a singular matrix.
impl Default for ClmTransform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0, 1.0],
        }
    }
}

/// Flat `[x, y, …]` arrays, so CBOR does not repeat a per-element header.
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
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / 2
    }

    pub fn triangle_count(&self) -> usize {
        match &self.indices {
            ClmIndices::U16(v) => v.len() / 3,
            ClmIndices::U32(v) => v.len() / 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClmIndices {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

impl Default for ClmIndices {
    fn default() -> Self {
        Self::U16(Vec::new())
    }
}

/// Holds only authored cells; unauthored cells are derived, never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClmBindingValues {
    /// Per cell: flat `[x, y, x, y, …]` per-vertex offsets.
    Deform(ClmCells<Vec<f32>>),
    ZOrder(ClmCells<f32>),
    TransformTX(ClmCells<f32>),
    TransformTY(ClmCells<f32>),
    TransformTZ(ClmCells<f32>),
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
}

/// Kept sorted by `(y, x)` so save→save stays byte-stable.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClmCells<T> {
    #[serde(default)]
    pub cells: Vec<ClmCell<T>>,
}

/// One authored keypoint: `(x, y)` indexes the binding's key grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmCell<T> {
    pub x: u32,
    pub y: u32,
    pub value: T,
}

pub const MAGIC: [u8; 8] = *b"NYANPASU";
pub const FORMAT_VERSION: u16 = 5;

const SECTION_STRUCTURE: u32 = 0;
const SECTION_TEXTURES: u32 = 1;
/// Its presence marks a structure-only container.
const SECTION_TEXTURE_MANIFEST: u32 = 2;
const SECTION_EXTENSIONS: u32 = 3;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClmError {
    #[error("container framing: {0}")]
    Container(#[from] ContainerError),
    #[error("CBOR encode: {0}")]
    Encode(#[source] ciborium::ser::Error<std::io::Error>),
    #[error("extension {key:?} names bytes the file does not carry")]
    ExtensionBytesMissing { key: String },
    #[error("the file carries bytes for extension {key:?}, which its structure does not name")]
    ExtensionBytesOrphan { key: String },
    #[error("the file carries the bytes of extension {key:?} twice")]
    ExtensionBytesDuplicate { key: String },
    #[error("extension {key:?} carries {actual} bytes but its marker says {expected}")]
    ExtensionSizeMismatch {
        key: String,
        expected: u64,
        actual: u64,
    },
    #[error("extension {key:?}'s bytes do not hash to what its marker says")]
    ExtensionHashMismatch { key: String },
    #[error("CBOR decode: {0}")]
    Decode(#[source] ciborium::de::Error<std::io::Error>),
    #[error("missing {0} section")]
    MissingSection(&'static str),
    #[error("unsupported .clm format_version {0}")]
    UnsupportedVersion(u16),
    #[error(
        "these bytes carry a texture manifest, so they are a structure-only container rather \
         than a complete .clm; read them with decode_structure"
    )]
    StructureOnly,
    #[error(
        "these bytes carry a texture table, so they are a complete .clm rather than a \
         structure-only container; read them with decode"
    )]
    NotAStructure,
    #[error(transparent)]
    LoadLimit(#[from] crate::load_budget::LoadLimitError),
    #[error(
        "section kind {0} is one this format defines, so it cannot be carried as a section \
         the format does not recognise"
    )]
    ReservedSectionKind(u32),
    #[error("section kind {0} appears twice; this format defines it once per file")]
    DuplicateSection(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClmFile {
    pub doc: ClmStructure,
    pub textures: Vec<ClmTexture>,
    /// Payloads behind the byte markers in `doc.extensions`, in key order.
    pub extensions: Vec<ClmExtensionBlob>,
    pub unknown_sections: Vec<ClmUnknownSection>,
}

impl ClmFile {
    /// Unlike [`encode`], also writes the unrecognised sections.
    pub fn encode(&self) -> Result<Vec<u8>, ClmError> {
        let payloads = self.textures.iter().map(|t| t.data.len() as u64).sum();
        encode_sections(
            &self.doc,
            &self.textures,
            payloads,
            &self.extensions,
            &self.unknown_sections,
        )
    }
}

/// A section of unrecognised kind, written back byte for byte after the known
/// sections, so an older tool does not destroy a newer writer's data.
/// Structure-only containers neither read nor write them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClmUnknownSection {
    pub kind: u32,
    pub data: std::sync::Arc<[u8]>,
}

/// The manifest supplies texture order and encoding, which `Structure` lacks.
#[derive(Debug, Clone, PartialEq)]
pub struct ClmStructureFile {
    pub doc: ClmStructure,
    pub textures: Vec<ClmTextureRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmTextureRef {
    pub id: TexId,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
}

impl From<&ClmTexture> for ClmTextureRef {
    fn from(tex: &ClmTexture) -> Self {
        Self {
            id: tex.id.clone(),
            encoding: tex.encoding,
            alpha: tex.alpha,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClmStructure {
    /// Topological, with one root. Sibling order is draw order at equal z.
    #[serde(default)]
    pub nodes: Vec<ClmNode>,
    #[serde(default)]
    pub params: Vec<ClmParam>,
    #[serde(default)]
    pub bindings: Vec<ClmBinding>,
    #[serde(default)]
    pub welds: Vec<ClmWeld>,
    /// Skipped when empty, so files without extensions keep their bytes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<ExtensionKey, ClmExtension>,
}

/// Externally tagged: a marker is itself a JSON object, so an untagged value
/// would be ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClmExtension {
    /// Non-finite floats are refused rather than rewritten as `null`.
    Json(#[serde(deserialize_with = "json_value")] serde_json::Value),
    Bytes(ClmExtensionMarker),
    /// Every reader keeps `refs` current; only the extension's module reads
    /// `payload`, which names Ids by their index into `refs`.
    Maintained {
        version: u32,
        refs: Vec<IdRef>,
        payload: ClmPayload,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClmPayload {
    Json(#[serde(deserialize_with = "json_value")] serde_json::Value),
    /// The bytes live in the `Extensions` section under the extension's key.
    Bytes(ClmExtensionMarker),
}

impl ClmExtension {
    /// The marker of the bytes this value keeps in the `Extensions` section.
    pub fn marker(&self) -> Option<&ClmExtensionMarker> {
        match self {
            Self::Bytes(marker)
            | Self::Maintained {
                payload: ClmPayload::Bytes(marker),
                ..
            } => Some(marker),
            Self::Json(_)
            | Self::Maintained {
                payload: ClmPayload::Json(_),
                ..
            } => None,
        }
    }

    /// The inline JSON this value carries in the structure.
    pub fn inline_json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(json)
            | Self::Maintained {
                payload: ClmPayload::Json(json),
                ..
            } => Some(json),
            Self::Bytes(_)
            | Self::Maintained {
                payload: ClmPayload::Bytes(_),
                ..
            } => None,
        }
    }
}

/// Unlike `serde_json::Value`, refuses NaN and infinity instead of reading
/// them as `null`, so a value round-trips unchanged.
fn json_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<serde_json::Value, D::Error> {
    deserializer.deserialize_any(JsonValue)
}

struct JsonValue;

impl<'de> serde::de::DeserializeSeed<'de> for JsonValue {
    type Value = serde_json::Value;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for JsonValue {
    type Value = serde_json::Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON-compatible value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(v.into()))
    }

    fn visit_i128<E: serde::de::Error>(self, v: i128) -> Result<Self::Value, E> {
        i64::try_from(v)
            .map(|v| serde_json::Value::Number(v.into()))
            .map_err(|_| E::custom("an integer outside the 64-bit range is not a JSON number"))
    }

    fn visit_u128<E: serde::de::Error>(self, v: u128) -> Result<Self::Value, E> {
        u64::try_from(v)
            .map(|v| serde_json::Value::Number(v.into()))
            .map_err(|_| E::custom("an integer outside the 64-bit range is not a JSON number"))
    }

    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(v)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom(format_args!("{v} is not a JSON number")))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(v))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Self)? {
            items.push(item);
        }
        Ok(serde_json::Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entries = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            entries.insert(key, map.next_value_seed(Self)?);
        }
        Ok(serde_json::Value::Object(entries))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmExtensionMarker {
    pub size: u64,
    /// Lowercase hex of [`extension_hash`] over the bytes.
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmExtensionBlob {
    pub key: ExtensionKey,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Length as compact JSON text, which any authoring tool can compute.
pub fn extension_json_len(value: &serde_json::Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // A `Value` always serializes and this writer never fails; were either
    // to stop being true, an unmeasurable value is one over every cap.
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        Err(_) => usize::MAX,
    }
}

pub fn extension_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// A texture as the author supplied it: never decoded, never re-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmTexture {
    pub id: TexId,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Serializes identically to [`ClmTexture`] without copying the payload.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ClmTextureBytes<'a> {
    pub id: &'a TexId,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    #[serde(with = "serde_bytes")]
    pub data: &'a [u8],
}

impl<'a> From<&'a ClmTexture> for ClmTextureBytes<'a> {
    fn from(tex: &'a ClmTexture) -> Self {
        Self {
            id: &tex.id,
            encoding: tex.encoding,
            alpha: tex.alpha,
            data: &tex.data,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClmTextureSpan<'a> {
    pub id: TexId,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: std::borrow::Cow<'a, [u8]>,
}

#[derive(Debug)]
pub(crate) struct ClmFileRef<'a> {
    pub doc: ClmStructure,
    pub textures: Vec<ClmTextureSpan<'a>>,
    pub extensions: Vec<ClmExtensionBlob>,
    pub unknown_sections: Vec<ClmUnknownSection>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmNode {
    pub id: NodeId,
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
    MotionResponse(crate::MotionResponse),
    Spine(ClmSpine),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmPart {
    pub mesh: ClmMesh,
    #[serde(default)]
    pub albedo: Option<TexId>,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    #[serde(default)]
    pub masks: Vec<ClmMask>,
    pub mask_threshold: f32,
    #[serde(default)]
    pub slots: Vec<ClmSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmSlot {
    pub id: SlotId,
    /// `None` for a slot whose part was re-meshed since it was filled.
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

/// Never drawn, so it stores no colour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmMeshGroup {
    pub mesh: ClmMesh,
    pub translate_children: bool,
}

/// Stores only the authored polyline; the per-vertex assignment is derived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmSpine {
    /// Far end of each link, in the node's space. Non-empty, finite, and no
    /// two consecutive points equal.
    pub joints: Vec<[f32; 2]>,
    /// Per link; `None` is rigid. Absent fills to `joints.len()`.
    #[serde(default)]
    pub targets: Vec<Option<ParamId>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmMask {
    pub source: NodeId,
    pub mode: MaskMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmParam {
    pub id: ParamId,
    #[serde(default)]
    pub name: String,
    pub min: f32,
    pub max: f32,
    /// Rest value, in param-value space.
    pub default: f32,
}

/// The grid is the product of `key_positions`, `x` along the first param.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmBinding {
    /// One or two distinct params. Any other count is a load error.
    pub params: Vec<ParamId>,
    /// One nonempty, strictly increasing normalized axis per driving param.
    pub key_positions: Vec<Vec<f32>>,
    pub node: NodeId,
    pub interpolate_mode: InterpolateMode,
    pub values: ClmBindingValues,
}

/// Symmetric: each unordered pair of parts appears at most once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmWeld {
    pub a: NodeId,
    pub b: NodeId,
    /// One entry per pair of slots joined; each names a slot the part on its
    /// side carries, and no slot twice.
    #[serde(default)]
    pub pairs: Vec<ClmSlotPair>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmSlotPair {
    pub a: SlotId,
    pub b: SlotId,
    /// A's share of the meeting point in `[0, 1]`: 1.0 pins A and snaps B
    /// to it, 0.5 meets midway.
    pub weight: f32,
}

fn cbor_into<T: Serialize>(
    value: &T,
    out: &mut container::SectionWriter<'_>,
) -> Result<(), ClmError> {
    ciborium::into_writer(value, out).map_err(ClmError::Encode)
}

fn cbor_from_slice<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ClmError> {
    ciborium::from_reader(bytes).map_err(ClmError::Decode)
}

/// Checked on read and on write, so a writer never leaves a marker without
/// its bytes.
pub fn check_extensions(doc: &ClmStructure, blobs: &[ClmExtensionBlob]) -> Result<(), ClmError> {
    let mut carried: BTreeMap<&ExtensionKey, &ClmExtensionBlob> = BTreeMap::new();
    for blob in blobs {
        if carried.insert(&blob.key, blob).is_some() {
            return Err(ClmError::ExtensionBytesDuplicate {
                key: blob.key.to_string(),
            });
        }
    }

    for (key, value) in &doc.extensions {
        let Some(marker) = value.marker() else {
            continue;
        };
        let blob = carried
            .remove(key)
            .ok_or_else(|| ClmError::ExtensionBytesMissing {
                key: key.to_string(),
            })?;
        if blob.data.len() as u64 != marker.size {
            return Err(ClmError::ExtensionSizeMismatch {
                key: key.to_string(),
                expected: marker.size,
                actual: blob.data.len() as u64,
            });
        }
        if extension_hash(&blob.data) != marker.hash {
            return Err(ClmError::ExtensionHashMismatch {
                key: key.to_string(),
            });
        }
    }

    // Whatever the markers did not claim is a payload nothing names, whether
    // its key is absent from the structure or filed there as a JSON value.
    if let Some((key, _)) = carried.into_iter().next() {
        return Err(ClmError::ExtensionBytesOrphan {
            key: key.to_string(),
        });
    }
    Ok(())
}

/// Writes only known sections; [`ClmFile::encode`] also carries unrecognised
/// ones.
pub fn encode(
    doc: &ClmStructure,
    textures: &[ClmTexture],
    extensions: &[ClmExtensionBlob],
) -> Result<Vec<u8>, ClmError> {
    let payloads = textures.iter().map(|t| t.data.len() as u64).sum();
    encode_sections(doc, textures, payloads, extensions, &[])
}

pub fn encode_borrowed(
    doc: &ClmStructure,
    textures: &[ClmTextureBytes<'_>],
    extensions: &[ClmExtensionBlob],
    unknown_sections: &[ClmUnknownSection],
) -> Result<Vec<u8>, ClmError> {
    let payloads = textures.iter().map(|t| t.data.len() as u64).sum();
    encode_sections(doc, textures, payloads, extensions, unknown_sections)
}

fn is_known_section(kind: u32) -> bool {
    matches!(
        kind,
        SECTION_STRUCTURE | SECTION_TEXTURES | SECTION_TEXTURE_MANIFEST | SECTION_EXTENSIONS
    )
}

fn encode_sections<T: Serialize>(
    doc: &ClmStructure,
    textures: &[T],
    payloads: u64,
    extensions: &[ClmExtensionBlob],
    unknown_sections: &[ClmUnknownSection],
) -> Result<Vec<u8>, ClmError> {
    check_extensions(doc, extensions)?;
    // The payloads are a lower bound on the section: refuse before serializing.
    container::check_section_len(SECTION_TEXTURES, payloads)?;
    // A carried section under a known kind can only have been hand-built.
    if let Some(reserved) = unknown_sections.iter().find(|s| is_known_section(s.kind)) {
        return Err(ClmError::ReservedSectionKind(reserved.kind));
    }

    // Omitted when empty, so files without byte extensions keep their bytes.
    let sections = 2 + usize::from(!extensions.is_empty()) + unknown_sections.len();
    let mut file = container::Writer::new(&MAGIC, FORMAT_VERSION, sections)?;
    file.section(SECTION_STRUCTURE, |out| cbor_into(doc, out))?;
    let carried: usize = unknown_sections.iter().map(|s| s.data.len()).sum();
    file.reserve(
        usize::try_from(payloads)
            .unwrap_or(0)
            .saturating_add(carried),
    );
    file.section(SECTION_TEXTURES, |out| cbor_into(&textures, out))?;
    if !extensions.is_empty() {
        file.section(SECTION_EXTENSIONS, |out| cbor_into(&extensions, out))?;
    }
    for section in unknown_sections {
        file.section(section.kind, |out| {
            out.extend_from_slice(&section.data);
            Ok::<(), ClmError>(())
        })?;
    }
    Ok(file.finish())
}

/// The `Structure` section is byte-identical to the one [`encode`] writes.
pub fn encode_structure(
    doc: &ClmStructure,
    textures: &[ClmTextureRef],
) -> Result<Vec<u8>, ClmError> {
    let mut file = container::Writer::new(&MAGIC, FORMAT_VERSION, 2)?;
    file.section(SECTION_STRUCTURE, |out| cbor_into(doc, out))?;
    file.section(SECTION_TEXTURE_MANIFEST, |out| cbor_into(&textures, out))?;
    Ok(file.finish())
}

pub fn decode(bytes: &[u8]) -> Result<ClmFile, ClmError> {
    decode_with_budget(bytes, &mut crate::load_budget::LoadBudget::default())
}

/// A complete `.clm` is refused rather than read as a model with no textures.
pub fn decode_structure(bytes: &[u8]) -> Result<ClmStructureFile, ClmError> {
    decode_structure_with_budget(bytes, &mut crate::load_budget::LoadBudget::default())
}

pub fn decode_structure_with_budget(
    bytes: &[u8],
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<ClmStructureFile, ClmError> {
    budget.charge(
        crate::load_budget::LoadResource::EncodedBytes,
        bytes.len() as u64,
    )?;
    let file = read_container(bytes)?;
    if file.section(SECTION_TEXTURES).is_some() {
        return Err(ClmError::NotAStructure);
    }
    // Byte extensions are fetched by hash, not carried in a structure feed.
    if file.section(SECTION_EXTENSIONS).is_some() {
        return Err(ClmError::NotAStructure);
    }
    let doc = decode_structure_section(&file, budget)?;
    let textures = decode_texture_manifest(&file, budget)?;
    Ok(ClmStructureFile { doc, textures })
}

/// Lets a client fetch missing payloads before applying the structure.
pub fn structure_texture_ids(
    bytes: &[u8],
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<Vec<ClmTextureRef>, ClmError> {
    decode_texture_manifest(&read_container(bytes)?, budget)
}

/// The manifest is charged as structure bytes before it is decoded, since
/// its count is read too late to bound the decode.
fn decode_texture_manifest(
    file: &container::Container<'_>,
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<Vec<ClmTextureRef>, ClmError> {
    let manifest = file
        .section(SECTION_TEXTURE_MANIFEST)
        .ok_or(ClmError::MissingSection("TextureManifest"))?;
    budget.charge(
        crate::load_budget::LoadResource::StructureBytes,
        manifest.len() as u64,
    )?;
    let textures: Vec<ClmTextureRef> = cbor_from_slice(manifest)?;
    budget.charge(
        crate::load_budget::LoadResource::Textures,
        textures.len() as u64,
    )?;
    Ok(textures)
}

pub fn decode_with_budget(
    bytes: &[u8],
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<ClmFile, ClmError> {
    let file = decode_borrowed(bytes, budget)?;
    Ok(ClmFile {
        doc: file.doc,
        textures: file
            .textures
            .into_iter()
            .map(|t| ClmTexture {
                id: t.id,
                encoding: t.encoding,
                alpha: t.alpha,
                data: t.data.into_owned(),
            })
            .collect(),
        extensions: file.extensions,
        unknown_sections: file.unknown_sections,
    })
}

pub(crate) fn decode_borrowed<'a>(
    bytes: &'a [u8],
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<ClmFileRef<'a>, ClmError> {
    budget.charge(
        crate::load_budget::LoadResource::EncodedBytes,
        bytes.len() as u64,
    )?;
    let file = read_container(bytes)?;
    if file.section(SECTION_TEXTURE_MANIFEST).is_some() {
        return Err(ClmError::StructureOnly);
    }
    let doc = decode_structure_section(&file, budget)?;
    let textures = match file.section(SECTION_TEXTURES) {
        Some(b) => decode_textures(b)?,
        None => Vec::new(),
    };
    let extensions: Vec<ClmExtensionBlob> = match file.section(SECTION_EXTENSIONS) {
        Some(b) => cbor_from_slice(b)?,
        None => Vec::new(),
    };
    check_extensions(&doc, &extensions)?;
    // Copied out because the model that holds them outlives `bytes`.
    let unknown_sections = file
        .sections
        .iter()
        .filter(|section| !is_known_section(section.kind))
        .map(|section| ClmUnknownSection {
            kind: section.kind,
            data: section.data.into(),
        })
        .collect();
    Ok(ClmFileRef {
        doc,
        textures,
        extensions,
        unknown_sections,
    })
}

/// Finds payloads in place via [`texture_spans`], avoiding ciborium's double
/// copy. A table the walk does not understand goes to the full decoder, whose
/// verdict stands.
fn decode_textures(section: &[u8]) -> Result<Vec<ClmTextureSpan<'_>>, ClmError> {
    if let Some(spans) = texture_spans(section) {
        return Ok(spans);
    }
    let owned: Vec<ClmTexture> = cbor_from_slice(section)?;
    Ok(owned
        .into_iter()
        .map(|t| ClmTextureSpan {
            id: t.id,
            encoding: t.encoding,
            alpha: t.alpha,
            data: std::borrow::Cow::Owned(t.data),
        })
        .collect())
}

const CBOR_BYTES: u8 = 2;
const CBOR_TEXT: u8 = 3;
const CBOR_ARRAY: u8 = 4;
const CBOR_MAP: u8 = 5;

fn cbor_head(data: &[u8], major: u8) -> Option<(u64, &[u8])> {
    let (&first, rest) = data.split_first()?;
    if first >> 5 != major {
        return None;
    }
    let (argument, used) = match first & 0x1f {
        small @ 0..=23 => (u64::from(small), 0),
        24 => (u64::from(*rest.first()?), 1),
        25 => (
            u64::from(u16::from_be_bytes(rest.get(..2)?.try_into().ok()?)),
            2,
        ),
        26 => (
            u64::from(u32::from_be_bytes(rest.get(..4)?.try_into().ok()?)),
            4,
        ),
        27 => (u64::from_be_bytes(rest.get(..8)?.try_into().ok()?), 8),
        // Indefinite length, or reserved.
        _ => return None,
    };
    Some((argument, &rest[used..]))
}

fn cbor_string(data: &[u8], major: u8) -> Option<(&[u8], &[u8])> {
    let (len, rest) = cbor_head(data, major)?;
    let len = usize::try_from(len).ok().filter(|len| *len <= rest.len())?;
    Some(rest.split_at(len))
}

/// `None`, which is not a verdict, unless the table is laid out exactly as
/// [`encode`] writes it.
fn texture_spans(section: &[u8]) -> Option<Vec<ClmTextureSpan<'_>>> {
    let (count, mut rest) = cbor_head(section, CBOR_ARRAY)?;
    // Not sized from `count`: it is the file's claim, and every entry below
    // has to be paid for in section bytes before it is pushed.
    let mut out = Vec::new();
    for _ in 0..count {
        let (fields, after) = cbor_head(rest, CBOR_MAP)?;
        rest = after;
        let (mut id, mut encoding, mut alpha, mut data) = (None, None, None, None);
        for _ in 0..fields {
            let (key, value) = cbor_string(rest, CBOR_TEXT)?;
            if key == b"data" {
                let (payload, after) = cbor_string(value, CBOR_BYTES)?;
                if data.replace(payload).is_some() {
                    return None;
                }
                rest = after;
                continue;
            }
            let (_, after) = cbor_string(value, CBOR_TEXT)?;
            let item = &value[..value.len() - after.len()];
            let repeated = match key {
                b"id" => id.replace(cbor_from_slice::<TexId>(item).ok()?).is_some(),
                b"encoding" => encoding.replace(cbor_from_slice(item).ok()?).is_some(),
                b"alpha" => alpha.replace(cbor_from_slice(item).ok()?).is_some(),
                _ => return None,
            };
            if repeated {
                return None;
            }
            rest = after;
        }
        out.push(ClmTextureSpan {
            id: id?,
            encoding: encoding?,
            alpha: alpha?,
            data: std::borrow::Cow::Borrowed(data?),
        });
    }
    Some(out)
}

/// Charges the section length first: it is the only bound available before
/// decoding allocates.
fn decode_structure_section(
    file: &container::Container<'_>,
    budget: &mut crate::load_budget::LoadBudget,
) -> Result<ClmStructure, ClmError> {
    let structure = file
        .section(SECTION_STRUCTURE)
        .ok_or(ClmError::MissingSection("Structure"))?;
    budget.charge(
        crate::load_budget::LoadResource::StructureBytes,
        structure.len() as u64,
    )?;
    cbor_from_slice(structure)
}

fn read_container(bytes: &[u8]) -> Result<container::Container<'_>, ClmError> {
    let file = container::read(bytes, &MAGIC)?;
    if file.version != FORMAT_VERSION {
        return Err(ClmError::UnsupportedVersion(file.version));
    }
    let mut known = Vec::with_capacity(4);
    for section in &file.sections {
        if is_known_section(section.kind) {
            if known.contains(&section.kind) {
                return Err(ClmError::DuplicateSection(section.kind));
            }
            known.push(section.kind);
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value;

    fn cbor_to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, ClmError> {
        let mut out = Vec::new();
        ciborium::into_writer(value, &mut out).map_err(ClmError::Encode)?;
        Ok(out)
    }

    fn sample() -> (ClmStructure, Vec<ClmTexture>) {
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
            }),
        };
        let mouth = ParamId::new("param-mouth").unwrap();
        let doc = ClmStructure {
            extensions: Default::default(),
            nodes: vec![root, part],
            params: vec![ClmParam {
                id: mouth.clone(),
                name: "Mouth".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
            }],
            bindings: vec![ClmBinding {
                key_positions: vec![vec![0.0, 1.0]],
                params: vec![mouth],
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
            welds: Vec::new(),
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
        let bytes = encode(&doc, &textures, &[]).unwrap();
        let file = decode(&bytes).unwrap();
        assert_eq!(file.doc, doc);
        assert_eq!(file.textures, textures);
    }

    #[test]
    fn save_is_byte_stable() {
        let (doc, textures) = sample();
        let a = encode(&doc, &textures, &[]).unwrap();
        let b = encode(&doc, &textures, &[]).unwrap();
        assert_eq!(
            a, b,
            "encode must be deterministic for editor dirty-checking"
        );
    }

    #[test]
    fn borrowed_payloads_write_the_same_file() {
        let (doc, textures) = sample();
        let borrowed: Vec<ClmTextureBytes> = textures.iter().map(Into::into).collect();
        assert_eq!(
            encode_borrowed(&doc, &borrowed, &[], &[]).unwrap(),
            encode(&doc, &textures, &[]).unwrap()
        );
    }

    #[test]
    fn the_reader_leaves_payloads_in_the_file() {
        let (doc, mut textures) = sample();
        // Longer than every short-length CBOR head, and than the decoder's
        // scratch buffer.
        textures[0].data = (0..70_000u32).map(|i| i as u8).collect();
        let bytes = encode(&doc, &textures, &[]).unwrap();

        let file = decode_borrowed(&bytes, &mut crate::load_budget::LoadBudget::default()).unwrap();

        assert_eq!(file.textures.len(), 1);
        let std::borrow::Cow::Borrowed(payload) = file.textures[0].data else {
            panic!("the payload was copied out of the file");
        };
        assert_eq!(payload, &textures[0].data[..]);
        let file_range = bytes.as_ptr_range();
        assert!(
            file_range.contains(&payload.as_ptr()),
            "the payload is not a view into the file's bytes"
        );
        assert_eq!(decode(&bytes).unwrap().textures, textures);
    }

    fn texture_entry(fields: Vec<(&str, Value)>) -> Value {
        Value::Map(
            fields
                .into_iter()
                .map(|(key, value)| (Value::Text(key.into()), value))
                .collect(),
        )
    }

    fn written(data: &[u8]) -> Vec<(&'static str, Value)> {
        vec![
            ("id", Value::Text("tex-skin".into())),
            ("encoding", Value::Text("Png".into())),
            ("alpha", Value::Text("Straight".into())),
            ("data", Value::Bytes(data.to_vec())),
        ]
    }

    /// The in-place walk must agree with the full decoder wherever it answers.
    #[test]
    fn the_in_place_texture_reader_agrees_with_the_decoder() {
        let mut reordered = written(b"abc");
        reordered.reverse();
        let mut unknown_field = written(b"abc");
        unknown_field.push(("mip_bias", Value::Integer(1.into())));
        let mut repeated = written(b"abc");
        repeated.push(("id", Value::Text("tex-other".into())));
        let missing_data: Vec<_> = written(b"abc").into_iter().take(3).collect();
        let mut bytes_as_array = written(b"");
        bytes_as_array[3].1 = Value::Array(vec![Value::Integer(7.into()); 3]);
        let mut unknown_encoding = written(b"abc");
        unknown_encoding[1].1 = Value::Text("Webp".into());
        let mut bad_id = written(b"abc");
        bad_id[0].1 = Value::Text("has space".into());

        let table = |entries: Vec<Vec<(&str, Value)>>| {
            cbor_to_vec(&Value::Array(
                entries.into_iter().map(texture_entry).collect(),
            ))
            .unwrap()
        };
        let plain = table(vec![written(b"abc"), written(&[9; 300])]);
        let mut truncated = plain.clone();
        truncated.truncate(plain.len() - 10);
        let mut trailing = plain.clone();
        trailing.extend_from_slice(&[0, 0, 0]);
        // The same one-entry table with its array head and its payload head
        // spelled in a longer form than they need, and as an
        // indefinite-length array.
        let one = table(vec![written(b"abc")]);
        let mut long_heads = vec![0x98, 0x01];
        long_heads.extend_from_slice(&one[1..one.len() - 4]);
        long_heads.extend_from_slice(&[0x59, 0x00, 0x03, b'a', b'b', b'c']);
        let mut indefinite = vec![0x9f];
        indefinite.extend_from_slice(&one[1..]);
        indefinite.push(0xff);

        // `(name, section, whether the walk is expected to answer)`.
        let cases: Vec<(&str, Vec<u8>, bool)> = vec![
            ("as written", plain, true),
            ("empty", table(vec![]), true),
            ("fields in another order", table(vec![reordered]), true),
            ("trailing bytes", trailing, true),
            ("longer heads than needed", long_heads, true),
            ("an unknown field", table(vec![unknown_field]), false),
            ("a field twice", table(vec![repeated]), false),
            ("no payload", table(vec![missing_data]), false),
            ("payload as an array", table(vec![bytes_as_array]), false),
            ("an unknown encoding", table(vec![unknown_encoding]), false),
            ("an id off the charset", table(vec![bad_id]), false),
            ("cut short", truncated, false),
            ("indefinite length", indefinite, false),
            (
                "not an array",
                cbor_to_vec(&Value::Integer(3.into())).unwrap(),
                false,
            ),
        ];
        for (name, section, walked) in cases {
            assert_eq!(
                texture_spans(&section).is_some(),
                walked,
                "{name}: whether the walk answers"
            );
            let decoder = cbor_from_slice::<Vec<ClmTexture>>(&section);
            let reader = decode_textures(&section).map(|spans| {
                spans
                    .into_iter()
                    .map(|t| ClmTexture {
                        id: t.id,
                        encoding: t.encoding,
                        alpha: t.alpha,
                        data: t.data.into_owned(),
                    })
                    .collect::<Vec<_>>()
            });
            match (decoder, reader) {
                (Ok(expected), Ok(got)) => assert_eq!(got, expected, "{name}"),
                (Err(_), Err(_)) => {}
                (decoder, reader) => panic!("{name}: decoder {decoder:?}, reader {reader:?}"),
            }
        }
    }

    #[test]
    fn encode_refuses_a_texture_table_the_reader_would_refuse() {
        let (doc, mut textures) = sample();
        textures[0].data = vec![0u8; container::MAX_SECTION_LEN as usize + 1];

        let err = encode(&doc, &textures, &[]).unwrap_err();

        assert!(
            matches!(
                err,
                ClmError::Container(ContainerError::SectionTooLarge {
                    kind: SECTION_TEXTURES,
                    ..
                })
            ),
            "{err:?}"
        );
    }

    #[test]
    fn decode_rejects_an_encoded_file_over_the_aggregate_budget() {
        let (doc, textures) = sample();
        let bytes = encode(&doc, &textures, &[]).unwrap();
        let mut budget = crate::load_budget::LoadBudget::new(crate::load_budget::LoadLimits {
            encoded_bytes: bytes.len() as u64 - 1,
            ..crate::load_budget::LoadLimits::default()
        });

        let err = decode_with_budget(&bytes, &mut budget).unwrap_err();

        assert!(matches!(
            err,
            ClmError::LoadLimit(crate::load_budget::LoadLimitError {
                resource: crate::LoadResource::EncodedBytes,
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
        let back: ClmStructure = cbor_from_slice(&cbor_to_vec(&value).unwrap()).unwrap();
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

    #[test]
    fn an_empty_structure_decodes() {
        let doc: ClmStructure = cbor_from_slice(&cbor_to_vec(&Value::Map(Vec::new())).unwrap())
            .expect("an empty map is a legal structure");
        assert_eq!(doc, ClmStructure::default());
    }

    /// All containers share a magic. Check the version before decoding sections.
    #[test]
    fn an_older_or_newer_version_is_refused_not_silently_misread() {
        let (doc, textures) = sample();
        let bytes = encode(&doc, &textures, &[]).unwrap();
        for version in (0..FORMAT_VERSION).chain([FORMAT_VERSION + 1, u16::MAX]) {
            let mut bytes = bytes.clone();
            bytes[8..10].copy_from_slice(&version.to_le_bytes());
            match decode(&bytes) {
                Err(ClmError::UnsupportedVersion(v)) => assert_eq!(v, version),
                other => panic!("expected UnsupportedVersion({version}), got {other:?}"),
            }
        }
    }

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
