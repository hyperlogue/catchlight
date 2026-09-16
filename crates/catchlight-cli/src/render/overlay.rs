//! Mesh lines are an independent image overlay. Coverage is antialiased in
//! image pixels and composited as straight sRGB inputs in linear light.
use super::spec::{bad, Framing, MeshOverlay, Positions};
use crate::Error;
use catchlight_core::geometry::EvaluatedGeometry;
use catchlight_core::{Model, Puppet};
use std::collections::BTreeSet;

pub fn draw(
    pixels: &mut [u8],
    model: &Model,
    puppet: &Puppet,
    framing: &Framing,
    overlay: &MeshOverlay,
) -> Result<(), Error> {
    let mut neutral;
    let observed = match overlay.positions {
        Positions::Posed => puppet,
        Positions::Rest => {
            neutral = Puppet::new(model);
            neutral.set_physics_enabled(false);
            neutral.tick(model, 0.0);
            &neutral
        }
    };
    let geometry = EvaluatedGeometry::new(model, observed).map_err(|e| bad(e.to_string()))?;
    for id in &overlay.parts {
        let mesh = geometry.mesh(id).map_err(|e| bad(e.to_string()))?;
        let points = mesh
            .vertices()
            .map(|v| {
                let p = match overlay.positions {
                    Positions::Posed => v.world,
                    Positions::Rest => mesh
                        .local_to_world()
                        .transform_point3((v.rest - mesh.origin()).extend(0.0)),
                };
                if !p.is_finite() {
                    return Err(bad(format!(
                        "non-finite overlay vertex {} in {id}",
                        v.index
                    )));
                }
                let m = &framing.world_to_pixel;
                Ok([
                    m[0][0] * f64::from(p.x) + m[0][2],
                    m[1][1] * f64::from(p.y) + m[1][2],
                ])
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut edges = BTreeSet::new();
        for (_, t) in mesh.triangles() {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                edges.insert((a.min(b), a.max(b)));
            }
        }
        for (a, b) in edges {
            line(
                pixels,
                framing.size,
                points[a as usize],
                points[b as usize],
                overlay.color,
                overlay.width_px,
            );
        }
    }
    Ok(())
}
pub fn linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb(v: f64) -> f64 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
fn line(pixels: &mut [u8], size: [u32; 2], a: [f64; 2], b: [f64; 2], color: [f32; 4], width: f32) {
    let radius = f64::from(width) * 0.5;
    let xmin = (a[0].min(b[0]) - radius - 0.5)
        .floor()
        .clamp(0.0, f64::from(size[0])) as u32;
    let xmax = (a[0].max(b[0]) + radius + 0.5)
        .ceil()
        .clamp(0.0, f64::from(size[0])) as u32;
    let ymin = (a[1].min(b[1]) - radius - 0.5)
        .floor()
        .clamp(0.0, f64::from(size[1])) as u32;
    let ymax = (a[1].max(b[1]) + radius + 0.5)
        .ceil()
        .clamp(0.0, f64::from(size[1])) as u32;
    let d = [b[0] - a[0], b[1] - a[1]];
    let norm = d[0] * d[0] + d[1] * d[1];
    for y in ymin..ymax {
        // Restrict each scanline to the stroke's expanded segment interval.
        // A diagonal costs length × width, not its entire bounding rectangle.
        let (row_min, row_max) = if d[1].abs() > f64::EPSILON {
            let first = ((f64::from(y) + 0.5 - radius - 0.5 - a[1]) / d[1]).clamp(0.0, 1.0);
            let last = ((f64::from(y) + 0.5 + radius + 0.5 - a[1]) / d[1]).clamp(0.0, 1.0);
            let x1 = a[0] + first * d[0];
            let x2 = a[0] + last * d[0];
            (
                ((x1.min(x2) - radius - 0.5)
                    .floor()
                    .clamp(f64::from(xmin), f64::from(xmax))) as u32,
                ((x1.max(x2) + radius + 0.5)
                    .ceil()
                    .clamp(f64::from(xmin), f64::from(xmax))) as u32,
            )
        } else {
            (xmin, xmax)
        };
        for x in row_min..row_max {
            let p = [f64::from(x) + 0.5 - a[0], f64::from(y) + 0.5 - a[1]];
            let t = if norm > 0.0 {
                ((p[0] * d[0] + p[1] * d[1]) / norm).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let distance = ((p[0] - t * d[0]).powi(2) + (p[1] - t * d[1]).powi(2)).sqrt();
            let alpha = (radius + 0.5 - distance).clamp(0.0, 1.0) * f64::from(color[3]);
            if alpha == 0.0 {
                continue;
            }
            let index = ((y * size[0] + x) * 4) as usize;
            let pixel = &mut pixels[index..index + 4];
            let old_alpha = f64::from(pixel[3]) / 255.0;
            let out_alpha = alpha + old_alpha * (1.0 - alpha);
            for c in 0..3 {
                let value = (linear(f64::from(color[c])) * alpha
                    + linear(f64::from(pixel[c]) / 255.0) * old_alpha * (1.0 - alpha))
                    / out_alpha;
                pixel[c] = (srgb(value).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            pixel[3] = (out_alpha * 255.0).round() as u8;
        }
    }
}
