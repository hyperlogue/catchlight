//! Headless preview: pose an already-built puppet and render it to a PNG,
//!
//! A render cache's slots name GPU state inside the one warm renderer, so only
//! one session's cache is live; switching the previewed session re-prepares it.

use anyhow::{anyhow, Result};
use catchlight_core::{Model, Pose, Puppet};
use catchlight_editor_protocol::SessionId;
use catchlight_wgpu::{
    collect, create_headless_context, Framing, PrepareOptions, RenderCache, RenderContext,
    WgpuRenderer,
};

pub(super) struct PreviewRenderer {
    ctx: RenderContext,
    cache: Option<(SessionId, RenderCache)>,
}

impl PreviewRenderer {
    /// Expensive (a device and every pipeline); create once and reuse.
    pub(super) fn new() -> Result<Self> {
        let (device, queue) =
            pollster::block_on(create_headless_context()).map_err(|e| anyhow!("gpu init: {e}"))?;
        let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb);
        // Resized on first preview; this size only has to be legal.
        let ctx = RenderContext::with_renderer(renderer, 1, 1);
        Ok(Self { ctx, cache: None })
    }

    pub(super) fn max_texture_dimension(&self) -> u32 {
        self.ctx.renderer.max_texture_dimension()
    }

    /// Cleared to opaque white, so the premultiplied readback needs nothing
    /// undone before encoding.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_png(
        &mut self,
        session: SessionId,
        model: &Model,
        puppet: &mut Puppet,
        pose: &Pose,
        width: u32,
        height: u32,
        camera: Framing,
    ) -> Result<Vec<u8>> {
        let (pixels, w, h) =
            self.render_rgba(session, model, puppet, pose, width, height, camera)?;
        let mut png = Vec::new();
        image::write_buffer_with_format(
            &mut std::io::Cursor::new(&mut png),
            &pixels,
            w,
            h,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(|e| anyhow!("png encode: {e}"))?;
        Ok(png)
    }

    /// Tightly-packed RGBA8.
    #[allow(clippy::too_many_arguments)]
    fn render_rgba(
        &mut self,
        session: SessionId,
        model: &Model,
        puppet: &mut Puppet,
        pose: &Pose,
        width: u32,
        height: u32,
        camera: Framing,
    ) -> Result<(Vec<u8>, u32, u32)> {
        let width = width.max(1);
        let height = height.max(1);

        if !matches!(&self.cache, Some((held, _)) if *held == session) {
            let cache =
                RenderCache::prepare(&mut self.ctx.renderer, model, PrepareOptions::default())
                    .map_err(|e| anyhow!("prepare: {e}"))?;
            self.cache = Some((session, cache));
        }
        let Some((_, cache)) = self.cache.as_mut() else {
            return Err(anyhow!("render cache unavailable"));
        };

        puppet.apply_pose(pose);
        puppet.tick(model, 0.0);
        cache
            .refresh(&mut self.ctx.renderer, model, puppet)
            .map_err(|e| anyhow!("refresh: {e}"))?;
        let render_list = collect(cache, puppet);

        let pixels = self
            .ctx
            .render_rgba(
                &render_list,
                camera,
                width,
                height,
                Some(wgpu::Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                }),
            )
            .map_err(|e| anyhow!("render: {e}"))?;
        Ok((pixels, width, height))
    }
}
