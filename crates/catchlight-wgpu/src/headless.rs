use catchlight_core::{DeformSource, GlobalTransforms, LegacyPuppet, NodeKind, Puppet, Vec2};

use crate::{
    create_headless_context, read_texture_to_rgba, CompositePool, FramebufferSnapshotPool,
    RenderList, RenderStats, StencilTarget, WgpuRenderer,
};

pub struct RenderContext {
    pub renderer: WgpuRenderer,
    pub stencil: StencilTarget,
    pub composites: CompositePool,
    pub snapshots: FramebufferSnapshotPool,
    pub target: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub transforms: GlobalTransforms,
}

impl RenderContext {
    pub async fn new(width: u32, height: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let (device, queue) = create_headless_context()
            .await
            .map_err(|e| format!("create_headless_context: {e}"))?;
        let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await;
        Self::with_renderer(renderer, width, height)
    }

    pub fn with_renderer(
        renderer: WgpuRenderer,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let format = renderer.shared.surface_format;
        let target = renderer.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("headless-render-target"),
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
        let stencil =
            StencilTarget::new_for_pipelines(&renderer.shared, &renderer.device, width, height);
        let composites = CompositePool::new(width, height);
        let snapshots = FramebufferSnapshotPool::new(width, height);
        Ok(Self {
            renderer,
            stencil,
            composites,
            snapshots,
            target,
            view,
            width,
            height,
            transforms: GlobalTransforms::new(),
        })
    }

    pub fn render(
        &mut self,
        render_list: &RenderList,
        clear: Option<wgpu::Color>,
    ) -> Result<RenderStats, Box<dyn std::error::Error>> {
        self.renderer.begin_camera_submit();
        let mut encoder =
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("headless-render-encoder"),
                });
        let stats = self.renderer.render_list_ext(
            render_list,
            &mut encoder,
            &self.view,
            &self.stencil,
            &mut self.composites,
            Some(&self.target),
            Some(&mut self.snapshots),
            self.width,
            self.height,
            clear,
        )?;
        self.renderer
            .queue
            .submit(std::iter::once(encoder.finish()));
        Ok(stats)
    }

    pub fn read_rgba(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        pollster::block_on(read_texture_to_rgba(
            &self.renderer.device,
            &self.renderer.queue,
            &self.target,
            self.width,
            self.height,
        ))
        .map_err(|e| format!("read_texture_to_rgba: {e}").into())
    }
}

/// Shift every part's vertices by `shift` through the puppet's scratch
/// deform, then recombine — the end-to-end "a deform reaches the shader"
/// check, with no param machinery in the way.
pub fn apply_uniform_scratch_deform(puppet: &mut Puppet, shift: Vec2) {
    let parts: Vec<(catchlight_core::NodeIdx, usize)> = puppet
        .iter()
        .filter_map(|(id, node)| match &node.kind {
            NodeKind::Part(p) => Some((id, p.mesh.vertices.len())),
            _ => None,
        })
        .collect();
    for (id, count) in parts {
        if count == 0 {
            continue;
        }
        puppet.set_scratch_deform(id, &vec![shift; count]);
    }
    puppet.combine_deforms();
}

// legacy: removed by cl-32i.8
pub fn apply_uniform_test_deform(puppet: &mut LegacyPuppet, shift: Vec2) {
    let ids_and_lens: Vec<_> = puppet
        .iter()
        .filter_map(|(id, node)| match &node.kind {
            NodeKind::Part(p) => Some((id, p.mesh.vertices.len())),
            _ => None,
        })
        .collect();
    for (id, n) in ids_and_lens {
        if n == 0 {
            continue;
        }
        let _ = puppet.update_deform_source(id, DeformSource::Test, |buf| {
            debug_assert_eq!(buf.len(), n);
            buf.fill(shift);
        });
    }
    puppet.combine_deforms();
}
