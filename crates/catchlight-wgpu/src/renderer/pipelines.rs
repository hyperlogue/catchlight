use super::*;

/// Blend modes with a fixed-function state of their own, in slot order.
/// The dst-in-shader modes draw their source as `Normal`.
const FIXED_BLENDS: [BlendMode; 12] = [
    BlendMode::Normal,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::ClipToLower,
    BlendMode::SliceFromLower,
    BlendMode::ColorDodge,
    BlendMode::LinearDodge,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::Add,
    BlendMode::Inverse,
    BlendMode::Subtract,
];

fn blend_slot(mode: BlendMode) -> usize {
    match mode {
        BlendMode::Normal | BlendMode::Overlay | BlendMode::ColorBurn | BlendMode::LinearBurn => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
        BlendMode::ClipToLower => 3,
        BlendMode::SliceFromLower => 4,
        BlendMode::ColorDodge => 5,
        BlendMode::LinearDodge => 6,
        BlendMode::Darken => 7,
        BlendMode::Lighten => 8,
        BlendMode::Add => 9,
        BlendMode::Inverse => 10,
        BlendMode::Subtract => 11,
    }
}

/// One pipeline per fixed-function blend state.
pub(crate) struct BlendPipelines([wgpu::RenderPipeline; FIXED_BLENDS.len()]);

impl BlendPipelines {
    fn build(make: impl FnMut(BlendMode) -> wgpu::RenderPipeline) -> Self {
        Self(FIXED_BLENDS.map(make))
    }

    pub(crate) fn get(&self, mode: BlendMode) -> &wgpu::RenderPipeline {
        &self.0[blend_slot(mode)]
    }
}

/// Blend modes whose math reads the destination color, which no
/// fixed-function `BlendState` can express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShaderBlend {
    Overlay,
    ColorBurn,
    LinearBurn,
}

impl ShaderBlend {
    const ALL: [Self; 3] = [Self::Overlay, Self::ColorBurn, Self::LinearBurn];

    pub(crate) fn of(mode: BlendMode) -> Option<Self> {
        match mode {
            BlendMode::Overlay => Some(Self::Overlay),
            BlendMode::ColorBurn => Some(Self::ColorBurn),
            BlendMode::LinearBurn => Some(Self::LinearBurn),
            _ => None,
        }
    }

    fn fragment_entry(self) -> &'static str {
        match self {
            Self::Overlay => "fs_overlay",
            Self::ColorBurn => "fs_color_burn",
            Self::LinearBurn => "fs_linear_burn",
        }
    }
}

/// How a mask reaches the content it clips.
pub(crate) enum MaskPipelines {
    Stencil(StencilMasks),
    Sampled(SampledMasks),
}

/// Sources REPLACE a stencil reference and content tests Equal against it.
pub(crate) struct StencilMasks {
    pub(crate) write: wgpu::RenderPipeline,
    pub(crate) composite_write: wgpu::RenderPipeline,
    /// Seeds an all-DodgeMask batch so dodge sources can punch 0 out of it.
    pub(crate) fill: wgpu::RenderPipeline,
    pub(crate) masked: BlendPipelines,
    pub(crate) masked_blit: BlendPipelines,
}

/// For backends without a usable stencil attachment (Chromium swiftshader
/// WebGL2 fails Depth24PlusStencil8): sources write 1, or 0 for a dodge, into
/// an offscreen alpha texture that content samples.
pub(crate) struct SampledMasks {
    pub(crate) bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) sampler: wgpu::Sampler,
    pub(crate) write: wgpu::RenderPipeline,
    pub(crate) write_dodge: wgpu::RenderPipeline,
    pub(crate) composite_write: wgpu::RenderPipeline,
    pub(crate) composite_write_dodge: wgpu::RenderPipeline,
    pub(crate) masked: BlendPipelines,
    pub(crate) masked_blit: BlendPipelines,
}

impl MaskPipelines {
    pub(crate) fn stencil(&self) -> Option<&StencilMasks> {
        match self {
            Self::Stencil(masks) => Some(masks),
            Self::Sampled(_) => None,
        }
    }

    pub(crate) fn sampled(&self) -> Option<&SampledMasks> {
        match self {
            Self::Stencil(_) => None,
            Self::Sampled(masks) => Some(masks),
        }
    }

    pub(crate) fn masked(&self) -> &BlendPipelines {
        match self {
            Self::Stencil(masks) => &masks.masked,
            Self::Sampled(masks) => &masks.masked,
        }
    }

    pub(crate) fn masked_blit(&self) -> &BlendPipelines {
        match self {
            Self::Stencil(masks) => &masks.masked_blit,
            Self::Sampled(masks) => &masks.masked_blit,
        }
    }
}

/// Shared across every puppet in one world; built once per (device,
/// surface_format).
pub struct Pipelines {
    pub surface_format: wgpu::TextureFormat,
    pub(crate) parts: BlendPipelines,
    pub(crate) blits: BlendPipelines,
    pub(crate) masks: MaskPipelines,
    pub(crate) composite_mask_part_pipeline: wgpu::RenderPipeline,
    pub(crate) blit_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) blit_sampler: wgpu::Sampler,
    // Fixed to `Rgba8UnormSrgb`, independent of `surface_format`.
    pub(crate) mip_pipeline: wgpu::RenderPipeline,
    pub(crate) mip_bind_group_layout: wgpu::BindGroupLayout,
    /// Indexed by [`ShaderBlend`].
    blit_dst_in_shader_pipelines: [wgpu::RenderPipeline; 3],
    pub(crate) snapshot_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) snapshot_sampler: wgpu::Sampler,
    // Dynamic offset: one camera slot per view in a submit, so views do not
    // alias under `write_buffer` batching.
    pub(crate) camera_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) camera_stride: u64,
    pub(crate) texture_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) texture_sampler: wgpu::Sampler,
    pub(crate) part_uniform_bind_group_layout: wgpu::BindGroupLayout,
    // Rounded up to `min_uniform_buffer_offset_alignment`. Blits share the
    // wire format.
    pub(crate) part_uniform_stride: u64,
    // Native backends drive `first_instance` in hardware; GL/WebGL2 and the
    // adapter-less constructors re-slice the instance buffer per draw.
    pub(crate) base_instance: bool,
}

fn stencil_forced_off() -> bool {
    std::env::var("CATCHLIGHT_DISABLE_STENCIL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn sampled_texture_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
        label: Some(label),
    })
}

/// Depth is never tested or written; only the stencil half is used.
fn stencil_only(
    compare: wgpu::CompareFunction,
    pass_op: wgpu::StencilOperation,
    write_mask: u32,
) -> wgpu::DepthStencilState {
    let face = wgpu::StencilFaceState {
        compare,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op,
    };
    wgpu::DepthStencilState {
        format: wgpu::TextureFormat::Depth24PlusStencil8,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 0xff,
            write_mask,
        },
        bias: wgpu::DepthBiasState::default(),
    }
}

impl Pipelines {
    /// `CATCHLIGHT_DISABLE_STENCIL=1` forces the alpha-discard fallback.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        Self::new_with_options(device, surface_format, !stencil_forced_off())
    }

    /// GL adapters (WebGL2 included) get the stencil-free fallback;
    /// `CATCHLIGHT_DISABLE_STENCIL=1` forces it elsewhere.
    pub fn new_autodetect(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let non_gl = adapter.get_info().backend != wgpu::Backend::Gl;
        let has_stencil = !stencil_forced_off() && non_gl;
        let mut pipelines = Self::new_with_options(device, surface_format, has_stencil);
        // wgpu's GL backend only emulates `first_instance` by rebinding per draw,
        // so GL keeps the explicit per-draw slice.
        pipelines.base_instance = non_gl;
        pipelines
    }

    pub(crate) fn has_stencil(&self) -> bool {
        matches!(self.masks, MaskPipelines::Stencil(_))
    }

    pub(crate) fn blit_dst_in_shader(&self, blend: ShaderBlend) -> &wgpu::RenderPipeline {
        &self.blit_dst_in_shader_pipelines[blend as usize]
    }

    pub fn new_with_options(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        has_stencil: bool,
    ) -> Self {
        // Without CLAMP_TO_BORDER, out-of-range UVs are discarded so ClampToEdge
        // edge samples don't paint ghost slivers.
        let has_clamp_to_border = device
            .features()
            .contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER);
        let uv_discard = [("UV_DISCARD", if has_clamp_to_border { 0.0 } else { 1.0 })];
        let shader = Shader::new(
            device,
            "Basic Shader",
            include_str!("../shaders/basic.wgsl"),
            &uv_discard,
        );

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: std::num::NonZeroU64::new(
                            std::mem::size_of::<CameraUniform>() as u64,
                        ),
                    },
                    count: None,
                }],
                label: Some("camera_bind_group_layout"),
            });

        let camera_stride = {
            let align = device.limits().min_uniform_buffer_offset_alignment as u64;
            (std::mem::size_of::<CameraUniform>() as u64).div_ceil(align) * align
        };

        let texture_bind_group_layout = sampled_texture_layout(device, "texture_bind_group_layout");

        let part_uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: std::num::NonZeroU64::new(
                            std::mem::size_of::<PartUniform>() as u64,
                        ),
                    },
                    count: None,
                }],
                label: Some("part_uniform_bind_group_layout"),
            });

        let alignment = device.limits().min_uniform_buffer_offset_alignment as u64;
        let part_uniform_stride = {
            let raw = std::mem::size_of::<PartUniform>() as u64;
            raw.div_ceil(alignment) * alignment
        };

        let render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Render Pipeline Layout"),
                immediate_size: 0,
                bind_group_layouts: &[
                    Some(&camera_bind_group_layout),
                    Some(&texture_bind_group_layout),
                    Some(&part_uniform_bind_group_layout),
                ],
            });

        // Blit pipelines generate a fullscreen triangle and take `&[]`.
        let part_vertex_buffers = [Vertex::desc(), InstanceRaw::desc(), DeformAttr::desc()];

        // wgpu requires pipeline and pass depth-stencil formats to match, so
        // unmasked pipelines in the shared stencil pass carry a no-op stencil state.
        let part_depth_stencil = has_stencil.then(|| {
            stencil_only(
                wgpu::CompareFunction::Always,
                wgpu::StencilOperation::Keep,
                0x00,
            )
        });

        let parts = BlendPipelines::build(|blend_mode| {
            make_render_pipeline(
                device,
                &format!("Render Pipeline {:?}", blend_mode),
                &render_pipeline_layout,
                &shader,
                "vs_main",
                &part_vertex_buffers,
                &shader,
                "fs_main",
                surface_format,
                blend_mode_to_wgpu(blend_mode),
                wgpu::ColorWrites::ALL,
                part_depth_stencil.clone(),
            )
        });

        let blit_shader = Shader::new(
            device,
            "Blit Shader",
            include_str!("../shaders/blit.wgsl"),
            &[],
        );

        let blit_bind_group_layout = sampled_texture_layout(device, "blit_bind_group_layout");

        // Blits share the part-uniform layout and cursor-managed buffer so each
        // blit in a submit reads its own slot.
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Blit Pipeline Layout"),
            immediate_size: 0,
            bind_group_layouts: &[
                Some(&blit_bind_group_layout),
                Some(&part_uniform_bind_group_layout),
            ],
        });

        let blits = BlendPipelines::build(|blend_mode| {
            make_render_pipeline(
                device,
                &format!("Blit Pipeline {:?}", blend_mode),
                &blit_pipeline_layout,
                &blit_shader,
                "vs_main",
                &[],
                &blit_shader,
                "fs_main",
                surface_format,
                blend_mode_to_wgpu(blend_mode),
                wgpu::ColorWrites::ALL,
                None,
            )
        });

        let snapshot_bind_group_layout =
            sampled_texture_layout(device, "snapshot_bind_group_layout");

        let snapshot_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Framebuffer Snapshot Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            // `textureLoad` ignores filtering; Nearest avoids bilinear if a shader
            // ever switches to textureSample.
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let blit_dst_in_shader_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Blit (dst-in-shader) Pipeline Layout"),
                immediate_size: 0,
                bind_group_layouts: &[
                    Some(&blit_bind_group_layout),
                    Some(&part_uniform_bind_group_layout),
                    Some(&snapshot_bind_group_layout),
                ],
            });

        let blit_dst_in_shader_module = Shader::new(
            device,
            "Blit (dst-in-shader) Shader",
            include_str!("../shaders/blit_dst_in_shader.wgsl"),
            &[],
        );

        let blit_dst_in_shader_pipelines = ShaderBlend::ALL.map(|blend| {
            make_render_pipeline(
                device,
                &format!("Blit (dst-in-shader) Pipeline {:?}", blend),
                &blit_dst_in_shader_layout,
                &blit_dst_in_shader_module,
                "vs_main",
                &[],
                &blit_dst_in_shader_module,
                blend.fragment_entry(),
                surface_format,
                wgpu::BlendState::REPLACE,
                wgpu::ColorWrites::ALL,
                None,
            )
        });

        let composite_mask_part_pipeline = make_render_pipeline(
            device,
            "Composite Mask Part Pipeline",
            &render_pipeline_layout,
            &shader,
            "vs_main",
            &part_vertex_buffers,
            &shader,
            "fs_mask_alpha",
            surface_format,
            wgpu::BlendState::REPLACE,
            wgpu::ColorWrites::ALL,
            None,
        );

        let masks = if has_stencil {
            // fs_mask discards texels at or below mask_threshold; otherwise masks
            // would be triangle-shaped rather than alpha-shaped.
            let replace = stencil_only(
                wgpu::CompareFunction::Always,
                wgpu::StencilOperation::Replace,
                0xff,
            );
            let equal = stencil_only(
                wgpu::CompareFunction::Equal,
                wgpu::StencilOperation::Keep,
                0x00,
            );
            let write = make_render_pipeline(
                device,
                "Mask Write Pipeline",
                &render_pipeline_layout,
                &shader,
                "vs_main",
                &part_vertex_buffers,
                &shader,
                "fs_mask",
                surface_format,
                blend_mode_to_wgpu(BlendMode::Normal),
                wgpu::ColorWrites::empty(),
                Some(replace.clone()),
            );
            let composite_write = make_render_pipeline(
                device,
                "Composite Mask Write Pipeline",
                &blit_pipeline_layout,
                &blit_shader,
                "vs_main",
                &[],
                &blit_shader,
                "fs_composite_mask",
                surface_format,
                wgpu::BlendState::REPLACE,
                wgpu::ColorWrites::empty(),
                Some(replace.clone()),
            );
            let masked = BlendPipelines::build(|blend_mode| {
                make_render_pipeline(
                    device,
                    &format!("Masked Render Pipeline {:?}", blend_mode),
                    &render_pipeline_layout,
                    &shader,
                    "vs_main",
                    &part_vertex_buffers,
                    &shader,
                    "fs_main",
                    surface_format,
                    blend_mode_to_wgpu(blend_mode),
                    wgpu::ColorWrites::ALL,
                    Some(equal.clone()),
                )
            });
            let masked_blit = BlendPipelines::build(|blend_mode| {
                make_render_pipeline(
                    device,
                    &format!("Masked Blit Pipeline {:?}", blend_mode),
                    &blit_pipeline_layout,
                    &blit_shader,
                    "vs_main",
                    &[],
                    &blit_shader,
                    "fs_main",
                    surface_format,
                    blend_mode_to_wgpu(blend_mode),
                    wgpu::ColorWrites::ALL,
                    Some(equal.clone()),
                )
            });

            let stencil_fill_shader = Shader::new(
                device,
                "Stencil Fill Shader",
                include_str!("../shaders/stencil_fill.wgsl"),
                &[],
            );
            let stencil_fill_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Stencil Fill Pipeline Layout"),
                    immediate_size: 0,
                    bind_group_layouts: &[],
                });
            let fill = make_render_pipeline(
                device,
                "Stencil Fill Pipeline",
                &stencil_fill_layout,
                &stencil_fill_shader,
                "vs_main",
                &[],
                &stencil_fill_shader,
                "fs_main",
                surface_format,
                wgpu::BlendState::REPLACE,
                wgpu::ColorWrites::empty(),
                Some(replace),
            );
            MaskPipelines::Stencil(StencilMasks {
                write,
                composite_write,
                fill,
                masked,
                masked_blit,
            })
        } else {
            let bind_group_layout = sampled_texture_layout(device, "mask_bind_group_layout");

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Mask Alpha Sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            });

            let masked_part_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Masked (Sampled) Pipeline Layout"),
                    immediate_size: 0,
                    bind_group_layouts: &[
                        Some(&camera_bind_group_layout),
                        Some(&texture_bind_group_layout),
                        Some(&part_uniform_bind_group_layout),
                        Some(&bind_group_layout),
                    ],
                });

            // Group 0 composite, group 1 blit uniforms, group 2 mask.
            let masked_blit_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Masked Blit (Sampled) Pipeline Layout"),
                    immediate_size: 0,
                    bind_group_layouts: &[
                        Some(&blit_bind_group_layout),
                        Some(&part_uniform_bind_group_layout),
                        Some(&bind_group_layout),
                    ],
                });

            // REPLACE mirrors the stencil path's op; only alpha is read.
            let part_write = |label: &str, fragment_entry: &str| {
                make_render_pipeline(
                    device,
                    label,
                    &render_pipeline_layout,
                    &shader,
                    "vs_main",
                    &part_vertex_buffers,
                    &shader,
                    fragment_entry,
                    surface_format,
                    wgpu::BlendState::REPLACE,
                    wgpu::ColorWrites::ALL,
                    None,
                )
            };
            let composite_write = |label: &str, fragment_entry: &str| {
                make_render_pipeline(
                    device,
                    label,
                    &blit_pipeline_layout,
                    &blit_shader,
                    "vs_main",
                    &[],
                    &blit_shader,
                    fragment_entry,
                    surface_format,
                    wgpu::BlendState::REPLACE,
                    wgpu::ColorWrites::ALL,
                    None,
                )
            };

            let masked = BlendPipelines::build(|blend_mode| {
                make_render_pipeline(
                    device,
                    &format!("Masked (Sampled) Pipeline {:?}", blend_mode),
                    &masked_part_layout,
                    &shader,
                    "vs_main",
                    &part_vertex_buffers,
                    &shader,
                    "fs_masked_sampled",
                    surface_format,
                    blend_mode_to_wgpu(blend_mode),
                    wgpu::ColorWrites::ALL,
                    None,
                )
            });
            let masked_blit = BlendPipelines::build(|blend_mode| {
                make_render_pipeline(
                    device,
                    &format!("Masked Blit (Sampled) Pipeline {:?}", blend_mode),
                    &masked_blit_layout,
                    &blit_shader,
                    "vs_main",
                    &[],
                    &blit_shader,
                    "fs_masked_sampled",
                    surface_format,
                    blend_mode_to_wgpu(blend_mode),
                    wgpu::ColorWrites::ALL,
                    None,
                )
            });

            MaskPipelines::Sampled(SampledMasks {
                write: part_write("Mask Alpha Write Pipeline", "fs_mask_alpha"),
                write_dodge: part_write("Mask Alpha Dodge Write Pipeline", "fs_mask_alpha_dodge"),
                composite_write: composite_write(
                    "Composite Mask Write Pipeline",
                    "fs_composite_mask",
                ),
                composite_write_dodge: composite_write(
                    "Composite Mask Alpha Dodge Pipeline",
                    "fs_composite_mask_dodge",
                ),
                masked,
                masked_blit,
                bind_group_layout,
                sampler,
            })
        };

        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Blit Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let mip_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                }],
                label: Some("mip_bind_group_layout"),
            });
        let mip_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mip_downsample.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/mip_downsample.wgsl").into()),
        });
        let mip_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Mip Pipeline Layout"),
            immediate_size: 0,
            bind_group_layouts: &[Some(&mip_bind_group_layout)],
        });
        let mip_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Mip Downsample Pipeline"),
            layout: Some(&mip_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &mip_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &mip_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: PUPPET_TEXTURE_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        // CLAMP_TO_BORDER is native-only; elsewhere ClampToEdge plus `UV_DISCARD`
        // replicates TransparentBlack borders.
        let (address_mode, border_color) = if has_clamp_to_border {
            (
                wgpu::AddressMode::ClampToBorder,
                Some(wgpu::SamplerBorderColor::TransparentBlack),
            )
        } else {
            (wgpu::AddressMode::ClampToEdge, None)
        };
        // Linear mip filtering avoids banding at fractional mip levels. wgpu
        // clamps anisotropy to the hardware cap (1 without ANISOTROPIC_FILTERING)
        // and requires all filters Linear above 1.
        let texture_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Texture Sampler"),
            address_mode_u: address_mode,
            address_mode_v: address_mode,
            address_mode_w: address_mode,
            border_color,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });

        Self {
            surface_format,
            parts,
            blits,
            masks,
            composite_mask_part_pipeline,
            blit_bind_group_layout,
            blit_sampler,
            mip_pipeline,
            mip_bind_group_layout,
            blit_dst_in_shader_pipelines,
            snapshot_bind_group_layout,
            snapshot_sampler,
            camera_bind_group_layout,
            camera_stride,
            texture_bind_group_layout,
            texture_sampler,
            part_uniform_bind_group_layout,
            part_uniform_stride,
            // `new_autodetect` overrides this from the adapter.
            base_instance: false,
        }
    }
}

/// A shader module and the override values its pipelines compile with.
struct Shader<'a> {
    module: wgpu::ShaderModule,
    constants: &'a [(&'a str, f64)],
}

impl<'a> Shader<'a> {
    fn new(
        device: &wgpu::Device,
        label: &str,
        source: &str,
        constants: &'a [(&'a str, f64)],
    ) -> Self {
        Self {
            module: device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            }),
            constants,
        }
    }

    fn options(&self) -> wgpu::PipelineCompilationOptions<'_> {
        wgpu::PipelineCompilationOptions {
            constants: self.constants,
            ..Default::default()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn make_render_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    vertex: &Shader,
    vertex_entry: &str,
    vertex_buffers: &[wgpu::VertexBufferLayout],
    fragment: &Shader,
    fragment_entry: &str,
    format: wgpu::TextureFormat,
    blend: wgpu::BlendState,
    write_mask: wgpu::ColorWrites,
    depth_stencil: Option<wgpu::DepthStencilState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &vertex.module,
            entry_point: Some(vertex_entry),
            buffers: vertex_buffers,
            compilation_options: vertex.options(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fragment.module,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask,
            })],
            compilation_options: fragment.options(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixed_blend_owns_its_slot() {
        for (slot, mode) in FIXED_BLENDS.into_iter().enumerate() {
            assert_eq!(blend_slot(mode), slot, "{mode:?}");
        }
    }
}
