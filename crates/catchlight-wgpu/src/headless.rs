//! The one headless render path: [`RenderContext`].
//!
//! The readback is premultiplied alpha, sRGB-encoded linear. Unpremultiply in
//! linear (`catchlight_core::texture::unpremultiply_linear_from_srgb_inplace`)
//! before writing straight-alpha output.

use crate::{
    create_headless_context, create_orthographic_camera_at, read_texture_to_rgba, FrameTargets,
    RenderList, RenderStats, RenderTarget, RendererResult, WgpuRenderer,
};

/// `height` is in world units; width follows the target's aspect. Distinct
/// from the wire camera because this crate must not depend on the protocol.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Framing {
    pub center: glam::Vec2,
    pub height: f32,
}

impl Framing {
    pub fn centered(height: f32) -> Self {
        Self {
            center: glam::Vec2::ZERO,
            height,
        }
    }
}

pub struct RenderContext {
    pub renderer: WgpuRenderer,
    targets: FrameTargets,
    target: wgpu::Texture,
    view: wgpu::TextureView,
}

impl RenderContext {
    pub async fn new(width: u32, height: u32) -> RendererResult<Self> {
        let (device, queue) = create_headless_context().await?;
        let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb);
        Ok(Self::with_renderer(renderer, width, height))
    }

    /// Zero is clamped to one.
    pub fn with_renderer(renderer: WgpuRenderer, width: u32, height: u32) -> Self {
        let target = create_target(&renderer, width, height);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            renderer,
            targets: FrameTargets::new(),
            target,
            view,
        }
    }

    pub fn width(&self) -> u32 {
        self.target.width()
    }

    pub fn height(&self) -> u32 {
        self.target.height()
    }

    /// The texture [`Self::render`] draws into.
    pub fn target(&self) -> &wgpu::Texture {
        &self.target
    }

    /// A no-op when the size already matches. Zero is clamped to one.
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width() == width.max(1) && self.height() == height.max(1) {
            return;
        }
        self.target = create_target(&self.renderer, width, height);
        self.view = self
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
    }

    /// Returns tightly packed premultiplied RGBA8.
    pub fn render_rgba(
        &mut self,
        render_list: &RenderList,
        camera: Framing,
        width: u32,
        height: u32,
        clear: Option<wgpu::Color>,
    ) -> RendererResult<Vec<u8>> {
        self.resize(width, height);
        let aspect = self.width() as f32 / self.height() as f32;
        self.renderer.update_camera(create_orthographic_camera_at(
            camera.height,
            aspect,
            camera.center,
        ));
        self.render(render_list, clear);
        self.read_rgba()
    }

    pub fn render(&mut self, render_list: &RenderList, clear: Option<wgpu::Color>) -> RenderStats {
        self.render_many(std::slice::from_ref(&render_list), clear)
    }

    pub fn render_many(
        &mut self,
        render_lists: &[&RenderList],
        clear: Option<wgpu::Color>,
    ) -> RenderStats {
        let target = RenderTarget {
            view: &self.view,
            texture: Some(&self.target),
            width: self.target.width(),
            height: self.target.height(),
        };
        let done = self
            .renderer
            .frame()
            .render(render_lists, target, &mut self.targets, clear);
        let stats = done.stats();
        done.submit();
        stats
    }

    pub fn read_rgba(&self) -> RendererResult<Vec<u8>> {
        pollster::block_on(read_texture_to_rgba(
            &self.renderer.device,
            &self.renderer.queue,
            &self.target,
        ))
    }
}

fn create_target(renderer: &WgpuRenderer, width: u32, height: u32) -> wgpu::Texture {
    renderer.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless-render-target"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: renderer.pipelines().surface_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}
