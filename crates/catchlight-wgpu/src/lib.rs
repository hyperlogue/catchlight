//! The wgpu rendering backend: it turns a posed [`catchlight_core::Puppet`]
//! into pixels.
//!
//! A renderer holds exactly one [`RenderCache`]; that cache serves every
//! puppet of its model.
//!
//! ```no_run
//! use catchlight_core::{load_model, ModelFormat, Pose, Puppet};
//! use catchlight_wgpu::{collect, Framing, PrepareOptions, RenderCache, RenderContext};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bytes = std::fs::read("model.clm")?;
//! let model = load_model(&bytes, ModelFormat::Clm)?;
//! let mut ctx = pollster::block_on(RenderContext::new(960, 1600))?;
//! let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())?;
//! let mut puppet = Puppet::new(&model);
//!
//! puppet.apply_pose(&Pose::new());
//! puppet.tick(&model, 1.0 / 60.0);
//! cache.refresh(&mut ctx.renderer, &model, &puppet)?;
//! let list = collect(&cache, &puppet);
//! let pixels = ctx.render_rgba(&list, Framing::centered(5000.0), 960, 1600, None)?;
//! # let _ = pixels;
//! # Ok(())
//! # }
//! ```

mod collect;
mod headless;
mod render_cache;
mod renderer;

pub use collect::{
    CompositeMaskPartData, CompositeMaskSourceData, DrawableInfo, MaskSourceData, MaskSources,
    RenderList,
};
pub use headless::{Framing, RenderContext};
pub use render_cache::{collect, CacheStats, PrepareOptions, RenderCache};
pub use renderer::{
    DeformSet, Frame, FrameStats, FrameTargets, Pipelines, Recorded, RenderStats, RenderTarget,
    RendererError, RendererResult, Submission, WgpuRenderer, CAMERA_RING_SLOTS,
};

pub fn create_orthographic_camera(camera_height: f32, aspect: f32) -> glam::Mat4 {
    create_orthographic_camera_at(camera_height, aspect, glam::Vec2::ZERO)
}

/// `camera_height` is in world units. World space is Y-up and the camera
/// holds no axis flip.
pub fn create_orthographic_camera_at(
    camera_height: f32,
    aspect: f32,
    center: glam::Vec2,
) -> glam::Mat4 {
    let camera_width = camera_height * aspect;
    glam::Mat4::orthographic_rh(
        center.x - camera_width / 2.0,
        center.x + camera_width / 2.0,
        center.y - camera_height / 2.0,
        center.y + camera_height / 2.0,
        -1000.0,
        1000.0,
    )
}

pub struct SurfaceContext {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub surface_format: wgpu::TextureFormat,
    pub render_format: wgpu::TextureFormat,
}

impl SurfaceContext {
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(device, &self.config);
    }

    pub fn reconfigure(&self, device: &wgpu::Device) {
        self.surface.configure(device, &self.config);
    }

    pub fn acquire(&self) -> Option<(wgpu::SurfaceTexture, wgpu::TextureView)> {
        // `None` tells the caller to re-acquire.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => return None,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.render_format),
            ..Default::default()
        });
        Some((frame, view))
    }
}

/// `optional` features are requested only where the adapter has them.
async fn request_device(
    adapter: &wgpu::Adapter,
    optional: wgpu::Features,
) -> RendererResult<(wgpu::Device, wgpu::Queue)> {
    Ok(adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: adapter.features() & optional,
            // Mobile Safari reports no compute limits, so `Limits::default()` fails.
            required_limits: adapter.limits(),
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        })
        .await?)
}

async fn headless_adapter(backends: wgpu::Backends) -> RendererResult<wgpu::Adapter> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    // `HighPerformance` makes a hardware adapter win over lavapipe when both
    // are present.
    Ok(instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await?)
}

/// Renders through an sRGB view: shaders work in linear space, so a
/// non-sRGB target would skip the gamma encode on present.
pub async fn create_surface_context(
    instance: &wgpu::Instance,
    surface: wgpu::Surface<'static>,
    width: u32,
    height: u32,
) -> RendererResult<(wgpu::Device, wgpu::Queue, SurfaceContext)> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await?;
    let (device, queue) =
        request_device(&adapter, wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER).await?;
    let context = configure_surface(&adapter, &device, surface, width, height);
    Ok((device, queue, context))
}

/// For a second canvas on an existing device; a new device per canvas could
/// not share textures.
pub fn configure_surface(
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
    surface: wgpu::Surface<'static>,
    width: u32,
    height: u32,
) -> SurfaceContext {
    let caps = surface.get_capabilities(adapter);
    // WebGPU canvases advertise only non-sRGB formats, so render through an
    // sRGB view of the native format.
    let (surface_format, render_format, view_formats) =
        if let Some(&fmt) = caps.formats.iter().find(|f| f.is_srgb()) {
            (fmt, fmt, Vec::<wgpu::TextureFormat>::new())
        } else {
            let base = *caps
                .formats
                .iter()
                .find(|f| f.add_srgb_suffix() != **f)
                .unwrap_or(&caps.formats[0]);
            let srgb = base.add_srgb_suffix();
            (base, srgb, vec![srgb])
        };

    // The frame clear writes alpha 1.0, so Opaque lets the compositor skip
    // blending against the page.
    let alpha_mode = caps
        .alpha_modes
        .iter()
        .copied()
        .find(|m| *m == wgpu::CompositeAlphaMode::Opaque)
        .unwrap_or(caps.alpha_modes[0]);

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: width.max(1),
        height: height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode,
        view_formats,
        desired_maximum_frame_latency: 2,
    };
    surface.configure(device, &config);

    SurfaceContext {
        surface,
        config,
        surface_format,
        render_format,
    }
}

/// `Backends::PRIMARY`, not `all()`: the workspace-unified `webgl` feature
/// enables GL, which panics initializing EGL headless.
pub async fn create_headless_context() -> RendererResult<(wgpu::Device, wgpu::Queue)> {
    let adapter = headless_adapter(wgpu::Backends::PRIMARY).await?;
    request_device(&adapter, wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER).await
}

/// Benchmarking variant. The profiler uses encoder-scope queries, which
/// `wgpu_profiler` opens only under `TIMESTAMP_QUERY_INSIDE_ENCODERS`.
pub async fn create_headless_context_ext(
    backends: wgpu::Backends,
) -> RendererResult<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let adapter = headless_adapter(backends).await?;
    let (device, queue) = request_device(
        &adapter,
        wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER
            | wgpu::Features::TIMESTAMP_QUERY
            | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
    )
    .await?;
    Ok((adapter, device, queue))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadbackLayout {
    unpadded_bytes_per_row: usize,
    padded_bytes_per_row: u32,
    buffer_size: u64,
}

/// `None` for an empty texture or one whose rows overflow the copy layout or
/// the address space.
fn readback_layout(width: u32, height: u32) -> Option<ReadbackLayout> {
    if width == 0 || height == 0 {
        return None;
    }
    let unpadded = width.checked_mul(4)?;
    let padded = unpadded.checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)?;
    let buffer_size = u64::from(padded) * u64::from(height);
    usize::try_from(buffer_size).ok()?;
    Some(ReadbackLayout {
        unpadded_bytes_per_row: usize::try_from(unpadded).ok()?,
        padded_bytes_per_row: padded,
        buffer_size,
    })
}

/// Mip 0 of an RGBA8 `texture` carrying `COPY_SRC`, tightly packed.
pub async fn read_texture_to_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> RendererResult<Vec<u8>> {
    let (width, height) = (texture.width(), texture.height());
    let layout =
        readback_layout(width, height).ok_or(RendererError::ReadbackSize { width, height })?;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size: layout.buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Readback Encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(layout.padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(std::iter::once(encoder.finish()));

    let buffer_slice = buffer.slice(..);
    let (sender, receiver) = futures_channel::oneshot::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: None,
    })?;
    #[cfg(target_arch = "wasm32")]
    let _ = submission;
    receiver
        .await
        .map_err(|_| RendererError::ReadbackAbandoned)??;

    let data = buffer_slice.get_mapped_range();
    let pixels = data
        .chunks(layout.padded_bytes_per_row as usize)
        .flat_map(|row| &row[..layout.unpadded_bytes_per_row])
        .copied()
        .collect();
    drop(data);
    buffer.unmap();
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_layout_checks_dimensions_and_alignment() {
        let layout = readback_layout(3, 2).unwrap();
        assert_eq!(layout.unpadded_bytes_per_row, 12);
        assert_eq!(layout.padded_bytes_per_row, 256);
        assert_eq!(layout.buffer_size, 512);

        assert!(readback_layout(0, 1).is_none());
        assert!(readback_layout(1, 0).is_none());
        assert!(readback_layout(u32::MAX, 1).is_none());
        assert!(readback_layout(u32::MAX / 4, 1).is_none());
    }
}
