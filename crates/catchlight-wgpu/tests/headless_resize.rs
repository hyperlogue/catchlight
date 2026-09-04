#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! [`RenderContext::render_rgba`] across a size change.
//!
//! The context caches everything a frame draws into — target, stencil,
//! composite and snapshot pools — so rendering at a second size has to rebuild
//! all four together. Miss one and the frame either draws into a target of the
//! old size or reads back a buffer that does not match it, which is why this
//! asserts on the byte counts rather than on any pixel.

mod common;

use catchlight_core::{Mesh, Model, Puppet};
use catchlight_wgpu::{
    collect, create_headless_context, Framing, PrepareOptions, RenderCache, RenderContext,
    WgpuRenderer,
};
use common::NO_ADAPTER;

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
    let (device, queue) = pollster::block_on(create_headless_context()).expect(NO_ADAPTER);
    let renderer = pollster::block_on(WgpuRenderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
    ));
    let mut ctx = RenderContext::with_renderer(renderer, 64, 64).expect("render context");

    let model = one_quad_model();
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .expect("prepare");
    let mut puppet = Puppet::new(&model);
    puppet.tick(&model, 0.0);
    cache
        .refresh(&mut ctx.renderer, &model, &puppet)
        .expect("refresh");
    let list = collect(&cache, &puppet);

    // The size it was built at, then a larger one, then a smaller one: growing
    // and shrinking take the same path but not the same allocations.
    for (width, height) in [(64, 64), (128, 96), (32, 48)] {
        let pixels = ctx
            .render_rgba(&list, CAMERA, width, height, Some(CLEAR))
            .expect("render");
        assert_eq!(
            pixels.len(),
            (width as usize) * (height as usize) * 4,
            "readback is tightly packed RGBA8 at {width}x{height}"
        );
        assert_eq!((ctx.width, ctx.height), (width, height));
    }
}

/// A zero in either axis is clamped rather than handed to wgpu, which would
/// reject it.
#[test]
fn a_zero_size_is_clamped_to_one_pixel() {
    let (device, queue) = pollster::block_on(create_headless_context()).expect(NO_ADAPTER);
    let renderer = pollster::block_on(WgpuRenderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
    ));
    let mut ctx = RenderContext::with_renderer(renderer, 0, 0).expect("render context");
    assert_eq!((ctx.width, ctx.height), (1, 1));
    ctx.resize(8, 0);
    assert_eq!((ctx.width, ctx.height), (8, 1));
}
