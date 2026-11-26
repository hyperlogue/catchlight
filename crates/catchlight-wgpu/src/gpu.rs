//! Finding a GPU: [`GpuContext`] holds the instance, adapter, device and
//! queue together, so a host can configure further surfaces on the same
//! device.

use crate::{RendererError, RendererResult};

/// Features requested only where the adapter has them.
const OPTIONAL_FEATURES: wgpu::Features = wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER;

#[derive(Debug, Clone)]
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// `Backends::PRIMARY`, not `all()`: the workspace-unified `webgl`
    /// feature enables GL, which panics initializing EGL headless.
    pub async fn headless() -> RendererResult<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: Default::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });
        // `HighPerformance` makes a hardware adapter win over lavapipe when
        // both are present.
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await?;
        Self::from_adapter(instance, adapter).await
    }

    /// An adapter that can present to `surface`, with `surface` configured on
    /// its device.
    pub async fn with_surface(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> RendererResult<(Self, SurfaceContext)> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await?;
        let gpu = Self::from_adapter(instance, adapter).await?;
        let surface = gpu.configure_surface(surface, width, height)?;
        Ok((gpu, surface))
    }

    /// Opens a device on an adapter the host chose itself.
    pub async fn from_adapter(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
    ) -> RendererResult<Self> {
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: adapter.features() & OPTIONAL_FEATURES,
                // Mobile Safari reports no compute limits, so
                // `Limits::default()` fails.
                required_limits: adapter.limits(),
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Renders through an sRGB view: shaders work in linear space, so a
    /// non-sRGB target would skip the gamma encode on present. Every canvas
    /// of a host goes through one context; a device per canvas could not
    /// share textures.
    pub fn configure_surface(
        &self,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> RendererResult<SurfaceContext> {
        let caps = surface.get_capabilities(&self.adapter);
        let (Some(&first_format), Some(&first_alpha)) =
            (caps.formats.first(), caps.alpha_modes.first())
        else {
            return Err(RendererError::SurfaceUnsupported);
        };
        // WebGPU canvases advertise only non-sRGB formats, so render through
        // an sRGB view of the native format.
        let (surface_format, render_format, view_formats) =
            if let Some(&fmt) = caps.formats.iter().find(|f| f.is_srgb()) {
                (fmt, fmt, Vec::<wgpu::TextureFormat>::new())
            } else {
                let base = *caps
                    .formats
                    .iter()
                    .find(|f| f.add_srgb_suffix() != **f)
                    .unwrap_or(&first_format);
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
            .unwrap_or(first_alpha);

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
        surface.configure(&self.device, &config);

        Ok(SurfaceContext {
            surface,
            config,
            surface_format,
            render_format,
        })
    }
}

#[derive(Debug)]
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

    pub fn acquire(&self) -> Acquired {
        use wgpu::CurrentSurfaceTexture as Current;
        match self.surface.get_current_texture() {
            Current::Success(frame) | Current::Suboptimal(frame) => {
                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(self.render_format),
                    ..Default::default()
                });
                Acquired::Frame { frame, view }
            }
            Current::Timeout | Current::Occluded | Current::Validation => Acquired::Skip,
            Current::Outdated | Current::Lost => Acquired::Reconfigure,
        }
    }
}

/// What [`SurfaceContext::acquire`] found.
#[derive(Debug)]
pub enum Acquired {
    /// Draw through `view` (in `render_format`), then present `frame`.
    Frame {
        frame: wgpu::SurfaceTexture,
        view: wgpu::TextureView,
    },
    /// The surface timed out or its window is hidden: draw nothing this
    /// frame and try again on the next.
    Skip,
    /// The surface no longer matches its window:
    /// [`SurfaceContext::reconfigure`] it before the next attempt.
    Reconfigure,
}
