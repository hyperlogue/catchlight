//! Bake settled LivLiv poses into existing one/two-param deform bindings.
//!
//! Yaw/pitch share a grid to preserve their nonlinear interaction. Other
//! controls contribute neutral-relative offsets plus pairwise residual grids.
//! Three-way and higher interactions remain an approximation, measured on
//! held-out mixed poses below.
//! The same mesh and bindings drive every texture variant and its mask.
use super::runtime::{Pose, Runtime, Vertex, PARAMETERS};
use super::{Error, Layer, Result};
use catchlight_core::formats::clm::*;
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::{NodeId, ParamId};
use serde::Serialize;

const SAMPLES: usize = 9;
// Charge before allocating each table, including all texture variants. Keep
// below core's deform budget and bounded independently of ZIP/image sizes.
const MAX_COMPONENTS: usize = 32_000_000;

#[derive(Debug, Clone, Default, Serialize)]
pub struct BakeSummary {
    pub samples_per_axis: usize,
    pub interaction_samples_per_axis: usize,
    pub deform_bindings: usize,
    pub stored_offset_components: usize,
    pub measured_vertices: usize,
    /// Distance in source canvas pixels, before Y reflection. This measures
    /// the binding approximation to this evaluator, not renderer parity.
    pub max_mixed_pose_error_pixels: f64,
}

pub(super) fn add_params(doc: &mut ClmStructure) -> Result<()> {
    for (name, min, max, default) in PARAMETERS {
        doc.params.push(ClmParam {
            id: ParamId::new(name)?,
            name: name.into(),
            min: min as f32,
            max: max as f32,
            default: default as f32,
        });
    }
    Ok(())
}

pub(super) fn mesh(l: &Layer, canvas: [u32; 2]) -> (ClmMesh, Vec<[f64; 2]>) {
    let cell = ((canvas[0].max(canvas[1]) as f64 / 128.).round() as u32).max(6);
    let axis = |lo: u32, size: u32| {
        let mut out = vec![lo];
        let mut v = (lo / cell + 1) * cell;
        while v < lo + size {
            out.push(v);
            v += cell;
        }
        out.push(lo + size);
        out
    };
    let xs = axis(l.x, l.w);
    let ys = axis(l.y, l.h);
    let mut points = Vec::new();
    let mut mesh = ClmMesh::default();
    for &y in &ys {
        for &x in &xs {
            points.push([x as f64, y as f64]);
            mesh.verts.extend([
                x as f32 - canvas[0] as f32 / 2.,
                canvas[1] as f32 / 2. - y as f32,
            ]);
            mesh.uvs
                .extend([(x - l.x) as f32 / l.w as f32, (y - l.y) as f32 / l.h as f32]);
        }
    }
    let mut indices = Vec::new();
    for row in 0..ys.len() - 1 {
        for col in 0..xs.len() - 1 {
            let a = (row * xs.len() + col) as u32;
            let b = a + 1;
            let c = a + xs.len() as u32;
            let d = c + 1;
            indices.extend([a, c, b, b, c, d]);
        }
    }
    mesh.indices = ClmIndices::U32(indices);
    (mesh, points)
}

struct Table {
    axes: Vec<Vec<f32>>,
    parameters: Vec<usize>,
    mouth: bool,
    values: Vec<Vec<f32>>,
}
impl Table {
    fn evaluate(&self, p: Pose, vertex: usize) -> [f64; 2] {
        let coord = |axis: usize| {
            if self.mouth && axis == 0 {
                p.1 as f64 / self.axes[0].len().saturating_sub(1).max(1) as f64
            } else {
                let i = self.parameters[axis];
                (p.0[i] - PARAMETERS[i].1) / (PARAMETERS[i].2 - PARAMETERS[i].1)
            }
        };
        let locate = |axis: usize| {
            let t = coord(axis).clamp(0., 1.) * (self.axes[axis].len() - 1) as f64;
            let lo = (t.floor() as usize).min(self.axes[axis].len().saturating_sub(2));
            (lo, t - lo as f64)
        };
        let (x, tx) = locate(0);
        let (y, ty) = if self.axes.len() == 2 {
            locate(1)
        } else {
            (0, 0.)
        };
        let width = self.axes[0].len();
        let value =
            |x: usize, y: usize, k: usize| self.values[y * width + x][vertex * 2 + k] as f64;
        let mut out = [0., 0.];
        for (k, v) in out.iter_mut().enumerate() {
            let a = value(x, y, k) * (1. - tx) + value(x + 1, y, k) * tx;
            *v = if self.axes.len() == 2 {
                a * (1. - ty) + (value(x, y + 1, k) * (1. - tx) + value(x + 1, y + 1, k) * tx) * ty
            } else {
                a
            };
        }
        out
    }
}

pub(super) fn layer(
    runtime: &Runtime,
    index: usize,
    points: &[[f64; 2]],
    nodes: &[NodeId],
    doc: &mut ClmStructure,
    summary: &mut BakeSummary,
) -> Result<()> {
    summary.samples_per_axis = SAMPLES;
    summary.interaction_samples_per_axis = 5;
    let vertices = runtime.prepare(index, points)?;
    let controls = runtime.controls(index, &vertices);
    let axis: Vec<_> = (0..SAMPLES)
        .map(|i| i as f32 / (SAMPLES - 1) as f32)
        .collect();
    let mut tables = Vec::new();
    for params in std::iter::once(vec![0, 1]).chain(
        controls
            .iter()
            .copied()
            .filter(|&i| i >= 2)
            .map(|i| vec![i]),
    ) {
        let axes = vec![axis.clone(); params.len()];
        tables.push(table(
            runtime,
            index,
            &vertices,
            axes,
            params,
            false,
            false,
            nodes.len(),
            summary,
        )?);
    }
    if runtime.frame_jaw.len() > 1 && runtime.mouth_moves(index, &vertices) {
        let frames = runtime.frame_jaw.len();
        let mouth_axis = (0..frames)
            .map(|i| i as f32 / (frames - 1) as f32)
            .collect();
        // First coordinate is the discrete frame selector; second is continuous
        // opening within that drawing. Integer frame values reproduce samples.
        tables.push(table(
            runtime,
            index,
            &vertices,
            vec![mouth_axis, axis.clone()],
            vec![12, 12],
            true,
            false,
            nodes.len(),
            summary,
        )?);
    }
    // Pairwise residuals retain interactions using the existing two-param
    // binding format: F(a,b) - F(a,0) - F(0,b) + F(0,0). Their axes include
    // every default, so they contribute zero when either control is neutral.
    let interaction_axis: Vec<f32> = (0..=4).map(|i| i as f32 / 4.).collect();
    for &a in &controls {
        for &b in controls.iter().filter(|&&b| b > a) {
            if a == 0 && b == 1 {
                continue;
            }
            let t = table(
                runtime,
                index,
                &vertices,
                vec![interaction_axis.clone(); 2],
                vec![a, b],
                false,
                true,
                nodes.len(),
                summary,
            )?;
            if t.values.iter().flatten().any(|v| v.abs() >= 1e-6) {
                tables.push(t);
            }
        }
    }
    if runtime.frame_jaw.len() > 1 && runtime.mouth_moves(index, &vertices) {
        let frames = runtime.frame_jaw.len();
        let mouth_axis: Vec<f32> = (0..frames)
            .map(|i| i as f32 / (frames - 1) as f32)
            .collect();
        for &b in &controls {
            let t = table(
                runtime,
                index,
                &vertices,
                vec![mouth_axis.clone(), interaction_axis.clone()],
                vec![12, b],
                true,
                true,
                nodes.len(),
                summary,
            )?;
            if t.values.iter().flatten().any(|v| v.abs() >= 1e-6) {
                tables.push(t);
            }
        }
    }
    measure(runtime, index, &vertices, &tables, summary);
    for t in tables {
        if t.values.iter().flatten().all(|x| x.abs() < 1e-6) {
            continue;
        }
        let params: Vec<_> = t
            .parameters
            .iter()
            .enumerate()
            .map(|(axis, &i)| {
                ParamId::new(if t.mouth && axis == 0 {
                    format!(
                        "mouthFrame/{}",
                        runtime.mouth_id.as_deref().unwrap_or_default()
                    )
                } else {
                    PARAMETERS[i].0.into()
                })
            })
            .collect::<std::result::Result<_, _>>()?;
        let width = t.axes[0].len();
        let cells = ClmCells {
            cells: t
                .values
                .into_iter()
                .enumerate()
                .map(|(i, value)| ClmCell {
                    x: (i % width) as u32,
                    y: (i / width) as u32,
                    value,
                })
                .collect(),
        };
        for node in nodes {
            doc.bindings.push(ClmBinding {
                node: node.clone(),
                params: params.clone(),
                key_positions: t.axes.clone(),
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(cells.clone()),
            });
            summary.deform_bindings += 1;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn table(
    runtime: &Runtime,
    layer: usize,
    vertices: &[Vertex],
    axes: Vec<Vec<f32>>,
    parameters: Vec<usize>,
    mouth: bool,
    residual: bool,
    copies: usize,
    summary: &mut BakeSummary,
) -> Result<Table> {
    let count = axes.iter().map(Vec::len).product::<usize>();
    let components = count
        .saturating_mul(vertices.len())
        .saturating_mul(2)
        .saturating_mul(copies);
    if summary.stored_offset_components.saturating_add(components) > MAX_COMPONENTS {
        return Err(Error::Invalid("motion bake exceeds 32000000 offset components; package has too much geometry for this sampling grid".into()));
    }
    let mut values = Vec::with_capacity(count);
    for y in 0..axes.get(1).map_or(1, Vec::len) {
        for x in 0..axes[0].len() {
            let mut p = Pose::default();
            for (axis, &i) in parameters.iter().enumerate() {
                if mouth && axis == 0 {
                    p.1 = x;
                } else {
                    p.0[i] = PARAMETERS[i].1
                        + (PARAMETERS[i].2 - PARAMETERS[i].1)
                            * axes[axis][if axis == 0 { x } else { y }] as f64;
                }
            }
            let mut offsets = Vec::with_capacity(vertices.len() * 2);
            for v in vertices {
                let at = runtime.position(layer, v, p);
                // Existing native deforms sum, so each independent table stores a
                // neutral-relative contribution. The mouth table's frame-0 sample
                // is removed because mouthOpen already owns that contribution.
                let base = if residual {
                    let mut a = Pose::default();
                    let mut b = Pose::default();
                    if mouth {
                        a.1 = p.1;
                    } else {
                        a.0[parameters[0]] = p.0[parameters[0]];
                    }
                    b.0[parameters[1]] = p.0[parameters[1]];
                    let va = runtime.position(layer, v, a);
                    let vb = runtime.position(layer, v, b);
                    [va[0] + vb[0] - v.rest[0], va[1] + vb[1] - v.rest[1]]
                } else if mouth {
                    let mut neutral = p;
                    neutral.1 = 0;
                    runtime.position(layer, v, neutral)
                } else {
                    v.rest
                };
                let delta = [(at[0] - base[0]) as f32, -(at[1] - base[1]) as f32];
                if delta.iter().any(|v| !v.is_finite() || v.abs() > 1e7) {
                    return Err(Error::Invalid(
                        "motion evaluator produced an invalid displacement".into(),
                    ));
                }
                offsets.extend(delta);
            }
            values.push(offsets);
        }
    }
    if values.iter().flatten().any(|v| v.abs() >= 1e-6) {
        summary.stored_offset_components += components;
    }
    Ok(Table {
        axes,
        parameters,
        mouth,
        values,
    })
}

fn measure(
    runtime: &Runtime,
    layer: usize,
    vertices: &[Vertex],
    tables: &[Table],
    summary: &mut BakeSummary,
) {
    // Deterministic off-grid mixed poses, at most 64 vertices per layer.
    // Worst-case bounds are not claimed; report exactly what was measured.
    let stride = vertices.len().div_ceil(64).max(1);
    for case in 0..8 {
        let mut pose = Pose::default();
        for (i, (_, min, max, _)) in PARAMETERS.iter().enumerate() {
            let t = ((case * 37 + i * 19 + 11) % 97) as f64 / 97.;
            pose.0[i] = min + (max - min) * t;
        }
        if !runtime.frame_jaw.is_empty() {
            pose.1 = case % runtime.frame_jaw.len();
        }
        for (i, v) in vertices.iter().enumerate().step_by(stride) {
            let exact = runtime.position(layer, v, pose);
            let mut approx = v.rest;
            for t in tables {
                let d = t.evaluate(pose, i);
                approx[0] += d[0];
                approx[1] -= d[1];
            }
            let distance = (exact[0] - approx[0]).hypot(exact[1] - approx[1]);
            summary.max_mixed_pose_error_pixels = summary.max_mixed_pose_error_pixels.max(distance);
            summary.measured_vertices += 1;
        }
    }
}
