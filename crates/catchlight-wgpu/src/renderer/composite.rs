use super::*;

/// The labels one target's passes carry in a GPU capture.
pub(super) struct PassLabels {
    pub(super) draw: &'static str,
    pub(super) mask_write: &'static str,
    pub(super) masked_draw: &'static str,
    pub(super) fallback_clear: &'static str,
}

pub(super) const ROOT_PASSES: PassLabels = PassLabels {
    draw: "Render Pass",
    mask_write: "Mask Write Pass",
    masked_draw: "Masked Part Render Pass",
    fallback_clear: "render_list fallback clear",
};

pub(super) const COMPOSITE_PASSES: PassLabels = PassLabels {
    draw: "Composite Children Pass",
    mask_write: "Composite Mask Write Pass",
    masked_draw: "Composite Masked Part Render Pass",
    fallback_clear: "Composite Fallback Clear Pass",
};

/// `texture` is only needed by the dst-in-shader blit's snapshot; `None`
/// sends that blit down the Normal-OVER fallback.
#[derive(Clone, Copy)]
pub(super) struct CompositeTarget<'a> {
    pub(super) view: &'a wgpu::TextureView,
    pub(super) texture: Option<&'a wgpu::Texture>,
}

impl WgpuRenderer {
    /// Sampled-mask path only; the stencil path folds the mask write and blit
    /// into one pass (`blit_composite_with_masks`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn blit_composite_masked(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        composite_texture: &CompositeTexture,
        target_view: &wgpu::TextureView,
        stencil: &StencilTarget,
        blend_mode: BlendMode,
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) {
        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Masked Blit Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        render_pass.set_pipeline(self.shared.masks.masked_blit().get(blend_mode));
        render_pass.set_bind_group(0, &composite_texture.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        render_pass.set_bind_group(2, stencil.sampled_mask(), &[]);
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    pub(crate) fn blit_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        composite_texture: &CompositeTexture,
        target_view: &wgpu::TextureView,
        blend_mode: BlendMode,
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) {
        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Blit Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        render_pass.set_pipeline(self.shared.blits.get(blend_mode));
        render_pass.set_bind_group(0, &composite_texture.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn blit_composite_dst_in_shader(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        composite: &CompositeTexture,
        target_view: &wgpu::TextureView,
        target_color_texture: &wgpu::Texture,
        snapshot: &SnapshotTexture,
        blend: ShaderBlend,
        part_uniform_offset: u32,
        width: u32,
        height: u32,
        scissor: Option<ScreenRect>,
    ) {
        // The copy runs in encoder order (unlike write_buffer), so it sees every
        // prior pass. A scissored copy suffices: the snapshot is sampled 1:1 and
        // dst-in-shader modes return dst where src alpha is 0.
        let (copy_origin, copy_extent) = match scissor {
            Some(r) => (
                wgpu::Origin3d {
                    x: r.x,
                    y: r.y,
                    z: 0,
                },
                wgpu::Extent3d {
                    width: r.width,
                    height: r.height,
                    depth_or_array_layers: 1,
                },
            ),
            None => (
                wgpu::Origin3d::ZERO,
                wgpu::Extent3d {
                    width: width.max(1),
                    height: height.max(1),
                    depth_or_array_layers: 1,
                },
            ),
        };
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target_color_texture,
                mip_level: 0,
                origin: copy_origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &snapshot.texture,
                mip_level: 0,
                origin: copy_origin,
                aspect: wgpu::TextureAspect::All,
            },
            copy_extent,
        );

        let pipeline = self.shared.blit_dst_in_shader(blend);

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Blit (dst-in-shader) Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &composite.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        render_pass.set_bind_group(2, &snapshot.bind_group, &[]);
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    /// Gives the dst-in-shader blit a src texture independent of the framebuffer.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_part_to_composite_slot(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        slot: &CompositeTexture,
        mesh_id: u32,
        texture_id: u32,
        transform: glam::Mat4,
        mask_sources: &[crate::collect::MaskSourceData],
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) {
        let transparent = wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
        let draw = PreparedPartDraw {
            draw: PartDraw {
                mesh_id,
                albedo: texture_id,
                transform,
                blend_mode: BlendMode::Normal,
            },
            part_uniform_offset,
        };

        if self.shared.has_stencil() {
            let mut state = StencilPassState::new(
                &slot.view,
                &stencil.view,
                Some(transparent),
                "Composite Slot Pass (dst-in-shader)",
                scissor,
            );
            let mut sink = PartSink::Stencil(&mut state);
            if mask_sources.is_empty() {
                self.flush_pending_parts(encoder, &mut sink, &mut vec![draw]);
            } else {
                self.flush_pending_masked(
                    encoder,
                    &mut sink,
                    &mut vec![PreparedMaskedPartDraw { draw, mask_sources }],
                );
            }
            // If the draw filtered out, this emits the still-pending
            // transparent clear so the blit can't sample stale pool
            // contents.
            self.end_sink_pass(encoder, &mut sink);
            return;
        }

        let drew = if mask_sources.is_empty() {
            self.render_prepared_parts(
                encoder,
                &slot.view,
                &[draw],
                true,
                Some(transparent),
                scissor,
            )
        } else {
            self.render_masked_prepared_parts_to(
                encoder,
                &slot.view,
                stencil,
                &[PreparedMaskedPartDraw { draw, mask_sources }],
                true,
                Some(transparent),
                "Composite Slot Mask Write (dst-in-shader)",
                "Composite Slot Masked Part (dst-in-shader)",
                scissor,
            )
        };

        if !drew {
            self.explicit_clear_pass(
                encoder,
                &slot.view,
                transparent,
                "Composite Slot Fallback Clear (dst-in-shader)",
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn blit_composite_with_masks(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        composite: &CompositeTexture,
        target_view: &wgpu::TextureView,
        stencil: &StencilTarget,
        blend_mode: BlendMode,
        mask_sources: &[crate::collect::MaskSourceData],
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) {
        if mask_sources.is_empty() {
            self.blit_composite(
                encoder,
                composite,
                target_view,
                blend_mode,
                part_uniform_offset,
                scissor,
            );
            return;
        }

        let base_offset = self.reserve_instances(
            mask_sources
                .iter()
                .filter(|source| source.is_part())
                .count(),
        );
        let mut instance_data = std::mem::take(&mut self.instance_scratch);
        instance_data.clear();
        instance_data.extend(mask_sources.iter().filter_map(|source| match source {
            crate::collect::MaskSourceData::Part { transform, .. } => {
                Some(InstanceRaw::from_transform(*transform))
            }
            crate::collect::MaskSourceData::Composite { .. } => None,
        }));
        self.write_instances(base_offset, &instance_data);
        self.instance_scratch = instance_data;

        if !self.shared.has_stencil() {
            self.write_mask_sources(
                encoder,
                "Composite Blit Mask Write Pass",
                stencil,
                mask_sources,
                base_offset,
            );
            self.blit_composite_masked(
                encoder,
                composite,
                target_view,
                stencil,
                blend_mode,
                part_uniform_offset,
                scissor,
            );
            return;
        }

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Masked Blit Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &stencil.view,
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
        );
        // Mask-source draws use the part layout; the blit pipeline rebinds group 0.
        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        // Outside the children union the composite is a transparent clear and the
        // stencil is never tested.
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_mask_sources_stencil(&mut render_pass, mask_sources, base_offset, 1, &mut None);

        render_pass.set_pipeline(self.shared.masks.masked_blit().get(blend_mode));
        render_pass.set_stencil_reference(1);
        render_pass.set_bind_group(0, &composite.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    /// Serves root and nested composites identically. The caller must have
    /// flushed its pending runs and ended the sink's pass.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_composite_drawable(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        target: CompositeTarget<'_>,
        drawable: &crate::collect::DrawableInfo,
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        mut snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
    ) -> bool {
        use crate::collect::DrawableInfo;

        let DrawableInfo::Composite {
            node_id,
            blend_mode,
            opacity,
            tint,
            screen_tint,
            ref mask_sources,
            mask_threshold,
            ..
        } = drawable
        else {
            return false;
        };
        let children = match render_list.composite_children.get(node_id) {
            Some(c) if !c.is_empty() => c,
            _ => return false,
        };
        let _span =
            tracing::trace_span!("render_composite", masked = !mask_sources.is_empty()).entered();

        // Computed before the children render so the borrow doesn't collide.
        let bounds = self.drawable_screen_bounds(drawable, render_list, width, height);
        if bounds == ScreenBounds::Empty {
            return false;
        }
        let union = bounds.scissor();

        let part_uniform_offset =
            self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);

        // Rewinding after the blit lets siblings share slots; the pool grows to
        // the nesting depth and no further.
        let composite_mark = composites.mark();
        let snapshot_mark = snapshots.as_ref().map(|s| s.mark());
        let slot = composites.acquire(&self.shared, &self.device);
        // The slot's clear folds into its first drawing pass, or lands on its
        // own, so the blit never samples stale pool contents.
        self.render_to_target(
            encoder,
            stencil,
            CompositeTarget {
                view: &slot.view,
                texture: Some(&slot.texture),
            },
            Some(wgpu::Color::TRANSPARENT),
            &COMPOSITE_PASSES,
            children,
            render_list,
            composites,
            snapshots.as_deref_mut(),
            width,
            height,
        );

        // Without a texture or pool, fall through to the Normal-OVER blit.
        let mut blitted = false;
        if let Some(shader_blend) = ShaderBlend::of(*blend_mode) {
            if let (Some(main_tex), Some(snap_pool)) = (target.texture, snapshots.as_deref_mut()) {
                let mut source = slot.clone();
                let mut source_uniform_offset = part_uniform_offset;
                if !mask_sources.is_empty() {
                    let masked = composites.acquire(&self.shared, &self.device);
                    self.explicit_clear_pass(
                        encoder,
                        &masked.view,
                        wgpu::Color::TRANSPARENT,
                        "Masked Composite Source Clear",
                    );
                    self.blit_composite_with_masks(
                        encoder,
                        &slot,
                        &masked.view,
                        stencil,
                        BlendMode::Normal,
                        mask_sources,
                        part_uniform_offset,
                        union,
                    );
                    source = masked;
                    source_uniform_offset =
                        self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5);
                }
                let snapshot = snap_pool.acquire(&self.shared, &self.device);
                self.blit_composite_dst_in_shader(
                    encoder,
                    &source,
                    target.view,
                    main_tex,
                    snapshot,
                    shader_blend,
                    source_uniform_offset,
                    width,
                    height,
                    union,
                );
                blitted = true;
            }
        }

        if !blitted {
            let blit_scissor = if blend_transparent_src_is_identity(*blend_mode) {
                union
            } else {
                None
            };
            if mask_sources.is_empty() {
                self.blit_composite(
                    encoder,
                    &slot,
                    target.view,
                    *blend_mode,
                    part_uniform_offset,
                    blit_scissor,
                );
            } else {
                self.blit_composite_with_masks(
                    encoder,
                    &slot,
                    target.view,
                    stencil,
                    *blend_mode,
                    mask_sources,
                    part_uniform_offset,
                    blit_scissor,
                );
            }
        }

        composites.rewind(composite_mark);
        if let (Some(pool), Some(mark)) = (snapshots, snapshot_mark) {
            pool.rewind(mark);
        }
        self.current_stats.drawn_composites += 1;
        true
    }

    /// Draws `drawables` into `target`, spending `clear_color` exactly once even
    /// when nothing draws. Serves the frame target and every composite slot.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_to_target(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        target: CompositeTarget<'_>,
        clear_color: Option<wgpu::Color>,
        labels: &PassLabels,
        drawables: &[crate::collect::DrawableInfo],
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
    ) {
        if self.shared.has_stencil() {
            let mut state =
                StencilPassState::new(target.view, &stencil.view, clear_color, labels.draw, None);
            let mut sink = PartSink::Stencil(&mut state);
            self.render_drawables(
                encoder,
                stencil,
                &mut sink,
                target,
                drawables,
                render_list,
                composites,
                snapshots,
                width,
                height,
            );
            self.end_sink_pass(encoder, &mut sink);
            return;
        }

        let mut has_rendered = false;
        let mut sink = PartSink::Alpha {
            view: target.view,
            stencil,
            has_rendered: &mut has_rendered,
            clear_color,
            mask_write_label: labels.mask_write,
            masked_draw_label: labels.masked_draw,
        };
        self.render_drawables(
            encoder,
            stencil,
            &mut sink,
            target,
            drawables,
            render_list,
            composites,
            snapshots,
            width,
            height,
        );
        if !has_rendered {
            if let Some(color) = clear_color {
                self.explicit_clear_pass(encoder, target.view, color, labels.fallback_clear);
            }
        }
    }

    /// Splits at drawables that read the target's own contents (composites
    /// and dst-in-shader parts); each records its own passes between part
    /// runs, which keeps z-order.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_drawables(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        sink: &mut PartSink<'_, '_>,
        target: CompositeTarget<'_>,
        drawables: &[crate::collect::DrawableInfo],
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        mut snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
    ) {
        use crate::collect::DrawableInfo;

        // Without a snapshot pool and a copyable target, dst-in-shader parts
        // draw OVER inside the part runs.
        let shader_blends = snapshots.is_some() && target.texture.is_some();
        let is_barrier = |d: &DrawableInfo| match d {
            DrawableInfo::Composite { .. } => true,
            DrawableInfo::Part { blend_mode, .. } => shader_blends && is_dst_in_shader(*blend_mode),
        };
        if !drawables.iter().any(is_barrier) {
            return self.render_children_run(encoder, sink, drawables, width, height);
        }

        let mut run_start = 0;
        for (i, drawable) in drawables.iter().enumerate() {
            if !is_barrier(drawable) {
                continue;
            }
            let bounds = self.drawable_screen_bounds(drawable, render_list, width, height);
            if bounds == ScreenBounds::Empty {
                continue;
            }

            self.render_children_run(encoder, sink, &drawables[run_start..i], width, height);
            run_start = i + 1;
            self.end_sink_pass(encoder, sink);
            self.spend_pending_alpha_clear(encoder, sink, "Clear (barrier)");

            match drawable {
                DrawableInfo::Composite { .. } => {
                    if self.render_composite_drawable(
                        encoder,
                        stencil,
                        target,
                        drawable,
                        render_list,
                        composites,
                        snapshots.as_deref_mut(),
                        width,
                        height,
                    ) {
                        sink.mark_rendered();
                    }
                }
                DrawableInfo::Part {
                    mesh_id,
                    texture_id,
                    transform,
                    blend_mode,
                    opacity,
                    tint,
                    screen_tint,
                    ref mask_sources,
                    mask_threshold,
                    ..
                } => {
                    // `is_barrier` admits a part only when all three exist.
                    let (Some(shader_blend), Some(target_texture), Some(snap_pool)) = (
                        ShaderBlend::of(*blend_mode),
                        target.texture,
                        snapshots.as_deref_mut(),
                    ) else {
                        continue;
                    };
                    let part_uniform_offset =
                        self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);
                    // One rect bounds the slot render and the snapshot copy and
                    // blit; the shader returns dst outside the part's content.
                    let scissor = bounds.scissor();
                    // Both slots are released after the blit so the pools don't
                    // grow per part; encoder order makes reusing them safe.
                    let composite_mark = composites.mark();
                    let snapshot_mark = snap_pool.mark();
                    let scratch = composites.acquire(&self.shared, &self.device);
                    self.render_part_to_composite_slot(
                        encoder,
                        stencil,
                        &scratch,
                        *mesh_id,
                        *texture_id,
                        *transform,
                        mask_sources,
                        part_uniform_offset,
                        scissor,
                    );
                    let blit_uniform_offset =
                        self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5);
                    let snapshot = snap_pool.acquire(&self.shared, &self.device);
                    self.blit_composite_dst_in_shader(
                        encoder,
                        &scratch,
                        target.view,
                        target_texture,
                        snapshot,
                        shader_blend,
                        blit_uniform_offset,
                        width,
                        height,
                        scissor,
                    );
                    composites.rewind(composite_mark);
                    snap_pool.rewind(snapshot_mark);
                    sink.mark_rendered();
                }
            }
        }

        self.render_children_run(encoder, sink, &drawables[run_start..], width, height)
    }
}
