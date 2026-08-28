#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use catchlight_core::{load_model, GlobalTransforms, ModelFormat, Vec2};
use catchlight_wgpu::{
    apply_uniform_test_deform, collect_drawables, create_headless_context,
    create_orthographic_camera, RenderContext, WgpuRenderer,
};
use std::path::{Path, PathBuf};

const W: u32 = 320;
const H: u32 = 533;

fn load_puppet(path: &Path) -> catchlight_core::Puppet {
    let bytes = std::fs::read(path).unwrap();
    let format = ModelFormat::from_path(path).expect("recognized model extension");
    load_model(&bytes, format, 0).expect("load model")
}

fn find_reference() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../example_models/reference/reference.clp");
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

async fn render_with_test_deform(path: &Path, shift: Vec2) -> Vec<u8> {
    let mut puppet = load_puppet(path);

    if shift != Vec2::ZERO {
        apply_uniform_test_deform(&mut puppet, shift);
    }

    let (device, queue) = create_headless_context().await.expect("headless");
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm).await;
    let mut ctx = RenderContext::with_renderer(renderer, W, H).expect("render context");
    ctx.renderer.upload_puppet(&puppet).expect("upload");
    ctx.renderer
        .update_camera(create_orthographic_camera(5000.0, W as f32 / H as f32));
    ctx.renderer.sync_deforms(&puppet);

    let mut transforms = GlobalTransforms::new();
    puppet.compute_transforms(&mut transforms);
    let render_list = collect_drawables(&puppet, &transforms);
    ctx.render(
        &render_list,
        Some(wgpu::Color {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        }),
    )
    .expect("render");

    ctx.read_rgba().expect("readback")
}

#[test]
#[ignore = "needs the reference rig at example_models/reference/"]
fn deform_source_changes_pixels_end_to_end() {
    let path = find_reference().expect("reference.clp missing or an unfetched LFS pointer");

    let baseline = pollster::block_on(render_with_test_deform(&path, Vec2::ZERO));
    let shifted = pollster::block_on(render_with_test_deform(&path, Vec2::new(500.0, 0.0)));

    assert_eq!(baseline.len(), shifted.len());
    let diff: u64 = baseline
        .iter()
        .zip(shifted.iter())
        .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
        .sum();
    assert!(
        diff > 1_000_000,
        "expected a big CPU->GPU->shader shift, got summed |diff| = {}",
        diff
    );
}
