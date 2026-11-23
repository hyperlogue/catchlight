use super::*;

impl Pipelines {
    /// `CATCHLIGHT_DISABLE_STENCIL=1` forces the alpha-discard fallback.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let forced_off = std::env::var("CATCHLIGHT_DISABLE_STENCIL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Self::new_with_options(device, surface_format, !forced_off)
    }

    /// GL adapters (WebGL2 included) get the stencil-free fallback;
    /// `CATCHLIGHT_DISABLE_STENCIL=1` forces it elsewhere.
    pub fn new_autodetect(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let forced_off = std::env::var("CATCHLIGHT_DISABLE_STENCIL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let non_gl = adapter.get_info().backend != wgpu::Backend::Gl;
        let has_stencil = !forced_off && non_gl;
        let mut pipelines = Self::new_with_options(device, surface_format, has_stencil);
        // wgpu's GL backend only emulates `first_instance` by rebinding per draw,
        // so GL keeps the explicit per-draw slice.
        pipelines.base_instance = non_gl;
        pipelines
    }

    pub fn new_with_options(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        has_stencil: bool,
    ) -> Self {
        // Prepended rather than a pipeline override: wgpu 27's WebGPU backend
        // breaks pipeline creation with non-default PipelineCompilationOptions.
        // Without CLAMP_TO_BORDER, out-of-range UVs are discarded so ClampToEdge
        // edge samples don't paint ghost slivers.
        let has_clamp_to_border = device
            .features()
            .contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER);
        let uv_val = if has_clamp_to_border { 0.0f32 } else { 1.0 };
        let prefix = format!("const UV_DISCARD: f32 = {};\n", uv_val);
        let basic_wgsl = format!("{}{}", prefix, include_str!("../shaders/basic.wgsl"));
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Basic Shader"),
            source: wgpu::ShaderSource::Wgsl(basic_wgsl.into()),
        });

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

        let texture_bind_group_layout =
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
                label: Some("texture_bind_group_layout"),
            });

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

        let mut pipelines = HashMap::new();
        let blend_modes = [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::ClipToLower,
            BlendMode::SliceFromLower,
            BlendMode::ColorDodge,
            BlendMode::LinearDodge,
            BlendMode::Overlay,
            BlendMode::ColorBurn,
            BlendMode::LinearBurn,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Add,
            BlendMode::Inverse,
            BlendMode::Subtract,
        ];

        // Blit pipelines generate a fullscreen triangle and take `&[]`.
        let part_vertex_buffers = [Vertex::desc(), InstanceRaw::desc(), DeformAttr::desc()];

        // wgpu requires pipeline and pass depth-stencil formats to match, so
        // unmasked pipelines in the shared stencil pass carry a no-op stencil state.
        let noop_stencil_face = wgpu::StencilFaceState {
            compare: wgpu::CompareFunction::Always,
            fail_op: wgpu::StencilOperation::Keep,
            depth_fail_op: wgpu::StencilOperation::Keep,
            pass_op: wgpu::StencilOperation::Keep,
        };
        let part_depth_stencil = has_stencil.then(|| wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState {
                front: noop_stencil_face,
                back: noop_stencil_face,
                read_mask: 0xff,
                write_mask: 0x00,
            },
            bias: wgpu::DepthBiasState::default(),
        });

        for blend_mode in blend_modes {
            let pipeline = make_render_pipeline(
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
            );
            pipelines.insert(blend_mode, pipeline);
        }

        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Blit Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/blit.wgsl").into()),
        });

        let blit_bind_group_layout =
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
                label: Some("blit_bind_group_layout"),
            });

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

        let mut blit_pipelines = HashMap::new();

        for blend_mode in blend_modes {
            let blit_pipeline = make_render_pipeline(
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
            );
            blit_pipelines.insert(blend_mode, blit_pipeline);
        }

        let snapshot_bind_group_layout =
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
                label: Some("snapshot_bind_group_layout"),
            });

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

        let blit_dst_in_shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Blit (dst-in-shader) Shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../shaders/blit_dst_in_shader.wgsl").into(),
            ),
        });

        let dst_in_shader_modes: &[(BlendMode, &'static str)] = &[
            (BlendMode::Overlay, "fs_overlay"),
            (BlendMode::ColorBurn, "fs_color_burn"),
            (BlendMode::LinearBurn, "fs_linear_burn"),
        ];

        let mut blit_dst_in_shader_pipelines = HashMap::new();
        for (mode, entry) in dst_in_shader_modes {
            let pipeline = make_render_pipeline(
                device,
                &format!("Blit (dst-in-shader) Pipeline {:?}", mode),
                &blit_dst_in_shader_layout,
                &blit_dst_in_shader_module,
                "vs_main",
                &[],
                &blit_dst_in_shader_module,
                entry,
                surface_format,
                wgpu::BlendState::REPLACE,
                wgpu::ColorWrites::ALL,
                None,
            );
            blit_dst_in_shader_pipelines.insert(*mode, pipeline);
        }

        // fs_mask discards texels at or below mask_threshold; otherwise masks
        // would be triangle-shaped rather than alpha-shaped.
        let stencil_state = wgpu::StencilState {
            front: wgpu::StencilFaceState {
                compare: wgpu::CompareFunction::Always,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::Replace,
            },
            back: wgpu::StencilFaceState {
                compare: wgpu::CompareFunction::Always,
                fail_op: wgpu::StencilOperation::Keep,
                depth_fail_op: wgpu::StencilOperation::Keep,
                pass_op: wgpu::StencilOperation::Replace,
            },
            read_mask: 0xff,
            write_mask: 0xff,
        };
        let mask_depth_stencil = if has_stencil {
            Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: stencil_state.clone(),
                bias: wgpu::DepthBiasState::default(),
            })
        } else {
            None
        };
        let mask_write_pipeline = make_render_pipeline(
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
            if has_stencil {
                wgpu::ColorWrites::empty()
            } else {
                wgpu::ColorWrites::ALL
            },
            mask_depth_stencil,
        );

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
        let composite_mask_write_pipeline = make_render_pipeline(
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
            if has_stencil {
                wgpu::ColorWrites::empty()
            } else {
                wgpu::ColorWrites::ALL
            },
            if has_stencil {
                Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth24PlusStencil8,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: stencil_state.clone(),
                    bias: wgpu::DepthBiasState::default(),
                })
            } else {
                None
            },
        );
        let composite_mask_alpha_dodge_pipeline = (!has_stencil).then(|| {
            make_render_pipeline(
                device,
                "Composite Mask Alpha Dodge Pipeline",
                &blit_pipeline_layout,
                &blit_shader,
                "vs_main",
                &[],
                &blit_shader,
                "fs_composite_mask_dodge",
                surface_format,
                wgpu::BlendState::REPLACE,
                wgpu::ColorWrites::ALL,
                None,
            )
        });

        // Empty on the alpha path, which uses masked_sampled_pipelines.
        let mut masked_pipelines = HashMap::new();
        let mut masked_blit_pipelines = HashMap::new();
        let mut stencil_fill_pipeline = None;
        if has_stencil {
            // Identical for every blend mode, so build it once.
            let masked_stencil = wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24PlusStencil8,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState {
                    front: wgpu::StencilFaceState {
                        compare: wgpu::CompareFunction::Equal,
                        fail_op: wgpu::StencilOperation::Keep,
                        depth_fail_op: wgpu::StencilOperation::Keep,
                        pass_op: wgpu::StencilOperation::Keep,
                    },
                    back: wgpu::StencilFaceState {
                        compare: wgpu::CompareFunction::Equal,
                        fail_op: wgpu::StencilOperation::Keep,
                        depth_fail_op: wgpu::StencilOperation::Keep,
                        pass_op: wgpu::StencilOperation::Keep,
                    },
                    read_mask: 0xff,
                    write_mask: 0x00,
                },
                bias: wgpu::DepthBiasState::default(),
            };
            for blend_mode in blend_modes {
                let pipeline = make_render_pipeline(
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
                    Some(masked_stencil.clone()),
                );
                masked_pipelines.insert(blend_mode, pipeline);
            }

            for blend_mode in blend_modes {
                let pipeline = make_render_pipeline(
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
                    Some(masked_stencil.clone()),
                );
                masked_blit_pipelines.insert(blend_mode, pipeline);
            }

            let stencil_fill_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Stencil Fill Shader"),
                source: wgpu::ShaderSource::Wgsl(
                    include_str!("../shaders/stencil_fill.wgsl").into(),
                ),
            });
            let stencil_fill_layout =
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("Stencil Fill Pipeline Layout"),
                    immediate_size: 0,
                    bind_group_layouts: &[],
                });
            stencil_fill_pipeline = Some(make_render_pipeline(
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
                Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth24PlusStencil8,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: stencil_state.clone(),
                    bias: wgpu::DepthBiasState::default(),
                }),
            ));
        }

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

        // WebGL fallback: only compiled when has_stencil == false.
        let (
            mask_bind_group_layout,
            mask_sampler,
            mask_alpha_pipeline,
            mask_alpha_dodge_pipeline,
            masked_sampled_pipelines,
            masked_sampled_blit_pipelines,
        ) = if has_stencil {
            (None, None, None, None, HashMap::new(), HashMap::new())
        } else {
            let mask_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
                label: Some("mask_bind_group_layout"),
            });

            let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
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
                        Some(&mask_layout),
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
                        Some(&mask_layout),
                    ],
                });

            // REPLACE mirrors the stencil path's op; only alpha is read.
            let mask_alpha_pipeline = make_render_pipeline(
                device,
                "Mask Alpha Write Pipeline",
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

            let mask_alpha_dodge_pipeline = make_render_pipeline(
                device,
                "Mask Alpha Dodge Write Pipeline",
                &render_pipeline_layout,
                &shader,
                "vs_main",
                &part_vertex_buffers,
                &shader,
                "fs_mask_alpha_dodge",
                surface_format,
                wgpu::BlendState::REPLACE,
                wgpu::ColorWrites::ALL,
                None,
            );

            let mut masked_sampled_pipelines = HashMap::new();
            for blend_mode in blend_modes {
                let pipeline = make_render_pipeline(
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
                );
                masked_sampled_pipelines.insert(blend_mode, pipeline);
            }

            let mut masked_sampled_blit_pipelines = HashMap::new();
            for blend_mode in blend_modes {
                let pipeline = make_render_pipeline(
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
                );
                masked_sampled_blit_pipelines.insert(blend_mode, pipeline);
            }

            (
                Some(mask_layout),
                Some(mask_sampler),
                Some(mask_alpha_pipeline),
                Some(mask_alpha_dodge_pipeline),
                masked_sampled_pipelines,
                masked_sampled_blit_pipelines,
            )
        };

        Self {
            surface_format,
            pipelines,
            mask_write_pipeline,
            composite_mask_part_pipeline,
            composite_mask_write_pipeline,
            composite_mask_alpha_dodge_pipeline,
            masked_pipelines,
            blit_pipelines,
            masked_blit_pipelines,
            stencil_fill_pipeline,
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
            has_stencil,
            mask_bind_group_layout,
            mask_sampler,
            mask_alpha_pipeline,
            mask_alpha_dodge_pipeline,
            masked_sampled_pipelines,
            masked_sampled_blit_pipelines,
        }
    }
}
