#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Print a rig's render list and write a PNG of it.
//!
//! It runs the full `settle_physics` + `tick` pipeline on purpose: with a bare
//! `compute_transforms` the output stays byte-identical through a regression
//! in params, mesh groups or welds, so it is the hash-stability check.

use catchlight_core::{load_model, ModelFormat};
use catchlight_wgpu::{
    collect_drawables, create_orthographic_camera, DrawableInfo, RenderContext, RenderList,
};

fn print_render_list(render_list: &RenderList) {
    println!(
        "Render list: {} root drawables, {} composites with children",
        render_list.root_drawables.len(),
        render_list.composite_children.len()
    );

    println!("\n=== ROOT DRAWABLES (sorted by z-order) ===");
    for (idx, drawable) in render_list.root_drawables.iter().enumerate() {
        match drawable {
            DrawableInfo::Part {
                mesh_id,
                texture_id,
                transform,
                z_order,
                blend_mode,
                mask_sources,
                ..
            } => {
                let pos = transform.project_point3(glam::Vec3::ZERO);
                let mask_str = if mask_sources.is_empty() {
                    String::new()
                } else {
                    format!(" masks={}", mask_sources.len())
                };
                println!(
                    "[{}] Part entity={} z_order={:.2} texture={} blend={:?}{} pos=({:.1}, {:.1})",
                    idx, mesh_id, z_order, texture_id, blend_mode, mask_str, pos.x, pos.y
                );
            }
            DrawableInfo::Composite {
                node_id,
                z_order,
                blend_mode,
                opacity,
                ..
            } => {
                println!(
                    "[{}] Composite node_id={} z_order={:.2} blend={:?} opacity={:.2}",
                    idx, node_id, z_order, blend_mode, opacity
                );
            }
        }
    }

    for (composite_node_id, children) in &render_list.composite_children {
        println!("\n=== COMPOSITE {} CHILDREN ===", composite_node_id);
        for (idx, child) in children.iter().enumerate() {
            if let DrawableInfo::Part {
                mesh_id,
                texture_id,
                transform,
                z_order,
                blend_mode,
                mask_sources,
                ..
            } = child
            {
                let pos = transform.project_point3(glam::Vec3::ZERO);
                let mask_str = if mask_sources.is_empty() {
                    String::new()
                } else {
                    format!(" masks={}", mask_sources.len())
                };
                println!(
                    "[{}] Part entity={} z_order={:.2} texture={} blend={:?}{} pos=({:.1}, {:.1})",
                    idx, mesh_id, z_order, texture_id, blend_mode, mask_str, pos.x, pos.y
                );
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pollster::block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "example_models/reference/reference.clm".to_string());
    let path = std::path::PathBuf::from(path);

    let output_path = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "output.png".to_string());

    let width: u32 = std::env::args()
        .nth(3)
        .map(|s| s.parse().expect("width must be a positive integer"))
        .unwrap_or(960);
    let height: u32 = std::env::args()
        .nth(4)
        .map(|s| s.parse().expect("height must be a positive integer"))
        .unwrap_or(1600);
    let camera_height: f32 = std::env::args()
        .nth(5)
        .map(|s| s.parse().expect("camera height must be a number"))
        .unwrap_or(5000.0);

    println!("Loading model: {}", path.display());
    let bytes = std::fs::read(&path)?;
    let format = ModelFormat::from_path(&path)
        .ok_or_else(|| format!("unrecognized model extension: {}", path.display()))?;
    let mut puppet = load_model(&bytes, format, 0)?;
    println!("Loaded {} textures", puppet.textures().len());

    println!("Render size: {}x{}", width, height);
    let mut ctx = RenderContext::new(width, height).await?;

    println!("Uploading puppet to GPU...");
    let (tex_count, mesh_count) = ctx.renderer.upload_puppet(&puppet)?;
    println!("Uploaded {} textures, {} meshes", tex_count, mesh_count);

    // Set up camera
    let aspect = width as f32 / height as f32;
    ctx.renderer
        .update_camera(create_orthographic_camera(camera_height, aspect));

    // Stage 1: run the per-frame pipeline, then collect drawables.
    // `tick` (not a bare compute_transforms) is what folds params, mesh-group
    // warps and welds — without it this renders the raw authored pose, and the
    // output hash stays byte-identical through a regression in any of them.
    puppet.settle_physics();
    puppet.tick(&mut ctx.transforms, glam::Mat4::IDENTITY, 0.0);
    ctx.renderer.sync_deforms(&puppet);
    let render_list = collect_drawables(&puppet, &ctx.transforms);
    print_render_list(&render_list);

    // Stage 2: Render the RenderList to target.
    // White background for comparison.
    let clear_color = Some(wgpu::Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    });
    match ctx.render(&render_list, clear_color) {
        Ok(stats) => println!("Render stats: {:?}", stats),
        Err(e) => eprintln!("Render error: {}", e),
    }

    println!("Reading back pixels...");
    let pixels = ctx.read_rgba()?;

    // Save PNG
    println!("Saving to {}...", output_path);
    image::save_buffer(
        &output_path,
        &pixels,
        width,
        height,
        image::ColorType::Rgba8,
    )?;

    println!("Done!");
    Ok(())
}
