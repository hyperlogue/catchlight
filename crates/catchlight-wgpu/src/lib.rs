pub mod deform_snapshot;
pub mod drawable_collector;
pub mod headless;
pub mod renderer;

pub use deform_snapshot::*;
pub use drawable_collector::*;
pub use headless::{apply_uniform_test_deform, RenderContext};
pub use renderer::*;

pub fn create_orthographic_camera(camera_height: f32, aspect: f32) -> glam::Mat4 {
    create_orthographic_camera_at(camera_height, aspect, glam::Vec2::ZERO)
}

/// Orthographic view-projection framing `camera_height` world units around
/// `center`.
///
/// **The camera holds no axis flip.** This is a textbook Y-up ortho;
/// catchlight world space is Y-up end to end.
pub fn create_orthographic_camera_at(
    camera_height: f32,
    aspect: f32,
    center: glam::Vec2,
) -> glam::Mat4 {
    let camera_width = camera_height * aspect;
    // catchlight world space is Y-up. A textbook Y-up ortho (bottom < top)
    // maps world +Y to NDC +Y; no axis flip lives in the camera.
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
        // wgpu 29 returns an enum instead of a Result; present the texture for
        // the good/suboptimal cases, otherwise signal the caller to re-acquire.
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

/// Build a device/queue + SurfaceContext for the given surface. Picks
/// an sRGB surface format so the GPU encodes linear→sRGB on write. Textures upload as
/// `Rgba8UnormSrgb` and the fragment shader operates in linear space;
/// a non-sRGB surface would skip the gamma encode on present and the
/// result looks washed out / dark.
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
    // Mobile Safari's WebGPU has no compute support and reports
    // `max_compute_workgroups_per_dimension = 0`; `Limits::default()`
    // demands 65535 and device creation fails. Catchlight is pure
    // render, so just take whatever the adapter actually offers.
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

    let caps = surface.get_capabilities(&adapter);
    // WebGPU canvas contexts only advertise the non-sRGB swapchain
    // formats (Bgra8Unorm, Rgba8Unorm, Rgba16Float). To get the GPU
    // to encode linear→sRGB on write — which is what the rest of our
    // pipeline assumes (textures upload as Rgba8UnormSrgb, fragment
    // shader works in linear) — we keep the swapchain in its native
    // non-sRGB format and render through an sRGB view. WebGL backends
    // typically expose the sRGB-suffixed variant directly, in which
    // case we use it for both surface and view.
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

    // Prefer Opaque compositing: the frame clear writes alpha 1.0, so the
    // canvas never needs per-pixel alpha blending against the page. Opaque
    // lets the browser compositor skip that blend. Fall back to whatever
    // the surface advertises first if Opaque isn't offered. A future
    // transparent-canvas caller should make this a configurable option
    // rather than forcing PreMultiplied here.
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
    surface.configure(&device, &config);

    Ok((
        device,
        queue,
        SurfaceContext {
            surface,
            config,
            surface_format,
            render_format,
        },
    ))
}

/// Device and queue for headless rendering (tests, the visual baselines, the
/// editor server's warm renderer).
///
/// Requests `Backends::PRIMARY` — deliberately not `all()`, because the
/// `webgl` feature unifies `wgc/gles` across the workspace and GL then tries
/// to init EGL and panics headless. `create_headless_context_ext` takes its
/// backend set from the caller, but everything below applies to it too.
///
/// On a box with no GPU, point the Vulkan loader at mesa's CPU ICD
/// (lavapipe). `nix/shell.nix` exports the path as `CATCHLIGHT_LAVAPIPE_ICD`
/// but deliberately does **not** set `VK_ICD_FILENAMES`, which would force
/// lavapipe over a real driver for everyone in the shell. Set it per-command:
///
/// ```text
/// VK_ICD_FILENAMES=$CATCHLIGHT_LAVAPIPE_ICD cargo test --workspace
/// ```
///
/// Without it, `request_adapter` fails and every GPU test panics with
///
/// ```text
/// NotFound { active_backends: Backends(0x0), ... }
/// ```
///
/// `vulkaninfo --summary` shows which ICD the loader actually picked.
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
            power_preference: wgpu::PowerPreference::default(),
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

/// Headless context for GPU benchmarking: select the backend explicitly
/// and request `TIMESTAMP_QUERY` + `TIMESTAMP_QUERY_INSIDE_ENCODERS`
/// (when the adapter supports them) so the renderer's GPU profiler is
/// active. The renderer profiles with `begin_query(encoder)` — an
/// encoder-scope timer — which `wgpu_profiler` only opens under
/// `INSIDE_ENCODERS`; `TIMESTAMP_QUERY` alone leaves it silently
/// inactive. Returns the adapter too, for `new_autodetect` and backend
/// reporting. Kept separate from `create_headless_context` so the
/// deterministic render/test path is unaffected by the extra features.
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

    queue.submit(std::iter::once(encoder.finish()));

    let buffer_slice = buffer.slice(..);
    let (sender, receiver) = futures::channel::oneshot::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    // Native drives the mapping with a blocking poll; on the web the browser
    // resolves it while the future is parked.
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    })?;
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
