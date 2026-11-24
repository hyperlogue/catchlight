use super::*;

/// Views past the last are refused rather than wrapping onto a slot an
/// already-recorded command buffer still reads.
#[doc(hidden)]
pub const CAMERA_RING_SLOTS: u32 = 64;

/// `slot` must be below [`CAMERA_RING_SLOTS`]; the ring does not wrap.
pub(super) fn camera_slot_offset(slot: u32, stride: u64) -> u64 {
    debug_assert!(
        slot < CAMERA_RING_SLOTS,
        "camera slot {slot} is past the ring"
    );
    u64::from(slot) * stride
}

/// One host-owned submit's worth of views for [`WgpuRenderer::render_into`]:
/// each view takes its own camera slot and continues the frame buffers after
/// the previous one. Make a fresh one per submit: one kept across submits
/// runs the ring out, and two over one submit alias (hence not `Clone`).
#[derive(Debug)]
pub struct Submission {
    /// A token spent on another renderer is refused.
    pub(super) renderer: u64,
    pub(super) views: u32,
}

impl Submission {
    #[doc(hidden)]
    pub fn views_issued(&self) -> u32 {
        self.views
    }
}

/// A frame the renderer owns end to end: it made the encoder, and it will
/// make the submit.
///
/// Recording is the one thing a `Frame` does, and it does it once:
/// [`Self::render`] / [`Self::render_ext`] take `self` by value and hand back
/// a [`Recorded`]. A frame that errs is consumed with nothing submitted.
#[must_use = "a frame draws nothing until render() records it"]
pub struct Frame<'r> {
    pub(super) renderer: &'r mut WgpuRenderer,
    pub(super) encoder: wgpu::CommandEncoder,
}

impl<'r> Frame<'r> {
    /// See [`Self::render_ext`] for the dst-in-shader blend modes.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        self,
        lists: &[&crate::collect::RenderList],
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<Recorded<'r>> {
        self.render_ext(
            lists,
            view,
            stencil,
            composites,
            None,
            None,
            width,
            height,
            clear_color,
        )
    }

    /// Without the target texture and snapshot pool, `Overlay`, `ColorBurn`
    /// and `LinearBurn` fall back to Normal-OVER.
    #[allow(clippy::too_many_arguments)]
    pub fn render_ext(
        mut self,
        lists: &[&crate::collect::RenderList],
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<Recorded<'r>> {
        let stats = self.renderer.record_frame(
            0,
            lists,
            &mut self.encoder,
            view,
            stencil,
            composites,
            target_color_texture,
            snapshots,
            width,
            height,
            clear_color,
        );
        Ok(Recorded {
            renderer: self.renderer,
            encoder: Some(self.encoder),
            stats,
        })
    }
}

/// A recorded frame, waiting for its submit. Add caller work through
/// [`Self::encoder`]; a second render call would reset the spent cursors.
#[must_use = "a recorded frame reaches the queue only through submit()"]
pub struct Recorded<'r> {
    pub(super) renderer: &'r mut WgpuRenderer,
    pub(super) encoder: Option<wgpu::CommandEncoder>,
    pub(super) stats: RenderStats,
}

impl Recorded<'_> {
    pub fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        // A live `Recorded` always has one; the fallback only satisfies the type.
        self.encoder.get_or_insert_with(|| {
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("catchlight frame"),
                })
        })
    }

    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    /// Resolves timer queries into this encoder before submitting, then closes
    /// the profiler frame.
    pub fn submit(mut self) -> wgpu::SubmissionIndex {
        let mut encoder = self.encoder.take();
        if let Some(encoder) = encoder.as_mut() {
            self.renderer.resolve_gpu_queries(encoder);
        }
        let index = self
            .renderer
            .queue
            .submit(encoder.map(wgpu::CommandEncoder::finish));
        self.renderer.end_gpu_frame();
        index
    }
}

impl Drop for Recorded<'_> {
    fn drop(&mut self) {
        if self.encoder.is_some() {
            // Once per process: a caller that drops one frame drops every frame.
            static DROPPED_WARNED: std::sync::Once = std::sync::Once::new();
            DROPPED_WARNED.call_once(|| {
                tracing::warn!(
                    "a recorded catchlight frame was dropped without submit(); \
                     nothing it drew reached the queue",
                );
            });
        }
    }
}

impl WgpuRenderer {
    /// One frame is one submit, so it keeps camera slot 0 and monotonic
    /// cursors. Hosts that own the submit use [`Self::render_into`].
    pub fn frame(&mut self) -> Frame<'_> {
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("catchlight frame"),
            });
        Frame {
            renderer: self,
            encoder,
        }
    }

    /// Make a fresh one per submit; see [`Submission`].
    pub fn submission(&self) -> Submission {
        Submission {
            renderer: self.id,
            views: 0,
        }
    }

    /// Record into a caller-owned encoder (catchlight-bevy's render graph).
    /// Errors without recording when `submission` came from another renderer or
    /// has already issued [`CAMERA_RING_SLOTS`] views.
    #[allow(clippy::too_many_arguments)]
    pub fn render_into(
        &mut self,
        submission: &mut Submission,
        lists: &[&crate::collect::RenderList],
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<RenderStats> {
        if submission.renderer != self.id {
            return Err(RendererError::ForeignSubmission);
        }
        if submission.views >= CAMERA_RING_SLOTS {
            return Err(RendererError::CameraViewsExhausted {
                limit: CAMERA_RING_SLOTS,
            });
        }
        let slot = submission.views;
        submission.views += 1;
        Ok(self.record_frame(
            slot,
            lists,
            encoder,
            view,
            stencil,
            composites,
            target_color_texture,
            snapshots,
            width,
            height,
            clear_color,
        ))
    }

    /// Lists record back to back under one camera slot. A later view of the
    /// same submit takes the next `camera_slot` and continues the frame
    /// buffers. Only the first list may consume `clear_color`. Without `target_color_texture` (which
    /// must carry `COPY_SRC`) and `snapshots`, dst-in-shader modes fall back to
    /// Normal-OVER.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_frame(
        &mut self,
        camera_slot: u32,
        lists: &[&crate::collect::RenderList],
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        mut snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RenderStats {
        use crate::collect::DrawableInfo;

        let span = tracing::trace_span!(
            "render_lists",
            puppets = lists.len(),
            drawn_parts = tracing::field::Empty
        );
        let _entered = span.enter();

        // Must precede the camera write, this frame's first queue write.
        self.frame_stats = FrameStats::default();
        self.frame_sizing_closed = false;
        self.write_camera(camera_slot);
        self.current_stats = RenderStats::default();
        self.frame_render_passes.store(0, Ordering::Relaxed);
        self.frame_draw_calls.store(0, Ordering::Relaxed);

        let gpu_query = self
            .gpu_profiler
            .as_ref()
            .map(|p| p.begin_query("render_list", encoder));

        // Keeps frames without shader-math modes on the plain render path.
        let drawables = || {
            lists.iter().flat_map(|list| {
                list.root_drawables
                    .iter()
                    .chain(list.composite_children.values().flatten())
            })
        };
        let dst_in_shader_present = drawables().any(|d| is_dst_in_shader(d.blend_mode()));
        let dst_in_shader_active =
            dst_in_shader_present && target_color_texture.is_some() && snapshots.is_some();
        if dst_in_shader_present && !dst_in_shader_active {
            // Once per process: the plain render path hits this every frame.
            static FALLBACK_WARNED: std::sync::Once = std::sync::Once::new();
            FALLBACK_WARNED.call_once(|| {
                tracing::warn!(
                    "dst-in-shader blend mode present but the caller supplied no \
                     target_color_texture + snapshot pool; falling back to the \
                     Normal-OVER approximation",
                );
            });
        }

        // Sized over every list; mid-frame growth would strand earlier passes.
        let total_instances: usize = lists.iter().map(|l| l.total_instance_count()).sum();
        // Later views of one submit keep the bytes of the earlier ones.
        let continues = camera_slot > 0;
        self.begin_frame_instances(total_instances, continues);
        // One slot per drawable, composite child and mask source, plus one per
        // dst-in-shader part and masked dst-in-shader composite.
        let dst_in_shader_extra = if dst_in_shader_active {
            drawables()
                .filter(|d| match d {
                    DrawableInfo::Part { .. } => is_dst_in_shader(d.blend_mode()),
                    DrawableInfo::Composite { mask_sources, .. } => {
                        is_dst_in_shader(d.blend_mode()) && !mask_sources.is_empty()
                    }
                })
                .count()
        } else {
            0
        };
        let uniform_slot_count = lists
            .iter()
            .map(|list| {
                let child_parts: usize = list.composite_children.values().map(|v| v.len()).sum();
                list.root_drawables.len()
                    + child_parts
                    + list.total_mask_source_count()
                    + list
                        .composite_mask_sources
                        .values()
                        .map(|source| source.parts.len())
                        .sum::<usize>()
            })
            .sum::<usize>()
            + dst_in_shader_extra;
        self.begin_frame_uniforms(uniform_slot_count as u32, continues);

        for (i, list) in lists.iter().enumerate() {
            self.deform_recording_set = list.deform_set;
            self.deform_recording_base = self.deform_base(list.deform_set);
            self.record_list(
                list,
                encoder,
                view,
                stencil,
                composites,
                target_color_texture,
                snapshots.as_deref_mut(),
                width,
                height,
                // The clear belongs to the frame, not to a puppet: a later
                // list clearing would wipe the ones already recorded.
                if i == 0 { clear_color } else { None },
                dst_in_shader_active,
            );
            // Each list resets the pool, so the frame's peak is the deepest list's.
            self.current_stats.composite_slots_used = self
                .current_stats
                .composite_slots_used
                .max(composites.stats().0 as u32);
        }
        self.deform_recording_set = DeformSet::FIRST;
        self.deform_recording_base = 0;

        if let (Some(profiler), Some(query)) = (self.gpu_profiler.as_ref(), gpu_query) {
            profiler.end_query(encoder, query);
        }
        self.flush_instance_writes();
        self.flush_part_uniform_writes();
        self.finalize_frame_stats();
        self.pending_deform_bytes = 0;
        span.record("drawn_parts", self.current_stats.drawn_parts);
        self.frame_sizing_closed = false;
        self.current_stats
    }

    /// Frame-level state belongs to [`Self::record_frame`], so this may run
    /// several times per frame.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_list(
        &mut self,
        render_list: &crate::collect::RenderList,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
        dst_in_shader_active: bool,
    ) {
        use crate::collect::DrawableInfo;

        let span = tracing::trace_span!("render_list", drawn_parts = tracing::field::Empty);
        let _entered = span.enter();

        composites.ensure_size(width, height);
        composites.reset();
        self.composite_bounds.clear();
        let mut snapshots = if dst_in_shader_active {
            snapshots
        } else {
            None
        };
        if let Some(s) = snapshots.as_mut() {
            s.ensure_size(width, height);
            s.reset();
        }
        self.prepare_composite_mask_textures(encoder, render_list, composites, width, height);

        // Root-composite flattening: OVER is associative, so an identity-blit
        // root composite whose children all blend OVER draws them straight to the
        // main view. Nested composites, non-OVER children, and dst-in-shader
        // children on the snapshot path read the destination and block it.
        let flatten_children: Option<&[DrawableInfo]> = (|| {
            if render_list.root_drawables.len() != 1 {
                return None;
            }
            let DrawableInfo::Composite {
                node_id,
                blend_mode,
                opacity,
                tint,
                screen_tint,
                mask_sources,
                ..
            } = &render_list.root_drawables[0]
            else {
                return None;
            };
            if *blend_mode != BlendMode::Normal
                || *opacity != 1.0
                || *tint != glam::Vec3::ONE
                || *screen_tint != glam::Vec3::ZERO
                || !mask_sources.is_empty()
            {
                return None;
            }
            let children = render_list.composite_children.get(node_id)?;
            if children.is_empty() {
                return None;
            }
            for c in children {
                let blocks_flattening = match c {
                    DrawableInfo::Composite { .. } => true,
                    DrawableInfo::Part { blend_mode, .. } => {
                        !renders_as_over(*blend_mode)
                            || (dst_in_shader_active && is_dst_in_shader(*blend_mode))
                    }
                };
                if blocks_flattening
                    && self.drawable_screen_bounds(c, render_list, width, height)
                        != ScreenBounds::Empty
                {
                    return None;
                }
            }
            Some(children.as_slice())
        })();

        self.render_to_target(
            encoder,
            stencil,
            CompositeTarget {
                view,
                texture: target_color_texture,
            },
            clear_color,
            &ROOT_PASSES,
            flatten_children.unwrap_or(&render_list.root_drawables),
            render_list,
            composites,
            snapshots,
            width,
            height,
        );
        span.record("drawn_parts", self.current_stats.drawn_parts);
    }

    /// Must run while the encoder is open and before it is submitted; pair
    /// with `end_gpu_frame` after. [`Recorded::submit`] does both itself.
    pub fn resolve_gpu_queries(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(profiler) = self.gpu_profiler.as_mut() {
            profiler.resolve_queries(encoder);
        }
    }

    /// Must run after the submit carrying `resolve_gpu_queries`: it maps the
    /// read buffer, and a submit still writing a mapped buffer fails validation.
    pub fn end_gpu_frame(&mut self) {
        if let Some(profiler) = self.gpu_profiler.as_mut() {
            if let Err(e) = profiler.end_frame() {
                tracing::warn!("wgpu_profiler::end_frame: {e}");
            }
        }
    }

    /// `None` when timestamp queries are unsupported or no frame completed yet.
    pub fn process_gpu_frame(&mut self) -> Option<Vec<wgpu_profiler::GpuTimerQueryResult>> {
        let profiler = self.gpu_profiler.as_mut()?;
        let period = self.queue.get_timestamp_period();
        profiler.process_finished_frame(period)
    }
}
