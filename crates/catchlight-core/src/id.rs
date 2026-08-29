//! String identity: the Ids a model file stores and addons compose by.
//!
//! A node, param or texture is addressed by an [`NodeId`] / [`ParamId`] /
//! [`TexId`] — a string the author chose or the editor generated. Ids are
//! what the file stores and what an addon names when it reaches into a base
//! model, so they must survive everything that is not a deliberate rename:
//! moving a node, re-authoring a mesh, reordering a tree. Positional indices
//! (`NodeIdx` and friends) exist only inside a puppet or a render cache and
//! never reach a file.
//!
//! Invariants this module enforces:
//!
//! - **Charset.** An Id is non-empty, is made only of `[A-Za-z0-9_./-]`, and
//!   starts with neither `.` nor `/`. Every constructor validates; [`IdError`]
//!   names the offending byte and its offset. The set is deliberately narrow:
//!   an Id has to survive a file path, a URL, a JSON key and a shell argument
//!   without quoting. A leading `-` is left alone: it is a CLI hazard, and the
//!   CLI ends its option list with `--` rather than the format narrowing the
//!   charset for it.
//! - **Case-sensitive.** `Head` and `head` are two different Ids.
//! - **The `/` in an Id is not a path.** A generated Id carries its parent's
//!   Id as a prefix (`head/part-3f9a2c1e`) purely as a reading aid. It is
//!   *not* updated when the node is reparented, and no code may split an Id
//!   on `/` to learn anything about the tree. Ask the model for the parent.
//! - **Generation is caller-driven.** The 8 hex digits come from a
//!   [`HexSource`] the caller supplies, so a test can pin them with
//!   [`SeededHex`] and this crate needs no randomness dependency. Generated
//!   Ids are valid by construction, so the generators return an Id, not a
//!   `Result`.
//! - **Seams and slots are scoped.** A [`SeamId`] is unique within its part
//!   and a [`SlotId`] within its seam; neither is unique across a model.
//!   Nothing here can check that — the model that owns them does.
//! - **A [`Name`] is a label, never a key.** It is capped at
//!   [`MAX_NAME_BYTES`] and otherwise unconstrained: any text, empty,
//!   duplicated freely. It deliberately does not implement `Hash`, so it
//!   cannot quietly become a `HashMap` key; nothing may look anything up by
//!   name.
//!
//! Renaming an Id is an author's decision and a breaking change for any
//! addon that referenced the old one. There are no aliases.

use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The most bytes a [`Name`] may hold. Long enough that no real label hits
/// it; short enough that a corrupt file cannot allocate its way through
/// memory one name at a time.
pub const MAX_NAME_BYTES: usize = 256;

/// Why a string could not become an Id or a [`Name`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    /// An Id must have at least one byte.
    #[error("an id must not be empty")]
    Empty,
    /// A leading `.` is reserved: it is what a relative path starts with,
    /// and an Id is not a path.
    #[error("an id must not start with '.'")]
    LeadingDot,
    /// A leading `/` is reserved for the same reason: it is what an absolute
    /// path starts with. Interior `/` is ordinary — it is how a generated Id
    /// carries its parent — but an Id that opens with one reads as rooted at
    /// something, and it is not. Forbidden now because relaxing a rule later
    /// is additive while tightening one is not.
    #[error("an id must not start with '/'")]
    LeadingSlash,
    /// A byte outside `[A-Za-z0-9_./-]`.
    #[error("invalid byte '{}' (0x{byte:02x}) at offset {offset} of an id", .byte.escape_ascii())]
    Byte {
        /// The rejected byte, as it appeared in the input.
        byte: u8,
        /// Its zero-based byte offset in the input.
        offset: usize,
    },
    /// Only [`Name`] produces this; Ids have no length cap.
    #[error("a name may be at most {max} bytes, got {bytes}")]
    TooLong {
        /// The input's length in bytes.
        bytes: usize,
        /// The cap, [`MAX_NAME_BYTES`].
        max: usize,
    },
}

/// Whether `b` may appear in an Id.
const fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-')
}

/// The one place the Id charset is decided.
pub fn validate_id(s: &str) -> Result<(), IdError> {
    if s.is_empty() {
        return Err(IdError::Empty);
    }
    match s.as_bytes()[0] {
        b'.' => return Err(IdError::LeadingDot),
        b'/' => return Err(IdError::LeadingSlash),
        _ => {}
    }
    for (offset, byte) in s.bytes().enumerate() {
        if !is_id_byte(byte) {
            return Err(IdError::Byte { byte, offset });
        }
    }
    Ok(())
}

/// Wraps a string a generator in this module just built. The
/// `debug_assert` catches a generator that stopped producing valid Ids in
/// a test rather than in a file.
fn generated(s: String) -> Arc<str> {
    debug_assert!(validate_id(&s).is_ok(), "generated an invalid id: {s}");
    Arc::from(s)
}

/// Defines one Id newtype over `Arc<str>`: cheap to clone, ordered and
/// hashed as its string, serialized as a plain string, and validated on the
/// way in — including on the way in from a file.
macro_rules! string_id {
    ($name:ident, $doc:expr) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Arc<str>);

        impl $name {
            /// Validates `s` against the Id charset.
            pub fn new(s: impl AsRef<str>) -> Result<Self, IdError> {
                let s = s.as_ref();
                validate_id(s)?;
                Ok(Self(Arc::from(s)))
            }

            /// The Id exactly as it is written in the file.
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

        /// Sound because `Hash` and `Eq` both delegate to the same `str`,
        /// which lets `HashMap<$name, _>::get` take a `&str`.
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
     `<parent id>/<kind>-<8 hex>` by [`NodeId::generate`]."
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
    SeamId,
    "The identity of a seam, unique within the part that owns it — not \
     within the model. Always authored."
);
string_id!(
    SlotId,
    "The identity of a slot, unique within the seam that owns it — not \
     within the part or the model. Always authored."
);

/// The `kind` segment of a generated [`NodeId`]. It records what the node
/// was when it was created, which is also all a node's kind ever is; like
/// the parent prefix it is a reading aid and no code may parse it back out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NodeIdKind {
    /// A node with no geometry of its own.
    Group,
    /// A node that draws a mesh with a texture.
    Part,
    /// A node that renders its subtree as one image.
    Composite,
    /// A node whose mesh deforms the geometry beneath it.
    MeshGroup,
    /// A driver node holding a pendulum.
    SimplePhysics,
}

impl NodeIdKind {
    /// The segment as it appears in a generated Id.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Part => "part",
            Self::Composite => "composite",
            Self::MeshGroup => "mesh-group",
            Self::SimplePhysics => "physics",
        }
    }
}

impl fmt::Display for NodeIdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a generated Id's 8 hex digits come from.
///
/// Supplied by the caller so this crate depends on no random number
/// generator and so a test can make generation reproducible with
/// [`SeededHex`]. Any `FnMut() -> u32` is a `HexSource`.
pub trait HexSource {
    /// The next 32 bits, rendered as the 8 hex digits of one Id.
    fn next_bits(&mut self) -> u32;
}

impl<F: FnMut() -> u32> HexSource for F {
    fn next_bits(&mut self) -> u32 {
        self()
    }
}

/// A deterministic [`HexSource`]: the same seed always yields the same
/// sequence of Ids. For tests and for any caller that wants reproducible
/// output; it is not a cryptographic generator.
#[derive(Debug, Clone)]
pub struct SeededHex(u32);

impl SeededHex {
    /// Starts the sequence at `seed`.
    pub const fn new(seed: u32) -> Self {
        Self(seed)
    }
}

impl HexSource for SeededHex {
    /// SplitMix32: a Weyl step, then the murmur3 finalizer.
    fn next_bits(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9);
        let mut z = self.0;
        z = (z ^ (z >> 16)).wrapping_mul(0x85EB_CA6B);
        z = (z ^ (z >> 13)).wrapping_mul(0xC2B2_AE35);
        z ^ (z >> 16)
    }
}

/// Wraps a string this crate just built to the charset, for the Ids the
/// crate mints itself: generated Ids and the ones the `.clp` bridge derives
/// from arena indices. Seam and slot Ids are always authored, so they have
/// none. [`generated`] debug-asserts the charset, so a minter that drifts off
/// it fails a test rather than writing a file.
macro_rules! from_generated {
    ($t:ty) => {
        impl $t {
            pub(crate) fn from_generated(s: String) -> Self {
                Self(generated(s))
            }
        }
    };
}
from_generated!(NodeId);
from_generated!(ParamId);
from_generated!(TexId);

impl NodeId {
    /// Generates `<parent>/<kind>-<8 hex>`.
    ///
    /// The prefix is a reading aid only: it is not updated if the node is
    /// later reparented, and nothing may parse it. Uniqueness within the
    /// model is the caller's to check — 32 bits collide eventually.
    pub fn generate(parent: &NodeId, kind: NodeIdKind, hex: &mut impl HexSource) -> Self {
        Self(generated(format!(
            "{}/{}-{:08x}",
            parent.as_str(),
            kind.as_str(),
            hex.next_bits()
        )))
    }
}

impl ParamId {
    /// Generates `param-<8 hex>`. Params have no parent, so no prefix.
    pub fn generate(hex: &mut impl HexSource) -> Self {
        Self(generated(format!("param-{:08x}", hex.next_bits())))
    }
}

impl TexId {
    /// Generates `tex-<8 hex>`. Textures have no parent, so no prefix.
    pub fn generate(hex: &mut impl HexSource) -> Self {
        Self(generated(format!("tex-{:08x}", hex.next_bits())))
    }
}

/// The label a human sees on a node or param — free to change, free to
/// repeat, capped at [`MAX_NAME_BYTES`] and otherwise any text at all.
///
/// Deliberately not `Hash`: nothing may look anything up by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Name(Arc<str>);

impl Name {
    /// Fails only if `s` is longer than [`MAX_NAME_BYTES`].
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

    /// Truncates instead of failing, on a char boundary, for input a person
    /// is typing rather than a file supplying.
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

    /// The label as authored.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the label is empty; an unnamed node is normal.
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
            "-leading",
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
        assert_eq!(SeamId::new(""), Err(IdError::Empty));
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
        // Whitespace and control bytes.
        for s in ["a b", "a\tb", "a\nb", "a\rb", "a\0b", "a\x7fb"] {
            assert!(matches!(NodeId::new(s), Err(IdError::Byte { .. })), "{s:?}");
        }
        // Punctuation outside `_./-`.
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
        // Anything outside ASCII, including the letters other alphabets use.
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
            (NodeIdKind::SimplePhysics, "physics"),
        ] {
            let id = NodeId::generate(&parent, kind, &mut hex);
            let s = id.as_str();
            let rest = match s.strip_prefix("head/") {
                Some(rest) => rest,
                None => panic!("{s} should carry its parent as a prefix"),
            };
            let digits = match rest.strip_prefix(&format!("{segment}-")) {
                Some(digits) => digits,
                None => panic!("{s} should name its kind"),
            };
            assert_eq!(digits.len(), 8, "{s} should end in 8 hex digits");
            assert!(digits.bytes().all(|b| b.is_ascii_hexdigit()), "{s}");
            assert!(digits.bytes().all(|b| !b.is_ascii_uppercase()), "{s}");
            // Whatever we generate has to survive being read back.
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
        // The prefix is a reading aid, so it is whatever the parent's Id is
        // — nested or not, generated or authored.
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
        let seam = SeamId::new("collar").unwrap();
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
            serde_json::from_str::<SeamId>(&serde_json::to_string(&seam).unwrap()).unwrap(),
            seam
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
            cbor_round_trip(&SeamId::new("collar").unwrap()),
            SeamId::new("collar").unwrap()
        );
        assert_eq!(
            cbor_round_trip(&SlotId::new("collar.03").unwrap()),
            SlotId::new("collar.03").unwrap()
        );
        assert_eq!(
            cbor_round_trip(&Name::new("Head 頭").unwrap()),
            Name::new("Head 頭").unwrap()
        );

        // Not a map, not a tagged value: the wire form is one text string.
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
        // Anything a person can type, including what an Id forbids.
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
        let long = "é".repeat(MAX_NAME_BYTES);
        let name = Name::truncated(&long);
        assert!(name.as_str().len() <= MAX_NAME_BYTES);
        assert!(name.as_str().chars().all(|c| c == 'é'));
        assert_eq!(Name::truncated("short").as_str(), "short");
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
