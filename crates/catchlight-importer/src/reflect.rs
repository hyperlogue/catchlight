//! inochi2d JSON read into catchlight value types. `.inx` is Y-down with
//! lower `zsort` in front; catchlight is Y-up with higher `z_order` in front,
//! and this module is the only place that conversion happens.

use serde::Deserialize;
use std::collections::HashMap;

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::fill::{derive_dense, FillCell};
use catchlight_core::formats::clm::{
    ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmMesh, ClmTransform,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::load_budget::{
    LoadLimitError, LoadLimits, LoadResource, MAX_BINDING_GRID_CELLS,
};
use catchlight_core::NodeId;

use crate::error::ImportError;
use crate::schema::{SchemaMesh, SchemaNode, SchemaParam, SchemaTransform};

/// `.inx` names are not unique, so messages carry the minted Id too.
#[derive(Clone, Copy)]
pub(crate) struct NodeRef<'a> {
    pub(crate) id: &'a NodeId,
    pub(crate) name: &'a str,
}

/// DFS pre-order, so `parent < self` and sibling order is kept. Duplicate
/// uuids map to their first occurrence. Uses an explicit stack because
/// untrusted `.inx` depth is unbounded and recursion could overflow.
pub(crate) fn flatten(
    root: &serde_json::Value,
    root_parent: Option<u32>,
    flat: &mut Vec<(SchemaNode, Option<u32>)>,
    node_index: &mut HashMap<u32, u32>,
) -> Result<(), ImportError> {
    let mut stack: Vec<(&serde_json::Value, Option<u32>)> = vec![(root, root_parent)];
    while let Some((value, parent)) = stack.pop() {
        let idx = flat.len() as u32;
        let schema = parse_node_shallow(value)?;
        if let Some(uuid) = schema.uuid {
            node_index.entry(uuid).or_insert(idx);
        }
        flat.push((schema, parent));
        if let Some(children) = value.get("children").and_then(|c| c.as_array()) {
            stack.extend(children.iter().rev().map(|child| (child, Some(idx))));
        }
    }
    Ok(())
}

/// Every field but the positional arrays reads leniently, so an object fails
/// only on a malformed positional array. `children` is not a field, and
/// deserializing from a borrowed value skips it without descending into it.
fn parse_node_shallow(value: &serde_json::Value) -> Result<SchemaNode, ImportError> {
    let serde_json::Value::Object(map) = value else {
        return Ok(SchemaNode::default());
    };
    SchemaNode::deserialize(value).map_err(|e| {
        ImportError::MalformedPayload(format!(
            "node {:?} (uuid {}): {e}",
            map.get("name").and_then(|n| n.as_str()).unwrap_or_default(),
            map.get("uuid").unwrap_or(&serde_json::Value::Null),
        ))
    })
}

// Short arrays fill missing components individually, so `"scale": [2.0]`
// keeps its 2.0 instead of snapping to the whole default.
pub(crate) fn vec2_arr(v: &[f32], default: [f32; 2]) -> [f32; 2] {
    [
        v.first().copied().unwrap_or(default[0]),
        v.get(1).copied().unwrap_or(default[1]),
    ]
}

pub(crate) fn vec3_arr(v: &[f32], default: [f32; 3]) -> [f32; 3] {
    [
        v.first().copied().unwrap_or(default[0]),
        v.get(1).copied().unwrap_or(default[1]),
        v.get(2).copied().unwrap_or(default[2]),
    ]
}

pub(crate) fn convert_transform(t: Option<&SchemaTransform>) -> ClmTransform {
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

/// A Y-flip negates translation Y and rotation about X and Z; rotation about
/// Y and scale are unchanged.
fn reflect_transform_y(t: &mut ClmTransform) {
    t.translation[1] = -t.translation[1];
    t.rotation[0] = -t.rotation[0];
    t.rotation[2] = -t.rotation[2];
}

/// Out-of-range indices, or UVs that do not pair with the vertices when
/// `textured`, are refused: inochi2d's rendering of them is undefined.
pub(crate) fn convert_mesh(
    m: Option<&SchemaMesh>,
    node: NodeRef<'_>,
    textured: bool,
) -> Result<ClmMesh, ImportError> {
    let Some(m) = m else {
        return Ok(ClmMesh::default());
    };
    let malformed = |detail: String| ImportError::MalformedMesh {
        id: node.id.to_string(),
        name: node.name.to_string(),
        detail,
    };
    let vertices = m.verts.len() / 2;
    if textured && m.uvs.len() / 2 != vertices {
        return Err(malformed(format!(
            "{vertices} vertices against {} uvs",
            m.uvs.len() / 2
        )));
    }
    if let Some(past) = m.indices.iter().copied().find(|&i| i as usize >= vertices) {
        return Err(malformed(format!(
            "index {past} names a vertex past the mesh's {vertices}"
        )));
    }
    let max = m.indices.iter().copied().max().unwrap_or(0);
    let indices = if max <= u16::MAX as u32 {
        ClmIndices::U16(m.indices.iter().map(|&i| i as u16).collect())
    } else {
        ClmIndices::U32(m.indices.clone())
    };
    // Only an untextured mesh group reaches here with ragged UVs; nothing
    // samples them, and the `.clm` reader refuses unpaired UVs.
    let uvs = if m.uvs.len() / 2 == vertices {
        m.uvs[..vertices * 2].to_vec()
    } else {
        tracing::warn!(
            "node {} ({:?}): dropping {} uvs that do not pair with its {vertices} vertices; \
             nothing samples a mesh group's",
            node.id,
            node.name,
            m.uvs.len() / 2,
        );
        Vec::new()
    };
    let mut mesh = ClmMesh {
        verts: m.verts[..vertices * 2].to_vec(),
        uvs,
        indices,
        origin: vec2_arr(&m.origin, [0.0, 0.0]),
    };
    reflect_mesh_y(&mut mesh);
    Ok(mesh)
}

/// UVs are texture space and stay as authored.
fn reflect_mesh_y(m: &mut ClmMesh) {
    for y in m.verts.iter_mut().skip(1).step_by(2) {
        *y = -*y;
    }
    m.origin[1] = -m.origin[1];
}

/// `.clm` keeps only the `isSet` cells. If our fill does not reproduce the
/// baked dense matrix from them, every cell is kept to preserve pixel parity.
pub(crate) fn convert_binding_values(
    kind: &str,
    v: &serde_json::Value,
    is_set: Option<&[Vec<bool>]>,
    axis_x: &[f32],
    axis_y: &[f32],
    node: NodeRef<'_>,
    deform_len: usize,
) -> Result<Option<ClmBindingValues>, ImportError> {
    if kind == "deform" {
        let mut d = matrix_vec2_flat(v, node)?;
        for cell in &mut d.data {
            for y in cell.iter_mut().skip(1).step_by(2) {
                *y = -*y;
            }
        }
        let offsets = (d.data.len() as u64).saturating_mul(deform_len as u64 / 2);
        let limit = LoadLimits::default().deform_offsets;
        if offsets > limit {
            return Err(ImportError::LoadLimit(LoadLimitError {
                resource: LoadResource::DeformOffsets,
                limit,
                got: offsets,
            }));
        }
        fit_deform_cells(&mut d, deform_len, node);
        let cells = sparsify(
            d,
            is_set,
            axis_x,
            axis_y,
            &vec![0.0f32; deform_len],
            close_vec,
        );
        return Ok(Some(ClmBindingValues::Deform(cells)));
    }
    let reflected = matches!(
        kind,
        "zSort" | "transform.t.y" | "transform.r.x" | "transform.r.z"
    );
    let identity = match kind {
        "transform.s.x" | "transform.s.y" | "opacity" | "tint.r" | "tint.g" | "tint.b" => 1.0f32,
        _ => 0.0,
    };
    let mut d = matrix_f32(v, node, kind)?;
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
    Ok(Some(match kind {
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
        // No renderer path reads emissionStrength or unknown targets.
        _ => return Ok(None),
    }))
}

/// Zipping a ragged deform cell against the mesh draws what the source drew:
/// offsets past the last vertex drive nothing, and missing ones are zero.
fn fit_deform_cells(d: &mut Dense<Vec<f32>>, deform_len: usize, node: NodeRef<'_>) {
    let ragged = d.data.iter().filter(|c| c.len() != deform_len).count();
    if ragged == 0 {
        return;
    }
    let shortest = d.data.iter().map(Vec::len).min().unwrap_or(0);
    let longest = d.data.iter().map(Vec::len).max().unwrap_or(0);
    tracing::warn!(
        "node {} ({:?}): {ragged} deform cell(s) hold {shortest}..={longest} offsets where the \
         node's mesh takes {deform_len}; truncating the long ones and zero-padding the short ones",
        node.id,
        node.name,
    );
    for cell in &mut d.data {
        cell.resize(deform_len, 0.0);
    }
}

pub(crate) fn reflect_z(value: f32) -> f32 {
    if value == 0.0 {
        0.0
    } else {
        -value
    }
}

/// Row-major: `data[y * width + x]`.
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
    // Sub-0.01px on a 100px-range binding; a miss keeps every cell.
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

type Columns<'v> = Vec<&'v [serde_json::Value]>;

/// inx matrices are `values[x][y]` (outer = x-axis points). Every column must
/// be as tall as the first, and the grid must fit the `.clm` loader's limit,
/// before anything is allocated for it.
fn matrix_columns<'v>(
    v: &'v serde_json::Value,
    node: NodeRef<'_>,
    kind: &str,
) -> Result<Option<(Columns<'v>, usize)>, ImportError> {
    let Some(rows) = v.as_array() else {
        return Ok(None);
    };
    let malformed = |detail| malformed_values(node, kind, detail);
    let mut columns: Columns<'v> = Vec::with_capacity(rows.len());
    for (x, col) in rows.iter().enumerate() {
        let Some(col) = col.as_array() else {
            return Err(malformed(format!("column {x} is not an array")));
        };
        if let Some(first) = columns.first() {
            if col.len() != first.len() {
                return Err(malformed(format!(
                    "column {x} holds {} cells where column 0 holds {}",
                    col.len(),
                    first.len()
                )));
            }
        }
        columns.push(col.as_slice());
    }
    let height = columns.first().map_or(0, |c| c.len());
    if height == 0 {
        return Ok(None);
    }
    let cells = (columns.len() as u64).saturating_mul(height as u64);
    if cells > MAX_BINDING_GRID_CELLS {
        return Err(ImportError::LoadLimit(LoadLimitError {
            resource: LoadResource::BindingGrid,
            limit: MAX_BINDING_GRID_CELLS,
            got: cells,
        }));
    }
    Ok(Some((columns, height)))
}

fn malformed_values(node: NodeRef<'_>, kind: &str, detail: String) -> ImportError {
    ImportError::MalformedPayload(format!(
        "node {} ({:?}): {kind:?} binding values {detail}",
        node.id, node.name
    ))
}

fn matrix_f32(
    v: &serde_json::Value,
    node: NodeRef<'_>,
    kind: &str,
) -> Result<Dense<f32>, ImportError> {
    let Some((columns, height)) = matrix_columns(v, node, kind)? else {
        return Ok(Dense::default());
    };
    let width = columns.len();
    let mut data = vec![0.0f32; width * height];
    for (x, col) in columns.iter().enumerate() {
        for (y, cell) in col.iter().enumerate() {
            let Some(value) = cell.as_f64() else {
                return Err(malformed_values(
                    node,
                    kind,
                    format!("cell [{x}][{y}] is not a number"),
                ));
            };
            data[y * width + x] = value as f32;
        }
    }
    Ok(Dense {
        width,
        height,
        data,
    })
}

fn matrix_vec2_flat(
    v: &serde_json::Value,
    node: NodeRef<'_>,
) -> Result<Dense<Vec<f32>>, ImportError> {
    let Some((columns, height)) = matrix_columns(v, node, "deform")? else {
        return Ok(Dense::default());
    };
    let width = columns.len();
    let mut data: Vec<Vec<f32>> = vec![Vec::new(); width * height];
    for (x, col) in columns.iter().enumerate() {
        for (y, cell) in col.iter().enumerate() {
            let malformed = |detail: &str| {
                malformed_values(node, "deform", format!("cell [{x}][{y}] {detail}"))
            };
            let Some(points) = cell.as_array() else {
                return Err(malformed("is not an array of points"));
            };
            let mut flat = Vec::with_capacity(points.len() * 2);
            for (i, p) in points.iter().enumerate() {
                let coords = p.as_array();
                let px = coords.and_then(|a| a.first()).and_then(|n| n.as_f64());
                let py = coords.and_then(|a| a.get(1)).and_then(|n| n.as_f64());
                let (Some(px), Some(py)) = (px, py) else {
                    return Err(malformed(&format!("point {i} is not two numbers")));
                };
                flat.push(px as f32);
                flat.push(py as f32);
            }
            data[y * width + x] = flat;
        }
    }
    Ok(Dense {
        width,
        height,
        data,
    })
}

/// An unknown name is an error rather than Normal: a wrong-but-valid mode in
/// the written `.clm` would be unrecoverable.
pub(crate) fn blend(s: Option<&str>) -> Result<BlendMode, ImportError> {
    match s {
        None => Ok(BlendMode::default()),
        Some(name) => BlendMode::from_name(name)
            .ok_or_else(|| ImportError::UnknownBlendMode(name.to_string())),
    }
}

pub(crate) fn mask_mode(s: Option<&str>) -> MaskMode {
    match s {
        Some("DodgeMask") => MaskMode::DodgeMask,
        _ => MaskMode::Mask,
    }
}

pub(crate) fn interp(s: Option<&str>) -> InterpolateMode {
    match s {
        Some("Nearest") => InterpolateMode::Nearest,
        Some("Stepped") => InterpolateMode::Stepped,
        Some("Cubic") => InterpolateMode::Cubic,
        _ => InterpolateMode::Linear,
    }
}

/// inochi2d's axis-point defaults. An empty axis collapses to a single stop
/// so the param can still drive.
pub(crate) fn axes_of(p: &SchemaParam) -> (Vec<f32>, Vec<f32>) {
    let is_vec2 = p.is_vec2.unwrap_or(false);
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
    (axis_x, axis_y)
}
