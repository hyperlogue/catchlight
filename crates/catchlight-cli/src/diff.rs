//! `diff a.clm b.clm` — what changed between two model files, by Id.
//!
//! The verdict is exact: the result is empty only when the decoded files are
//! equal, and a change nothing here renders still reports [`UNRENDERED`]. List
//! order is part of a model (draw order, cache walk order), so reordering differs.

use std::collections::BTreeMap;
use std::path::Path;

use catchlight_core::formats::clm::{
    ClmBinding, ClmFile, ClmIndices, ClmMask, ClmMesh, ClmNode, ClmNodeKind, ClmParam,
    ClmStructure, ClmTexture, ClmWeld,
};
use catchlight_core::{deform_cells, mask_mode_name, scalar_cells, target_of};
use catchlight_extension_animations::{Animations, Clip};
use serde::Serialize;

use crate::{extension, file, Error};

/// Its presence is a bug report, not a normal outcome.
pub const UNRENDERED: &str = "~ (the two files differ in a way this summary does not render)";

/// Empty exactly when `a == b`.
pub fn diff(a: &ClmFile, b: &ClmFile) -> Vec<String> {
    let mut out = Vec::new();

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
    // Clips the build cannot decode still show as a changed extension below.
    let clips = |file: &ClmFile| {
        extension::typed::<Animations>(file)
            .and_then(Result::ok)
            .map(|animations| animations.clips())
            .unwrap_or_default()
    };
    let (clips_a, clips_b) = (clips(a), clips(b));
    compare(
        "animation",
        keyed(&clips_a, |clip| format!("{:?}", clip.name)),
        keyed(&clips_b, |clip| format!("{:?}", clip.name)),
        animation_fields,
        &mut out,
    );

    extensions(&a.doc, &b.doc, &mut out);

    if out.is_empty() && a != b {
        out.push(UNRENDERED.to_string());
    }
    out
}

pub fn run(a: &Path, b: &Path) -> Result<Vec<String>, Error> {
    let left = file::read(a)?;
    let right = file::read(b)?;
    Ok(diff(&left, &right))
}

/// A duplicate key keeps the first element, so a hostile file is compared rather
/// than panicked on.
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

fn extensions(a: &ClmStructure, b: &ClmStructure, out: &mut Vec<String>) {
    for (key, before) in &a.extensions {
        match b.extensions.get(key) {
            None => out.push(format!("- extension {key}: {}", extension::render(before))),
            Some(after) if after != before => out.push(format!(
                "~ extension {key}: {} -> {}",
                extension::render(before),
                extension::render(after)
            )),
            Some(_) => {}
        }
    }
    for (key, after) in &b.extensions {
        if !a.extensions.contains_key(key) {
            out.push(format!("+ extension {key}: {}", extension::render(after)));
        }
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
        ClmNodeKind::MotionResponse(r) => {
            f.insert("kind".into(), "MotionResponse".into());
            f.insert("period_s".into(), r.period_s.to_string());
            f.insert("damping_ratio".into(), r.damping_ratio.to_string());
            f.insert("inputs".into(), format!("{:?}", r.inputs));
            f.insert(
                "output_param".into(),
                r.output_param
                    .as_ref()
                    .map_or_else(|| "(none)".into(), ToString::to_string),
            );
        }
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
            f.insert(
                "slots".into(),
                format!("{} slots, {}", p.slots.len(), digest(&p.slots)),
            );
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
            f.insert(
                "translate_children".into(),
                g.translate_children.to_string(),
            );
            mesh(&mut f, &g.mesh);
        }
        ClmNodeKind::Spine(sp) => {
            f.insert("kind".into(), "Spine".into());
            for (i, joint) in sp.joints.iter().enumerate() {
                f.insert(format!("joints.{i}.x"), joint[0].to_string());
                f.insert(format!("joints.{i}.y"), joint[1].to_string());
            }
            for (i, slot) in sp.targets.iter().enumerate() {
                f.insert(
                    format!("targets.{i}"),
                    slot.as_ref()
                        .map_or_else(|| "(none)".to_string(), |t| format!("{:?}", t.as_str())),
                );
            }
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
    f.insert("key_positions".into(), format!("{:?}", b.key_positions));
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

/// Unordered: swapping the two ends is not a change.
fn weld_key(w: &ClmWeld) -> String {
    let (a, b) = if w.a <= w.b {
        (&w.a, &w.b)
    } else {
        (&w.b, &w.a)
    };
    format!("{a} <-> {b}")
}

fn weld_fields(w: &ClmWeld) -> Fields {
    let mut f = Fields::new();
    f.insert("a".into(), w.a.to_string());
    f.insert("b".into(), w.b.to_string());
    f.insert(
        "pairs".into(),
        format!("{} pairs, {}", w.pairs.len(), digest(&w.pairs)),
    );
    f
}

fn animation_fields(a: &Clip) -> Fields {
    let frame = |f: Option<u32>| f.map_or_else(|| "(none)".to_string(), |f| f.to_string());
    let mut f = Fields::new();
    f.insert("timestep".into(), a.timestep.to_string());
    f.insert("length".into(), a.length.to_string());
    f.insert("lead_in".into(), frame(a.lead_in));
    f.insert("lead_out".into(), frame(a.lead_out));
    for lane in &a.lanes {
        f.insert(
            format!("lane[{}]", lane.target),
            format!(
                "{}, {} keyframes, {}",
                enum_name(&lane.interpolation),
                lane.keyframes().len(),
                digest(&lane.keyframes())
            ),
        );
    }
    f
}

pub(crate) fn enum_name<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_string(),
    }
}

/// For display only; never used to decide whether a value changed.
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
