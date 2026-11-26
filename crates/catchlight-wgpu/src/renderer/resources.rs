use super::*;

pub(crate) struct Texture {
    pub(super) texture: wgpu::Texture,
    pub(super) bind_group: wgpu::BindGroup,
}

/// Bytes are premultiplied linear encoded as sRGB; the sampler's decode hands
/// shaders premultiplied linear, so CPU-side tints are converted to linear.
pub(super) const PUPPET_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Fill mips 1.. by successively halving the level above on the GPU.
pub(super) fn generate_mips(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip_level_count: u32,
    pipelines: &Pipelines,
    label: &str,
) {
    if mip_level_count < 2 {
        return;
    }
    let views: Vec<wgpu::TextureView> = (0..mip_level_count)
        .map(|mip| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: mip,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect();

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(&format!("{label} mips")),
    });
    for target in 1..mip_level_count as usize {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &pipelines.mip_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&views[target - 1]),
            }],
            label: Some("mip bind group"),
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mip downsample"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &views[target],
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The triangle covers every texel, so the cleared
                    // value is never read.
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipelines.mip_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
}

impl Texture {
    pub(super) fn from_decoded_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rgba: &[u8],
        width: u32,
        height: u32,
        pipelines: &Pipelines,
        label: &str,
    ) -> RendererResult<Self> {
        let bind_group_layout = &pipelines.texture_bind_group_layout;
        let sampler = &pipelines.texture_sampler;
        let (w, h) = (width, height);
        let bytes_per_row =
            validate_rgba8_texture(w, h, rgba.len(), device.limits().max_texture_dimension_2d)?;

        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };

        let mip_level_count = 32 - w.max(h).max(1).leading_zeros();

        // Bytes encode premultiplied linear color; the sRGB format makes the
        // sampler return linear values.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PUPPET_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(h),
            },
            size,
        );
        generate_mips(device, queue, &texture, mip_level_count, pipelines, label);

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
            label: Some(label),
        });

        Ok(Self {
            texture,
            bind_group,
        })
    }
}

pub(super) fn validate_rgba8_texture(
    width: u32,
    height: u32,
    actual: usize,
    dimension_limit: u32,
) -> RendererResult<u32> {
    if width == 0 || height == 0 || width > dimension_limit || height > dimension_limit {
        return Err(RendererError::TextureDimensionsOutOfRange {
            width,
            height,
            limit: dimension_limit,
        });
    }
    let expected_u64 = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(RendererError::TextureByteLengthOverflow { width, height })?;
    let expected = usize::try_from(expected_u64)
        .map_err(|_| RendererError::TextureByteLengthOverflow { width, height })?;
    if actual != expected {
        return Err(RendererError::TextureByteLengthMismatch {
            width,
            height,
            expected,
            actual,
        });
    }
    width
        .checked_mul(4)
        .ok_or(RendererError::TextureByteLengthOverflow { width, height })
}

pub(crate) struct MeshBuffer {
    pub(super) vertex_buffer: wgpu::Buffer,
    pub(super) index_buffer: wgpu::Buffer,
    pub(super) deform_offset: u64,
    pub(super) deform_size: u64,
    pub(super) vert_count: u32,
    pub(super) num_indices: u32,
    pub(super) index_format: wgpu::IndexFormat,
    /// AABB over `pos - origin`. None for an empty mesh.
    pub(super) local_bounds: Option<Aabb2>,
}

impl WgpuRenderer {
    /// Drops every mesh, as a rebuild that keeps none does.
    #[doc(hidden)]
    pub fn clear_meshes(&mut self) {
        self.mesh_buffers.clear();
        self.reset_deform_buffer_layout();
    }

    /// `keep` maps old slots to new slots; a moved resource keeps its debug label.
    pub(crate) fn retain_textures(&mut self, keep: &[(u32, u32)]) {
        let mut moved: Vec<(u32, Texture)> = Vec::with_capacity(keep.len());
        for &(from, to) in keep {
            if let Some(texture) = self.textures.take(from as usize) {
                moved.push((to, texture));
            }
        }
        self.textures.clear();
        for (to, texture) in moved {
            self.textures.insert(to as usize, texture);
        }
    }

    pub(crate) fn has_mesh(&self, slot: u32) -> bool {
        self.mesh_buffers.contains(slot as usize)
    }

    pub(crate) fn retain_meshes(&mut self, keep: &[(u32, u32)], total: usize) {
        if keep.len() == total
            && total == self.mesh_buffers.live()
            && keep.iter().all(|&(from, to)| from == to)
        {
            // Rebaking restarts stack generations even when geometry is unchanged.
            // Keep residency and active slots, but compare their bytes on refresh.
            for set in &mut self.deform_sets {
                for generation in set.uploaded.slots.iter_mut().flatten() {
                    *generation = None;
                }
            }
            return;
        }
        let moved: Vec<_> = keep
            .iter()
            .filter_map(|&(from, to)| self.mesh_buffers.take(from as usize).map(|mesh| (to, mesh)))
            .collect();
        self.clear_meshes();
        for (to, mut mesh) in moved {
            mesh.deform_offset = self.reserve_deform_buffer(mesh.deform_size);
            self.mesh_buffers.insert(to as usize, mesh);
        }
    }

    /// Checked without touching GPU state, so a rebuild can validate before
    /// releasing the build it replaces.
    pub(crate) fn validate_texture_upload(&self, tex: &DecodedTexture) -> RendererResult<()> {
        validate_rgba8_texture(
            tex.width,
            tex.height,
            tex.rgba.len(),
            self.device.limits().max_texture_dimension_2d,
        )
        .map(|_| ())
    }

    #[doc(hidden)]
    pub fn max_texture_dimension(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    #[doc(hidden)]
    pub fn live_mesh_slots(&self) -> usize {
        self.mesh_buffers.live()
    }

    #[doc(hidden)]
    pub fn live_texture_slots(&self) -> usize {
        self.textures.live()
    }

    #[doc(hidden)]
    pub fn reserved_deform_bytes(&self) -> u64 {
        self.deform_buffer_len
    }

    /// Missing uvs become zeros and a trailing partial triangle is ignored,
    /// matching the importer. An out-of-range index is an error.
    pub(crate) fn upload_mesh(
        &mut self,
        mesh_id: u32,
        mesh: &catchlight_core::Mesh,
    ) -> Result<(), MeshError> {
        if !mesh.uvs.is_empty() && mesh.vertices.len() != mesh.uvs.len() {
            return Err(MeshError::VertexUvMismatch {
                mesh_id,
                vertices: mesh.vertices.len(),
                uvs: mesh.uvs.len(),
            });
        }
        let vertex_count = mesh.vertices.len();
        if let Some(bad) = mesh
            .indices
            .iter_u32()
            .find(|&i| (i as usize) >= vertex_count)
        {
            return Err(MeshError::IndexOutOfBounds {
                mesh_id,
                index: bad,
                vertices: vertex_count,
            });
        }

        let uv_at = |i: usize| {
            mesh.uvs
                .get(i)
                .copied()
                .unwrap_or(catchlight_core::Vec2::ZERO)
        };
        let vertices: Vec<Vertex> = mesh
            .vertices
            .iter()
            .enumerate()
            .map(|(i, pos)| {
                let uv = uv_at(i);
                Vertex {
                    position: [pos.x - mesh.origin.x, pos.y - mesh.origin.y],
                    uv: [uv.x, uv.y],
                }
            })
            .collect();

        let local_bounds = vertices.first().map(|first| {
            let mut min = glam::Vec2::from(first.position);
            let mut max = min;
            for v in &vertices[1..] {
                let p = glam::Vec2::from(v.position);
                min = min.min(p);
                max = max.max(p);
            }
            Aabb2 { min, max }
        });

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("Vertex Buffer {mesh_id}")),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let deform_size = (vertex_count * size_of::<DeformAttr>()) as u64;
        let deform_offset = self.reserve_deform_buffer(deform_size);
        let deform_start = deform_offset as usize;
        let deform_end = deform_start + deform_size as usize;
        self.deform_upload_mirror[deform_start..deform_end].fill(0);
        self.note_queue_write(RenderBuffer::Deform);
        self.queue.write_buffer(
            &self.deform_buffer,
            deform_offset,
            &self.deform_upload_mirror[deform_start..deform_end],
        );

        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("Index Buffer {mesh_id}")),
                contents: mesh.indices.as_bytes(),
                usage: wgpu::BufferUsages::INDEX,
            });

        let index_format = match &mesh.indices {
            catchlight_core::MeshIndices::U16(_) => wgpu::IndexFormat::Uint16,
            catchlight_core::MeshIndices::U32(_) => wgpu::IndexFormat::Uint32,
        };

        let mesh_buffer = MeshBuffer {
            vertex_buffer,
            index_buffer,
            deform_offset,
            deform_size,
            vert_count: vertex_count as u32,
            num_indices: (mesh.indices.len() - mesh.indices.len() % 3) as u32,
            index_format,
            local_bounds,
        };

        self.mesh_buffers.insert(mesh_id as usize, mesh_buffer);
        Ok(())
    }

    pub(super) fn part_bind_group(&self, albedo: u32) -> Option<&wgpu::BindGroup> {
        self.textures.get(albedo as usize).map(|t| &t.bind_group)
    }

    pub(crate) fn upload_texture(
        &mut self,
        texture_id: u32,
        tex: &DecodedTexture,
    ) -> RendererResult<()> {
        let texture = Texture::from_decoded_rgba8(
            &self.device,
            &self.queue,
            &tex.rgba,
            tex.width,
            tex.height,
            &self.shared,
            &format!("Texture {texture_id}"),
        )?;
        // Mip generation submits its own command buffer.
        self.frame_stats.queue_submits += u32::from(texture.texture.mip_level_count() > 1);

        self.textures.insert(texture_id as usize, texture);
        Ok(())
    }
}
