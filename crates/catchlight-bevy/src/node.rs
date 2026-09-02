use bevy::prelude::*;
use bevy::render::{
    camera::ExtractedCamera,
    renderer::{RenderContext, RenderDevice, ViewQuery},
    view::{ExtractedView, ViewTarget},
};

use crate::extract::{ExtractedCatchlightCamera, ExtractedPuppet};
use crate::prepare::{CatchlightRenderInner, CatchlightRenderState, ModelKey};

/// Render system that draws every visible `ExtractedPuppet` onto the camera's
/// color attachment. Runs in the Core2d render schedule after the built-in main
/// pass (so puppets composite over bevy sprites/ui) and before post-process.
/// `ExtractedCatchlightCamera` in the [`ViewQuery`] gates it: a current view
/// without that marker fails the query and the system is skipped for that view.
// Invariant: the render-state Mutex is only poisoned on panic, treated as fatal.
#[allow(clippy::unwrap_used)]
pub(crate) fn catchlight_2d_pass(
    world: &World,
    view: ViewQuery<(
        &'static ExtractedCamera,
        &'static ExtractedView,
        &'static ViewTarget,
        &'static ExtractedCatchlightCamera,
    )>,
    mut render_context: RenderContext,
) {
    let (camera, view, target, _marked) = view.into_inner();
    let device = world.resource::<RenderDevice>();
    let state_res = world.resource::<CatchlightRenderState>();
    let mut guard = state_res.inner.lock().unwrap();

    // ExtractedView.clip_from_world is Option: bevy leaves it None
    // unless a system (TAA, custom projection) populates it. 2D
    // cameras leave it None and bevy derives the matrix on the fly
    // from `clip_from_view * view_from_world`. Do the same here so
    // the puppet doesn't silently render with identity view-proj.
    let view_proj = view
        .clip_from_world
        .unwrap_or_else(|| view.clip_from_view * view.world_from_view.to_matrix().inverse());

    // Size the shared per-format resources to *this* view's target
    // rather than the first window — a render-to-texture or
    // second-window camera has its own dimensions. The stencil is
    // resized below; render_lists_ext sizes the composite / snapshot
    // pools itself from (w, h).
    let (w, h) = camera
        .physical_target_size
        .map(|s| (s.x.max(1), s.y.max(1)))
        .unwrap_or(guard.size);
    if w == 0 || h == 0 {
        return;
    }

    let view_format = target.main_texture_format();

    // Destructure for disjoint borrows on the fields.
    let CatchlightRenderInner {
        gpus,
        puppets,
        formats,
        size,
        missing_format_warned,
        camera_overflow_warned,
        ..
    } = &mut *guard;

    let Some(res) = formats.get_mut(&view_format) else {
        if !*missing_format_warned {
            *missing_format_warned = true;
            tracing::warn!(
                "catchlight: no pipelines built for view format {:?}; skipping this camera",
                view_format
            );
        }
        return;
    };
    res.stencil
        .ensure_size_for_pipelines(&res.pipelines, device.wgpu_device(), w, h);
    *size = (w, h);

    let encoder = render_context.command_encoder();
    let color_view = target.main_texture_view();
    // bevy creates ViewTarget main textures with
    // `CameraMainTextureUsages::default()` = RENDER_ATTACHMENT |
    // TEXTURE_BINDING | COPY_SRC, so `render_lists_ext` can snapshot
    // the framebuffer for the dst-in-shader blend modes
    // (Overlay / ColorBurn / LinearBurn).
    let color_texture: &wgpu::Texture = target.main_texture();

    // Deterministic draw order: ascending entity z, so a higher-z
    // puppet composites in front (bevy 2D convention). `puppets` is
    // a HashMap, so equal-z puppets need the Entity tie-break or their
    // order would follow hash iteration order and flicker across
    // runs. Hidden puppets are skipped, and so is one whose list was not
    // collected at extract — a cache prepared this frame has not been
    // collected into yet, so its puppets draw from the next frame on rather
    // than drawing a list whose slots name another cache's resources.
    let mut order: Vec<(ModelKey, f32, Entity)> = puppets
        .iter()
        .filter_map(|(key, puppet)| {
            if key.format != view_format || !puppet.collected {
                return None;
            }
            let ex = world.get::<ExtractedPuppet>(key.entity)?;
            ex.visible.then_some((puppet.model, ex.z, key.entity))
        })
        .collect();
    order.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.2.cmp(&b.2)));

    // One frame call per renderer, and a renderer holds one model. Every
    // puppet of a model therefore draws in one `render_lists_ext`: the
    // frame's instance and part-uniform cursors are monotonic across the
    // whole call, and a second call on the same renderer inside this submit
    // would reset them and rewrite offsets the first call's draws read.
    //
    // So z decides the order **within** a model, and models are ordered by
    // their backmost puppet. Two models whose puppets interleave in z
    // therefore do not interleave on screen; cross-model z-interleaving
    // needs one renderer over both models' slots and is not what this is.
    let mut groups: Vec<(ModelKey, Vec<Entity>)> = Vec::new();
    for (model, _z, entity) in &order {
        match groups.iter_mut().find(|(key, _)| key == model) {
            Some((_, members)) => members.push(*entity),
            None => groups.push((*model, vec![*entity])),
        }
    }

    let mut lists: Vec<&catchlight_wgpu::RenderList> = Vec::new();
    for (model_key, members) in &groups {
        let Some(gpu) = gpus.get_mut(model_key) else {
            continue;
        };
        lists.clear();
        lists.extend(members.iter().filter_map(|entity| {
            puppets
                .get(&crate::prepare::PuppetKey {
                    entity: *entity,
                    format: view_format,
                })
                .map(|puppet| &puppet.render_list)
        }));
        if lists.is_empty() {
            continue;
        }

        // Per-renderer camera write: each view records its own
        // dynamic offset, so two marked cameras in one submit don't
        // alias on a shared offset-0 write.
        gpu.renderer.update_camera(view_proj);
        if let Err(e) = gpu.renderer.render_lists_ext(
            &lists,
            encoder,
            color_view,
            &res.stencil,
            &mut res.composites,
            Some(color_texture),
            Some(&mut res.snapshots),
            w,
            h,
            None,
        ) {
            if matches!(
                &e,
                catchlight_wgpu::RendererError::TooManyCameraViews { .. }
            ) {
                if !*camera_overflow_warned {
                    *camera_overflow_warned = true;
                    tracing::warn!("catchlight: {e}; skipping excess camera views");
                }
            } else {
                tracing::error!("catchlight render_list error: {e}");
            }
        }
    }

    // Resolve wgpu-profiler timestamp queries into the same encoder
    // bevy will submit. Without this, `process_gpu_frame` always
    // returns None and GPU timing never lands in the overlay. Calling
    // it per view is safe — resolve_queries only records the queries
    // added since the last call, so a second CatchlightCamera in the
    // same frame resolves its own work. The matching once-per-frame
    // end_gpu_frame runs in `prepare_puppets`, after this encoder has
    // been submitted.
    for (key, gpu) in gpus.iter_mut() {
        if key.format == view_format {
            gpu.renderer.resolve_gpu_queries(encoder);
        }
    }
}
