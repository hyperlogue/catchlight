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

pub mod collect;
pub mod headless;
pub mod render_cache;
pub mod renderer;

pub use collect::*;
pub use headless::{apply_uniform_scratch_deform, Framing, RenderContext};
pub use render_cache::{collect, CacheStats, PrepareOptions, RenderCache};
pub use renderer::*;

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

/// Renders through an sRGB view: shaders work in linear space, so a
/// non-sRGB target would skip the gamma encode on present.
pub async fn create_surface_context(
    instance: &wgpu::Instance,
    surface: wgpu::Surface<'static>,
    width: u32,
    height: u32,
) -> Result<(wgpu::Device, wgpu::Queue, SurfaceContext), Box<dyn std::error::Error>> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await?;

    let optional_features = wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER;
    let required_features = adapter.features() & optional_features;
    // Mobile Safari reports no compute limits, so `Limits::default()` fails.
    let required_limits = adapter.limits();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features,
            required_limits,
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        })
        .await?;

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
/// enables GL, which panics initializing EGL headless. `HighPerformance` makes
/// a hardware adapter win over lavapipe when both are present.
pub async fn create_headless_context(
) -> Result<(wgpu::Device, wgpu::Queue), Box<dyn std::error::Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await?;

    let optional_features = wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER;
    let required_features = adapter.features() & optional_features;
    let required_limits = adapter.limits();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features,
            required_limits,
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        })
        .await?;

    Ok((device, queue))
}

/// Benchmarking variant. The profiler uses encoder-scope queries, which
/// `wgpu_profiler` opens only under `TIMESTAMP_QUERY_INSIDE_ENCODERS`.
pub async fn create_headless_context_ext(
    backends: wgpu::Backends,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue), Box<dyn std::error::Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        display: None,
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await?;

    let optional_features = wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER
        | wgpu::Features::TIMESTAMP_QUERY
        | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let required_features = adapter.features() & optional_features;
    let required_limits = adapter.limits();
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features,
            required_limits,
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        })
        .await?;

    Ok((adapter, device, queue))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadbackLayout {
    unpadded_bytes_per_row: u32,
    padded_bytes_per_row: u32,
    buffer_size: u64,
    pixel_capacity: usize,
}

fn readback_layout(width: u32, height: u32) -> std::io::Result<ReadbackLayout> {
    if width == 0 || height == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "readback dimensions must be non-zero",
        ));
    }

    let unpadded_bytes_per_row = width.checked_mul(4).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "readback row size exceeds u32",
        )
    })?;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row
        .div_ceil(align)
        .checked_mul(align)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "aligned readback row size exceeds u32",
            )
        })?;
    let buffer_size = u64::from(padded_bytes_per_row)
        .checked_mul(u64::from(height))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "readback buffer size exceeds u64",
            )
        })?;
    usize::try_from(buffer_size).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "readback buffer does not fit address space",
        )
    })?;
    let pixel_bytes = u64::from(unpadded_bytes_per_row)
        .checked_mul(u64::from(height))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "readback pixel size exceeds u64",
            )
        })?;
    let pixel_capacity = usize::try_from(pixel_bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "readback pixels do not fit address space",
        )
    })?;

    Ok(ReadbackLayout {
        unpadded_bytes_per_row,
        padded_bytes_per_row,
        buffer_size,
        pixel_capacity,
    })
}

pub async fn read_texture_to_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let layout = readback_layout(width, height)?;
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
    receiver.await??;

    let data = buffer_slice.get_mapped_range();
    let mut pixels: Vec<u8> = Vec::with_capacity(layout.pixel_capacity);
    let row_len = usize::try_from(layout.unpadded_bytes_per_row)?;

    for row in 0..height {
        let start = usize::try_from(u64::from(row) * u64::from(layout.padded_bytes_per_row))?;
        let end = start.checked_add(row_len).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "mapped readback row exceeds address space",
            )
        })?;
        if end > data.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "mapped readback buffer is shorter than requested",
            )
            .into());
        }
        pixels.extend_from_slice(&data[start..end]);
    }

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
        assert_eq!(layout.pixel_capacity, 24);

        assert!(readback_layout(0, 1).is_err());
        assert!(readback_layout(1, 0).is_err());
        assert!(readback_layout(u32::MAX, 1).is_err());
        assert!(readback_layout(u32::MAX / 4, 1).is_err());
    }
}
