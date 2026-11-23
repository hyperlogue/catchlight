//! `.clm` — catchlight's model format: what a [`Model`](crate::Model) holds,
//! written down.
//!
//! A [`super::container`] file (magic `b"NYANPASU"`) with two sections:
//! `Structure` (one CBOR value) and `Textures` (verbatim source-encoded
//! bytes, never decoded or cropped). A third, `Extensions`, appears only when
//! some extension carries bytes: one keyed array of blobs, laid out like
//! `Textures` and read the same way, so a file with no byte extension is
//! byte-identical to one written before the section existed. A section of
//! any other kind is one a newer writer added: it is carried through a load
//! and a save without being read ([`ClmUnknownSection`]).
//!
//! There is a second shape on the same wire: a **structure-only** container,
//! `Structure` beside a `TextureManifest` — the texture table with every
//! payload taken out. It is what an editor server pushes a replica after each
//! edit, and what [`Model::replace_structure`](crate::Model::replace_structure)
//! reads. The `Structure` section is byte-identical in both shapes; only the
//! payloads are absent. The two shapes are **disjoint and self-describing**:
//! a manifest section means structure-only, its absence means a complete
//! file, and each reader refuses the other shape by name
//! ([`ClmError::StructureOnly`], [`ClmError::NotAStructure`]) rather than
//! reading a structure as a model whose textures all went missing. The
//! manifest also carries the model's texture *order* and each texture's
//! encoding, which the `Structure` section does not: a part names its albedo
//! and nothing more, so a client fetching a payload it lacks would otherwise
//! not know how to decode it or where it sits.
//!
//! Invariants this module and its reader
//! ([`Model::from_clm_bytes`](crate::Model::from_clm_bytes)) enforce:
//!
//! - **Everything is keyed by Id.** A node's `parent`, a mask's `source`, a
//!   binding's `node` and `params`, a response's input and output references, a part's
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
//!   the product of its own key-position axes. Bindings carry their axes even when referencing base params.
//! - **No vertex index outside a slot.** A part carries named slots, each
//!   filled by one of its vertices; a weld names two parts and pairs their
//!   slots. A slot may also be *unfilled* (`vertex: null`) — what re-authoring
//!   a part's mesh leaves behind — and a weld over an unfilled slot loads,
//!   resolving to one pair fewer. A [`Model`](crate::Model) holds the same
//!   shape, so this section is a copy in both directions.
//! - **A malformed file is an error, never a panic.** Every Id that names
//!   something must find it, a filled slot must be in mesh range, a weld must
//!   join two different parts once and pair slots they carry, and a binding
//!   must name one or two distinct params. The reader reports which Id in
//!   which field failed. On extensions it refuses, each time naming the key:
//!   a key off the charset or without its dot; a value that is neither
//!   JSON-compatible nor a marker (bytes inline, a CBOR tag, a non-string map
//!   key — `serde_json::Value` turns each into an "invalid type" on the way
//!   in); a float that is not finite, which JSON cannot spell; a marker whose
//!   bytes are absent; bytes nothing names; a size or a hash that disagrees
//!   with the marker; a byte value over [`MAX_EXTENSION_BYTES`]; and a JSON
//!   value over [`MAX_EXTENSION_JSON_BYTES`]. The pairing and both caps are
//!   checked on the way *out* too, so a writer never leaves a marker without
//!   its payload or a value the next reader refuses.
//! - **An extension is carried, never interpreted.** A vendor files
//!   annotations under a dotted key ([`crate::id::ExtensionKey`], vendor
//!   first) and catchlight reads none of them. A value is either
//!   JSON-compatible — string-keyed maps, arrays, strings, numbers, bools,
//!   null, written as native CBOR inline in the structure — or opaque bytes,
//!   which live in the `Extensions` section while the structure carries only
//!   a `{size, hash}` marker. The split is what keeps a structure feed small:
//!   a thumbnail travels once, by hash, instead of riding along with every
//!   unrelated edit. Unknown keys are pass-through by construction, so
//!   save→save stays byte-identical over a key this build has never heard of.
//! - **A binding has to be one the runtime can fold.** A colour target
//!   (`Opacity`, `Tint*`, `ScreenTint*`) on a mesh group has nowhere to land —
//!   a mesh group is never drawn — and a deform cell holds one `[dx, dy]` per
//!   mesh vertex, no more and no fewer. Both are refused on the way in, so a
//!   model that loads is one every runtime evaluates the same way. The second
//!   is refused on the way *out* too, and before that by the edit that would
//!   cause it: a refit that returns the wrong number of offsets is refused
//!   where it is made, not at the next save.
//!
//! This module also holds the leaf value types that are the same in memory and
//! on the wire — meshes, transforms, motion responses, binding cells — so a
//! Model stores exactly what the file stores and neither side owns a
//! translation of the other.
//!
//! CBOR maps keyed by field name give additive evolution: a future field
//! returns as `#[serde(default)]` and old/new readers interoperate. **Never**
//! add `deny_unknown_fields`; a breaking change bumps [`FORMAT_VERSION`], and
//! has to bring a way to carry the previous version's files forward with it
//! — [`crate::formats`] says what counts.
//!
//! The version is checked before reading a full file or a structure feed.
//! Externally tagged node kinds also cause an unknown variant to fail decoding.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::container::{self, ContainerError};
use crate::components::{BlendMode, MaskMode};
use crate::id::{ExtensionKey, NodeId, ParamId, SlotId, TexId};
use crate::interpolate::InterpolateMode;

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

    /// How many triangles the mesh has — `indices` is a flat triangle list, so
    /// a trailing partial triangle counts for nothing.
    pub fn triangle_count(&self) -> usize {
        match &self.indices {
            ClmIndices::U16(v) => v.len() / 3,
            ClmIndices::U32(v) => v.len() / 3,
        }
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

/// The variant *is* the binding's target, so a binding cannot name one target
/// and carry another's cells. Targets the runtime does not fold
/// (emissionStrength, unknown ones) are dropped at import. Each variant carries only the *authored* keypoint cells;
/// unauthored cells are derived, never stored — authored = present, so the
/// editor's set/partial/unset state is the data shape itself.
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
/// Version 5 shares the tagged response source representation with the editor.
pub const FORMAT_VERSION: u16 = 5;

const SECTION_STRUCTURE: u32 = 0;
const SECTION_TEXTURES: u32 = 1;
/// The texture table with every payload taken out — what a *structure-only*
/// container carries in place of `Textures`. Its presence is what tells the
/// two shapes apart, so neither reader ever guesses.
const SECTION_TEXTURE_MANIFEST: u32 = 2;
/// Every extension's byte value, keyed by extension key. Absent when no
/// extension carries bytes, which is every file today.
const SECTION_EXTENSIONS: u32 = 3;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ClmError {
    #[error("container framing: {0}")]
    Container(#[from] ContainerError),
    #[error("CBOR encode: {0}")]
    Encode(String),
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
    #[error("extension {key:?} carries {size} bytes, over the {max}-byte cap")]
    ExtensionTooLarge {
        key: String,
        size: usize,
        max: usize,
    },
    #[error("CBOR decode: {0}")]
    Decode(String),
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
}

/// The whole decoded `.clm`: the structure plus the texture table.
#[derive(Debug, Clone, PartialEq)]
pub struct ClmFile {
    pub doc: ClmStructure,
    pub textures: Vec<ClmTexture>,
    /// The bytes behind every `Bytes` marker in `doc.extensions`, in key
    /// order. Empty unless some extension carries bytes.
    pub extensions: Vec<ClmExtensionBlob>,
    /// Every section whose kind this build has no name for, in file order.
    /// See [`ClmUnknownSection`].
    pub unknown_sections: Vec<ClmUnknownSection>,
}

impl ClmFile {
    /// Serialize the whole file, the sections it does not recognise
    /// included. This is the writer for a file that was read: [`encode`]
    /// takes the three parts this build understands and writes nothing else.
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

/// A section of a `.clm` whose kind this build does not recognise, kept
/// exactly as the file held it.
///
/// The container is a table of numbered sections, and a newer writer may
/// add one without changing what the existing ones mean — that is how
/// `Extensions` arrived. A reader that dropped what it could not name would
/// make every tool built on an older catchlight destructive: open, save,
/// and the newer writer's data is gone. So an unrecognised section is
/// carried: read into the [`ClmFile`], held by the
/// [`Model`](crate::Model), and written back byte for byte.
///
/// What "carried" covers:
///
/// - **Order among themselves** is the file's. They are written after the
///   sections this build knows, whatever order the file interleaved them in,
///   so a save is byte-stable from the second one on.
/// - **Only a complete file carries them.** A structure-only container —
///   the push an editor server sends a replica — neither writes nor reads
///   them; a replica keeps the ones it was loaded with.
/// - **Nothing here reads them**, which is also why they do not survive
///   being split or joined: an addon extracted from a model carries none,
///   and installing one adds none.
/// - A second section of a kind this build *does* know is not unknown, and
///   is dropped as it always was: the first one is the section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClmUnknownSection {
    pub kind: u32,
    pub data: std::sync::Arc<[u8]>,
}

/// A decoded *structure-only* container: the same `Structure` section a
/// `.clm` carries, plus the texture table with the payloads taken out.
///
/// This is what a replica is sent after every edit — see
/// [`Model::to_structure_bytes`](crate::Model::to_structure_bytes). The
/// manifest is not decoration: a `Structure` section names a part's albedo
/// and nothing else, so without it a client that has to *fetch* a texture
/// knows neither the order the model holds its textures in nor how to decode
/// the bytes when they arrive.
#[derive(Debug, Clone, PartialEq)]
pub struct ClmStructureFile {
    pub doc: ClmStructure,
    /// The model's texture order, headers only.
    pub textures: Vec<ClmTextureRef>,
}

/// One texture's header without its payload: its Id and how to read the bytes
/// once they are fetched.
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
    /// Vendor annotations, keyed by [`ExtensionKey`]. Skipped when empty, so
    /// every file written before extensions existed still writes the same
    /// bytes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<ExtensionKey, ClmExtension>,
}

/// The most bytes one extension may carry. A structure feed is meant to stay
/// small enough to push after every edit, and a byte value big enough to
/// break that is a file, not an annotation.
pub const MAX_EXTENSION_BYTES: usize = 1024 * 1024;

/// The most one extension's JSON value may measure, by
/// [`extension_json_len`]. The same reasoning as [`MAX_EXTENSION_BYTES`],
/// and more pressing: a JSON value rides *inside* the structure, so unlike
/// bytes it is pushed whole with every structure feed.
pub const MAX_EXTENSION_JSON_BYTES: usize = 1024 * 1024;

/// What an extension holds, as the structure spells it.
///
/// Externally tagged, so the two arms are a one-key map (`{"json": …}`,
/// `{"bytes": …}`) and neither can be read as the other. That matters:
/// a marker is itself a JSON-compatible object, so an untagged value would
/// make `{"size": 3, "hash": "…"}` ambiguous.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClmExtension {
    /// A JSON-compatible value, written as native CBOR inline in the
    /// structure: string-keyed maps, arrays, strings, numbers, bools, null.
    /// Everything else is refused on the way in — a CBOR byte string, a tag,
    /// a non-string map key are each an "invalid type", and a float that is
    /// not finite is an error rather than the `null` `serde_json` would make
    /// of it: an extension is carried, never rewritten.
    Json(#[serde(deserialize_with = "json_value")] serde_json::Value),
    /// A stand-in for bytes in the `SECTION_EXTENSIONS` section. The
    /// structure carries only the size and the hash, so a client watching a
    /// structure feed sees that a thumbnail changed without the thumbnail
    /// travelling with every unrelated edit.
    Bytes(ClmExtensionMarker),
}

/// Read a JSON-compatible value without changing it.
///
/// `serde_json::Value`'s own `Deserialize` is almost this, except that JSON
/// has no NaN or infinity and it reads one as `null`. CBOR has both, so a
/// `.clm` can carry one where a number was meant — and a reader that turned
/// it into `null` would write back a different value from the one it read,
/// which is the one thing an extension is promised not to suffer.
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
        while let Some(item) = seq.next_element_seed(JsonValue)? {
            items.push(item);
        }
        Ok(serde_json::Value::Array(items))
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut entries = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            entries.insert(key, map.next_value_seed(JsonValue)?);
        }
        Ok(serde_json::Value::Object(entries))
    }
}

/// What the structure carries in place of an extension's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmExtensionMarker {
    pub size: u64,
    /// Lowercase hex of [`extension_hash`] over the bytes.
    pub hash: String,
}

/// One extension's bytes, as the `Extensions` section stores them — the same
/// shape the `Textures` section uses, a keyed array rather than a map, so the
/// payload names itself and the section needs no side table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClmExtensionBlob {
    pub key: ExtensionKey,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// The size of an extension's JSON value: its length as compact JSON text.
///
/// A JSON value has no size of its own — it is a tree in memory and CBOR in
/// the file — so this is the one the format holds it to, chosen because any
/// tool that authored the value can compute it without this crate.
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

/// The content hash on a byte extension's marker: blake3, lowercase hex.
pub fn extension_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
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

/// [`ClmTexture`] with the payload borrowed, for writing.
///
/// A [`Model`](crate::Model) holds its payloads behind `Arc`s, and saving it
/// through `ClmTexture` copied every byte of every texture into a `Vec` just
/// to have something to serialize. This serializes field for field as
/// `ClmTexture` does, so which of the two a writer was handed cannot be told
/// from the file.
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

/// One texture as [`decode_borrowed`] read it: the payload still inside the
/// file's bytes wherever the reader could find it there.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClmTextureSpan<'a> {
    pub id: TexId,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub data: std::borrow::Cow<'a, [u8]>,
}

/// A decoded `.clm` whose texture payloads have not been copied out of the
/// bytes it was read from. What [`decode`] returns, one copy earlier.
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
    MotionResponse(crate::MotionResponse),
    Spine(ClmSpine),
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
    /// The named vertices welds pair, filled by whoever owns the part.
    #[serde(default)]
    pub slots: Vec<ClmSlot>,
}

/// One named handle on a part, so that a weld can name a vertex without
/// naming an index.
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
    pub translate_children: bool,
}

/// A chain of joints drawn on the art, turning the geometry beneath the node.
///
/// Only the authored half: the per-vertex assignment the runtime derives from
/// this polyline is a cache a puppet builds, and a file that stored it would
/// be storing a bake.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClmSpine {
    /// The far end of each link, in the node's own space, so `joints[0]` ends
    /// the link that starts at the node itself. At least one, every
    /// coordinate finite, and no two consecutive points equal — a link with no
    /// length has no direction to turn about.
    pub joints: Vec<[f32; 2]>,
    /// The param each link's bend is read from, in link order; `None` where a
    /// link is rigid. Absent means "no link bends" — the reader fills it to
    /// the length of `joints`, which is the one length it may ever have.
    #[serde(default)]
    pub targets: Vec<Option<ParamId>>,
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
    /// Rest value, in param-value space.
    pub default: f32,
}

/// One or two params' control over one property of one node. `values` names
/// the property and carries its authored cells; the grid those cells index is
/// product of this binding's `key_positions`, `x` along the first param.
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

/// Two parts, and the slot pairs between them. Symmetric: each unordered pair
/// of parts appears at most once and the runtime solve moves both sides.
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
    /// A slot on the weld's `a` part.
    pub a: SlotId,
    /// A slot on the weld's `b` part.
    pub b: SlotId,
    /// A's share of the meeting point in `[0, 1]`: 1.0 pins A and snaps B
    /// to it, 0.5 meets midway.
    pub weight: f32,
}

/// Serialize `value` straight into the section being written, so a section
/// is never built in a buffer of its own and then copied into the file.
fn cbor_into<T: Serialize>(
    value: &T,
    out: &mut container::SectionWriter<'_>,
) -> Result<(), ClmError> {
    ciborium::into_writer(value, out).map_err(|e| ClmError::Encode(e.to_string()))
}

fn cbor_from_slice<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ClmError> {
    ciborium::from_reader(bytes).map_err(|e| ClmError::Decode(e.to_string()))
}

/// Hold a structure's extension markers against the bytes that came with
/// them: every marker has its bytes, every blob is named, and each pair
/// agrees on size and hash.
///
/// Run on the way in *and* on the way out. A reader trusts nothing, and a
/// writer that emitted a marker without its bytes would leave a file that
/// only fails at the next open.
pub fn check_extensions(doc: &ClmStructure, blobs: &[ClmExtensionBlob]) -> Result<(), ClmError> {
    let mut carried: BTreeMap<&ExtensionKey, &ClmExtensionBlob> = BTreeMap::new();
    for blob in blobs {
        if carried.insert(&blob.key, blob).is_some() {
            return Err(ClmError::ExtensionBytesDuplicate {
                key: blob.key.to_string(),
            });
        }
        if blob.data.len() > MAX_EXTENSION_BYTES {
            return Err(ClmError::ExtensionTooLarge {
                key: blob.key.to_string(),
                size: blob.data.len(),
                max: MAX_EXTENSION_BYTES,
            });
        }
    }

    for (key, value) in &doc.extensions {
        let marker = match value {
            ClmExtension::Bytes(marker) => marker,
            ClmExtension::Json(json) => {
                let size = extension_json_len(json);
                if size > MAX_EXTENSION_JSON_BYTES {
                    return Err(ClmError::ExtensionTooLarge {
                        key: key.to_string(),
                        size,
                        max: MAX_EXTENSION_JSON_BYTES,
                    });
                }
                continue;
            }
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

/// Serialize to `.clm` bytes. Deterministic for a given input: ciborium writes
/// struct fields in declaration order and the container lays sections out in
/// order, so save→save is byte-stable.
///
/// Writes the sections this build defines and no others. To write back a
/// file that was read, use [`ClmFile::encode`], which also carries the
/// sections it did not recognise.
pub fn encode(
    doc: &ClmStructure,
    textures: &[ClmTexture],
    extensions: &[ClmExtensionBlob],
) -> Result<Vec<u8>, ClmError> {
    let payloads = textures.iter().map(|t| t.data.len() as u64).sum();
    encode_sections(doc, textures, payloads, extensions, &[])
}

/// [`ClmFile::encode`] over textures whose payloads are borrowed from
/// wherever they already live. The bytes written are the same.
pub fn encode_borrowed(
    doc: &ClmStructure,
    textures: &[ClmTextureBytes<'_>],
    extensions: &[ClmExtensionBlob],
    unknown_sections: &[ClmUnknownSection],
) -> Result<Vec<u8>, ClmError> {
    let payloads = textures.iter().map(|t| t.data.len() as u64).sum();
    encode_sections(doc, textures, payloads, extensions, unknown_sections)
}

/// Whether `kind` is a section this build gives a meaning to.
fn is_known_section(kind: u32) -> bool {
    matches!(
        kind,
        SECTION_STRUCTURE | SECTION_TEXTURES | SECTION_TEXTURE_MANIFEST | SECTION_EXTENSIONS
    )
}

/// Write a complete `.clm`. `textures` is whatever serializes as the texture
/// table; `payloads` is how many payload bytes it holds.
///
/// Every section is serialized into the file's own buffer, so a payload is
/// copied once on the way out — from where the caller holds it into the
/// file — rather than into a section buffer and from there into the file.
fn encode_sections<T: Serialize>(
    doc: &ClmStructure,
    textures: &[T],
    payloads: u64,
    extensions: &[ClmExtensionBlob],
    unknown_sections: &[ClmUnknownSection],
) -> Result<Vec<u8>, ClmError> {
    check_extensions(doc, extensions)?;
    // The payloads alone are a lower bound on the section that holds them,
    // so a table the container would refuse is refused here, before it is
    // serialized only to be thrown away.
    container::check_section_len(SECTION_TEXTURES, payloads)?;
    // A carried section under a kind this format defines would be a second
    // `Structure`, or a manifest that turns the file into a structure-only
    // container: not something a file read by this crate can hold, so
    // somebody built it by hand.
    if let Some(reserved) = unknown_sections.iter().find(|s| is_known_section(s.kind)) {
        return Err(ClmError::ReservedSectionKind(reserved.kind));
    }

    // `Extensions` is absent unless it holds something, so a file with no
    // byte extension is byte-identical to one written before the section
    // existed.
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
    // After everything this build knows, in the order the file had them.
    for section in unknown_sections {
        file.section(section.kind, |out| {
            out.extend_from_slice(&section.data);
            Ok::<(), ClmError>(())
        })?;
    }
    Ok(file.finish())
}

/// Serialize the `Structure` half alone: the same CBOR value [`encode`]
/// writes, in the same container framing, with the texture payloads replaced
/// by their headers.
///
/// The `Structure` section is byte-for-byte the one [`encode`] would write
/// for the same structure, so a structure push and a save agree about what
/// the model is.
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

/// Read a **structure-only** container: the structure plus the texture
/// manifest, no payloads. A complete `.clm` is refused
/// ([`ClmError::NotAStructure`]) rather than read as a model with no
/// textures.
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
    // Byte extensions do not travel in a structure feed either: the markers
    // ride in the structure and a client fetches a changed one by its hash.
    if file.section(SECTION_EXTENSIONS).is_some() {
        return Err(ClmError::NotAStructure);
    }
    let doc = decode_structure_section(&file, budget)?;
    let manifest = file
        .section(SECTION_TEXTURE_MANIFEST)
        .ok_or(ClmError::MissingSection("TextureManifest"))?;
    let textures: Vec<ClmTextureRef> = cbor_from_slice(manifest)?;
    budget.charge(
        crate::load_budget::LoadResource::Textures,
        textures.len() as u64,
    )?;
    Ok(ClmStructureFile { doc, textures })
}

/// The textures a structure names, in the model's own order, without
/// building anything from the structure — so a client can fetch the payloads
/// it lacks before it applies the structure. Each entry carries the encoding
/// and alpha convention the fetched bytes are read with.
pub fn structure_texture_ids(bytes: &[u8]) -> Result<Vec<ClmTextureRef>, ClmError> {
    let file = read_container(bytes)?;
    let manifest = file
        .section(SECTION_TEXTURE_MANIFEST)
        .ok_or(ClmError::MissingSection("TextureManifest"))?;
    cbor_from_slice(manifest)
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

/// [`decode_with_budget`], stopping short of copying the texture payloads
/// out of `bytes`. Whoever wants them owned copies each one once, into the
/// form it keeps: a `Vec` for a [`ClmFile`], an `Arc` for a
/// [`Model`](crate::Model).
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
    // Whatever this build has no name for is kept, not skipped: see
    // `ClmUnknownSection`. Copied out because the model that holds them
    // outlives `bytes`.
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

/// Read a `Textures` section, leaving each payload where it lies.
///
/// A CBOR reader hands a byte string over as a buffer it filled, and
/// ciborium fills that buffer through a 4 KiB scratch: every payload was
/// copied twice inside the decoder before the caller made its own copy. But
/// the section is in memory already and a payload is a contiguous run of it,
/// so [`texture_spans`] finds the runs and nothing is copied here at all.
///
/// That walk only understands the table as this crate writes it. Anything
/// else — an unknown field, an indefinite length, bytes spelled as an array
/// — goes to the full decoder, which stays the definition of what a texture
/// table is: the two agree wherever the walk answers, and where it does not
/// the decoder's verdict, error included, is the one returned.
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

/// A CBOR head of the `major` type with a definite argument, at the front of
/// `data`: the argument, and what follows the head.
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

/// A definite-length text or byte string at the front of `data`: its
/// content, and what follows it.
fn cbor_string(data: &[u8], major: u8) -> Option<(&[u8], &[u8])> {
    let (len, rest) = cbor_head(data, major)?;
    let len = usize::try_from(len).ok().filter(|len| *len <= rest.len())?;
    Some(rest.split_at(len))
}

/// The texture table, if it is laid out the way [`encode`] lays it out: a
/// definite-length array of definite-length maps, each with exactly the four
/// fields of a [`ClmTexture`], the payload a definite-length byte string.
/// `None` for anything else, which is not a verdict — see
/// [`decode_textures`].
///
/// Only the framing is read by hand. Each field's value is handed to the
/// same decoder as its raw CBOR item, so what an Id may contain or which
/// encodings exist is decided in one place.
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
            // Every other field is written as a text item.
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

/// Parse the `Structure` section, having charged its size first.
///
/// The order is the point. Every count the budget holds a structure to is
/// read out of the decoded tree, which is after the tree was allocated; the
/// section's own length is the one number available before, and a structure
/// decodes to a bounded multiple of it. `encoded_bytes` cannot stand in: it
/// is sized for a file full of textures. See
/// [`LoadLimits::structure_bytes`](crate::load_budget::LoadLimits::structure_bytes).
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
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::value::Value;

    fn cbor_to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, ClmError> {
        let mut out = Vec::new();
        ciborium::into_writer(value, &mut out).map_err(|e| ClmError::Encode(e.to_string()))?;
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

    /// Which form a writer was handed its payloads in cannot be told from
    /// the file.
    #[test]
    fn borrowed_payloads_write_the_same_file() {
        let (doc, textures) = sample();
        let borrowed: Vec<ClmTextureBytes> = textures.iter().map(Into::into).collect();
        assert_eq!(
            encode_borrowed(&doc, &borrowed, &[], &[]).unwrap(),
            encode(&doc, &textures, &[]).unwrap()
        );
    }

    /// The reader finds each payload where it lies in the file instead of
    /// having the CBOR decoder copy it out — twice — before the caller makes
    /// the copy it wanted.
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

    /// The in-place walk is a shortcut, never a second opinion: on every
    /// table it answers for, the full decoder gives the same textures, and
    /// on every table it declines, the full decoder's verdict is the one
    /// that stands.
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

    /// A texture table the reader would refuse is refused on save, not
    /// written with its section length truncated. The payload is zero pages
    /// the refusal never touches.
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

    /// A structure with no `animations`, `bindings` or `welds` key at all is
    /// a legal model with none of them.
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
