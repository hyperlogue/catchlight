//! Headless preview: pose an already-built puppet and render it to a PNG,
//! reusing catchlight's production `render_list_ext` path. Nothing is built
//! here except the render cache the GPU needs.
//!
//! **One renderer, one session's cache.** The server keeps a single warm
//! renderer for every session, and a render cache's slots name GPU state
//! inside the renderer that prepared it — so two sessions cannot hold live
//! caches at once. The cache is therefore kept alongside the session it was
//! prepared for and re-prepared when the previewed session changes, which is
//! what the old per-render `upload_puppet` did unconditionally. Previewing
//! one session repeatedly keeps its decode memo.

use std::path::Path;

use anyhow::{anyhow, Result};
use catchlight_core::{Model, Pose, Puppet};
use catchlight_editor_protocol::SessionId;
use catchlight_wgpu::{
    collect, create_headless_context, create_orthographic_camera, read_texture_to_rgba,
    CompositePool, FramebufferSnapshotPool, PrepareOptions, RenderCache, StencilTarget,
    WgpuRenderer,
};

pub(super) struct PreviewRenderer {
    renderer: WgpuRenderer,
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
        Ok(Self {
            renderer,
            cache: None,
        })
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
                &mut self.renderer,
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
            .refresh(&mut self.renderer, model, puppet)
            .map_err(|e| anyhow!("refresh: {e}"))?;
        let render_list = collect(cache, puppet);
        let aspect = width as f32 / height as f32;
        self.renderer.begin_camera_submit();
        self.renderer
            .update_camera(create_orthographic_camera(camera_height, aspect));

        let format = self.renderer.shared.surface_format;
        let target = self
            .renderer
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("preview-target"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let stencil = StencilTarget::new_for_pipelines(
            &self.renderer.shared,
            &self.renderer.device,
            width,
            height,
        );
        let mut composites = CompositePool::new(width, height);
        let mut snapshots = FramebufferSnapshotPool::new(width, height);

        let mut encoder =
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("preview-encoder"),
                });
        self.renderer
            .render_list_ext(
                &render_list,
                &mut encoder,
                &view,
                &stencil,
                &mut composites,
                Some(&target),
                Some(&mut snapshots),
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
        self.renderer
            .queue
            .submit(std::iter::once(encoder.finish()));

        let pixels = pollster::block_on(read_texture_to_rgba(
            &self.renderer.device,
            &self.renderer.queue,
            &target,
            width,
            height,
        ))
        .map_err(|e| anyhow!("readback: {e}"))?;
        Ok((pixels, width, height))
    }
}
