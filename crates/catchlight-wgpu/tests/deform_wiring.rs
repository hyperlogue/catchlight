#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use catchlight_core::{Model, Vec2};
use catchlight_wgpu::{
    apply_uniform_scratch_deform, collect, create_orthographic_camera, RenderContext, WgpuRenderer,
};
use common::Scene;
use std::path::{Path, PathBuf};

const W: u32 = 320;
const H: u32 = 533;

fn lan() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/models/lan.clm")
}

async fn render_with_test_deform(path: &Path, shift: Vec2) -> Vec<u8> {
    let model = Model::from_clm_bytes(&std::fs::read(path).unwrap()).expect("load model");

    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);
    let mut ctx = RenderContext::with_renderer(renderer, W, H).expect("render context");
    let mut scene = Scene::new(&mut ctx.renderer, model);
    ctx.renderer
        .update_camera(create_orthographic_camera(2200.0, W as f32 / H as f32));

    scene.puppet.compute_transforms();
    if shift != Vec2::ZERO {
        apply_uniform_scratch_deform(&mut scene.puppet, shift);
    }
    scene
        .cache
        .refresh(&mut ctx.renderer, &scene.model, &scene.puppet)
        .expect("refresh the render cache");

    let render_list = collect(&scene.cache, &scene.puppet);
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
fn deform_source_changes_pixels_end_to_end() {
    let path = lan();

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
