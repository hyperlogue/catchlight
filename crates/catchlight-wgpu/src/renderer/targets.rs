use super::*;

#[derive(Clone)]
pub struct CompositeTexture {
    pub view: wgpu::TextureView,
    // A dst-in-shader child blends against this composite's contents, and
    // the snapshot copy needs the texture handle (COPY_SRC), not a view.
    pub(super) texture: wgpu::Texture,
    pub(super) blit_bind_group: wgpu::BindGroup,
}

impl CompositeTexture {
    pub fn new(pipelines: &Pipelines, device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Composite Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipelines.surface_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &pipelines.blit_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.blit_sampler),
                },
            ],
            label: Some("Composite blit bind group"),
        });
        Self {
            view,
            texture,
            blit_bind_group,
        }
    }
}

/// Snapshot of the main color attachment for the dst-in-shader blit. The
/// source must carry `COPY_SRC`.
pub(super) struct SnapshotTexture {
    pub(super) texture: wgpu::Texture,
    pub(super) bind_group: wgpu::BindGroup,
}

impl SnapshotTexture {
    pub(super) fn new(
        pipelines: &Pipelines,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Framebuffer Snapshot Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipelines.surface_format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &pipelines.snapshot_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.snapshot_sampler),
                },
            ],
            label: Some("Framebuffer Snapshot bind group"),
        });
        Self {
            texture,
            bind_group,
        }
    }
}

/// Allocates nothing until a dst-in-shader part shows up.
pub struct FramebufferSnapshotPool {
    pub(super) slots: Vec<SnapshotTexture>,
    pub(super) cursor: usize,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl FramebufferSnapshotPool {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            slots: Vec::new(),
            cursor: 0,
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn ensure_size(&mut self, width: u32, height: u32) {
        let w = width.max(1);
        let h = height.max(1);
        if self.width != w || self.height != h {
            self.width = w;
            self.height = h;
            self.slots.clear();
            self.cursor = 0;
        }
    }

    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    pub fn mark(&self) -> usize {
        self.cursor
    }

    pub fn rewind(&mut self, mark: usize) {
        self.cursor = self.cursor.min(mark);
    }

    pub(super) fn acquire(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
    ) -> &SnapshotTexture {
        if self.cursor >= self.slots.len() {
            self.slots.push(SnapshotTexture::new(
                pipelines,
                device,
                self.width,
                self.height,
            ));
        }
        let idx = self.cursor;
        self.cursor += 1;
        &self.slots[idx]
    }
}

/// Viewport-sized offscreen textures reused across composites; `reset()` at
/// the start of every frame.
pub struct CompositePool {
    pub(super) slots: Vec<CompositeTexture>,
    pub(super) cursor: usize,
    // Rewinds pull `cursor` back, so it cannot say how many slots a frame needed.
    pub(super) peak: usize,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl CompositePool {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            slots: Vec::new(),
            cursor: 0,
            peak: 0,
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn ensure_size(&mut self, width: u32, height: u32) {
        let w = width.max(1);
        let h = height.max(1);
        if self.width != w || self.height != h {
            self.width = w;
            self.height = h;
            self.slots.clear();
            self.cursor = 0;
            self.peak = 0;
        }
    }

    pub fn reset(&mut self) {
        self.cursor = 0;
        self.peak = 0;
    }

    /// Lets non-nested composites share a slot instead of each growing the pool.
    pub fn mark(&self) -> usize {
        self.cursor
    }

    /// Never moves the cursor forward, so a nested acquire that outlives its
    /// mark is not freed under a recorded pass.
    pub fn rewind(&mut self, mark: usize) {
        self.cursor = self.cursor.min(mark);
    }

    pub fn acquire(&mut self, pipelines: &Pipelines, device: &wgpu::Device) -> CompositeTexture {
        if self.cursor >= self.slots.len() {
            self.slots.push(CompositeTexture::new(
                pipelines,
                device,
                self.width,
                self.height,
            ));
        }
        let idx = self.cursor;
        self.cursor += 1;
        self.peak = self.peak.max(self.cursor);
        self.slots[idx].clone()
    }

    /// `(peak_this_frame, capacity, width, height)`.
    pub fn stats(&self) -> (usize, usize, u32, u32) {
        (self.peak, self.slots.len(), self.width, self.height)
    }
}

/// Viewport-sized mask target shared across every puppet in a frame:
/// `Depth24PlusStencil8` on the stencil path, otherwise a sampled color
/// mask texture (WebGL fallback).
pub struct StencilTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    /// `Some` exactly on the sampled-mask path.
    pub mask_bind_group: Option<wgpu::BindGroup>,
}

impl StencilTarget {
    /// Stencil-backed only; `new_for_pipelines` picks by backend.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Stencil Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            mask_bind_group: None,
        }
    }

    pub(super) fn new_mask_alpha(
        masks: &pipelines::SampledMasks,
        format: wgpu::TextureFormat,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Mask Alpha Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mask_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &masks.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&masks.sampler),
                },
            ],
            label: Some("Mask Alpha bind group"),
        });
        Self {
            texture,
            view,
            mask_bind_group: Some(mask_bind_group),
        }
    }

    pub fn new_for_pipelines(
        pipelines: &Pipelines,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Self {
        match &pipelines.masks {
            MaskPipelines::Stencil(_) => Self::new(device, width, height),
            MaskPipelines::Sampled(masks) => {
                Self::new_mask_alpha(masks, pipelines.surface_format, device, width, height)
            }
        }
    }

    /// A target built by [`Self::new_for_pipelines`] for sampled-mask pipelines
    /// always carries the bind group.
    #[allow(clippy::expect_used)]
    pub(super) fn sampled_mask(&self) -> &wgpu::BindGroup {
        self.mask_bind_group
            .as_ref()
            .expect("a stencil-backed target was passed to sampled-mask pipelines")
    }

    pub fn ensure_size_for_pipelines(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) {
        let w = width.max(1);
        let h = height.max(1);
        if self.texture.width() != w || self.texture.height() != h {
            *self = Self::new_for_pipelines(pipelines, device, w, h);
        }
    }
}
