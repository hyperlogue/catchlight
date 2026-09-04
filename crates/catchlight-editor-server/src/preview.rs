//! Headless preview: pose an already-built puppet and render it to a PNG,
//! reusing catchlight's production `render_list_ext` path. Nothing is built
//! here except the render cache the GPU needs.
//!
//! The wgpu plumbing — target, stencil, the two pools, camera, submit,
//! readback — is [`catchlight_wgpu::RenderContext`], the one headless render
//! path, which `catchlight-cli` renders through as well.
//!
//! **One renderer, one session's cache.** The server keeps a single warm
//! renderer for every session, and a render cache's slots name GPU state
//! inside the renderer that prepared it — so two sessions cannot hold live
//! caches at once. The cache is therefore kept alongside the session it was
//! prepared for and re-prepared when the previewed session changes, which is
//! what the old per-render whole-puppet upload did unconditionally.
//! Previewing one session repeatedly keeps its decode memo.

use std::path::Path;

use anyhow::{anyhow, Result};
use catchlight_core::{Model, Pose, Puppet};
use catchlight_editor_protocol::SessionId;
use catchlight_wgpu::{
    collect, create_headless_context, Framing, PrepareOptions, RenderCache, RenderContext,
    WgpuRenderer,
};

pub(super) struct PreviewRenderer {
    ctx: RenderContext,
    /// The session whose model this cache was prepared from, and the cache.
    cache: Option<(SessionId, RenderCache)>,
}

impl PreviewRenderer {
    /// Create the headless GPU context once. Expensive (device + ~18 pipelines);
    /// the server keeps one alive and reuses it across every session's previews.
    pub(super) fn new() -> Result<Self> {
        let (device, queue) =
            pollster::block_on(create_headless_context()).map_err(|e| anyhow!("gpu init: {e}"))?;
        let renderer = pollster::block_on(WgpuRenderer::new(
            device,
            queue,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ));
        // The first preview resizes this to whatever it asks for; the size
        // here only has to be legal.
        let ctx = RenderContext::with_renderer(renderer, 1, 1)
            .map_err(|e| anyhow!("render context: {e}"))?;
        Ok(Self { ctx, cache: None })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_png(
        &mut self,
        session: SessionId,
        model: &Model,
        puppet: &mut Puppet,
        pose: &Pose,
        width: u32,
        height: u32,
        camera_height: f32,
        out: &Path,
    ) -> Result<()> {
        let (pixels, w, h) =
            self.render_rgba(session, model, puppet, pose, width, height, camera_height)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        image::save_buffer(out, &pixels, w, h, image::ColorType::Rgba8)
            .map_err(|e| anyhow!("png encode: {e}"))?;
        Ok(())
    }

    /// Render to tightly-packed RGBA8 bytes (the in-process viewport path).
    #[allow(clippy::too_many_arguments)]
    fn render_rgba(
        &mut self,
        session: SessionId,
        model: &Model,
        puppet: &mut Puppet,
        pose: &Pose,
        width: u32,
        height: u32,
        camera_height: f32,
    ) -> Result<(Vec<u8>, u32, u32)> {
        let width = width.max(1);
        let height = height.max(1);

        if !matches!(&self.cache, Some((held, _)) if *held == session) {
            // The editor edits, so the decode memo earns its copy: a rebuild
            // after a keystroke re-uploads without re-decoding.
            let cache = RenderCache::prepare(
                &mut self.ctx.renderer,
                model,
                PrepareOptions {
                    texture_halvings: 0,
                    memoize_textures: true,
                },
            )
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
                Framing::centered(camera_height),
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
