use super::*;

impl WgpuRenderer {
    /// Sampled-mask path only; the stencil path folds the mask write and blit
    /// into one pass (`blit_composite_with_masks`).
    fn blit_composite_masked(
        &self,
        ctx: &mut PassCtx<'_>,
        blit: Blit<'_>,
        target_view: &wgpu::TextureView,
        blend_mode: BlendMode,
    ) {
        // `FrameTargets::prepare` builds the target to match the pipelines.
        let Some(mask) = ctx.stencil.sampled.as_ref() else {
            return;
        };
        let mut render_pass = self.counted_begin_pass(
            ctx.encoder,
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
        render_pass.set_bind_group(0, &blit.source.blit_bind_group, &[]);
        render_pass.set_bind_group(
            1,
            &self.part_uniform_bind_group,
            &[blit.part_uniform_offset],
        );
        render_pass.set_bind_group(2, mask, &[]);
        if let Some(r) = blit.scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    fn blit_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        blit: Blit<'_>,
        target_view: &wgpu::TextureView,
        blend_mode: BlendMode,
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
        render_pass.set_bind_group(0, &blit.source.blit_bind_group, &[]);
        render_pass.set_bind_group(
            1,
            &self.part_uniform_bind_group,
            &[blit.part_uniform_offset],
        );
        if let Some(r) = blit.scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    /// Blends `blit` onto `ctx`'s target in the shader, against a snapshot of
    /// the target taken from the frame's pool. The caller rewinds the pool.
    fn blit_composite_dst_in_shader(
        &self,
        ctx: &mut PassCtx<'_>,
        blit: Blit<'_>,
        blend: ShaderBlend,
    ) {
        let (Some(target_texture), Some(snapshots)) =
            (ctx.target.texture, ctx.snapshots.as_deref_mut())
        else {
            return;
        };
        let snapshot = snapshots.acquire(&self.shared, &self.device);
        // The copy runs in encoder order (unlike write_buffer), so it sees every
        // prior pass. A scissored copy suffices: the snapshot is sampled 1:1 and
        // dst-in-shader modes return dst where src alpha is 0.
        let (copy_origin, copy_extent) = match blit.scissor {
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
                    width: ctx.target.width.max(1),
                    height: ctx.target.height.max(1),
                    depth_or_array_layers: 1,
                },
            ),
        };
        ctx.encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target_texture,
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
            ctx.encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Blit (dst-in-shader) Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: ctx.target.view,
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
        render_pass.set_bind_group(0, &blit.source.blit_bind_group, &[]);
        render_pass.set_bind_group(
            1,
            &self.part_uniform_bind_group,
            &[blit.part_uniform_offset],
        );
        render_pass.set_bind_group(2, &snapshot.bind_group, &[]);
        if let Some(r) = blit.scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    /// Gives the dst-in-shader blit a src texture independent of the
    /// framebuffer: `ctx` targets the slot.
    fn render_part_to_composite_slot(
        &mut self,
        ctx: &mut PassCtx<'_>,
        draw: PreparedPartDraw,
        mask_sources: &[crate::collect::MaskSourceData],
        scissor: Option<ScreenRect>,
    ) {
        let transparent = wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };

        if self.shared.has_stencil() {
            let mut state = StencilPassState::new(
                ctx.target.view,
                &ctx.stencil.view,
                Some(transparent),
                ctx.labels.draw,
                scissor,
            );
            let mut sink = PartSink::Stencil(&mut state);
            if mask_sources.is_empty() {
                self.flush_pending_parts(ctx, &mut sink, std::slice::from_ref(&draw));
            } else {
                self.flush_pending_masked(
                    ctx,
                    &mut sink,
                    std::slice::from_ref(&PreparedMaskedPartDraw { draw, mask_sources }),
                );
            }
            // If the draw filtered out, this emits the still-pending
            // transparent clear so the blit can't sample stale pool
            // contents.
            self.end_sink_pass(ctx, &mut sink);
            return;
        }

        let drew = if mask_sources.is_empty() {
            self.render_prepared_parts(ctx, std::slice::from_ref(&draw), Some(transparent), scissor)
        } else {
            self.render_masked_prepared_parts_to(
                ctx,
                std::slice::from_ref(&PreparedMaskedPartDraw { draw, mask_sources }),
                Some(transparent),
                scissor,
            )
        };

        if !drew {
            self.explicit_clear_pass(
                ctx.encoder,
                ctx.target.view,
                transparent,
                ctx.labels.fallback_clear,
            );
        }
    }

    fn blit_composite_with_masks(
        &mut self,
        ctx: &mut PassCtx<'_>,
        blit: Blit<'_>,
        target_view: &wgpu::TextureView,
        blend_mode: BlendMode,
        mask_sources: &[crate::collect::MaskSourceData],
    ) {
        if mask_sources.is_empty() {
            self.blit_composite(ctx.encoder, blit, target_view, blend_mode);
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
                ctx,
                "Composite Blit Mask Write Pass",
                mask_sources,
                base_offset,
            );
            self.blit_composite_masked(ctx, blit, target_view, blend_mode);
            return;
        }

        let mut render_pass = self.counted_begin_pass(
            ctx.encoder,
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
                    view: &ctx.stencil.view,
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
        if let Some(r) = blit.scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_mask_sources_stencil(&mut render_pass, mask_sources, base_offset, 1, &mut None);

        render_pass.set_pipeline(self.shared.masks.masked_blit().get(blend_mode));
        render_pass.set_stencil_reference(1);
        render_pass.set_bind_group(0, &blit.source.blit_bind_group, &[]);
        render_pass.set_bind_group(
            1,
            &self.part_uniform_bind_group,
            &[blit.part_uniform_offset],
        );
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    /// Serves root and nested composites identically. The caller must have
    /// flushed its pending runs and ended the sink's pass.
    fn render_composite_drawable(
        &mut self,
        ctx: &mut PassCtx<'_>,
        drawable: &crate::collect::DrawableInfo,
    ) -> bool {
        use crate::collect::DrawableInfo;

        let (width, height) = (ctx.target.width, ctx.target.height);
        let DrawableInfo::Composite {
            node_id,
            blend_mode,
            opacity,
            tint,
            screen_tint,
            mask_sources,
            mask_threshold,
            ..
        } = drawable
        else {
            return false;
        };
        let list = ctx.list;
        let children = list.composite_children(*node_id);
        if children.is_empty() {
            return false;
        }
        let _span =
            tracing::trace_span!("render_composite", masked = !mask_sources.is_empty()).entered();

        // Computed before the children render so the borrow doesn't collide.
        let bounds = self.drawable_screen_bounds(drawable, list, width, height);
        if bounds == ScreenBounds::Empty {
            return false;
        }
        let union = bounds.scissor();

        let part_uniform_offset =
            self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);

        // Rewinding after the blit lets siblings share slots; the pool grows to
        // the nesting depth and no further.
        let composite_mark = ctx.composites.mark();
        let snapshot_mark = ctx.snapshots.as_ref().map(|s| s.mark());
        let slot = ctx.composites.acquire(&self.shared, &self.device);
        // The slot's clear folds into its first drawing pass, or lands on its
        // own, so the blit never samples stale pool contents.
        self.render_to_target(
            &mut ctx.with_target(
                RenderTarget {
                    view: &slot.view,
                    texture: Some(&slot.texture),
                    width,
                    height,
                },
                &COMPOSITE_PASSES,
            ),
            Some(wgpu::Color::TRANSPARENT),
            children,
        );

        let blit = Blit {
            source: &slot,
            part_uniform_offset,
            scissor: union,
        };
        // Without a texture or pool, fall through to the Normal-OVER blit.
        match ShaderBlend::of(*blend_mode).filter(|_| ctx.shader_blends()) {
            Some(shader_blend) if mask_sources.is_empty() => {
                self.blit_composite_dst_in_shader(ctx, blit, shader_blend);
            }
            Some(shader_blend) => {
                let masked = ctx.composites.acquire(&self.shared, &self.device);
                self.explicit_clear_pass(
                    ctx.encoder,
                    &masked.view,
                    wgpu::Color::TRANSPARENT,
                    "Masked Composite Source Clear",
                );
                self.blit_composite_with_masks(
                    ctx,
                    blit,
                    &masked.view,
                    BlendMode::Normal,
                    mask_sources,
                );
                let part_uniform_offset =
                    self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5);
                self.blit_composite_dst_in_shader(
                    ctx,
                    Blit {
                        source: &masked,
                        part_uniform_offset,
                        scissor: union,
                    },
                    shader_blend,
                );
            }
            None => {
                let scissor = if blend_transparent_src_is_identity(*blend_mode) {
                    union
                } else {
                    None
                };
                let target_view = ctx.target.view;
                self.blit_composite_with_masks(
                    ctx,
                    Blit { scissor, ..blit },
                    target_view,
                    *blend_mode,
                    mask_sources,
                );
            }
        }

        ctx.composites.rewind(composite_mark);
        if let (Some(pool), Some(mark)) = (ctx.snapshots.as_deref_mut(), snapshot_mark) {
            pool.rewind(mark);
        }
        self.current_stats.drawn_composites += 1;
        true
    }

    /// Draws `drawables` into `ctx`'s target, spending `clear_color` exactly
    /// once even when nothing draws. Serves the frame target and every
    /// composite slot.
    pub(super) fn render_to_target(
        &mut self,
        ctx: &mut PassCtx<'_>,
        clear_color: Option<wgpu::Color>,
        drawables: &[crate::collect::DrawableInfo],
    ) {
        if self.shared.has_stencil() {
            let mut state = StencilPassState::new(
                ctx.target.view,
                &ctx.stencil.view,
                clear_color,
                ctx.labels.draw,
                None,
            );
            let mut sink = PartSink::Stencil(&mut state);
            self.render_drawables(ctx, &mut sink, drawables);
            self.end_sink_pass(ctx, &mut sink);
            return;
        }

        let mut has_rendered = false;
        let mut sink = PartSink::Alpha {
            has_rendered: &mut has_rendered,
            clear_color,
        };
        self.render_drawables(ctx, &mut sink, drawables);
        if !has_rendered && let Some(color) = clear_color {
            self.explicit_clear_pass(
                ctx.encoder,
                ctx.target.view,
                color,
                ctx.labels.fallback_clear,
            );
        }
    }

    /// Splits at drawables that read the target's own contents (composites
    /// and dst-in-shader parts); each records its own passes between part
    /// runs, which keeps z-order.
    fn render_drawables(
        &mut self,
        ctx: &mut PassCtx<'_>,
        sink: &mut PartSink<'_, '_>,
        drawables: &[crate::collect::DrawableInfo],
    ) {
        use crate::collect::DrawableInfo;

        let (width, height) = (ctx.target.width, ctx.target.height);
        // Without a snapshot pool and a copyable target, dst-in-shader parts
        // draw OVER inside the part runs.
        let shader_blends = ctx.shader_blends();
        let is_barrier = |d: &DrawableInfo| match d {
            DrawableInfo::Composite { .. } => true,
            DrawableInfo::Part { blend_mode, .. } => shader_blends && is_dst_in_shader(*blend_mode),
        };
        if !drawables.iter().any(is_barrier) {
            return self.render_children_run(ctx, sink, drawables);
        }

        let mut run_start = 0;
        for (i, drawable) in drawables.iter().enumerate() {
            if !is_barrier(drawable) {
                continue;
            }
            let bounds = self.drawable_screen_bounds(drawable, ctx.list, width, height);
            if bounds == ScreenBounds::Empty {
                continue;
            }

            self.render_children_run(ctx, sink, &drawables[run_start..i]);
            run_start = i + 1;
            self.end_sink_pass(ctx, sink);
            self.spend_pending_alpha_clear(ctx, sink, "Clear (barrier)");

            match drawable {
                DrawableInfo::Composite { .. } => {
                    if self.render_composite_drawable(ctx, drawable) {
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
                    mask_sources,
                    mask_threshold,
                    ..
                } => {
                    // `is_barrier` admits a part only with a snapshot pool
                    // and a copyable target.
                    let Some(shader_blend) = ShaderBlend::of(*blend_mode) else {
                        continue;
                    };
                    let part_uniform_offset =
                        self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);
                    // One rect bounds the slot render and the snapshot copy and
                    // blit; the shader returns dst outside the part's content.
                    let scissor = bounds.scissor();
                    // Both slots are released after the blit so the pools don't
                    // grow per part; encoder order makes reusing them safe.
                    let composite_mark = ctx.composites.mark();
                    let snapshot_mark = ctx.snapshots.as_ref().map(|s| s.mark());
                    let scratch = ctx.composites.acquire(&self.shared, &self.device);
                    self.render_part_to_composite_slot(
                        &mut ctx.with_target(
                            RenderTarget {
                                view: &scratch.view,
                                texture: Some(&scratch.texture),
                                width,
                                height,
                            },
                            &SLOT_PASSES,
                        ),
                        PreparedPartDraw {
                            draw: PartDraw {
                                mesh_id: *mesh_id,
                                albedo: *texture_id,
                                transform: *transform,
                                blend_mode: BlendMode::Normal,
                            },
                            part_uniform_offset,
                        },
                        mask_sources,
                        scissor,
                    );
                    let part_uniform_offset =
                        self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5);
                    self.blit_composite_dst_in_shader(
                        ctx,
                        Blit {
                            source: &scratch,
                            part_uniform_offset,
                            scissor,
                        },
                        shader_blend,
                    );
                    ctx.composites.rewind(composite_mark);
                    if let (Some(pool), Some(mark)) = (ctx.snapshots.as_deref_mut(), snapshot_mark)
                    {
                        pool.rewind(mark);
                    }
                    sink.mark_rendered();
                }
            }
        }

        self.render_children_run(ctx, sink, &drawables[run_start..]);
    }
}
