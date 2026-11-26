use super::*;

#[derive(Copy, Clone, Debug)]
pub(super) struct PartDraw {
    pub(super) mesh_id: u32,
    pub(super) albedo: u32,
    pub(super) transform: glam::Mat4,
    pub(super) blend_mode: BlendMode,
}

#[derive(Copy, Clone, Debug)]
pub(super) struct PreparedPartDraw {
    pub(super) draw: PartDraw,
    pub(super) part_uniform_offset: u32,
}

#[derive(Copy, Clone)]
pub(super) struct PreparedMaskedPartDraw<'a> {
    pub(super) draw: PreparedPartDraw,
    pub(super) mask_sources: &'a [crate::collect::MaskSourceData],
}

/// Stencil path only. Masked batch k's sources REPLACE k (dodge sources
/// punch 0) and its content tests Equal k; earlier batches' values are all
/// < k, so no clear between batches is needed.
pub(super) struct StencilPassState<'v> {
    pub(super) view: &'v wgpu::TextureView,
    pub(super) stencil_view: &'v wgpu::TextureView,
    pub(super) label: &'static str,
    pub(super) next_load: wgpu::LoadOp<wgpu::Color>,
    // Must be dropped before the encoder records anything else (wgpu
    // validates this at runtime).
    pub(super) pass: Option<wgpu::RenderPass<'static>>,
    pub(super) next_ref: u32,
    // Bind-group state is pass-scoped, so a draw whose texture is already
    // bound can skip the rebind.
    pub(super) bound_texture: Option<u32>,
    // Only for a single-part slot pass. A LoadOp::Clear still clears the whole
    // attachment.
    pub(super) scissor: Option<ScreenRect>,
}

impl<'v> StencilPassState<'v> {
    pub(super) fn new(
        view: &'v wgpu::TextureView,
        stencil_view: &'v wgpu::TextureView,
        clear_color: Option<wgpu::Color>,
        label: &'static str,
        scissor: Option<ScreenRect>,
    ) -> Self {
        Self {
            view,
            stencil_view,
            label,
            next_load: match clear_color {
                Some(color) => wgpu::LoadOp::Clear(color),
                None => wgpu::LoadOp::Load,
            },
            pass: None,
            next_ref: 1,
            bound_texture: None,
            scissor,
        }
    }
}

/// Alpha is the WebGL fallback: per-batch passes with mask-alpha textures.
pub(super) enum PartSink<'a, 'v> {
    Stencil(&'a mut StencilPassState<'v>),
    Alpha {
        view: &'v wgpu::TextureView,
        stencil: &'v StencilTarget,
        has_rendered: &'a mut bool,
        clear_color: Option<wgpu::Color>,
        mask_write_label: &'static str,
        masked_draw_label: &'static str,
    },
}

impl PartSink<'_, '_> {
    /// A pass outside the sink wrote the target, spending the frame clear.
    pub(super) fn mark_rendered(&mut self) {
        match self {
            PartSink::Stencil(s) => s.next_load = wgpu::LoadOp::Load,
            PartSink::Alpha { has_rendered, .. } => **has_rendered = true,
        }
    }
}

impl WgpuRenderer {
    /// Writes the batch's instance block `[mask0..maskK-1, part0..partN-1]` and
    /// returns the surviving indices plus mask and part bases. The caller must
    /// return the indices to `draw_filter_scratch`.
    pub(super) fn prepare_masked_part_draws(
        &mut self,
        draws: &[PreparedMaskedPartDraw<'_>],
    ) -> Option<(Vec<usize>, u64, u64)> {
        // All draws in a batch share one mask signature (caller
        // guarantees), so the sources come from the first draw.
        let mask_sources = draws.first()?.mask_sources;

        let mut valid = std::mem::take(&mut self.draw_filter_scratch);
        valid.clear();
        for (i, d) in draws.iter().enumerate() {
            let has_mesh = self.mesh_buffers.contains(d.draw.draw.mesh_id as usize);
            let has_tex = self.textures.contains(d.draw.draw.albedo as usize);
            if !has_mesh {
                self.current_stats.skipped_missing_mesh += 1;
            }
            if !has_tex {
                self.current_stats.skipped_missing_texture += 1;
            }
            if has_mesh && has_tex {
                valid.push(i);
            }
        }
        if valid.is_empty() {
            self.draw_filter_scratch = valid;
            return None;
        }
        self.current_stats.drawn_parts += valid.len() as u32;

        let stride = INSTANCE_STRIDE;
        let mask_part_count = mask_sources
            .iter()
            .filter(|source| source.is_part())
            .count();
        let base_offset = self.reserve_instances(mask_part_count + valid.len());
        let parts_base = base_offset + mask_part_count as u64 * stride;

        let mut instance_data = std::mem::take(&mut self.instance_scratch);
        instance_data.clear();
        for m in mask_sources {
            if let crate::collect::MaskSourceData::Part { transform, .. } = m {
                instance_data.push(InstanceRaw::from_transform(*transform));
            }
        }
        for &idx in &valid {
            instance_data.push(InstanceRaw::from_transform(draws[idx].draw.draw.transform));
        }
        self.write_instances(base_offset, &instance_data);
        self.instance_scratch = instance_data;

        Some((valid, base_offset, parts_base))
    }

    /// Every index in `valid` was verified resident by
    /// `prepare_masked_part_draws`.
    pub(super) fn record_masked_part_draws(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        draws: &[PreparedMaskedPartDraw<'_>],
        valid: &[usize],
        parts_base: u64,
        bound_texture: &mut Option<u32>,
    ) {
        let stride = INSTANCE_STRIDE;
        let mut current_blend_mode: Option<BlendMode> = None;
        let mut current_mesh: Option<u32> = None;
        let mut pipeline_swaps = 0u32;
        let mut texture_binds = 0u32;

        self.bind_pass_instances(render_pass);
        for (i, &idx) in valid.iter().enumerate() {
            let d = &draws[idx];
            let part = &d.draw.draw;
            let blend = canonical_part_blend(part.blend_mode);
            if current_blend_mode != Some(blend) {
                let pipeline = self.shared.masks.masked().get(blend);
                render_pass.set_pipeline(pipeline);
                current_blend_mode = Some(blend);
                pipeline_swaps += 1;
            }

            let (Some(mesh), Some(part_bg)) = (
                self.mesh_buffers.get(part.mesh_id as usize),
                self.part_bind_group(part.albedo),
            ) else {
                continue;
            };
            let key = part.albedo;
            if *bound_texture != Some(key) {
                render_pass.set_bind_group(1, part_bg, &[]);
                *bound_texture = Some(key);
                texture_binds += 1;
            }
            if current_mesh != Some(part.mesh_id) {
                render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(2, self.deform_slice(mesh));
                render_pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                current_mesh = Some(part.mesh_id);
            }
            render_pass.set_bind_group(
                2,
                &self.part_uniform_bind_group,
                &[d.draw.part_uniform_offset],
            );
            let off = parts_base + i as u64 * stride;
            self.emit_part_draw(render_pass, mesh.num_indices, off);
        }

        self.current_stats.pipeline_swaps += pipeline_swaps;
        self.current_stats.texture_binds += texture_binds;
    }

    // Alpha path: a same-signature masked run is one mask-alpha write pass
    // plus one sampled masked-draw pass. `clear_target` folds a composite-slot
    // clear into the masked-draw pass. Returns whether that pass was emitted.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_masked_prepared_parts_to(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        stencil: &StencilTarget,
        draws: &[PreparedMaskedPartDraw<'_>],
        clear_target: bool,
        clear_color: Option<wgpu::Color>,
        mask_write_label: &str,
        masked_draw_label: &str,
        scissor: Option<ScreenRect>,
    ) -> bool {
        // `FrameTargets::prepare` builds the target to match the pipelines.
        let Some(mask) = stencil.sampled.as_ref() else {
            return false;
        };
        let Some((valid, base_offset, parts_base)) = self.prepare_masked_part_draws(draws) else {
            return false;
        };
        let mask_sources = draws[0].mask_sources;

        self.write_mask_sources(
            encoder,
            mask_write_label,
            stencil,
            mask_sources,
            base_offset,
        );

        let load_op = if clear_target {
            wgpu::LoadOp::Clear(clear_color.unwrap_or(wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }))
        } else {
            wgpu::LoadOp::Load
        };

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some(masked_draw_label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        render_pass.set_bind_group(3, mask, &[]);
        // Only the content draw is scissored; it samples the mask only inside the
        // rect.
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_masked_part_draws(&mut render_pass, draws, &valid, parts_base, &mut None);
        drop(render_pass);

        self.draw_filter_scratch = valid;
        true
    }

    /// The caller must hand the indices back to `draw_filter_scratch`.
    pub(super) fn prepare_part_draws(
        &mut self,
        drawables: &[PreparedPartDraw],
    ) -> Option<(Vec<usize>, u64)> {
        let mut filtered = std::mem::take(&mut self.draw_filter_scratch);
        filtered.clear();
        for (i, p) in drawables.iter().enumerate() {
            let d = &p.draw;
            let has_mesh = self.mesh_buffers.contains(d.mesh_id as usize);
            let has_tex = self.textures.contains(d.albedo as usize);
            if !(has_mesh && has_tex) {
                tracing::trace!(
                    mesh_id = d.mesh_id,
                    texture_id = d.albedo,
                    has_mesh,
                    has_tex,
                    "dropping drawable with missing GPU resources",
                );
                if !has_mesh {
                    self.current_stats.skipped_missing_mesh += 1;
                }
                if !has_tex {
                    self.current_stats.skipped_missing_texture += 1;
                }
                continue;
            }
            filtered.push(i);
        }
        self.current_stats.drawn_parts += filtered.len() as u32;
        if filtered.is_empty() {
            self.draw_filter_scratch = filtered;
            return None;
        }

        let base_offset = self.reserve_instances(filtered.len());
        let mut instance_data = std::mem::take(&mut self.instance_scratch);
        instance_data.clear();
        instance_data.extend(
            filtered
                .iter()
                .map(|&i| InstanceRaw::from_transform(drawables[i].draw.transform)),
        );
        self.write_instances(base_offset, &instance_data);
        self.instance_scratch = instance_data;

        Some((filtered, base_offset))
    }

    /// Every index in `filtered` was verified resident by `prepare_part_draws`.
    pub(super) fn record_unmasked_part_draws(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        drawables: &[PreparedPartDraw],
        filtered: &[usize],
        base_offset: u64,
        bound_texture: &mut Option<u32>,
    ) {
        let stride = INSTANCE_STRIDE;

        let mut current_blend_mode: Option<BlendMode> = None;
        let mut current_mesh: Option<u32> = None;
        let mut pipeline_swaps = 0u32;
        let mut texture_binds = 0u32;

        self.bind_pass_instances(render_pass);
        for (i, &idx) in filtered.iter().enumerate() {
            let p = &drawables[idx];
            let d = &p.draw;
            let blend = canonical_part_blend(d.blend_mode);
            if current_blend_mode != Some(blend) {
                render_pass.set_pipeline(self.shared.parts.get(blend));
                current_blend_mode = Some(blend);
                pipeline_swaps += 1;
            }

            let (Some(mesh_buffer), Some(bind_group)) = (
                self.mesh_buffers.get(d.mesh_id as usize),
                self.part_bind_group(d.albedo),
            ) else {
                continue;
            };
            let off = base_offset + i as u64 * stride;
            let key = d.albedo;
            if *bound_texture != Some(key) {
                render_pass.set_bind_group(1, bind_group, &[]);
                *bound_texture = Some(key);
                texture_binds += 1;
            }
            if current_mesh != Some(d.mesh_id) {
                render_pass.set_vertex_buffer(0, mesh_buffer.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(2, self.deform_slice(mesh_buffer));
                render_pass
                    .set_index_buffer(mesh_buffer.index_buffer.slice(..), mesh_buffer.index_format);
                current_mesh = Some(d.mesh_id);
            }
            render_pass.set_bind_group(2, &self.part_uniform_bind_group, &[p.part_uniform_offset]);
            self.emit_part_draw(render_pass, mesh_buffer.num_indices, off);
        }

        self.current_stats.pipeline_swaps += pipeline_swaps;
        self.current_stats.texture_binds += texture_binds;
    }

    // Alpha path. Returns whether a draw pass was emitted, so callers can fold
    // the composite-slot clear into the first batch that draws.
    pub(super) fn render_prepared_parts(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        drawables: &[PreparedPartDraw],
        clear: bool,
        clear_color: Option<wgpu::Color>,
        scissor: Option<ScreenRect>,
    ) -> bool {
        let Some((filtered, base_offset)) = self.prepare_part_draws(drawables) else {
            return false;
        };

        let load_op = if clear {
            let color = clear_color.unwrap_or(wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            });
            wgpu::LoadOp::Clear(color)
        } else {
            wgpu::LoadOp::Load
        };

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_unmasked_part_draws(
            &mut render_pass,
            drawables,
            &filtered,
            base_offset,
            &mut None,
        );
        drop(render_pass);

        self.draw_filter_scratch = filtered;
        true
    }

    /// Stencil is cleared at open and discarded at the end: nothing reads it
    /// across passes.
    pub(super) fn open_stencil_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        s: &mut StencilPassState<'_>,
    ) {
        if s.pass.is_some() {
            return;
        }
        let mut pass = self
            .counted_begin_pass(
                encoder,
                &wgpu::RenderPassDescriptor {
                    label: Some(s.label),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        depth_slice: None,
                        view: s.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: s.next_load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: s.stencil_view,
                        depth_ops: None,
                        stencil_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0),
                            store: wgpu::StoreOp::Discard,
                        }),
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                },
            )
            .forget_lifetime();
        pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        if let Some(r) = s.scissor {
            pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        s.next_load = wgpu::LoadOp::Load;
        s.next_ref = 1;
        s.bound_texture = None;
        s.pass = Some(pass);
    }

    /// Ends the stencil sink's pass before an outside pass touches the target,
    /// emitting a still-pending frame clear first. No-op for the alpha sink.
    pub(super) fn end_sink_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
    ) {
        let PartSink::Stencil(s) = sink else { return };
        s.pass = None;
        if let wgpu::LoadOp::Clear(color) = s.next_load {
            self.explicit_clear_pass(encoder, s.view, color, "render_list deferred clear");
            s.next_load = wgpu::LoadOp::Load;
        }
    }

    /// No-op when the batch is empty, so barriers can call it unconditionally.
    pub(super) fn flush_pending_parts(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        pending: &[PreparedPartDraw],
    ) {
        if pending.is_empty() {
            return;
        }
        match sink {
            PartSink::Stencil(s) => {
                if let Some((filtered, base_offset)) = self.prepare_part_draws(pending) {
                    self.open_stencil_pass(encoder, s);
                    if let Some(pass) = s.pass.as_mut() {
                        self.record_unmasked_part_draws(
                            pass,
                            pending,
                            &filtered,
                            base_offset,
                            &mut s.bound_texture,
                        );
                    }
                    self.draw_filter_scratch = filtered;
                }
            }
            PartSink::Alpha {
                view,
                has_rendered,
                clear_color,
                ..
            } => {
                let clear = !**has_rendered && clear_color.is_some();
                let drew =
                    self.render_prepared_parts(encoder, view, pending, clear, *clear_color, None);
                **has_rendered |= drew;
            }
        }
    }

    pub(super) fn flush_pending_masked(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        pending: &[PreparedMaskedPartDraw<'_>],
    ) {
        if pending.is_empty() {
            return;
        }
        match sink {
            PartSink::Stencil(s) => {
                let mask_sources = pending[0].mask_sources;
                if let Some((valid, base_offset, parts_base)) =
                    self.prepare_masked_part_draws(pending)
                {
                    self.open_stencil_pass(encoder, s);
                    // Correctness backstop (unreachable for real rigs):
                    // the 8-bit stencil holds refs 1..=255 per cleared
                    // segment; reopening clears and restarts at 1.
                    if s.next_ref > 255 {
                        s.pass = None;
                        self.open_stencil_pass(encoder, s);
                    }
                    let ref_value = s.next_ref;
                    s.next_ref += 1;
                    if let Some(pass) = s.pass.as_mut() {
                        self.record_mask_sources_stencil(
                            pass,
                            mask_sources,
                            base_offset,
                            ref_value,
                            &mut s.bound_texture,
                        );
                        pass.set_stencil_reference(ref_value);
                        self.record_masked_part_draws(
                            pass,
                            pending,
                            &valid,
                            parts_base,
                            &mut s.bound_texture,
                        );
                    }
                    self.draw_filter_scratch = valid;
                }
            }
            PartSink::Alpha {
                view,
                stencil,
                has_rendered,
                clear_color,
                mask_write_label,
                masked_draw_label,
            } => {
                let clear = !**has_rendered && clear_color.is_some();
                let drew = self.render_masked_prepared_parts_to(
                    encoder,
                    view,
                    stencil,
                    pending,
                    clear,
                    *clear_color,
                    mask_write_label,
                    masked_draw_label,
                    None,
                );
                **has_rendered |= drew;
            }
        }
    }

    // At most one pending run is non-empty at a time, preserving z-order.
    // The caller owns the sink's end-of-run handling.
    pub(super) fn render_children_run(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        children: &[crate::collect::DrawableInfo],
        width: u32,
        height: u32,
    ) {
        use crate::collect::DrawableInfo;

        let mut pending_parts = std::mem::take(&mut self.pending_parts_scratch);
        pending_parts.clear();
        let mut pending_masked: Vec<PreparedMaskedPartDraw> = Vec::new();
        for child in children {
            let DrawableInfo::Part {
                mesh_id,
                texture_id,
                transform,
                blend_mode,
                opacity,
                tint,
                screen_tint,
                mask_sources,
                mask_threshold,
                ..
            } = child
            else {
                continue;
            };
            if self.part_screen_bounds(*mesh_id, *transform, width, height) == ScreenBounds::Empty {
                continue;
            }
            let part_uniform_offset =
                self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);

            if mask_sources.is_empty() {
                self.flush_pending_masked(encoder, sink, &pending_masked);
                pending_masked.clear();
                pending_parts.push(PreparedPartDraw {
                    draw: PartDraw {
                        mesh_id: *mesh_id,
                        albedo: *texture_id,
                        transform: *transform,
                        blend_mode: *blend_mode,
                    },
                    part_uniform_offset,
                });
            } else {
                self.flush_pending_parts(encoder, sink, &pending_parts);
                pending_parts.clear();
                if let Some(first) = pending_masked.first() {
                    if !same_mask_signature(first.mask_sources, mask_sources) {
                        self.flush_pending_masked(encoder, sink, &pending_masked);
                        pending_masked.clear();
                    }
                }
                pending_masked.push(PreparedMaskedPartDraw {
                    draw: PreparedPartDraw {
                        draw: PartDraw {
                            mesh_id: *mesh_id,
                            albedo: *texture_id,
                            transform: *transform,
                            blend_mode: *blend_mode,
                        },
                        part_uniform_offset,
                    },
                    mask_sources,
                });
            }
        }

        self.flush_pending_parts(encoder, sink, &pending_parts);
        self.flush_pending_masked(encoder, sink, &pending_masked);
        pending_parts.clear();
        self.pending_parts_scratch = pending_parts;
    }

    /// Alpha sink only: a pending clear must land before a barrier reads the
    /// target.
    pub(super) fn spend_pending_alpha_clear(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        label: &str,
    ) {
        if let PartSink::Alpha {
            view,
            has_rendered,
            clear_color,
            ..
        } = sink
        {
            if !**has_rendered {
                if let Some(color) = *clear_color {
                    self.explicit_clear_pass(encoder, view, color, label);
                    **has_rendered = true;
                }
            }
        }
    }
}
