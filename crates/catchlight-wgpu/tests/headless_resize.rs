#![expect(clippy::expect_used, reason = "a test aborts on a broken fixture")]

//! [`RenderContext::render_rgba`] across a size change must rebuild target,
//! stencil and both pools together; asserted on byte counts.

mod common;

use catchlight_core::{Mesh, Model, Puppet};
use catchlight_wgpu::{Framing, PrepareOptions, RenderCache, RenderContext, WgpuRenderer, collect};

const CAMERA: Framing = Framing {
    center: glam::Vec2::ZERO,
    height: 100.0,
};

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

fn one_quad_model() -> Model {
    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, [230, 120, 40, 255]));
    let root = build.root();
    build.part(
        &root,
        "quad",
        0.0,
        common::mesh_to_clm(&Mesh::quad(20.0, 20.0)),
        &tex,
        |_| {},
    );
    build.model
}

#[test]
fn rendering_at_a_second_size_returns_a_buffer_for_that_size() {
    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);
    let mut ctx = RenderContext::with_renderer(renderer, 64, 64);

    let model = one_quad_model();
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .expect("prepare");
    let mut puppet = Puppet::new(&model);
    puppet.tick(&model, 0.0);
    cache
        .refresh(&mut ctx.renderer, &model, &puppet)
        .expect("refresh");
    let list = collect(&cache, &puppet);

    for (width, height) in [(64, 64), (128, 96), (32, 48)] {
        let pixels = ctx
            .render_rgba(&list, CAMERA, width, height, Some(CLEAR))
            .expect("render");
        assert_eq!(
            pixels.len(),
            (width as usize) * (height as usize) * 4,
            "readback is tightly packed RGBA8 at {width}x{height}"
        );
        assert_eq!((ctx.width(), ctx.height()), (width, height));
    }
}

/// wgpu rejects zero sizes, so they are clamped.
#[test]
fn a_zero_size_is_clamped_to_one_pixel() {
    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);
    let mut ctx = RenderContext::with_renderer(renderer, 0, 0);
    assert_eq!((ctx.width(), ctx.height()), (1, 1));
    ctx.resize(8, 0);
    assert_eq!((ctx.width(), ctx.height()), (8, 1));
}

#[test]
fn reading_into_a_buffer_replaces_it_with_the_current_frame() {
    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);
    let mut ctx = RenderContext::with_renderer(renderer, 64, 64);
    let model = one_quad_model();
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .expect("prepare");
    let mut puppet = Puppet::new(&model);
    puppet.tick(&model, 0.0);
    cache
        .refresh(&mut ctx.renderer, &model, &puppet)
        .expect("refresh");
    let list = collect(&cache, &puppet);

    let mut pixels = vec![7; 3];
    for (width, height) in [(64, 64), (128, 96), (64, 64)] {
        let fresh = ctx
            .render_rgba(&list, CAMERA, width, height, Some(CLEAR))
            .expect("render");
        ctx.read_rgba_into(&mut pixels).expect("read back");
        assert_eq!(pixels, fresh, "at {width}x{height}");
    }
}
