//! Headless preview: build nothing, just render an already-built `LegacyPuppet` at a
//! pose to a PNG, reusing catchlight's production `render_list_ext` path.

use std::path::Path;

use anyhow::{anyhow, Result};
use catchlight_core::{GlobalTransforms, LegacyPuppet, Vec2};
use catchlight_wgpu::{
    collect_drawables, create_headless_context, create_orthographic_camera, read_texture_to_rgba,
    CompositePool, FramebufferSnapshotPool, StencilTarget, WgpuRenderer,
};

pub(super) struct PreviewRenderer {
    renderer: WgpuRenderer,
    transforms: GlobalTransforms,
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
            transforms: GlobalTransforms::new(),
        })
    }

    pub(super) fn render_png(
        &mut self,
        puppet: &mut LegacyPuppet,
        pose: &[(String, Vec2)],
        width: u32,
        height: u32,
        camera_height: f32,
        out: &Path,
    ) -> Result<()> {
        let (pixels, w, h) = self.render_rgba(puppet, pose, width, height, camera_height)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        image::save_buffer(out, &pixels, w, h, image::ColorType::Rgba8)
            .map_err(|e| anyhow!("png encode: {e}"))?;
        Ok(())
    }

    /// Render to tightly-packed RGBA8 bytes (the in-process viewport path).
    fn render_rgba(
        &mut self,
        puppet: &mut LegacyPuppet,
        pose: &[(String, Vec2)],
        width: u32,
        height: u32,
        camera_height: f32,
    ) -> Result<(Vec<u8>, u32, u32)> {
        let width = width.max(1);
        let height = height.max(1);

        self.renderer
            .upload_puppet(puppet)
            .map_err(|e| anyhow!("upload: {e}"))?;
        puppet.apply_pose_overlay(pose);
        puppet.tick(&mut self.transforms, glam::Mat4::IDENTITY, 0.0);
        self.renderer.sync_deforms(puppet);
        let aspect = width as f32 / height as f32;
        self.renderer.begin_camera_submit();
        self.renderer
            .update_camera(create_orthographic_camera(camera_height, aspect));
        let render_list = collect_drawables(puppet, &self.transforms);

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
