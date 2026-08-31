//! The reflection: inochi2d's JSON read into the value types catchlight
//! stores. Everything here is about the *source* convention, and nothing here
//! knows what document the values end up in.
//!
//! `.inx` is authored **Y-down with lower `zsort` in front** and catchlight is
//! **Y-up with higher `z_order` in front**, so this module is the one place
//! that conversion happens. See the crate doc for the exact set of fields it
//! negates, and `to_clm.rs`'s
//! `the_import_reflects_exactly_the_y_bearing_fields` for the guard.
//!
//! It is also where the reader's tolerance lives: a shorter-than-expected
//! array fills its missing components one at a time, a field of the wrong
//! JSON type falls back to its default, and the node walk carries its own
//! stack because `.inx` is untrusted and unbounded in depth.

use std::collections::HashMap;

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::fill::{derive_dense, FillCell};
use catchlight_core::formats::clm::{
    ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmMesh, ClmTransform,
};
use catchlight_core::interpolate::InterpolateMode;

use crate::error::ImportError;
use crate::schema::{SchemaMesh, SchemaNode, SchemaParam, SchemaTransform};

/// DFS pre-order flatten: append this node (parsed shallow) with its parent
/// index, map its uuid → index, then descend into children. Pre-order keeps
/// `parent < self` and preserves sibling order.
///
/// The descent carries its own stack. `.inx` is untrusted input and its node
/// tree is unbounded in depth, so recursing here would let a deep enough file
/// overflow the thread's stack instead of loading or erroring — and this is
/// the only path a `.inx` still has.
pub(crate) fn flatten(
    root: &serde_json::Value,
    root_parent: Option<u32>,
    flat: &mut Vec<(SchemaNode, Option<u32>)>,
    node_index: &mut HashMap<u32, u32>,
) {
    // Children are pushed in reverse so the top of the stack is always the
    // next node in document order.
    let mut stack: Vec<(&serde_json::Value, Option<u32>)> = vec![(root, root_parent)];
    while let Some((value, parent)) = stack.pop() {
        let idx = flat.len() as u32;
        let schema = parse_node_shallow(value);
        if let Some(uuid) = schema.uuid {
            node_index.entry(uuid).or_insert(idx);
        }
        flat.push((schema, parent));
        if let Some(children) = value.get("children").and_then(|c| c.as_array()) {
            stack.extend(children.iter().rev().map(|child| (child, Some(idx))));
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

// Shorter-than-expected arrays fill the missing components individually.
// Falling back to the whole default instead would snap a node with
// `"scale": [2.0]` to unit scale, which is not what the file says.
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

/// inochi2d authors in a Y-down frame; catchlight is Y-up. Reflect across the
/// X axis: negate translation Y and the two Euler angles a Y-flip inverts
/// (rotation about X and Z). Rotation about Y and scale are unchanged.
fn reflect_transform_y(t: &mut ClmTransform) {
    t.translation[1] = -t.translation[1];
    t.rotation[0] = -t.rotation[0];
    t.rotation[2] = -t.rotation[2];
}

pub(crate) fn convert_mesh(m: Option<&SchemaMesh>, node: &str) -> ClmMesh {
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
    normalize_mesh(&mut mesh, node);
    mesh
}

/// Hold an `.inx` mesh to the shape a [`ClmMesh`] is allowed to be: `[x, y]`
/// pairs, uvs that match them or none at all, and every index naming a vertex
/// the mesh has. `.inx` is untrusted and the `.clm` reader refuses a mesh that
/// is none of those, so a source that got it wrong is repaired here rather
/// than written into a file that will not open.
fn normalize_mesh(m: &mut ClmMesh, node: &str) {
    if !m.verts.len().is_multiple_of(2) {
        tracing::debug!("mesh on {node:?}: dropping a vertex with no y");
        m.verts.pop();
    }
    if !m.uvs.is_empty() && m.uvs.len() != m.verts.len() {
        tracing::debug!(
            "mesh on {node:?}: dropping {} uvs for {} vertices",
            m.uvs.len() / 2,
            m.verts.len() / 2,
        );
        m.uvs.clear();
    }
    let vertices = m.verts.len() / 2;
    match &mut m.indices {
        ClmIndices::U16(v) => keep_triangles_in_range(v, vertices, node),
        ClmIndices::U32(v) => keep_triangles_in_range(v, vertices, node),
    }
}

/// Drop the triangles that name a vertex the mesh does not have, whole, so
/// the remaining ones still describe the same surface. A trailing group of
/// fewer than three indices is not a triangle and is kept or dropped on the
/// same test — the reader only asks that every index name a vertex.
fn keep_triangles_in_range<T: Copy + Into<u32>>(indices: &mut Vec<T>, vertices: usize, node: &str) {
    let kept: Vec<T> = indices
        .chunks(3)
        .filter(|t| t.iter().all(|&i| (i.into() as usize) < vertices))
        .flatten()
        .copied()
        .collect();
    if kept.len() != indices.len() {
        tracing::debug!(
            "mesh on {node:?}: dropping {} index/indices naming a vertex it does not have",
            indices.len() - kept.len(),
        );
    }
    *indices = kept;
}

/// Reflect mesh geometry into catchlight's Y-up frame (see [`reflect_transform_y`]).
/// UVs are texture space and stay as authored.
fn reflect_mesh_y(m: &mut ClmMesh) {
    for y in m.verts.iter_mut().skip(1).step_by(2) {
        *y = -*y;
    }
    m.origin[1] = -m.origin[1];
}

/// `.inx` carries both the baked dense matrix and the authored `isSet` mask;
/// `.clm` keeps only the authored cells. The baked values double as the oracle:
/// if our fill does not reproduce them from the authored set, the binding falls
/// back to all-cells-authored, preserving pixel parity by construction.
pub(crate) fn convert_binding_values(
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
        // so they are dropped here.
        _ => return None,
    })
}

pub(crate) fn reflect_z(value: f32) -> f32 {
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

/// An unrecognised blend-mode name is an error, not a silent downgrade to
/// Normal. `.clm` is the source of truth after import, so
/// a wrong-but-valid blend mode baked in here is unrecoverable.
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

/// inochi2d's axis-point defaults, resolved once so the document holds final
/// key positions: an absent axis defaults to `[0, 1]` on x and to `[0, 1]` /
/// `[0]` on y (vec2 / scalar), and a degenerate empty axis collapses to a
/// single stop so the param can still drive.
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
