use super::*;

/// Compares mask sources only; the masked draw rebinds its own mesh and
/// texture per draw.
pub(super) fn same_mask_signature(
    a_sources: &[crate::collect::MaskSourceData],
    b_sources: &[crate::collect::MaskSourceData],
) -> bool {
    use crate::collect::MaskSourceData;

    a_sources.len() == b_sources.len()
        && a_sources.iter().zip(b_sources).all(|(a, b)| match (a, b) {
            (
                MaskSourceData::Part {
                    mesh_id: a_mesh,
                    texture_id: a_texture,
                    mode: a_mode,
                    mask_threshold: a_threshold,
                    ..
                },
                MaskSourceData::Part {
                    mesh_id: b_mesh,
                    texture_id: b_texture,
                    mode: b_mode,
                    mask_threshold: b_threshold,
                    ..
                },
            ) => {
                a_mesh == b_mesh
                    && a_texture == b_texture
                    && a_mode == b_mode
                    && a_threshold.to_bits() == b_threshold.to_bits()
            }
            (
                MaskSourceData::Composite {
                    node_id: a_node,
                    mode: a_mode,
                },
                MaskSourceData::Composite {
                    node_id: b_node,
                    mode: b_mode,
                },
            ) => a_node == b_node && a_mode == b_mode,
            _ => false,
        })
}

#[derive(Clone)]
pub(super) struct PreparedCompositeMask {
    pub(super) texture: CompositeTexture,
    pub(super) opacity: f32,
    pub(super) mask_threshold: f32,
}

impl WgpuRenderer {
    pub(super) fn prepare_composite_mask_textures(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        width: u32,
        height: u32,
    ) {
        use crate::collect::{DrawableInfo, MaskSourceData};
        self.composite_mask_textures.clear();
        if render_list
            .composite_mask_sources
            .iter()
            .all(Option::is_none)
        {
            return;
        }
        let mut source_ids = Vec::new();
        let mut pending: Vec<_> = render_list.root_drawables.iter().collect();
        while let Some(drawable) = pending.pop() {
            if self.drawable_screen_bounds(drawable, render_list, width, height)
                == ScreenBounds::Empty
            {
                continue;
            }
            let masks = match drawable {
                DrawableInfo::Part { mask_sources, .. } => mask_sources,
                DrawableInfo::Composite {
                    node_id,
                    mask_sources,
                    ..
                } => {
                    pending.extend(render_list.composite_children(*node_id));
                    mask_sources
                }
            };
            source_ids.extend(masks.iter().filter_map(|mask| match mask {
                MaskSourceData::Composite { node_id, .. } => Some(*node_id),
                MaskSourceData::Part { .. } => None,
            }));
        }
        source_ids.sort_unstable();
        source_ids.dedup();

        for node_id in source_ids {
            let Some(source) = render_list.composite_mask_source(node_id) else {
                continue;
            };
            let slot = composites.acquire(&self.shared, &self.device);
            let base_offset = self.reserve_instances(source.parts.len());
            let instances: Vec<_> = source
                .parts
                .iter()
                .map(|part| InstanceRaw::from_transform(part.transform))
                .collect();
            self.write_instances(base_offset, &instances);
            let uniform_offsets: Vec<_> = source
                .parts
                .iter()
                .map(|part| {
                    self.write_part_uniform(
                        1.0,
                        glam::Vec3::ONE,
                        glam::Vec3::ZERO,
                        part.mask_threshold,
                    )
                })
                .collect();

            let mut pass = self.counted_begin_pass(
                encoder,
                &wgpu::RenderPassDescriptor {
                    label: Some("Composite Mask Source Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        depth_slice: None,
                        view: &slot.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                },
            );
            pass.set_pipeline(&self.shared.composite_mask_part_pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
            self.bind_pass_instances(&mut pass);
            let stride = INSTANCE_STRIDE;
            for (i, part) in source.parts.iter().enumerate() {
                let (Some(mesh), Some(texture)) = (
                    self.mesh_buffers.get(part.mesh_id as usize),
                    self.textures.get(part.texture_id as usize),
                ) else {
                    if !self.mesh_buffers.contains(part.mesh_id as usize) {
                        self.current_stats.skipped_missing_mask_mesh += 1;
                    }
                    if !self.textures.contains(part.texture_id as usize) {
                        self.current_stats.skipped_missing_mask_texture += 1;
                    }
                    continue;
                };
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.set_bind_group(2, &self.part_uniform_bind_group, &[uniform_offsets[i]]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(2, self.deform_slice(mesh));
                pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                self.emit_part_draw(&mut pass, mesh.num_indices, base_offset + i as u64 * stride);
            }
            drop(pass);
            self.composite_mask_textures.insert(
                node_id as usize,
                PreparedCompositeMask {
                    texture: slot,
                    opacity: source.opacity,
                    mask_threshold: source.mask_threshold,
                },
            );
        }
    }

    /// Sampled-mask path. Each source's instance must already be at
    /// `base_offset + i * size_of::<InstanceRaw>()`.
    pub(super) fn write_mask_sources(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
        stencil: &StencilTarget,
        mask_sources: &[crate::collect::MaskSourceData],
        base_offset: u64,
    ) {
        use crate::collect::MaskSourceData;

        let stride = INSTANCE_STRIDE;

        for m in mask_sources {
            if let MaskSourceData::Part {
                mesh_id,
                texture_id,
                ..
            } = m
            {
                if !self.mesh_buffers.contains(*mesh_id as usize) {
                    self.current_stats.skipped_missing_mask_mesh += 1;
                }
                if !self.textures.contains(*texture_id as usize) {
                    self.current_stats.skipped_missing_mask_texture += 1;
                }
            }
        }

        let source_uniform_offsets: SmallVec<[u32; 2]> = mask_sources
            .iter()
            .map(|m| match m {
                MaskSourceData::Part { mask_threshold, .. } => {
                    self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, *mask_threshold)
                }
                MaskSourceData::Composite { node_id, .. } => {
                    let (opacity, threshold) = self
                        .composite_mask_textures
                        .get(*node_id as usize)
                        .map(|source| (source.opacity, source.mask_threshold))
                        .unwrap_or((0.0, 1.0));
                    self.write_part_uniform(opacity, glam::Vec3::ONE, glam::Vec3::ZERO, threshold)
                }
            })
            .collect();

        // Start at 1 for an all-DodgeMask batch, else 0; sources REPLACE with 1
        // (Mask) or 0 (DodgeMask) and content passes where the mask is 1.
        let any_regular_mask = mask_sources
            .iter()
            .any(|m| m.mode() == catchlight_core::MaskMode::Mask);
        let c = if any_regular_mask { 0.0 } else { 1.0 };
        let alpha_clear = wgpu::Color {
            r: c,
            g: c,
            b: c,
            a: c,
        };

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: &stencil.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(alpha_clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        self.bind_pass_instances(&mut render_pass);

        let Some(masks) = self.shared.masks.sampled() else {
            return;
        };
        let mut current_part_dodge: Option<bool> = None;
        let mut part_index = 0usize;
        for (i, mask_source) in mask_sources.iter().enumerate() {
            match mask_source {
                MaskSourceData::Part {
                    mesh_id,
                    texture_id,
                    mode,
                    ..
                } => {
                    let off = base_offset + part_index as u64 * stride;
                    part_index += 1;
                    let (Some(mask_mesh), Some(mask_texture)) = (
                        self.mesh_buffers.get(*mesh_id as usize),
                        self.textures.get(*texture_id as usize),
                    ) else {
                        continue;
                    };
                    let dodge = *mode == catchlight_core::MaskMode::DodgeMask;
                    if current_part_dodge != Some(dodge) {
                        let pipeline = if dodge {
                            &masks.write_dodge
                        } else {
                            &masks.write
                        };
                        render_pass.set_pipeline(pipeline);
                        render_pass.set_bind_group(
                            0,
                            &self.camera_bind_group,
                            &[self.camera_offset],
                        );
                        current_part_dodge = Some(dodge);
                    }
                    render_pass.set_bind_group(1, &mask_texture.bind_group, &[]);
                    render_pass.set_bind_group(
                        2,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    render_pass.set_vertex_buffer(0, mask_mesh.vertex_buffer.slice(..));
                    render_pass.set_vertex_buffer(2, self.deform_slice(mask_mesh));
                    render_pass
                        .set_index_buffer(mask_mesh.index_buffer.slice(..), mask_mesh.index_format);
                    self.emit_part_draw(&mut render_pass, mask_mesh.num_indices, off);
                }
                MaskSourceData::Composite { node_id, mode } => {
                    let Some(source) = self.composite_mask_textures.get(*node_id as usize).cloned()
                    else {
                        continue;
                    };
                    let dodge = *mode == catchlight_core::MaskMode::DodgeMask;
                    let pipeline = if dodge {
                        &masks.composite_write_dodge
                    } else {
                        &masks.composite_write
                    };
                    render_pass.set_pipeline(pipeline);
                    render_pass.set_bind_group(0, &source.texture.blit_bind_group, &[]);
                    render_pass.set_bind_group(
                        1,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
                    render_pass.draw(0..3, 0..1);
                    current_part_dodge = None;
                }
            }
        }
    }

    /// Stencil path: regular sources REPLACE `ref_value`, dodge sources punch
    /// 0, and an all-DodgeMask batch first fills the viewport with `ref_value`.
    /// The caller must have bound the camera at group 0.
    pub(super) fn record_mask_sources_stencil(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        mask_sources: &[crate::collect::MaskSourceData],
        base_offset: u64,
        ref_value: u32,
        bound_texture: &mut Option<u32>,
    ) {
        use crate::collect::MaskSourceData;

        let stride = INSTANCE_STRIDE;

        for m in mask_sources {
            if let MaskSourceData::Part {
                mesh_id,
                texture_id,
                ..
            } = m
            {
                if !self.mesh_buffers.contains(*mesh_id as usize) {
                    self.current_stats.skipped_missing_mask_mesh += 1;
                }
                if !self.textures.contains(*texture_id as usize) {
                    self.current_stats.skipped_missing_mask_texture += 1;
                }
            }
        }

        let source_uniform_offsets: SmallVec<[u32; 2]> = mask_sources
            .iter()
            .map(|m| match m {
                MaskSourceData::Part { mask_threshold, .. } => {
                    self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, *mask_threshold)
                }
                MaskSourceData::Composite { node_id, .. } => {
                    let (opacity, threshold) = self
                        .composite_mask_textures
                        .get(*node_id as usize)
                        .map(|source| (source.opacity, source.mask_threshold))
                        .unwrap_or((0.0, 1.0));
                    self.write_part_uniform(opacity, glam::Vec3::ONE, glam::Vec3::ZERO, threshold)
                }
            })
            .collect();

        let Some(masks) = self.shared.masks.stencil() else {
            return;
        };
        let any_regular_mask = mask_sources
            .iter()
            .any(|m| m.mode() == catchlight_core::MaskMode::Mask);
        if !any_regular_mask {
            let fill = &masks.fill;
            render_pass.set_pipeline(fill);
            render_pass.set_stencil_reference(ref_value);
            self.current_stats.pipeline_swaps += 1;
            self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
            render_pass.draw(0..3, 0..1);
        }

        self.bind_pass_instances(render_pass);
        let mut part_pipeline_bound = false;
        let mut part_index = 0usize;
        for (i, mask_source) in mask_sources.iter().enumerate() {
            match mask_source {
                MaskSourceData::Part {
                    mesh_id,
                    texture_id,
                    mode,
                    ..
                } => {
                    let off = base_offset + part_index as u64 * stride;
                    part_index += 1;
                    let (Some(mask_mesh), Some(mask_texture)) = (
                        self.mesh_buffers.get(*mesh_id as usize),
                        self.textures.get(*texture_id as usize),
                    ) else {
                        continue;
                    };
                    if !part_pipeline_bound {
                        render_pass.set_pipeline(&masks.write);
                        render_pass.set_bind_group(
                            0,
                            &self.camera_bind_group,
                            &[self.camera_offset],
                        );
                        self.current_stats.pipeline_swaps += 1;
                        part_pipeline_bound = true;
                    }
                    render_pass.set_stencil_reference(
                        if *mode == catchlight_core::MaskMode::DodgeMask {
                            0
                        } else {
                            ref_value
                        },
                    );
                    if *bound_texture != Some(*texture_id) {
                        render_pass.set_bind_group(1, &mask_texture.bind_group, &[]);
                        *bound_texture = Some(*texture_id);
                        self.current_stats.texture_binds += 1;
                    }
                    render_pass.set_bind_group(
                        2,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    render_pass.set_vertex_buffer(0, mask_mesh.vertex_buffer.slice(..));
                    render_pass.set_vertex_buffer(2, self.deform_slice(mask_mesh));
                    render_pass
                        .set_index_buffer(mask_mesh.index_buffer.slice(..), mask_mesh.index_format);
                    self.emit_part_draw(render_pass, mask_mesh.num_indices, off);
                }
                MaskSourceData::Composite { node_id, mode } => {
                    let Some(source) = self.composite_mask_textures.get(*node_id as usize).cloned()
                    else {
                        continue;
                    };
                    render_pass.set_pipeline(&masks.composite_write);
                    render_pass.set_stencil_reference(
                        if *mode == catchlight_core::MaskMode::DodgeMask {
                            0
                        } else {
                            ref_value
                        },
                    );
                    render_pass.set_bind_group(0, &source.texture.blit_bind_group, &[]);
                    render_pass.set_bind_group(
                        1,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    self.current_stats.pipeline_swaps += 1;
                    self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
                    render_pass.draw(0..3, 0..1);
                    part_pipeline_bound = false;
                    *bound_texture = None;
                }
            }
        }
        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
    }
}
