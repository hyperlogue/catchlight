#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! With `Some(clear_color)`, `render_list` must clear the target even
//! when nothing draws — an empty render list (or one whose draws all
//! filter out) previously left the previous frame's pixels in place.

use catchlight_wgpu::{
    create_headless_context, CompositePool, RenderList, StencilTarget, WgpuRenderer,
};

const W: u32 = 4;
const H: u32 = 4;

const NO_ADAPTER: &str =
    "no Vulkan adapter for the headless context; see AGENTS.md, \"Native headless rendering\"";

#[test]
fn empty_render_list_still_clears_the_target() {
    let pixels = pollster::block_on(async {
        let (device, queue) = create_headless_context().await.expect(NO_ADAPTER);
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("clear-fallback target"),
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut renderer = WgpuRenderer::new(device, queue, format).await;
        let stencil = StencilTarget::new_for_pipelines(&renderer.shared, &renderer.device, W, H);
        let mut composites = CompositePool::new(W, H);

        // Frame 1: green garbage standing in for "the previous frame".
        let mut paint = |renderer: &mut WgpuRenderer, list: &RenderList, color: wgpu::Color| {
            renderer.begin_camera_submit();
            let mut encoder = renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            renderer
                .render_list(
                    list,
                    &mut encoder,
                    &view,
                    &stencil,
                    &mut composites,
                    W,
                    H,
                    Some(color),
                )
                .expect("render");
            renderer.queue.submit(std::iter::once(encoder.finish()));
        };
        let empty = RenderList::default();
        paint(
            &mut renderer,
            &empty,
            wgpu::Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
        );
        // Frame 2: empty list with a red clear must fully replace it.
        paint(
            &mut renderer,
            &empty,
            wgpu::Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        );

        catchlight_wgpu::read_texture_to_rgba(&renderer.device, &renderer.queue, &texture, W, H)
            .await
            .expect("readback")
    });

    for px in pixels.chunks_exact(4) {
        assert_eq!(px, [255, 0, 0, 255], "expected red clear, got {px:?}");
    }
}
