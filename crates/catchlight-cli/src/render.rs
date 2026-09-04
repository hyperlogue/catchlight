//! `render`: the whole model at rest, as a PNG, plus the render list it drew.
//!
//! This is the one command here that needs a GPU. It was the `render-to-png`
//! example in `catchlight-wgpu` and moved in when the seat for inspection
//! opened; that example is gone.
//!
//! **The full pipeline runs on purpose.** `settle_physics` then `tick`, not a
//! bare `compute_transforms`: `tick` is what folds params, mesh-group warps
//! and welds, and without it the output stays byte-identical through a
//! regression in any of them. That is what makes this the hash-stability
//! check as well as a picture.
//!
//! The render list printed alongside the PNG is the inspection half, and is
//! why the example existed at all: it names every drawable in z order with
//! its texture, blend mode, mask count and where it landed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use catchlight_core::Puppet;
use catchlight_wgpu::{
    collect, DrawableInfo, Framing, PrepareOptions, RenderCache, RenderContext, RenderList,
};

use crate::Error;

/// Defaults inherited from the example this replaced.
pub const DEFAULT_WIDTH: u32 = 960;
pub const DEFAULT_HEIGHT: u32 = 1600;
pub const DEFAULT_CAMERA_HEIGHT: f32 = 5000.0;

/// What a render produced: the picture on disk, and the list that drew it.
pub struct Rendered {
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    pub textures: usize,
    pub meshes: usize,
    /// The render list, formatted for reading.
    pub listing: String,
}

impl std::fmt::Display for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}\nwrote {} ({}x{}) from {} textures, {} meshes",
            self.listing,
            self.out.display(),
            self.width,
            self.height,
            self.textures,
            self.meshes
        )
    }
}

/// Render `path` at rest into `out`, and hand back the render list it drew.
pub fn run(
    path: &Path,
    out: &Path,
    width: u32,
    height: u32,
    camera_height: f32,
) -> Result<Rendered, Error> {
    let width = width.max(1);
    let height = height.max(1);
    let model = crate::file::load_model(path)?;

    let mut ctx = pollster::block_on(RenderContext::new(width, height))
        .map_err(|e| Error::gpu("gpu init", e))?;
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .map_err(|e| Error::gpu("prepare", e))?;

    // `tick` (not a bare `compute_transforms`) is what folds params,
    // mesh-group warps and welds; see this module's doc.
    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    puppet.tick(&model, 0.0);
    cache
        .refresh(&mut ctx.renderer, &model, &puppet)
        .map_err(|e| Error::gpu("refresh", e))?;
    let render_list = collect(&cache, &puppet);
    let listing = listing(&render_list);

    let pixels = ctx
        .render_rgba(
            &render_list,
            Framing::centered(camera_height),
            width,
            height,
            Some(wgpu::Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            }),
        )
        .map_err(|e| Error::gpu("render", e))?;

    // The clear is opaque, so nothing in the frame is partly transparent and
    // the premultiplied readback is already straight alpha.
    image::save_buffer(out, &pixels, width, height, image::ColorType::Rgba8).map_err(|source| {
        Error::Png {
            path: out.to_path_buf(),
            source,
        }
    })?;

    Ok(Rendered {
        out: out.to_path_buf(),
        width,
        height,
        textures: cache.texture_count(),
        meshes: cache.mesh_count(),
        listing,
    })
}

/// Every drawable in z order, roots first and then each composite's children.
fn listing(render_list: &RenderList) -> String {
    let mut out = String::new();
    // Writing into a String cannot fail, so the results are discarded.
    let _ = writeln!(
        out,
        "Render list: {} root drawables, {} composites with children",
        render_list.root_drawables.len(),
        render_list.composite_children.len()
    );
    let _ = writeln!(out, "\n=== ROOT DRAWABLES (sorted by z-order) ===");
    for (idx, drawable) in render_list.root_drawables.iter().enumerate() {
        match drawable {
            DrawableInfo::Part { .. } => part_line(&mut out, idx, drawable),
            DrawableInfo::Composite {
                node_id,
                z_order,
                blend_mode,
                opacity,
                ..
            } => {
                let _ = writeln!(
                    out,
                    "[{idx}] Composite node_id={node_id} z_order={z_order:.2} \
                     blend={blend_mode:?} opacity={opacity:.2}"
                );
            }
        }
    }
    for (composite_node_id, children) in &render_list.composite_children {
        let _ = writeln!(out, "\n=== COMPOSITE {composite_node_id} CHILDREN ===");
        for (idx, child) in children.iter().enumerate() {
            part_line(&mut out, idx, child);
        }
    }
    // The trailing newline is the caller's to add.
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

fn part_line(out: &mut String, idx: usize, drawable: &DrawableInfo) {
    let DrawableInfo::Part {
        mesh_id,
        texture_id,
        transform,
        z_order,
        blend_mode,
        mask_sources,
        ..
    } = drawable
    else {
        return;
    };
    let pos = transform.project_point3(glam::Vec3::ZERO);
    let masks = if mask_sources.is_empty() {
        String::new()
    } else {
        format!(" masks={}", mask_sources.len())
    };
    let _ = writeln!(
        out,
        "[{idx}] Part entity={mesh_id} z_order={z_order:.2} texture={texture_id} \
         blend={blend_mode:?}{masks} pos=({:.1}, {:.1})",
        pos.x, pos.y
    );
}
