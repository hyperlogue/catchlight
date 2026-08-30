//! `diff a.clm b.clm` — what changed between two model files, by Id.
//!
//! Stable identity is what makes this possible at all: a node, param or
//! texture is the same thing in both files exactly when its Id is the same
//! string, so the comparison is a set difference over Ids rather than a guess
//! about which subtree moved where. Bindings are keyed by their params, node
//! and target, welds by their two ends and animations by name — the same keys
//! the model itself uses.
//!
//! Two rules keep the output honest:
//!
//! - **The verdict is exact; the rendering is a summary.** Whether the files
//!   differ is decided by comparing the decoded [`ClmFile`]s, not by whether
//!   this module found something to print. A change it does not know how to
//!   describe still reports as a difference, with a line saying so
//!   ([`UNRENDERED`]). Nothing but equality of the two files produces an empty
//!   result, which is what makes the exit status usable in CI.
//! - **Bulk arrays are digested, not dumped.** A mesh, a binding's cells, a
//!   weld's weights and a texture's bytes are rendered as a count and a
//!   64-bit FNV-1a digest. The digest exists to *show* a change; whether there
//!   was one is always decided by comparing the values themselves.
//!
//! Order is part of a model — sibling order is draw order at equal z, and
//! texture order is the order a render cache walks — so a file whose lists
//! hold the same things in a different order differs, and says so on an
//! `order` line.

use std::collections::BTreeMap;
use std::path::Path;

use catchlight_core::formats::clm::{
    ClmAnimation, ClmBinding, ClmDocument, ClmFile, ClmIndices, ClmMask, ClmMesh, ClmNode,
    ClmNodeKind, ClmParam, ClmTexture, ClmWeld,
};
use catchlight_core::{deform_cells, mask_mode_name, scalar_cells, target_of};
use serde::Serialize;

use crate::{file, Error};

/// The line `diff` emits when the two files differ in something no renderer
/// here describes. Its presence is a bug report, not a normal outcome.
pub const UNRENDERED: &str = "~ (the two files differ in a way this summary does not render)";

/// Compare two files that have already been decoded.
///
/// The result is empty exactly when `a == b`.
pub fn diff(a: &ClmFile, b: &ClmFile) -> Vec<String> {
    let mut out = Vec::new();

    physics(&a.doc, &b.doc, &mut out);
    compare(
        "node",
        keyed(&a.doc.nodes, |n| n.id.to_string()),
        keyed(&b.doc.nodes, |n| n.id.to_string()),
        node_fields,
        &mut out,
    );
    compare(
        "param",
        keyed(&a.doc.params, |p| p.id.to_string()),
        keyed(&b.doc.params, |p| p.id.to_string()),
        param_fields,
        &mut out,
    );
    compare(
        "texture",
        keyed(&a.textures, |t| t.id.to_string()),
        keyed(&b.textures, |t| t.id.to_string()),
        texture_fields,
        &mut out,
    );
    compare(
        "binding",
        keyed(&a.doc.bindings, binding_key),
        keyed(&b.doc.bindings, binding_key),
        binding_fields,
        &mut out,
    );
    compare(
        "weld",
        keyed(&a.doc.welds, weld_key),
        keyed(&b.doc.welds, weld_key),
        weld_fields,
        &mut out,
    );
    compare(
        "animation",
        keyed(&a.doc.animations, |an| format!("{:?}", an.name)),
        keyed(&b.doc.animations, |an| format!("{:?}", an.name)),
        animation_fields,
        &mut out,
    );

    if out.is_empty() && a != b {
        out.push(UNRENDERED.to_string());
    }
    out
}

/// Read both files and compare them. No image is decoded on either side.
pub fn run(a: &Path, b: &Path) -> Result<Vec<String>, Error> {
    let left = file::read(a)?;
    let right = file::read(b)?;
    Ok(diff(&left, &right))
}

// ---- the generic shape of a comparison -----------------------------------

/// One list indexed by the key its elements are identified by, in file order.
///
/// A duplicate key keeps the first element; `order` still lists every
/// occurrence, so a hostile file (which no reader would load) is compared
/// rather than panicked on.
struct Keyed<'a, T> {
    by_key: BTreeMap<String, &'a T>,
    order: Vec<String>,
}

fn keyed<T>(items: &[T], key: impl Fn(&T) -> String) -> Keyed<'_, T> {
    let mut by_key = BTreeMap::new();
    let mut order = Vec::with_capacity(items.len());
    for item in items {
        let k = key(item);
        by_key.entry(k.clone()).or_insert(item);
        order.push(k);
    }
    Keyed { by_key, order }
}

/// Added / removed / changed over one keyed list, then whether what both hold
/// is in the same order.
fn compare<T: PartialEq>(
    kind: &str,
    a: Keyed<'_, T>,
    b: Keyed<'_, T>,
    fields: impl Fn(&T) -> Fields,
    out: &mut Vec<String>,
) {
    for key in b.by_key.keys() {
        if !a.by_key.contains_key(key) {
            out.push(format!("+ {kind} {key}"));
        }
    }
    for (key, left) in &a.by_key {
        match b.by_key.get(key) {
            None => out.push(format!("- {kind} {key}")),
            Some(right) => {
                if left != right {
                    let changes = field_diff(&fields(left), &fields(right));
                    if changes.is_empty() {
                        out.push(format!("~ {kind} {key} (differs)"));
                    }
                    for (field, before, after) in changes {
                        out.push(format!("~ {kind} {key} {field}: {before} -> {after}"));
                    }
                }
            }
        }
    }

    let shared = |k: &Keyed<'_, T>, other: &Keyed<'_, T>| -> Vec<String> {
        k.order
            .iter()
            .filter(|key| other.by_key.contains_key(*key))
            .cloned()
            .collect()
    };
    if shared(&a, &b) != shared(&b, &a) {
        out.push(format!(
            "~ order {kind}s: the {kind}s both files carry are in a different order"
        ));
    }
}

/// A value's fields, flattened to `path -> rendered value`. Comparing two of
/// these turns "what changed on this node" into a map difference, so a node
/// that changed kind reports the kind plus the fields that came and went
/// rather than needing a case of its own.
type Fields = BTreeMap<String, String>;

fn field_diff(a: &Fields, b: &Fields) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (field, before) in a {
        match b.get(field) {
            Some(after) if after == before => {}
            Some(after) => out.push((field.clone(), before.clone(), after.clone())),
            None => out.push((field.clone(), before.clone(), "(none)".to_string())),
        }
    }
    for (field, after) in b {
        if !a.contains_key(field) {
            out.push((field.clone(), "(none)".to_string(), after.clone()));
        }
    }
    out.sort();
    out
}

// ---- rendering one value's fields ----------------------------------------

fn physics(a: &ClmDocument, b: &ClmDocument, out: &mut Vec<String>) {
    if a.physics.pixels_per_meter != b.physics.pixels_per_meter {
        out.push(format!(
            "~ physics pixels_per_meter: {} -> {}",
            a.physics.pixels_per_meter, b.physics.pixels_per_meter
        ));
    }
    if a.physics.gravity != b.physics.gravity {
        out.push(format!(
            "~ physics gravity: {} -> {}",
            a.physics.gravity, b.physics.gravity
        ));
    }
}

fn node_fields(n: &ClmNode) -> Fields {
    let mut f = Fields::new();
    f.insert(
        "parent".into(),
        n.parent
            .as_ref()
            .map_or_else(|| "(none)".to_string(), |p| format!("{:?}", p.as_str())),
    );
    f.insert("name".into(), format!("{:?}", n.name));
    f.insert("enabled".into(), n.enabled.to_string());
    f.insert("z_order".into(), n.z_order.to_string());
    f.insert("lock_to_root".into(), n.lock_to_root.to_string());
    for (i, axis) in ["x", "y", "z"].iter().enumerate() {
        f.insert(
            format!("translation.{axis}"),
            n.transform.translation[i].to_string(),
        );
        f.insert(
            format!("rotation.{axis}"),
            n.transform.rotation[i].to_string(),
        );
    }
    for (i, axis) in ["x", "y"].iter().enumerate() {
        f.insert(format!("scale.{axis}"), n.transform.scale[i].to_string());
    }

    match &n.kind {
        ClmNodeKind::Group => {
            f.insert("kind".into(), "Group".into());
        }
        ClmNodeKind::Part(p) => {
            f.insert("kind".into(), "Part".into());
            f.insert("opacity".into(), p.opacity.to_string());
            f.insert("blend_mode".into(), enum_name(&p.blend_mode));
            f.insert("mask_threshold".into(), p.mask_threshold.to_string());
            colours(&mut f, p.tint, p.screen_tint);
            f.insert(
                "albedo".into(),
                p.albedo
                    .as_ref()
                    .map_or_else(|| "(none)".to_string(), |t| format!("{:?}", t.as_str())),
            );
            mesh(&mut f, &p.mesh);
            f.insert("masks".into(), masks(&p.masks));
            for seam in &p.seams {
                f.insert(
                    format!("seam[{:?}]", seam.id.as_str()),
                    format!("{} slots, {}", seam.slots.len(), digest(&seam.slots)),
                );
            }
        }
        ClmNodeKind::Composite(c) => {
            f.insert("kind".into(), "Composite".into());
            f.insert("opacity".into(), c.opacity.to_string());
            f.insert("blend_mode".into(), enum_name(&c.blend_mode));
            f.insert("mask_threshold".into(), c.mask_threshold.to_string());
            f.insert(
                "propagate_meshgroup".into(),
                c.propagate_meshgroup.to_string(),
            );
            colours(&mut f, c.tint, c.screen_tint);
            f.insert("masks".into(), masks(&c.masks));
        }
        ClmNodeKind::MeshGroup(g) => {
            f.insert("kind".into(), "MeshGroup".into());
            f.insert("dynamic".into(), g.dynamic.to_string());
            f.insert(
                "translate_children".into(),
                g.translate_children.to_string(),
            );
            mesh(&mut f, &g.mesh);
        }
        ClmNodeKind::SimplePhysics(p) => {
            f.insert("kind".into(), "SimplePhysics".into());
            f.insert("pendulum".into(), enum_name(&p.kind));
            f.insert("map_mode".into(), enum_name(&p.map_mode));
            f.insert("local_only".into(), p.local_only.to_string());
            for (i, slot) in p.target_params.iter().enumerate() {
                f.insert(
                    format!("target_params.{i}"),
                    slot.as_ref()
                        .map_or_else(|| "(none)".to_string(), |t| format!("{:?}", t.as_str())),
                );
            }
            f.insert("gravity".into(), p.gravity.to_string());
            f.insert("length".into(), p.length.to_string());
            f.insert("frequency".into(), p.frequency.to_string());
            f.insert("angle_damping".into(), p.angle_damping.to_string());
            f.insert("length_damping".into(), p.length_damping.to_string());
            f.insert("output_scale.x".into(), p.output_scale[0].to_string());
            f.insert("output_scale.y".into(), p.output_scale[1].to_string());
        }
    }
    f
}

fn colours(f: &mut Fields, tint: [f32; 3], screen_tint: [f32; 3]) {
    for (i, c) in ["r", "g", "b"].iter().enumerate() {
        f.insert(format!("tint.{c}"), tint[i].to_string());
        f.insert(format!("screen_tint.{c}"), screen_tint[i].to_string());
    }
}

fn masks(masks: &[ClmMask]) -> String {
    let rendered: Vec<String> = masks
        .iter()
        .map(|m| format!("{}:{}", m.source, mask_mode_name(m.mode)))
        .collect();
    format!("[{}]", rendered.join(", "))
}

fn mesh(f: &mut Fields, m: &ClmMesh) {
    f.insert(
        "mesh.verts".into(),
        format!("{} values, {}", m.verts.len(), digest(&m.verts)),
    );
    f.insert(
        "mesh.uvs".into(),
        format!("{} values, {}", m.uvs.len(), digest(&m.uvs)),
    );
    let (width, count) = match &m.indices {
        ClmIndices::U16(v) => ("u16", v.len()),
        ClmIndices::U32(v) => ("u32", v.len()),
    };
    f.insert(
        "mesh.indices".into(),
        format!("{width}, {count} values, {}", digest(&m.indices)),
    );
    f.insert(
        "mesh.origin".into(),
        format!("[{}, {}]", m.origin[0], m.origin[1]),
    );
}

fn param_fields(p: &ClmParam) -> Fields {
    let mut f = Fields::new();
    f.insert("name".into(), format!("{:?}", p.name));
    f.insert("min".into(), p.min.to_string());
    f.insert("max".into(), p.max.to_string());
    f.insert("default".into(), p.default.to_string());
    f.insert(
        "key_positions".into(),
        format!(
            "[{}]",
            p.key_positions
                .iter()
                .map(f32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    f
}

fn texture_fields(t: &ClmTexture) -> Fields {
    let mut f = Fields::new();
    f.insert("encoding".into(), enum_name(&t.encoding));
    f.insert("alpha".into(), enum_name(&t.alpha));
    f.insert(
        "data".into(),
        format!("{} bytes, h {:016x}", t.data.len(), fnv1a(&t.data)),
    );
    f
}

fn binding_key(b: &ClmBinding) -> String {
    let params: Vec<&str> = b.params.iter().map(|p| p.as_str()).collect();
    format!(
        "[{}] {:?} {}",
        params.join(", "),
        b.node.as_str(),
        target_of(&b.values).name()
    )
}

fn binding_fields(b: &ClmBinding) -> Fields {
    let mut f = Fields::new();
    f.insert("interpolate_mode".into(), enum_name(&b.interpolate_mode));
    let cells = scalar_cells(&b.values)
        .map(<[_]>::len)
        .or_else(|| deform_cells(&b.values).map(<[_]>::len))
        .unwrap_or(0);
    f.insert(
        "cells".into(),
        format!("{cells} authored, {}", digest(&b.values)),
    );
    f
}

fn weld_key(w: &ClmWeld) -> String {
    format!("{}/{} <-> {}/{}", w.a.node, w.a.seam, w.b.node, w.b.seam)
}

fn weld_fields(w: &ClmWeld) -> Fields {
    let mut f = Fields::new();
    f.insert(
        "weights".into(),
        format!("{} slots, {}", w.weights.len(), digest(&w.weights)),
    );
    f
}

fn animation_fields(a: &ClmAnimation) -> Fields {
    let mut f = Fields::new();
    f.insert("timestep".into(), a.timestep.to_string());
    f.insert("length".into(), a.length.to_string());
    f.insert("lead_in".into(), a.lead_in.to_string());
    f.insert("lead_out".into(), a.lead_out.to_string());
    for lane in &a.lanes {
        f.insert(
            format!("lane[{:?}]", lane.param.as_str()),
            format!(
                "{}, {} keyframes, {}",
                enum_name(&lane.interpolation),
                lane.keyframes.len(),
                digest(&lane.keyframes)
            ),
        );
    }
    f
}

// ---- rendering helpers ---------------------------------------------------

/// The name of a unit enum variant, taken from its own serialization so this
/// module never keeps a second copy of a `.clm` variant list.
pub(crate) fn enum_name<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_string(),
    }
}

/// A short, stable digest of anything serializable — for showing that a bulk
/// array changed. Never used to *decide* whether it changed.
fn digest<T: Serialize>(v: &T) -> String {
    match serde_json::to_vec(v) {
        Ok(bytes) => format!("h {:016x}", fnv1a(&bytes)),
        Err(_) => "h (unrenderable)".to_string(),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
