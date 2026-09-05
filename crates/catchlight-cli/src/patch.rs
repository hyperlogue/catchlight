//! `patch <file> <id> <field> <value>` — set one field on one node or param.
//!
//! The edit happens on the decoded structure document and nowhere else: the
//! texture table is carried from the input to the output untouched, so the
//! cost of patching a model is the cost of copying its bytes and never the
//! cost of decoding its images.
//!
//! What this module is responsible for:
//!
//! - **One field, named by its path.** `z_order`, `translation.x`, `tint.g`,
//!   `angle_damping`; on a param `min`, `max`, `default`, `name`. Which fields
//!   a node has depends on its kind, and an unknown one is refused with the
//!   list it does have rather than being silently ignored.
//! - **The value is parsed to the field's own type.** A number, `true`/`false`,
//!   free text, or one of a small enum's names; anything else is
//!   [`Error::BadValue`](crate::Error::BadValue) naming the field and what it
//!   takes.
//! - **The only cross-reference it will touch is `albedo`**, and it refuses an
//!   Id the file does not carry. Everything else `patch` writes is a scalar,
//!   because changing what names what is what `extract` and `merge` are for.
//! - **A patch that would break the file is refused, not saved.** The edited
//!   document is rebuilt into a [`Model`](catchlight_core::Model) — which
//!   resolves every Id and runs every load-time invariant, and decodes no
//!   images — before anything is written. A `min` above `max`, a name past the
//!   256-byte cap, an albedo naming nothing: caught here, with the reader's
//!   own error.
//!
//! A node Id and a param Id are separate namespaces, so one string can name
//! both. `patch` resolves whichever exists, and asks with
//! [`Error::AmbiguousId`](crate::Error::AmbiguousId) when both do.

use std::path::Path;

use catchlight_core::components::BlendMode;
use catchlight_core::formats::clm::{ClmFile, ClmNode, ClmNodeKind, ClmParam, ClmTexture};
use catchlight_core::id::{TexId, MAX_NAME_BYTES};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};

use crate::diff::enum_name;
use crate::{file, Error};

/// Which namespace an Id was resolved in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Node,
    Param,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Param => "param",
        }
    }
}

/// What one `patch` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: Kind,
    pub id: String,
    pub field: String,
    pub before: String,
    pub after: String,
}

impl Change {
    /// Whether the value actually moved. A patch that sets a field to what it
    /// already held still rewrites the file — byte for byte the same file.
    pub fn changed(&self) -> bool {
        self.before != self.after
    }
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:?} {}: {} -> {}",
            self.kind.as_str(),
            self.id,
            self.field,
            self.before,
            self.after
        )
    }
}

/// Read `path`, set the field, check the result still loads, and write it —
/// to `out` if given, over the input otherwise.
pub fn run(
    path: &Path,
    id: &str,
    field: &str,
    value: &str,
    want: Option<Kind>,
    out: Option<&Path>,
) -> Result<Change, Error> {
    let mut clm = file::read(path)?;
    let change = patch(&mut clm, path, id, field, value, want)?;
    verify(&clm, path)?;

    let dest = out.unwrap_or(path);
    let bytes = file::encode(&clm, dest)?;
    file::write(dest, &bytes)?;
    Ok(change)
}

/// Set one field on an already-decoded file.
pub fn patch(
    clm: &mut ClmFile,
    path: &Path,
    id: &str,
    field: &str,
    value: &str,
    want: Option<Kind>,
) -> Result<Change, Error> {
    let kind = resolve(clm, path, id, want)?;
    // Extensions are not a patchable field, so they are carried through
    // untouched: a patch of an unrelated field leaves them byte-identical.
    let ClmFile {
        doc,
        textures,
        extensions: _,
    } = clm;

    let (before, after) = match kind {
        Kind::Node => {
            let Some(node) = doc.nodes.iter_mut().find(|n| n.id.as_str() == id) else {
                return Err(Error::NoSuchId {
                    path: path.to_path_buf(),
                    kind: "node",
                    id: id.to_string(),
                });
            };
            let owner = format!("node {:?} ({})", id, kind_name(&node.kind));
            let known = node_fields(&node.kind);
            let Some(slot) = node_slot(node, field) else {
                return Err(Error::NoSuchField {
                    owner,
                    field: field.to_string(),
                    known,
                });
            };
            apply(slot, field, value, textures)?
        }
        Kind::Param => {
            let Some(param) = doc.params.iter_mut().find(|p| p.id.as_str() == id) else {
                return Err(Error::NoSuchId {
                    path: path.to_path_buf(),
                    kind: "param",
                    id: id.to_string(),
                });
            };
            let Some(slot) = param_slot(param, field) else {
                return Err(Error::NoSuchField {
                    owner: format!("param {id:?}"),
                    field: field.to_string(),
                    known: PARAM_FIELDS.to_vec(),
                });
            };
            apply(slot, field, value, textures)?
        }
    };

    Ok(Change {
        kind,
        id: id.to_string(),
        field: field.to_string(),
        before,
        after,
    })
}

/// Which namespace `id` names, refusing to guess when it names both.
fn resolve(clm: &ClmFile, path: &Path, id: &str, want: Option<Kind>) -> Result<Kind, Error> {
    let node = clm.doc.nodes.iter().any(|n| n.id.as_str() == id);
    let param = clm.doc.params.iter().any(|p| p.id.as_str() == id);
    let missing = |kind| {
        Err(Error::NoSuchId {
            path: path.to_path_buf(),
            kind,
            id: id.to_string(),
        })
    };
    match (want, node, param) {
        (Some(Kind::Node), true, _) => Ok(Kind::Node),
        (Some(Kind::Node), false, _) => missing("node"),
        (Some(Kind::Param), _, true) => Ok(Kind::Param),
        (Some(Kind::Param), _, false) => missing("param"),
        (None, true, false) => Ok(Kind::Node),
        (None, false, true) => Ok(Kind::Param),
        (None, false, false) => missing("node or param"),
        (None, true, true) => Err(Error::AmbiguousId {
            path: path.to_path_buf(),
            id: id.to_string(),
        }),
    }
}

/// Rebuild a Model from the patched document, so a patch that breaks a
/// load-time invariant is refused instead of written.
///
/// If the file did not load before the patch either, that is what gets
/// reported — otherwise the message would blame the patch for damage it did
/// not do.
fn verify(clm: &ClmFile, path: &Path) -> Result<(), Error> {
    let source = match file::load(clm, path) {
        Ok(_) => return Ok(()),
        Err(Error::NotAModel { source, .. } | Error::NotAFragment { source, .. }) => source,
        Err(other) => return Err(other),
    };
    if let Ok(original) = file::read(path) {
        match file::load(&original, path) {
            Ok(_) => {}
            Err(Error::NotAModel { source, .. } | Error::NotAFragment { source, .. }) => {
                return Err(Error::AlreadyBroken {
                    path: path.to_path_buf(),
                    source,
                })
            }
            Err(other) => return Err(other),
        }
    }
    Err(Error::PatchBreaksFile {
        path: path.to_path_buf(),
        source,
    })
}

// ---- fields ---------------------------------------------------------------

/// One writable field of a node or param, and the type it takes.
enum Slot<'a> {
    F32(&'a mut f32),
    Bool(&'a mut bool),
    Text(&'a mut String),
    Blend(&'a mut BlendMode),
    Pendulum(&'a mut PendulumKind),
    MapMode(&'a mut PhysicsParamMapMode),
    Albedo(&'a mut Option<TexId>),
}

const BLEND_MODES: &[&str] = &[
    "Normal",
    "Multiply",
    "ColorDodge",
    "LinearDodge",
    "Screen",
    "ClipToLower",
    "SliceFromLower",
    "Overlay",
    "ColorBurn",
    "LinearBurn",
    "Darken",
    "Lighten",
    "Add",
    "Inverse",
    "Subtract",
];
const PENDULUM_KINDS: &[&str] = &["RigidPendulum", "SpringPendulum"];
const MAP_MODES: &[&str] = &["XY", "YX", "AngleLength", "LengthAngle"];

/// Fields every node has, whatever its kind.
const COMMON_FIELDS: &[&str] = &[
    "enabled",
    "lock_to_root",
    "name",
    "rotation.x",
    "rotation.y",
    "rotation.z",
    "scale.x",
    "scale.y",
    "translation.x",
    "translation.y",
    "translation.z",
    "z_order",
];
const PART_FIELDS: &[&str] = &[
    "albedo",
    "blend_mode",
    "mask_threshold",
    "opacity",
    "screen_tint.b",
    "screen_tint.g",
    "screen_tint.r",
    "tint.b",
    "tint.g",
    "tint.r",
];
const COMPOSITE_FIELDS: &[&str] = &[
    "blend_mode",
    "mask_threshold",
    "opacity",
    "propagate_meshgroup",
    "screen_tint.b",
    "screen_tint.g",
    "screen_tint.r",
    "tint.b",
    "tint.g",
    "tint.r",
];
const MESH_GROUP_FIELDS: &[&str] = &["translate_children"];
const PHYSICS_FIELDS: &[&str] = &[
    "angle_damping",
    "frequency",
    "gravity",
    "length",
    "length_damping",
    "local_only",
    "map_mode",
    "output_scale.x",
    "output_scale.y",
    "pendulum",
];
/// The fields a param has. `key_positions` is a list, not a scalar, so it is
/// not patchable here.
pub const PARAM_FIELDS: &[&str] = &["default", "max", "min", "name"];

/// Every field this node accepts, sorted — what an unknown field is reported
/// against.
pub fn node_fields(kind: &ClmNodeKind) -> Vec<&'static str> {
    let mut all: Vec<&'static str> = COMMON_FIELDS.to_vec();
    all.extend_from_slice(kind_fields(kind));
    all.sort_unstable();
    all
}

fn kind_fields(kind: &ClmNodeKind) -> &'static [&'static str] {
    match kind {
        ClmNodeKind::Group => &[],
        ClmNodeKind::Part(_) => PART_FIELDS,
        ClmNodeKind::Composite(_) => COMPOSITE_FIELDS,
        ClmNodeKind::MeshGroup(_) => MESH_GROUP_FIELDS,
        ClmNodeKind::SimplePhysics(_) => PHYSICS_FIELDS,
    }
}

fn kind_name(kind: &ClmNodeKind) -> &'static str {
    match kind {
        ClmNodeKind::Group => "group",
        ClmNodeKind::Part(_) => "part",
        ClmNodeKind::Composite(_) => "composite",
        ClmNodeKind::MeshGroup(_) => "mesh group",
        ClmNodeKind::SimplePhysics(_) => "simple physics",
    }
}

fn node_slot<'a>(node: &'a mut ClmNode, field: &str) -> Option<Slot<'a>> {
    if COMMON_FIELDS.contains(&field) {
        return common_slot(node, field);
    }
    kind_slot(&mut node.kind, field)
}

fn common_slot<'a>(node: &'a mut ClmNode, field: &str) -> Option<Slot<'a>> {
    Some(match field {
        "name" => Slot::Text(&mut node.name),
        "enabled" => Slot::Bool(&mut node.enabled),
        "lock_to_root" => Slot::Bool(&mut node.lock_to_root),
        "z_order" => Slot::F32(&mut node.z_order),
        "translation.x" => Slot::F32(&mut node.transform.translation[0]),
        "translation.y" => Slot::F32(&mut node.transform.translation[1]),
        "translation.z" => Slot::F32(&mut node.transform.translation[2]),
        "rotation.x" => Slot::F32(&mut node.transform.rotation[0]),
        "rotation.y" => Slot::F32(&mut node.transform.rotation[1]),
        "rotation.z" => Slot::F32(&mut node.transform.rotation[2]),
        "scale.x" => Slot::F32(&mut node.transform.scale[0]),
        "scale.y" => Slot::F32(&mut node.transform.scale[1]),
        _ => return None,
    })
}

fn kind_slot<'a>(kind: &'a mut ClmNodeKind, field: &str) -> Option<Slot<'a>> {
    Some(match kind {
        ClmNodeKind::Group => return None,
        ClmNodeKind::Part(p) => match field {
            "opacity" => Slot::F32(&mut p.opacity),
            "blend_mode" => Slot::Blend(&mut p.blend_mode),
            "mask_threshold" => Slot::F32(&mut p.mask_threshold),
            "tint.r" => Slot::F32(&mut p.tint[0]),
            "tint.g" => Slot::F32(&mut p.tint[1]),
            "tint.b" => Slot::F32(&mut p.tint[2]),
            "screen_tint.r" => Slot::F32(&mut p.screen_tint[0]),
            "screen_tint.g" => Slot::F32(&mut p.screen_tint[1]),
            "screen_tint.b" => Slot::F32(&mut p.screen_tint[2]),
            "albedo" => Slot::Albedo(&mut p.albedo),
            _ => return None,
        },
        ClmNodeKind::Composite(c) => match field {
            "opacity" => Slot::F32(&mut c.opacity),
            "blend_mode" => Slot::Blend(&mut c.blend_mode),
            "mask_threshold" => Slot::F32(&mut c.mask_threshold),
            "propagate_meshgroup" => Slot::Bool(&mut c.propagate_meshgroup),
            "tint.r" => Slot::F32(&mut c.tint[0]),
            "tint.g" => Slot::F32(&mut c.tint[1]),
            "tint.b" => Slot::F32(&mut c.tint[2]),
            "screen_tint.r" => Slot::F32(&mut c.screen_tint[0]),
            "screen_tint.g" => Slot::F32(&mut c.screen_tint[1]),
            "screen_tint.b" => Slot::F32(&mut c.screen_tint[2]),
            _ => return None,
        },
        ClmNodeKind::MeshGroup(g) => match field {
            "translate_children" => Slot::Bool(&mut g.translate_children),
            _ => return None,
        },
        ClmNodeKind::SimplePhysics(s) => match field {
            "pendulum" => Slot::Pendulum(&mut s.kind),
            "map_mode" => Slot::MapMode(&mut s.map_mode),
            "local_only" => Slot::Bool(&mut s.local_only),
            "gravity" => Slot::F32(&mut s.gravity),
            "length" => Slot::F32(&mut s.length),
            "frequency" => Slot::F32(&mut s.frequency),
            "angle_damping" => Slot::F32(&mut s.angle_damping),
            "length_damping" => Slot::F32(&mut s.length_damping),
            "output_scale.x" => Slot::F32(&mut s.output_scale[0]),
            "output_scale.y" => Slot::F32(&mut s.output_scale[1]),
            _ => return None,
        },
    })
}

fn param_slot<'a>(param: &'a mut ClmParam, field: &str) -> Option<Slot<'a>> {
    Some(match field {
        "name" => Slot::Text(&mut param.name),
        "min" => Slot::F32(&mut param.min),
        "max" => Slot::F32(&mut param.max),
        "default" => Slot::F32(&mut param.default),
        _ => return None,
    })
}

// ---- parsing --------------------------------------------------------------

fn apply(
    slot: Slot<'_>,
    field: &str,
    value: &str,
    textures: &[ClmTexture],
) -> Result<(String, String), Error> {
    let bad = |expected: &str| Error::BadValue {
        field: field.to_string(),
        expected: expected.to_string(),
        value: value.to_string(),
    };
    Ok(match slot {
        Slot::F32(slot) => {
            let parsed: f32 = value.parse().map_err(|_| bad("a number"))?;
            let before = slot.to_string();
            *slot = parsed;
            (before, slot.to_string())
        }
        Slot::Bool(slot) => {
            let parsed = match value {
                "true" => true,
                "false" => false,
                _ => return Err(bad("true or false")),
            };
            let before = slot.to_string();
            *slot = parsed;
            (before, slot.to_string())
        }
        Slot::Text(slot) => {
            if value.len() > MAX_NAME_BYTES {
                return Err(bad(&format!("at most {MAX_NAME_BYTES} bytes")));
            }
            let before = format!("{slot:?}");
            *slot = value.to_string();
            (before, format!("{slot:?}"))
        }
        Slot::Blend(slot) => {
            let parsed = parse_enum(value).ok_or_else(|| bad(&one_of(BLEND_MODES)))?;
            let before = enum_name(slot);
            *slot = parsed;
            (before, enum_name(slot))
        }
        Slot::Pendulum(slot) => {
            let parsed = parse_enum(value).ok_or_else(|| bad(&one_of(PENDULUM_KINDS)))?;
            let before = enum_name(slot);
            *slot = parsed;
            (before, enum_name(slot))
        }
        Slot::MapMode(slot) => {
            let parsed = parse_enum(value).ok_or_else(|| bad(&one_of(MAP_MODES)))?;
            let before = enum_name(slot);
            *slot = parsed;
            (before, enum_name(slot))
        }
        Slot::Albedo(slot) => {
            let parsed = match value {
                "none" | "null" => None,
                id => {
                    let id = TexId::new(id).map_err(|source| Error::BadId {
                        value: id.to_string(),
                        source,
                    })?;
                    if !textures.iter().any(|t| t.id == id) {
                        return Err(bad("none, or the id of a texture this file carries"));
                    }
                    Some(id)
                }
            };
            let show = |slot: &Option<TexId>| {
                slot.as_ref()
                    .map_or_else(|| "none".to_string(), |t| format!("{:?}", t.as_str()))
            };
            let before = show(slot);
            *slot = parsed;
            (before, show(slot))
        }
    })
}

/// Parse a unit enum by its serialized name, so this crate never keeps a
/// second copy of a `.clm` variant list.
fn parse_enum<T: serde::de::DeserializeOwned>(value: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(value.to_string())).ok()
}

fn one_of(names: &[&str]) -> String {
    format!("one of {}", names.join(", "))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn every_listed_field_resolves_to_a_slot() {
        use catchlight_core::formats::clm::{
            ClmComposite, ClmMeshGroup, ClmPart, ClmSimplePhysics, ClmTransform,
        };
        use catchlight_core::id::NodeId;

        let kinds = [
            ClmNodeKind::Group,
            ClmNodeKind::Part(ClmPart {
                mesh: Default::default(),
                albedo: None,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                tint: [1.0; 3],
                screen_tint: [0.0; 3],
                masks: Vec::new(),
                mask_threshold: 0.5,
                slots: Vec::new(),
            }),
            ClmNodeKind::Composite(ClmComposite {
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                tint: [1.0; 3],
                screen_tint: [0.0; 3],
                masks: Vec::new(),
                mask_threshold: 0.5,
                propagate_meshgroup: false,
            }),
            ClmNodeKind::MeshGroup(ClmMeshGroup {
                mesh: Default::default(),
                translate_children: false,
            }),
            ClmNodeKind::SimplePhysics(ClmSimplePhysics {
                kind: PendulumKind::RigidPendulum,
                map_mode: PhysicsParamMapMode::AngleLength,
                local_only: false,
                target_params: [None, None],
                gravity: 1.0,
                length: 1.0,
                frequency: 1.0,
                angle_damping: 0.5,
                length_damping: 0.5,
                output_scale: [1.0, 1.0],
            }),
        ];

        for kind in kinds {
            let fields = node_fields(&kind);
            let mut node = ClmNode {
                id: NodeId::new("n").unwrap(),
                parent: None,
                name: String::new(),
                enabled: true,
                z_order: 0.0,
                transform: ClmTransform::default(),
                lock_to_root: false,
                kind,
            };
            for field in &fields {
                assert!(
                    node_slot(&mut node, field).is_some(),
                    "{field} is listed for {} but resolves to no slot",
                    kind_name(&node.kind)
                );
            }
            assert!(node_slot(&mut node, "no-such-field").is_none());
        }

        let mut param = ClmParam {
            id: catchlight_core::id::ParamId::new("p").unwrap(),
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            key_positions: vec![0.0, 1.0],
        };
        for field in PARAM_FIELDS {
            assert!(param_slot(&mut param, field).is_some(), "{field}");
        }
        assert!(param_slot(&mut param, "key_positions").is_none());
    }

    #[test]
    fn an_enum_is_parsed_by_the_name_it_serializes_as() {
        assert_eq!(
            parse_enum::<BlendMode>("Multiply"),
            Some(BlendMode::Multiply)
        );
        assert_eq!(parse_enum::<BlendMode>("multiply"), None);
        assert_eq!(parse_enum::<BlendMode>("nonsense"), None);
        assert_eq!(enum_name(&BlendMode::ColorDodge), "ColorDodge");
    }
}
