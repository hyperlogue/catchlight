#![expect(clippy::expect_used, reason = "a test aborts on a broken fixture")]

//! With `Some(clear_color)`, `render_list` clears even when nothing draws.

use catchlight_wgpu::{
    FrameTargets, RenderContext, RenderList, RenderTarget, RendererError, WgpuRenderer,
    create_headless_context,
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
        let mut renderer = WgpuRenderer::new(device, queue, format);
        let mut targets = FrameTargets::new();

        let mut paint = |renderer: &mut WgpuRenderer, list: &RenderList, color: wgpu::Color| {
            renderer
                .frame()
                .render(
                    &[list],
                    RenderTarget {
                        view: &view,
                        texture: None,
                        width: W,
                        height: H,
                    },
                    &mut targets,
                    Some(color),
                )
                .submit();
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

        catchlight_wgpu::read_texture_to_rgba(&renderer.device, &renderer.queue, &texture)
            .await
            .expect("readback")
    });

    for px in pixels.as_chunks::<4>().0 {
        assert_eq!(px, &[255, 0, 0, 255], "expected red clear, got {px:?}");
    }
}

fn cleared_context(format: wgpu::TextureFormat) -> RenderContext {
    let (device, queue) = pollster::block_on(create_headless_context()).expect(NO_ADAPTER);
    let mut ctx = RenderContext::with_renderer(WgpuRenderer::new(device, queue, format), W, H);
    ctx.render(
        &RenderList::default(),
        Some(wgpu::Color {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }),
    );
    ctx
}

#[test]
fn a_bgra_target_reads_back_as_rgba() {
    let pixels = cleared_context(wgpu::TextureFormat::Bgra8UnormSrgb)
        .read_rgba()
        .expect("readback");
    assert_eq!(pixels.len(), (W * H * 4) as usize);
    for px in pixels.as_chunks::<4>().0 {
        assert_eq!(px, &[255, 0, 0, 255], "expected red clear, got {px:?}");
    }
}

#[test]
fn a_float_target_is_refused_by_readback() {
    let refused = cleared_context(wgpu::TextureFormat::Rgba16Float).read_rgba();
    assert!(
        matches!(
            refused,
            Err(RendererError::ReadbackFormat(
                wgpu::TextureFormat::Rgba16Float
            ))
        ),
        "{refused:?}",
    );
}
