//! The one headless render path: an offscreen target, the pools a frame
//! needs, a camera and a readback, held together and reused across frames.
//!
//! Everything that renders without a window goes through [`RenderContext`]:
//! `catchlight-cli`'s `render` and `isolate`, and the editor server's
//! preview. That is the point of it being one type. A second
//! hand-rolled copy of "make a target, size the stencil and the two pools,
//! set the camera, submit, read back" is exactly how two callers drift apart
//! on pool sizing, on the clear colour, or on what the bytes coming back
//! mean.
//!
//! [`RenderContext::render_rgba`] is that whole sequence in one call, and
//! [`RenderContext::resize`] is a no-op when the size already matches, so a
//! caller previewing the same size over and over rebuilds nothing.
//!
//! **The readback is premultiplied, linear under sRGB encoding.** The
//! renderer composites into a premultiplied-alpha `Rgba8UnormSrgb` target,
//! which blends in linear and encodes on store, and `render_rgba` hands back
//! what the target holds, untouched. A caller writing a straight-alpha PNG of
//! a frame with any transparency in it has to unpremultiply first, and in
//! linear: `catchlight_core::texture::unpremultiply_linear_from_srgb_inplace`,
//! not the byte-space inverse beside it. One clearing to an opaque colour has
//! nothing to undo, because alpha is 1 everywhere.

use catchlight_core::{NodeKind, Puppet, Vec2};

use crate::{
    create_headless_context, create_orthographic_camera_at, read_texture_to_rgba, CompositePool,
    FramebufferSnapshotPool, RenderList, RenderStats, StencilTarget, WgpuRenderer,
};

/// What an orthographic frame looks at: `height` world units tall, centred on
/// `center`, with the width following from the target's aspect.
///
/// The wire has a camera of the same shape, but this is not it: nothing in
/// this crate may depend on the editor protocol, so the two are converted at
/// the edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Framing {
    pub center: glam::Vec2,
    pub height: f32,
}

impl Framing {
    /// `height` world units tall, centred on the origin.
    pub fn centered(height: f32) -> Self {
        Self {
            center: glam::Vec2::ZERO,
            height,
        }
    }
}

pub struct RenderContext {
    pub renderer: WgpuRenderer,
    pub stencil: StencilTarget,
    pub composites: CompositePool,
    pub snapshots: FramebufferSnapshotPool,
    pub target: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
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
        let width = width.max(1);
        let height = height.max(1);
        let target = create_target(
            &renderer.device,
            renderer.shared.surface_format,
            width,
            height,
        );
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
        })
    }

    /// Re-make the target, its view, the stencil and the two pools for a new
    /// size. A no-op when the size already matches, so re-rendering at one
    /// size costs nothing. Zero in either axis is clamped to one, which is
    /// what wgpu will accept.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.width == width && self.height == height {
            return;
        }
        self.target = create_target(
            &self.renderer.device,
            self.renderer.shared.surface_format,
            width,
            height,
        );
        self.view = self
            .target
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.stencil = StencilTarget::new_for_pipelines(
            &self.renderer.shared,
            &self.renderer.device,
            width,
            height,
        );
        self.composites = CompositePool::new(width, height);
        self.snapshots = FramebufferSnapshotPool::new(width, height);
        self.width = width;
        self.height = height;
    }

    /// Resize to `width` x `height`, frame `render_list` with `camera`, draw
    /// it over `clear` and read the target back as tightly packed RGBA8.
    ///
    /// The bytes are premultiplied; see this module's doc.
    pub fn render_rgba(
        &mut self,
        render_list: &RenderList,
        camera: Framing,
        width: u32,
        height: u32,
        clear: Option<wgpu::Color>,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.resize(width, height);
        let aspect = self.width as f32 / self.height as f32;
        self.renderer.update_camera(create_orthographic_camera_at(
            camera.height,
            aspect,
            camera.center,
        ));
        self.render(render_list, clear)?;
        self.read_rgba()
    }

    pub fn render(
        &mut self,
        render_list: &RenderList,
        clear: Option<wgpu::Color>,
    ) -> Result<RenderStats, Box<dyn std::error::Error>> {
        self.render_many(std::slice::from_ref(&render_list), clear)
    }

    /// One frame, one submit, several puppets of the one model this context's
    /// renderer holds — each list drawing from the deform set it carries.
    pub fn render_many(
        &mut self,
        render_lists: &[&RenderList],
        clear: Option<wgpu::Color>,
    ) -> Result<RenderStats, Box<dyn std::error::Error>> {
        self.renderer.begin_camera_submit();
        let mut encoder =
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("headless-render-encoder"),
                });
        let stats = self.renderer.render_lists_ext(
            render_lists,
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

fn create_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
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
    })
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
