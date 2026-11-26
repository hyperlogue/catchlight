#![expect(clippy::expect_used, reason = "a test aborts on a broken fixture")]

//! A viewport draws the same frame as a target of its own size, offset into
//! its rectangle, and nothing outside it.

use crate::common;

use catchlight_core::Model;
use catchlight_wgpu::{
    FrameTargets, PipelineOptions, Pipelines, RenderTarget, StencilMode, WgpuRenderer,
    create_orthographic_camera, read_texture_to_rgba,
};
use std::path::PathBuf;
use std::sync::Arc;

const W: u32 = 96;
const H: u32 = 96;
/// The viewport's corner in the larger target.
const AT: (u32, u32) = (40, 24);
const TARGET: (u32, u32) = (160, 136);
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The visual-test baselines' framing for these models, at this size.
const CAMERA_HEIGHT: f32 = 512.0;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.1,
    g: 0.2,
    b: 0.3,
    a: 1.0,
};

fn load(stem: &str) -> Model {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/models")
        .join(format!("{stem}.clm"));
    Model::from_clm_bytes(&std::fs::read(path).expect("read the model")).expect("load the model")
}

fn draw(
    renderer: &mut WgpuRenderer,
    list: &catchlight_wgpu::RenderList,
    size: (u32, u32),
    viewport: Option<(u32, u32)>,
) -> Vec<u8> {
    let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport-test target"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut target = RenderTarget::copyable(&view);
    if let Some((x, y)) = viewport {
        target = target.with_viewport(x, y, W, H);
    }
    let mut targets = FrameTargets::new();
    renderer
        .frame()
        .render(&[list], target, &mut targets, Some(CLEAR))
        .submit();
    pollster::block_on(read_texture_to_rgba(
        &renderer.device,
        &renderer.queue,
        &texture,
    ))
    .expect("readback")
}

const fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

#[test]
fn a_viewport_draws_its_frame_inside_its_rectangle_only() {
    let (device, queue) = common::shared_context();
    for stencil in [StencilMode::Auto, StencilMode::Off] {
        let pipelines = Arc::new(Pipelines::new(&device, FORMAT, PipelineOptions { stencil }));
        // Masks, composites and the dst-in-shader blends each draw through
        // passes of their own.
        for stem in [
            "composite_masks",
            "blend_modes_composite",
            "disk_masked_by_disk",
        ] {
            let mut renderer =
                WgpuRenderer::from_pipelines(device.clone(), queue.clone(), pipelines.clone());
            renderer.update_camera(create_orthographic_camera(CAMERA_HEIGHT, 1.0));
            let mut scene = common::Scene::new(&mut renderer, load(stem));
            let list = scene.frame(&mut renderer, 0.0);

            let alone = draw(&mut renderer, &list, (W, H), None);
            let inset = draw(&mut renderer, &list, TARGET, Some(AT));

            let clear = pixel(&inset, TARGET.0, 0, 0);
            assert!(
                (0..H).any(|y| (0..W).any(|x| pixel(&alone, W, x, y) != clear)),
                "{stem}: the model draws something",
            );
            for y in 0..TARGET.1 {
                for x in 0..TARGET.0 {
                    let got = pixel(&inset, TARGET.0, x, y);
                    let inside = (AT.0..AT.0 + W).contains(&x) && (AT.1..AT.1 + H).contains(&y);
                    let want = if inside {
                        pixel(&alone, W, x - AT.0, y - AT.1)
                    } else {
                        clear
                    };
                    assert_eq!(got, want, "{stem} ({stencil:?}) at ({x}, {y})");
                }
            }
        }
    }
}
