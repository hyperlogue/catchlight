//! String identity: the Ids a model file stores and addons compose by.
//!
//! The charset is narrow so an Id survives a file path, URL, JSON key and shell
//! argument unquoted. The `/` in a generated Id is a reading aid only: it is not
//! updated on reparent, and no code may split an Id on `/`. A [`SlotId`] is
//! unique only within its part. A [`Name`] is a label, never a key.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Caps what a corrupt file can allocate one name at a time.
pub const MAX_NAME_BYTES: usize = 256;

/// Caps an id as [`MAX_NAME_BYTES`] caps a name; a generated [`NodeId`] stops
/// carrying its parent's id rather than grow past it.
pub const MAX_ID_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    #[error("an id must not be empty")]
    Empty,
    #[error("an id must not start with '.'")]
    LeadingDot,
    /// Forbidden now because relaxing a rule later is additive.
    #[error("an id must not start with '/'")]
    LeadingSlash,
    /// A command line would read it as an option.
    #[error("an id must not start with '-'")]
    LeadingDash,
    #[error("invalid byte '{}' (0x{byte:02x}) at offset {offset} of an id", .byte.escape_ascii())]
    Byte {
        byte: u8,
        /// Zero-based byte offset.
        offset: usize,
    },
    #[error("an extension key needs a dot: the vendor comes first, as in `acme.camera`")]
    MissingDot,
    #[error("an extension key must not end with '.'")]
    TrailingDot,
    #[error("a name may be at most {max} bytes, got {bytes}")]
    TooLong { bytes: usize, max: usize },
    #[error("an id may be at most {max} bytes, got {bytes}")]
    IdTooLong { bytes: usize, max: usize },
}

const fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-')
}

pub fn validate_id(s: &str) -> Result<(), IdError> {
    if s.is_empty() {
        return Err(IdError::Empty);
    }
    if s.len() > MAX_ID_BYTES {
        return Err(IdError::IdTooLong {
            bytes: s.len(),
            max: MAX_ID_BYTES,
        });
    }
    match s.as_bytes()[0] {
        b'.' => return Err(IdError::LeadingDot),
        b'/' => return Err(IdError::LeadingSlash),
        b'-' => return Err(IdError::LeadingDash),
        _ => {}
    }
    for (offset, byte) in s.bytes().enumerate() {
        if !is_id_byte(byte) {
            return Err(IdError::Byte { byte, offset });
        }
    }
    Ok(())
}

/// [`EXTENSION_RESERVED_PREFIX`] is not checked here: a file may carry a
/// reserved key; refusing to author one is [`crate::Model::set_extension`]'s job.
pub fn validate_extension_key(s: &str) -> Result<(), IdError> {
    validate_id(s)?;
    if !s.contains('.') {
        return Err(IdError::MissingDot);
    }
    if s.ends_with('.') {
        return Err(IdError::TrailingDot);
    }
    Ok(())
}

pub const EXTENSION_RESERVED_PREFIX: &str = "catchlight.";

fn generated(s: String) -> Arc<str> {
    debug_assert!(validate_id(&s).is_ok(), "generated an invalid id: {s}");
    Arc::from(s)
}

macro_rules! string_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        // Named rather than inlined so a TypeScript reader sees which Id a field takes.
        #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(type = "string"))]
        pub struct $name(Arc<str>);

        impl $name {
            pub fn new(s: impl AsRef<str>) -> Result<Self, IdError> {
                let s = s.as_ref();
                validate_id(s)?;
                Ok(Self(Arc::from(s)))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, IdError> {
                Self::new(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        /// Sound because `Hash` and `Eq` both delegate to the same `str`.
        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                Self::new(s).map_err(serde::de::Error::custom)
            }
        }
    };
}

string_id!(
    NodeId,
    "The identity of a node. Unique within a model; generated as \
     `<parent id>/<kind>-<8 hex>` by [`NodeId::generate`], or without the \
     parent where that would be too long."
);
string_id!(
    ParamId,
    "The identity of a param. Unique within a model; generated as \
     `param-<8 hex>` by [`ParamId::generate`]."
);
string_id!(
    TexId,
    "The identity of a texture. Unique within a model; generated as \
     `tex-<8 hex>` by [`TexId::generate`]."
);
string_id!(
    SlotId,
    "The identity of a slot, unique within the part that owns it — not \
     within the model. Generated as `slot-<8 hex>` by [`SlotId::generate`]."
);

/// One of the Ids unique within a model, for an error that may name any of them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnyId {
    Node(NodeId),
    Param(ParamId),
    Texture(TexId),
}

impl AnyId {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Node(_) => "node",
            Self::Param(_) => "param",
            Self::Texture(_) => "texture",
        }
    }
}

impl fmt::Display for AnyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(id) => id.fmt(f),
            Self::Param(id) => id.fmt(f),
            Self::Texture(id) => id.fmt(f),
        }
    }
}

impl From<NodeId> for AnyId {
    fn from(id: NodeId) -> Self {
        Self::Node(id)
    }
}

impl From<ParamId> for AnyId {
    fn from(id: ParamId) -> Self {
        Self::Param(id)
    }
}

impl From<TexId> for AnyId {
    fn from(id: TexId) -> Self {
        Self::Texture(id)
    }
}

/// A reading aid in a generated [`NodeId`]; no code may parse it back out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeIdKind {
    Group,
    Part,
    Composite,
    MeshGroup,
    MotionResponse,
    Spine,
}

impl NodeIdKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Part => "part",
            Self::Composite => "composite",
            Self::MeshGroup => "mesh-group",
            Self::MotionResponse => "response",
            Self::Spine => "spine",
        }
    }
}

impl fmt::Display for NodeIdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a generated Id's 8 hex digits come from; supplied by the caller so tests can pin them.
pub trait HexSource {
    fn next_bits(&mut self) -> u32;
}

impl<F: FnMut() -> u32> HexSource for F {
    fn next_bits(&mut self) -> u32 {
        self()
    }
}

/// Deterministic; not a cryptographic generator.
#[derive(Debug, Clone)]
pub struct SeededHex(u32);

impl SeededHex {
    #[must_use]
    pub const fn new(seed: u32) -> Self {
        Self(seed)
    }
}

impl HexSource for SeededHex {
    /// `SplitMix32`: a Weyl step, then the murmur3 finalizer.
    fn next_bits(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9);
        let mut z = self.0;
        z = (z ^ (z >> 16)).wrapping_mul(0x85EB_CA6B);
        z = (z ^ (z >> 13)).wrapping_mul(0xC2B2_AE35);
        z ^ (z >> 16)
    }
}

impl NodeId {
    pub(crate) fn from_generated(s: String) -> Self {
        Self(generated(s))
    }

    /// Generates `<parent>/<kind>-<8 hex>`, or `<kind>-<8 hex>` where that would
    /// pass [`MAX_ID_BYTES`]. Uniqueness within the model is the caller's to check.
    pub fn generate(parent: &Self, kind: NodeIdKind, hex: &mut impl HexSource) -> Self {
        let own = format!("{}-{:08x}", kind.as_str(), hex.next_bits());
        if parent.0.len() + 1 + own.len() > MAX_ID_BYTES {
            return Self(generated(own));
        }
        Self(generated(format!("{}/{own}", parent.as_str())))
    }
}

impl ParamId {
    pub fn generate(hex: &mut impl HexSource) -> Self {
        Self(generated(format!("param-{:08x}", hex.next_bits())))
    }
}

impl TexId {
    pub fn generate(hex: &mut impl HexSource) -> Self {
        Self(generated(format!("tex-{:08x}", hex.next_bits())))
    }
}

impl SlotId {
    /// Freedom on the owning part is the model's to check; see
    /// [`Model::slot_add_generated`](crate::Model::slot_add_generated).
    pub fn generate(hex: &mut impl HexSource) -> Self {
        Self(generated(format!("slot-{:08x}", hex.next_bits())))
    }
}

/// An extension key: the Id charset plus a required interior dot, vendor first (`acme.camera`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(type = "string"))]
pub struct ExtensionKey(Arc<str>);

impl ExtensionKey {
    pub fn new(s: impl AsRef<str>) -> Result<Self, IdError> {
        let s = s.as_ref();
        validate_extension_key(s)?;
        Ok(Self(Arc::from(s)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_reserved(&self) -> bool {
        self.0.starts_with(EXTENSION_RESERVED_PREFIX)
    }
}

impl fmt::Display for ExtensionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ExtensionKey {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, IdError> {
        Self::new(s)
    }
}

impl AsRef<str> for ExtensionKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Sound because `Hash` and `Eq` both delegate to the same `str`.
impl Borrow<str> for ExtensionKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl Serialize for ExtensionKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ExtensionKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::new(&s).map_err(|e| serde::de::Error::custom(format!("extension key {s:?}: {e}")))
    }
}

/// A human-facing label. Deliberately not `Hash`: nothing may look anything up by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(Arc<str>);

impl Name {
    pub fn new(s: impl AsRef<str>) -> Result<Self, IdError> {
        let s = s.as_ref();
        if s.len() > MAX_NAME_BYTES {
            return Err(IdError::TooLong {
                bytes: s.len(),
                max: MAX_NAME_BYTES,
            });
        }
        Ok(Self(Arc::from(s)))
    }

    /// Truncates on a char boundary instead of failing.
    pub fn truncated(s: impl AsRef<str>) -> Self {
        let s = s.as_ref();
        if s.len() <= MAX_NAME_BYTES {
            return Self(Arc::from(s));
        }
        let mut end = MAX_NAME_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        Self(Arc::from(&s[..end]))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Name {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, IdError> {
        Self::new(s)
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for Name {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cbor_round_trip<T>(value: &T) -> T
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let mut buf = Vec::new();
        match ciborium::into_writer(value, &mut buf) {
            Ok(()) => {}
            Err(e) => panic!("cbor encode failed: {e}"),
        }
        match ciborium::from_reader(&buf[..]) {
            Ok(v) => v,
            Err(e) => panic!("cbor decode failed: {e}"),
        }
    }

    #[test]
    fn accepts_the_whole_charset() {
        for s in [
            "a",
            "Z",
            "0",
            "head",
            "head/part-3f9a2c1e",
            "a.b",
            "a-b_c",
            "UPPER/lower-9",
            "trailing.",
            "a//b",
            "_leading",
            "0123456789",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_./-",
        ] {
            assert!(NodeId::new(s).is_ok(), "{s:?} should be a valid id");
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(NodeId::new(""), Err(IdError::Empty));
        assert_eq!(ParamId::new(""), Err(IdError::Empty));
        assert_eq!(TexId::new(""), Err(IdError::Empty));
        assert_eq!(SlotId::new(""), Err(IdError::Empty));
    }

    #[test]
    fn rejects_leading_slash() {
        assert_eq!(NodeId::new("/"), Err(IdError::LeadingSlash));
        assert_eq!(NodeId::new("/head"), Err(IdError::LeadingSlash));
        assert_eq!(NodeId::new("//head"), Err(IdError::LeadingSlash));
        // Only *leading*: the `/` a generated id carries is ordinary.
        assert!(NodeId::new("head/part-3f9a2c1e").is_ok());
        assert!(NodeId::new("a//b").is_ok());
    }

    #[test]
    fn rejects_leading_dash() {
        assert_eq!(NodeId::new("-"), Err(IdError::LeadingDash));
        assert_eq!(NodeId::new("-hat"), Err(IdError::LeadingDash));
        assert_eq!(ParamId::new("--"), Err(IdError::LeadingDash));
        // Only *leading*: the `-` a generated id puts before its hex is
        // ordinary, and so is one anywhere else.
        assert!(NodeId::new("head/part-3f9a2c1e").is_ok());
        assert!(NodeId::new("a-b").is_ok());
        assert!(NodeId::new("trailing-").is_ok());
    }

    #[test]
    fn rejects_leading_dot() {
        assert_eq!(NodeId::new("."), Err(IdError::LeadingDot));
        assert_eq!(NodeId::new(".."), Err(IdError::LeadingDot));
        assert_eq!(NodeId::new("./head"), Err(IdError::LeadingDot));
        assert_eq!(NodeId::new("../head"), Err(IdError::LeadingDot));
        // Only *leading*: a dot anywhere else is ordinary.
        assert!(NodeId::new("head/.hidden").is_ok());
    }

    #[test]
    fn rejects_every_forbidden_character_class() {
        for s in ["a b", "a\tb", "a\nb", "a\rb", "a\0b", "a\x7fb"] {
            assert!(matches!(NodeId::new(s), Err(IdError::Byte { .. })), "{s:?}");
        }
        for c in [
            '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', ':', ';', '<', '=', '>',
            '?', '@', '[', '\\', ']', '^', '`', '{', '|', '}', '~',
        ] {
            let s = format!("a{c}b");
            assert!(
                matches!(NodeId::new(&s), Err(IdError::Byte { .. })),
                "{s:?} should be rejected"
            );
        }
        for s in ["héad", "頭", "a\u{200b}b", "emoji-\u{1f600}"] {
            assert!(matches!(NodeId::new(s), Err(IdError::Byte { .. })), "{s:?}");
        }
    }

    #[test]
    fn error_names_the_offending_byte() {
        assert_eq!(
            NodeId::new("ab cd"),
            Err(IdError::Byte {
                byte: b' ',
                offset: 2
            })
        );
        // The first byte of a multi-byte char is the one reported.
        assert_eq!(
            NodeId::new("hé"),
            Err(IdError::Byte {
                byte: 0xc3,
                offset: 1
            })
        );
        let message = IdError::Byte {
            byte: b' ',
            offset: 2,
        }
        .to_string();
        assert!(message.contains("0x20"), "{message}");
        assert!(message.contains("offset 2"), "{message}");
    }

    #[test]
    fn ids_are_case_sensitive() {
        let lower = NodeId::new("head").unwrap();
        let upper = NodeId::new("Head").unwrap();
        assert_ne!(lower, upper);
        assert_ne!(lower.as_str(), upper.as_str());
        // Ordering is the string's, so case matters there too.
        assert!(upper < lower);
        let mut map = HashMap::new();
        map.insert(lower.clone(), 1);
        assert_eq!(map.get(&upper), None);
        assert_eq!(map.get(&lower), Some(&1));
        // `Borrow<str>` lets a lookup skip building an Id.
        assert_eq!(map.get("head"), Some(&1));
        assert_eq!(map.get("Head"), None);
    }

    #[test]
    fn generated_node_ids_have_the_documented_shape() {
        let parent = NodeId::new("head").unwrap();
        let mut hex = SeededHex::new(7);
        for (kind, segment) in [
            (NodeIdKind::Group, "group"),
            (NodeIdKind::Part, "part"),
            (NodeIdKind::Composite, "composite"),
            (NodeIdKind::MeshGroup, "mesh-group"),
            (NodeIdKind::MotionResponse, "response"),
            (NodeIdKind::Spine, "spine"),
        ] {
            let id = NodeId::generate(&parent, kind, &mut hex);
            let s = id.as_str();
            let Some(rest) = s.strip_prefix("head/") else {
                panic!("{s} should carry its parent as a prefix")
            };
            let Some(digits) = rest.strip_prefix(&format!("{segment}-")) else {
                panic!("{s} should name its kind")
            };
            assert_eq!(digits.len(), 8, "{s} should end in 8 hex digits");
            assert!(digits.bytes().all(|b| b.is_ascii_hexdigit()), "{s}");
            assert!(digits.bytes().all(|b| !b.is_ascii_uppercase()), "{s}");
            assert_eq!(NodeId::new(s), Ok(id));
        }
    }

    #[test]
    fn generated_param_and_texture_ids_have_the_documented_shape() {
        let mut hex = SeededHex::new(1);
        let param = ParamId::generate(&mut hex);
        let tex = TexId::generate(&mut hex);
        assert!(param.as_str().starts_with("param-"), "{param}");
        assert!(tex.as_str().starts_with("tex-"), "{tex}");
        assert_eq!(param.as_str().len(), "param-".len() + 8);
        assert_eq!(tex.as_str().len(), "tex-".len() + 8);
        assert_eq!(ParamId::new(param.as_str()), Ok(param));
        assert_eq!(TexId::new(tex.as_str()), Ok(tex));
    }

    #[test]
    fn a_deep_parent_prefix_is_copied_verbatim() {
        let parent = NodeId::new("body/head/part-00000001").unwrap();
        let mut hex = SeededHex::new(3);
        let child = NodeId::generate(&parent, NodeIdKind::Part, &mut hex);
        assert!(
            child.as_str().starts_with("body/head/part-00000001/part-"),
            "{child}"
        );
    }

    #[test]
    fn generation_is_deterministic_under_a_seed() {
        let parent = NodeId::new("head").unwrap();
        let run = |seed| {
            let mut hex = SeededHex::new(seed);
            let a = NodeId::generate(&parent, NodeIdKind::Part, &mut hex);
            let b = NodeId::generate(&parent, NodeIdKind::Part, &mut hex);
            let c = ParamId::generate(&mut hex);
            (a, b, c)
        };
        assert_eq!(run(42), run(42), "one seed, one sequence");
        assert_ne!(run(42).0, run(43).0, "different seeds diverge");
        let (a, b, _) = run(42);
        assert_ne!(a, b, "the source advances between calls");
    }

    #[test]
    fn any_closure_is_a_hex_source() {
        let parent = NodeId::new("head").unwrap();
        let mut fixed = || 0xdead_beef_u32;
        let id = NodeId::generate(&parent, NodeIdKind::Composite, &mut fixed);
        assert_eq!(id.as_str(), "head/composite-deadbeef");
    }

    #[test]
    fn json_round_trip_is_a_plain_string() {
        let node = NodeId::new("head/part-3f9a2c1e").unwrap();
        assert_eq!(
            serde_json::to_string(&node).unwrap(),
            "\"head/part-3f9a2c1e\""
        );
        assert_eq!(
            serde_json::from_str::<NodeId>("\"head/part-3f9a2c1e\"").unwrap(),
            node
        );

        let param = ParamId::new("head-turn").unwrap();
        let tex = TexId::new("tex-0000000f").unwrap();
        let slot = SlotId::new("collar.03").unwrap();
        let name = Name::new("Head — 頭 \u{1f600}").unwrap();
        assert_eq!(
            serde_json::from_str::<ParamId>(&serde_json::to_string(&param).unwrap()).unwrap(),
            param
        );
        assert_eq!(
            serde_json::from_str::<TexId>(&serde_json::to_string(&tex).unwrap()).unwrap(),
            tex
        );
        assert_eq!(
            serde_json::from_str::<SlotId>(&serde_json::to_string(&slot).unwrap()).unwrap(),
            slot
        );
        assert_eq!(
            serde_json::from_str::<Name>(&serde_json::to_string(&name).unwrap()).unwrap(),
            name
        );
    }

    #[test]
    fn cbor_round_trip_is_a_text_string() {
        let node = NodeId::new("head/part-3f9a2c1e").unwrap();
        assert_eq!(cbor_round_trip(&node), node);
        assert_eq!(
            cbor_round_trip(&ParamId::new("head-turn").unwrap()),
            ParamId::new("head-turn").unwrap()
        );
        assert_eq!(
            cbor_round_trip(&TexId::new("tex-0000000f").unwrap()),
            TexId::new("tex-0000000f").unwrap()
        );
        assert_eq!(
            cbor_round_trip(&SlotId::new("collar.03").unwrap()),
            SlotId::new("collar.03").unwrap()
        );
        assert_eq!(
            cbor_round_trip(&Name::new("Head 頭").unwrap()),
            Name::new("Head 頭").unwrap()
        );

        let mut buf = Vec::new();
        ciborium::into_writer(&node, &mut buf).unwrap();
        let value: ciborium::value::Value = ciborium::from_reader(&buf[..]).unwrap();
        assert_eq!(value.as_text(), Some("head/part-3f9a2c1e"));
    }

    #[test]
    fn deserializing_validates() {
        for bad in ["\"\"", "\".hidden\"", "\"has space\"", "\"a:b\""] {
            let err = serde_json::from_str::<NodeId>(bad).unwrap_err();
            assert!(err.to_string().len() > 1, "{bad} should be rejected");
        }
        let mut buf = Vec::new();
        ciborium::into_writer(&"has space", &mut buf).unwrap();
        assert!(ciborium::from_reader::<NodeId, _>(&buf[..]).is_err());
    }

    #[test]
    fn a_name_is_capped_and_otherwise_free() {
        for s in ["", "Head", "  spaces  ", "頭 / \u{1f600}", "a\nb"] {
            assert!(Name::new(s).is_ok(), "{s:?} should be a valid name");
        }
        let at_cap = "n".repeat(MAX_NAME_BYTES);
        assert!(Name::new(&at_cap).is_ok());
        let over_cap = "n".repeat(MAX_NAME_BYTES + 1);
        assert_eq!(
            Name::new(&over_cap),
            Err(IdError::TooLong {
                bytes: MAX_NAME_BYTES + 1,
                max: MAX_NAME_BYTES
            })
        );
        // The cap counts bytes, not chars.
        let multibyte = "é".repeat(MAX_NAME_BYTES / 2 + 1);
        assert!(matches!(
            Name::new(&multibyte),
            Err(IdError::TooLong { .. })
        ));
        assert!(Name::new("").unwrap().is_empty());
        assert!(Name::default().is_empty());
    }

    #[test]
    fn truncating_a_name_keeps_char_boundaries() {
        // The leading ASCII byte puts the cap in the middle of a two-byte char.
        let long = format!("a{}", "é".repeat(MAX_NAME_BYTES));
        let name = Name::truncated(&long);
        assert_eq!(name.as_str().len(), MAX_NAME_BYTES - 1);
        assert!(long.starts_with(name.as_str()));
        assert_eq!(Name::truncated("short").as_str(), "short");
    }

    #[test]
    fn an_id_is_capped_and_a_deep_generated_id_drops_its_parent() {
        assert!(NodeId::new("n".repeat(MAX_ID_BYTES)).is_ok());
        assert_eq!(
            NodeId::new("n".repeat(MAX_ID_BYTES + 1)),
            Err(IdError::IdTooLong {
                bytes: MAX_ID_BYTES + 1,
                max: MAX_ID_BYTES
            })
        );
        let mut buf = Vec::new();
        ciborium::into_writer(&"n".repeat(MAX_ID_BYTES + 1), &mut buf).unwrap();
        assert!(ciborium::from_reader::<ParamId, _>(&buf[..]).is_err());

        let mut hex = || 0xabcd_0123;
        // `/part-abcd0123` is 14 bytes.
        let roomy = NodeId::new("n".repeat(MAX_ID_BYTES - 14)).unwrap();
        let child = NodeId::generate(&roomy, NodeIdKind::Part, &mut hex);
        assert_eq!(child.as_str().len(), MAX_ID_BYTES);
        assert!(child.as_str().starts_with(roomy.as_str()));
        let deep = NodeId::new("n".repeat(MAX_ID_BYTES - 13)).unwrap();
        assert_eq!(
            NodeId::generate(&deep, NodeIdKind::Part, &mut hex).as_str(),
            "part-abcd0123"
        );
    }

    #[test]
    fn parsing_and_displaying_are_inverse() {
        let s = "head/mesh-group-00000001";
        let id: NodeId = s.parse().unwrap();
        assert_eq!(id.to_string(), s);
        assert_eq!("Head".parse::<Name>().unwrap().to_string(), "Head");
        assert!("a b".parse::<NodeId>().is_err());
        assert_eq!(NodeIdKind::MeshGroup.to_string(), "mesh-group");
    }
}
