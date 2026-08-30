//! One-time `.inx → legacy document` import: turn an inochi2d puppet into the
//! arena a [`Model`](crate::Model) is built from, which
//! `cargo xtask import` then writes as a `.clm`.
//!
//! The inx node *tree* is flattened (DFS pre-order) into the arena: each node
//! lands at a sequential index with its `parent` index recorded, so the result
//! is topologically ordered (`parent < self`). inochi's globally-unique
//! `uuid`s are resolved to those indices — `binding.node` / `mask.source` to a
//! node index, `SimplePhysics.target_param` to a param index — and then
//! dropped; the array position is the only identity the arena has, and the
//! Ids a Model mints from it are what the file stores. References that don't
//! resolve are dropped, exactly as the LegacyPuppet path drops them; duplicate uuids
//! collapse to the first occurrence (the LegacyPuppet path's renumbering, for free).
//!
//! Like [`super::convert`], this keeps only what catchlight models and drops the
//! rest (meta, groups, automation, animations, cameras, emissive/bump slots,
//! emissionStrength). Textures are kept verbatim; the build crops them.

use std::collections::HashMap;

use crate::components::{BlendMode, MaskMode};
use crate::fill::{derive_dense, FillCell};
use crate::formats::clm::{
    ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmMesh, ClmPhysics, ClmTransform,
    TextureAlpha, TextureEncoding,
};
use crate::formats::legacy::{
    LegacyBinding, LegacyComposite, LegacyDocument, LegacyFile, LegacyMask, LegacyMeshGroup,
    LegacyNode, LegacyNodeKind, LegacyParam, LegacyPart, LegacySimplePhysics, LegacyTexture,
};
use crate::formats::{InxModel, TextureFormat};
use crate::params::InterpolateMode;
use crate::physics::{PendulumKind, PhysicsParamMapMode};

use super::error::ImportError;
use super::schema::{
    source_binding_is_color, SchemaBinding, SchemaMask, SchemaMesh, SchemaNode, SchemaParam,
    SchemaPuppetPhysics, SchemaTransform,
};

/// Convert a parsed `.inx` model into an editable [`LegacyFile`].
pub fn from_inx_model_to_legacy(model: &InxModel) -> Result<LegacyFile, ImportError> {
    let obj = model
        .payload
        .as_object()
        .ok_or_else(|| ImportError::MalformedPayload("inx payload root is not an object".into()))?;

    let physics = obj
        .get("physics")
        .and_then(|v| serde_json::from_value::<SchemaPuppetPhysics>(v.clone()).ok())
        .map(|p| ClmPhysics {
            pixels_per_meter: p.pixels_per_meter.unwrap_or(1000.0),
            gravity: p.gravity.unwrap_or(9.8),
        })
        .unwrap_or_default();

    // Flatten the node tree, recording each node's parent index and a
    // uuid → index map for resolving cross-references.
    let mut flat: Vec<(SchemaNode, Option<u32>)> = Vec::new();
    let mut node_index: HashMap<u32, u32> = HashMap::new();
    match obj.get("nodes") {
        Some(n) => flatten(n, None, &mut flat, &mut node_index),
        None => flat.push((SchemaNode::default(), None)),
    }

    // Params keep their array order; build a uuid → index map (first-wins, which
    // is the LegacyPuppet path's duplicate-uuid renumbering).
    let schema_params: Vec<SchemaParam> = obj
        .get("param")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| serde_json::from_value::<SchemaParam>(p.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut param_index: HashMap<u32, u32> = HashMap::new();
    for (i, p) in schema_params.iter().enumerate() {
        if let Some(uuid) = p.uuid {
            param_index.entry(uuid).or_insert(i as u32);
        }
    }

    let nodes = flat
        .iter()
        .map(|(s, parent)| convert_node(s, *parent, &node_index, &param_index))
        .collect::<Result<Vec<_>, ImportError>>()?;
    let params = schema_params
        .iter()
        .map(|p| convert_param(p, &node_index, &nodes))
        .collect();

    let textures = model
        .textures
        .iter()
        .map(|t| LegacyTexture {
            encoding: match t.format {
                TextureFormat::Png => TextureEncoding::Png,
                TextureFormat::Tga => TextureEncoding::Tga,
            },
            alpha: TextureAlpha::PremultipliedSrgb,
            data: t.data.to_vec(),
        })
        .collect();

    Ok(LegacyFile {
        doc: LegacyDocument {
            physics,
            nodes,
            params,
            welds: Vec::new(),
        },
        textures,
    })
}

/// DFS pre-order flatten: append this node (parsed shallow) with its parent
/// index, map its uuid → index, then recurse into children. Pre-order keeps
/// `parent < self` and preserves sibling order.
fn flatten(
    value: &serde_json::Value,
    parent: Option<u32>,
    flat: &mut Vec<(SchemaNode, Option<u32>)>,
    node_index: &mut HashMap<u32, u32>,
) {
    let idx = flat.len() as u32;
    let schema = parse_node_shallow(value);
    if let Some(uuid) = schema.uuid {
        node_index.entry(uuid).or_insert(idx);
    }
    flat.push((schema, parent));
    if let Some(children) = value.get("children").and_then(|c| c.as_array()) {
        for child in children {
            flatten(child, Some(idx), flat, node_index);
        }
    }
}

/// A node's own fields, without descending into `children`.
fn parse_node_shallow(value: &serde_json::Value) -> SchemaNode {
    let serde_json::Value::Object(map) = value else {
        return SchemaNode::default();
    };
    let mut sanitized = serde_json::Map::with_capacity(map.len());
    for (k, v) in map {
        if k != "children" {
            sanitized.insert(k.clone(), v.clone());
        }
    }
    serde_json::from_value(serde_json::Value::Object(sanitized)).unwrap_or_default()
}

// Shorter-than-expected arrays fill the missing components individually,
// matching `convert::convert_vec2` / `convert_vec3`. Falling back to the whole
// default instead would snap a node with `"scale": [2.0]` to unit scale on this
// path while the LegacyPuppet path reads (2.0, 1.0).
fn vec2_arr(v: &[f32], default: [f32; 2]) -> [f32; 2] {
    [
        v.first().copied().unwrap_or(default[0]),
        v.get(1).copied().unwrap_or(default[1]),
    ]
}

fn vec3_arr(v: &[f32], default: [f32; 3]) -> [f32; 3] {
    [
        v.first().copied().unwrap_or(default[0]),
        v.get(1).copied().unwrap_or(default[1]),
        v.get(2).copied().unwrap_or(default[2]),
    ]
}

fn convert_transform(t: Option<&SchemaTransform>) -> ClmTransform {
    let mut transform = match t {
        Some(t) => ClmTransform {
            translation: vec3_arr(&t.trans, [0.0, 0.0, 0.0]),
            rotation: vec3_arr(&t.rot, [0.0, 0.0, 0.0]),
            scale: vec2_arr(&t.scale, [1.0, 1.0]),
        },
        None => ClmTransform::default(),
    };
    reflect_transform_y(&mut transform);
    transform
}

/// inochi2d authors in a Y-down frame; catchlight is Y-up. Reflect across the
/// X axis: negate translation Y and the two Euler angles a Y-flip inverts
/// (rotation about X and Z). Rotation about Y and scale are unchanged.
fn reflect_transform_y(t: &mut ClmTransform) {
    t.translation[1] = -t.translation[1];
    t.rotation[0] = -t.rotation[0];
    t.rotation[2] = -t.rotation[2];
}

fn convert_node(
    s: &SchemaNode,
    parent: Option<u32>,
    node_index: &HashMap<u32, u32>,
    param_index: &HashMap<u32, u32>,
) -> Result<LegacyNode, ImportError> {
    Ok(LegacyNode {
        parent,
        name: s.name.clone().unwrap_or_default(),
        enabled: s.enabled.unwrap_or(true),
        z_order: reflect_z(s.zsort.unwrap_or(0.0)),
        transform: convert_transform(s.transform.as_ref()),
        lock_to_root: s.lock_to_root.unwrap_or(false),
        kind: convert_node_kind(s, node_index, param_index)?,
    })
}

fn convert_node_kind(
    s: &SchemaNode,
    node_index: &HashMap<u32, u32>,
    param_index: &HashMap<u32, u32>,
) -> Result<LegacyNodeKind, ImportError> {
    Ok(match s.ty.as_deref().unwrap_or("") {
        "Part" => LegacyNodeKind::Part(convert_part(s, node_index)?),
        "Composite" => LegacyNodeKind::Composite(convert_composite(s, node_index)?),
        "MeshGroup" => LegacyNodeKind::MeshGroup(convert_mesh_group(s)),
        "SimplePhysics" => LegacyNodeKind::SimplePhysics(convert_simple_physics(s, param_index)),
        // Node, Camera, and any unmodeled type all become a container Group.
        _ => LegacyNodeKind::Group,
    })
}

fn convert_mesh(m: Option<&SchemaMesh>) -> ClmMesh {
    let Some(m) = m else {
        return ClmMesh::default();
    };
    let max = m.indices.iter().copied().max().unwrap_or(0);
    let indices = if max <= u16::MAX as u32 {
        ClmIndices::U16(m.indices.iter().map(|&i| i as u16).collect())
    } else {
        ClmIndices::U32(m.indices.clone())
    };
    let mut mesh = ClmMesh {
        verts: m.verts.clone(),
        uvs: m.uvs.clone(),
        indices,
        origin: vec2_arr(&m.origin, [0.0, 0.0]),
    };
    reflect_mesh_y(&mut mesh);
    mesh
}

/// Reflect mesh geometry into catchlight's Y-up frame (see [`reflect_transform_y`]).
/// UVs are texture space and stay as authored.
fn reflect_mesh_y(m: &mut ClmMesh) {
    for y in m.verts.iter_mut().skip(1).step_by(2) {
        *y = -*y;
    }
    m.origin[1] = -m.origin[1];
}

fn convert_masks(masks: &[SchemaMask], node_index: &HashMap<u32, u32>) -> Vec<LegacyMask> {
    masks
        .iter()
        .filter_map(|m| {
            // Drop masks whose source node doesn't resolve, as the LegacyPuppet path does.
            let source = *node_index.get(&m.source?)?;
            Some(LegacyMask {
                source,
                mode: mask_mode(m.mode.as_deref()),
            })
        })
        .collect()
}

fn convert_part(s: &SchemaNode, node_index: &HashMap<u32, u32>) -> Result<LegacyPart, ImportError> {
    let albedo = match s.textures.first() {
        None => 0,
        Some(&v) => u32::try_from(v).unwrap_or(u32::MAX),
    };
    Ok(LegacyPart {
        mesh: convert_mesh(s.mesh.as_ref()),
        albedo,
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), node_index),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
    })
}

fn convert_composite(
    s: &SchemaNode,
    node_index: &HashMap<u32, u32>,
) -> Result<LegacyComposite, ImportError> {
    Ok(LegacyComposite {
        opacity: s.opacity.unwrap_or(1.0),
        blend_mode: blend(s.blend_mode.as_deref())?,
        tint: vec3_arr(&s.tint, [1.0, 1.0, 1.0]),
        screen_tint: vec3_arr(&s.screen_tint, [0.0, 0.0, 0.0]),
        masks: convert_masks(s.masks.as_deref().unwrap_or(&[]), node_index),
        mask_threshold: s.mask_threshold.unwrap_or(0.5),
        propagate_meshgroup: s.propagate_meshgroup.unwrap_or(true),
    })
}

fn convert_mesh_group(s: &SchemaNode) -> LegacyMeshGroup {
    s.log_dropped_mesh_group_color();
    LegacyMeshGroup {
        mesh: convert_mesh(s.mesh.as_ref()),
        dynamic: s.dynamic_deformation.unwrap_or(false),
        translate_children: s.translate_children.unwrap_or(false),
    }
}

fn convert_simple_physics(s: &SchemaNode, param_index: &HashMap<u32, u32>) -> LegacySimplePhysics {
    LegacySimplePhysics {
        kind: s
            .model_type
            .as_deref()
            .and_then(PendulumKind::from_str)
            .unwrap_or_default(),
        map_mode: s
            .map_mode
            .as_deref()
            .and_then(PhysicsParamMapMode::from_str)
            .unwrap_or_default(),
        local_only: s.local_only.unwrap_or(false),
        target_param: s.param.and_then(|uuid| param_index.get(&uuid).copied()),
        // Authored, unscaled — the global g-scale fold is a build step.
        gravity: s.gravity.unwrap_or(1.0),
        length: s.length.unwrap_or(100.0),
        frequency: s.frequency.unwrap_or(1.0),
        angle_damping: s.angle_damping.unwrap_or(0.5),
        length_damping: s.length_damping.unwrap_or(0.5),
        output_scale: vec2_arr(&s.output_scale, [1.0, 1.0]),
    }
}

fn convert_param(
    p: &SchemaParam,
    node_index: &HashMap<u32, u32>,
    nodes: &[LegacyNode],
) -> LegacyParam {
    let is_vec2 = p.is_vec2.unwrap_or(false);
    // Resolve inochi2d's axis-point defaults here so the wire holds final axis
    // stops (the same logic the LegacyPuppet path applies in convert.rs): absent axes
    // default to [0,1] on x and [0,1]/[0] on y (vec2/scalar), and a degenerate
    // empty axis collapses to a single stop so the param can still drive.
    let default_y = || if is_vec2 { vec![0.0, 1.0] } else { vec![0.0] };
    let (mut axis_x, mut axis_y) = match p.axis_points.as_ref() {
        Some(axes) => {
            let x = axes.first().cloned().unwrap_or_else(|| vec![0.0, 1.0]);
            let y = axes.get(1).cloned().unwrap_or_else(default_y);
            (x, y)
        }
        None => (vec![0.0, 1.0], default_y()),
    };
    if axis_x.is_empty() {
        axis_x = vec![0.0];
    }
    if axis_y.is_empty() {
        axis_y = vec![0.0];
    }
    let bindings = p
        .bindings
        .iter()
        .filter_map(|b| convert_binding(b, node_index, nodes, &axis_x, &axis_y))
        .collect();
    LegacyParam {
        name: p.name.clone().unwrap_or_default(),
        is_vec2,
        min: vec2_arr(&p.min, [0.0, 0.0]),
        max: vec2_arr(&p.max, [1.0, 1.0]),
        defaults: vec2_arr(&p.defaults, [0.0, 0.0]),
        axis_points_x: axis_x,
        axis_points_y: axis_y,
        bindings,
    }
}

fn convert_binding(
    b: &SchemaBinding,
    node_index: &HashMap<u32, u32>,
    nodes: &[LegacyNode],
    axis_x: &[f32],
    axis_y: &[f32],
) -> Option<LegacyBinding> {
    // Drop bindings whose target node doesn't resolve, as the LegacyPuppet path does.
    let node = *node_index.get(&b.node?)?;
    let values_json = b.values.as_ref()?;
    let kind = b.param_name.as_deref().unwrap_or("");
    // A mesh group is never drawn and carries no colour, so a colour binding on
    // one has nowhere to land — and writing it out would produce a `.clm` the
    // loader rejects. The LegacyPuppet path drops the same shape.
    if source_binding_is_color(kind)
        && matches!(
            nodes.get(node as usize).map(|n| &n.kind),
            Some(LegacyNodeKind::MeshGroup(_))
        )
    {
        tracing::debug!(
            "dropping {:?} binding on mesh group node {}: a mesh group is never drawn",
            kind,
            node
        );
        return None;
    }
    let values = convert_binding_values(kind, values_json, b.is_set.as_deref(), axis_x, axis_y)?;
    Some(LegacyBinding {
        node,
        interpolate_mode: interp(b.interpolate_mode.as_deref()),
        values,
    })
}

/// `.inx` carries both the baked dense matrix and the authored `isSet` mask;
/// `.clm` keeps only the authored cells. The baked values double as the oracle:
/// if our fill does not reproduce them from the authored set, the binding falls
/// back to all-cells-authored, preserving pixel parity by construction.
fn convert_binding_values(
    kind: &str,
    v: &serde_json::Value,
    is_set: Option<&[Vec<bool>]>,
    axis_x: &[f32],
    axis_y: &[f32],
) -> Option<ClmBindingValues> {
    if kind == "deform" {
        let mut d = matrix_vec2_flat(v);
        for cell in &mut d.data {
            for y in cell.iter_mut().skip(1).step_by(2) {
                *y = -*y;
            }
        }
        let vlen = d.data.iter().map(Vec::len).max().unwrap_or(0);
        let cells = sparsify(d, is_set, axis_x, axis_y, &vec![0.0f32; vlen], close_vec);
        return Some(ClmBindingValues::Deform(cells));
    }
    // Reflect source-space outputs at the import boundary: spatial Y into
    // Catchlight's Y-up frame, and zSort into its higher-in-front convention.
    let reflected = matches!(
        kind,
        "zSort" | "transform.t.y" | "transform.r.x" | "transform.r.z"
    );
    let identity = match kind {
        "transform.s.x" | "transform.s.y" | "opacity" | "tint.r" | "tint.g" | "tint.b"
        | "outputScale.x" | "outputScale.y" => 1.0f32,
        _ => 0.0,
    };
    let mut d = matrix_f32(v);
    if reflected {
        for val in &mut d.data {
            *val = if kind == "zSort" {
                reflect_z(*val)
            } else {
                -*val
            };
        }
    }
    let cells = sparsify(d, is_set, axis_x, axis_y, &identity, close_f32);
    Some(match kind {
        "zSort" => ClmBindingValues::ZOrder(cells),
        "transform.t.x" => ClmBindingValues::TransformTX(cells),
        "transform.t.y" => ClmBindingValues::TransformTY(cells),
        "transform.s.x" => ClmBindingValues::TransformSX(cells),
        "transform.s.y" => ClmBindingValues::TransformSY(cells),
        "transform.r.x" => ClmBindingValues::TransformRX(cells),
        "transform.r.y" => ClmBindingValues::TransformRY(cells),
        "transform.r.z" => ClmBindingValues::TransformRZ(cells),
        "opacity" => ClmBindingValues::Opacity(cells),
        "tint.r" => ClmBindingValues::TintR(cells),
        "tint.g" => ClmBindingValues::TintG(cells),
        "tint.b" => ClmBindingValues::TintB(cells),
        "screenTint.r" => ClmBindingValues::ScreenTintR(cells),
        "screenTint.g" => ClmBindingValues::ScreenTintG(cells),
        "screenTint.b" => ClmBindingValues::ScreenTintB(cells),
        "outputScale.x" => ClmBindingValues::OutputScaleX(cells),
        "outputScale.y" => ClmBindingValues::OutputScaleY(cells),
        // emissionStrength and any unknown target are folded by no renderer path,
        // so they're dropped here exactly as the LegacyPuppet importer drops them.
        _ => return None,
    })
}

fn reflect_z(value: f32) -> f32 {
    if value == 0.0 {
        0.0
    } else {
        -value
    }
}

/// In-memory dense matrix, row-major `data[y * width + x]` — the import-side
/// intermediate between inx JSON and the sparse wire form.
struct Dense<T> {
    width: usize,
    height: usize,
    data: Vec<T>,
}

impl<T: Default + Clone> Default for Dense<T> {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            data: Vec::new(),
        }
    }
}

#[allow(clippy::needless_range_loop)]
fn sparsify<T: FillCell + Clone>(
    d: Dense<T>,
    is_set: Option<&[Vec<bool>]>,
    axis_x: &[f32],
    axis_y: &[f32],
    identity: &T,
    close: fn(&T, &T) -> bool,
) -> ClmCells<T> {
    let all_authored = |d: &Dense<T>| ClmCells {
        cells: d
            .data
            .iter()
            .enumerate()
            .map(|(i, value)| ClmCell {
                x: (i % d.width.max(1)) as u32,
                y: (i / d.width.max(1)) as u32,
                value: value.clone(),
            })
            .collect(),
    };
    let Some(mask) = is_set else {
        return all_authored(&d);
    };
    // isSet is [x][y] like the value matrix; a shape mismatch means the mask
    // is unusable, so keep everything.
    if mask.len() != d.width || mask.iter().any(|col| col.len() != d.height) {
        return all_authored(&d);
    }
    let mut cells = Vec::new();
    for y in 0..d.height {
        for x in 0..d.width {
            if mask[x][y] {
                cells.push(ClmCell {
                    x: x as u32,
                    y: y as u32,
                    value: d.data[y * d.width + x].clone(),
                });
            }
        }
    }
    let authored: Vec<((u32, u32), T)> = cells
        .iter()
        .map(|c| ((c.x, c.y), c.value.clone()))
        .collect();
    let derived = derive_dense(d.width, d.height, axis_x, axis_y, &authored, identity);
    let reproduced = derived.iter().zip(&d.data).all(|(a, b)| close(a, b));
    if reproduced {
        ClmCells { cells }
    } else {
        all_authored(&d)
    }
}

fn close_f32(a: &f32, b: &f32) -> bool {
    // Tight enough that a passing fill is visually indistinguishable from the
    // baked values (sub-0.01px on a 100px-range binding); anything looser
    // falls back to all-cells-authored, which preserves parity exactly.
    (a - b).abs() <= 1e-4 + 1e-4 * b.abs()
}

// &Vec, not &[f32]: this must match the `fn(&T, &T) -> bool` shape sparsify
// takes with `T = Vec<f32>`.
#[allow(clippy::ptr_arg)]
fn close_vec(a: &Vec<f32>, b: &Vec<f32>) -> bool {
    let n = a.len().max(b.len());
    (0..n).all(|i| {
        close_f32(
            &a.get(i).copied().unwrap_or(0.0),
            &b.get(i).copied().unwrap_or(0.0),
        )
    })
}

/// inx matrices are `values[x][y]` (outer = x-axis points); the intermediate
/// stores them row-major as `data[y * width + x]`, matching the runtime layout.
fn matrix_dims(rows: &[serde_json::Value]) -> (usize, usize) {
    let width = rows.len();
    let height = rows
        .first()
        .and_then(|c| c.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    (width, height)
}

fn matrix_f32(v: &serde_json::Value) -> Dense<f32> {
    let Some(rows) = v.as_array() else {
        return Dense::default();
    };
    let (width, height) = matrix_dims(rows);
    if width == 0 || height == 0 {
        return Dense::default();
    }
    let mut data = vec![0.0f32; width * height];
    for (x, col) in rows.iter().enumerate() {
        let Some(arr) = col.as_array() else { continue };
        for (y, cell) in arr.iter().enumerate().take(height) {
            data[y * width + x] = cell.as_f64().unwrap_or(0.0) as f32;
        }
    }
    Dense {
        width,
        height,
        data,
    }
}

/// Deform cells are arrays of `[x, y]` points; each cell flattens to
/// `[x0, y0, x1, y1, …]`.
fn matrix_vec2_flat(v: &serde_json::Value) -> Dense<Vec<f32>> {
    let Some(rows) = v.as_array() else {
        return Dense::default();
    };
    let (width, height) = matrix_dims(rows);
    if width == 0 || height == 0 {
        return Dense::default();
    }
    let mut data: Vec<Vec<f32>> = vec![Vec::new(); width * height];
    for (x, col) in rows.iter().enumerate() {
        let Some(arr) = col.as_array() else { continue };
        for (y, cell) in arr.iter().enumerate().take(height) {
            let mut flat = Vec::new();
            if let Some(points) = cell.as_array() {
                for p in points {
                    let coords = p.as_array();
                    let px = coords.and_then(|a| a.first()).and_then(|n| n.as_f64());
                    let py = coords.and_then(|a| a.get(1)).and_then(|n| n.as_f64());
                    if let (Some(px), Some(py)) = (px, py) {
                        flat.push(px as f32);
                        flat.push(py as f32);
                    }
                }
            }
            data[y * width + x] = flat;
        }
    }
    Dense {
        width,
        height,
        data,
    }
}

/// Mirrors `convert::convert_blend_mode`: an unrecognised name is an error, not
/// a silent downgrade to Normal. `.clm` is the source of truth after import, so
/// a wrong-but-valid blend mode baked in here is unrecoverable.
fn blend(s: Option<&str>) -> Result<BlendMode, ImportError> {
    match s {
        None => Ok(BlendMode::default()),
        Some(name) => BlendMode::from_name(name)
            .ok_or_else(|| ImportError::UnknownBlendMode(name.to_string())),
    }
}

fn mask_mode(s: Option<&str>) -> MaskMode {
    match s {
        Some("DodgeMask") => MaskMode::DodgeMask,
        _ => MaskMode::Mask,
    }
}

fn interp(s: Option<&str>) -> InterpolateMode {
    match s {
        Some("Nearest") => InterpolateMode::Nearest,
        Some("Stepped") => InterpolateMode::Stepped,
        Some("Cubic") => InterpolateMode::Cubic,
        _ => InterpolateMode::Linear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::InxModel;

    /// The full reference model. No such model ships in the tree yet, so
    /// every test that needs one is `#[ignore]`d; drop a model at this path and
    /// remove the attributes to re-enable them.
    fn load_reference() -> InxModel {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../example_models/reference/reference.inx"
        );
        let bytes = std::fs::read(path).expect("read reference.inx");
        InxModel::parse(std::io::Cursor::new(bytes.as_slice())).expect("parse reference.inx")
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn reference_inx_roundtrips_through_clm() {
        let model = load_reference();
        let file = from_inx_model_to_legacy(&model).unwrap();

        let parts = file
            .doc
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, LegacyNodeKind::Part(_)))
            .count();
        assert_eq!(parts, 117, "expected 117 Part nodes");
        assert_eq!(file.textures.len(), 87);

        let bytes = crate::Model::from_legacy(&file)
            .and_then(|m| m.to_clm_bytes())
            .unwrap();
        let reopened = crate::Model::from_clm_bytes(&bytes)
            .unwrap()
            .to_legacy()
            .unwrap();
        assert_eq!(
            reopened.doc, file.doc,
            "structure must round-trip through .clm"
        );
        assert_eq!(reopened.textures, file.textures, "textures must round-trip");
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn arena_is_topologically_ordered_with_one_root() {
        let model = load_reference();
        let file = from_inx_model_to_legacy(&model).unwrap();
        let nodes = &file.doc.nodes;

        let roots = nodes.iter().filter(|n| n.parent.is_none()).count();
        assert_eq!(roots, 1, "exactly one root");
        for (i, n) in nodes.iter().enumerate() {
            if let Some(p) = n.parent {
                assert!(
                    (p as usize) < i,
                    "parent {p} must precede child {i} (topo order)"
                );
            }
        }

        // Every cross-reference resolves to an in-range index.
        let n = nodes.len() as u32;
        let p = file.doc.params.len() as u32;
        for node in nodes {
            match &node.kind {
                LegacyNodeKind::Part(part) => {
                    assert!(part.masks.iter().all(|m| m.source < n));
                }
                LegacyNodeKind::Composite(c) => assert!(c.masks.iter().all(|m| m.source < n)),
                LegacyNodeKind::SimplePhysics(sp) => {
                    assert!(sp.target_param.is_none_or(|t| t < p));
                }
                _ => {}
            }
        }
        for param in &file.doc.params {
            assert!(param.bindings.iter().all(|b| b.node < n));
        }
    }

    #[test]
    #[ignore = "needs the reference model at example_models/reference/"]
    fn captures_authored_propagate_meshgroup() {
        let model = load_reference();
        let file = from_inx_model_to_legacy(&model).unwrap();
        // The root "Puppet Body" composite authors propagate_meshgroup=false —
        // the divergence the LegacyPuppet path hardcodes to true.
        assert!(
            file.doc.nodes.iter().any(|n| matches!(
                &n.kind,
                LegacyNodeKind::Composite(c) if !c.propagate_meshgroup
            )),
            "an authored propagate_meshgroup=false composite must be captured"
        );
        assert!(file.doc.params.iter().any(|p| !p.bindings.is_empty()));
    }
}
