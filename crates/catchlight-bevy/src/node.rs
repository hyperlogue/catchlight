use bevy::prelude::*;
use bevy::render::{
    camera::ExtractedCamera,
    renderer::{RenderContext, ViewQuery},
    view::{ExtractedView, ViewTarget},
};

use crate::extract::{ExtractedCatchlightCamera, ExtractedPuppet};
use crate::prepare::{CatchlightRenderInner, CatchlightRenderState, ModelGpu, ModelKey};

pub(crate) fn catchlight_2d_pass(
    world: &World,
    view: ViewQuery<(
        Entity,
        &'static ExtractedCamera,
        &'static ExtractedView,
        &'static ViewTarget,
        &'static ExtractedCatchlightCamera,
    )>,
    mut render_context: RenderContext,
) {
    let (view_entity, camera, view, target, _marked) = view.into_inner();
    let state_res = world.resource::<CatchlightRenderState>();
    let mut guard = state_res.lock();

    // 2D cameras leave clip_from_world None; bevy derives it the same way.
    let view_proj = view
        .clip_from_world
        .unwrap_or_else(|| view.clip_from_view * view.world_from_view.to_matrix().inverse());

    // Per view, not per window: render-to-texture cameras have their own size.
    let (w, h) = camera
        .physical_target_size
        .map_or(guard.size, |s| (s.x.max(1), s.y.max(1)));
    if w == 0 || h == 0 {
        return;
    }

    let view_format = target.main_texture_format();

    let CatchlightRenderInner {
        gpus,
        puppets,
        formats,
        size,
        missing_format_warned,
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
    let targets = res.targets.entry(view_entity).or_default();
    *size = (w, h);

    let encoder = render_context.command_encoder();
    let color_view = target.main_texture_view();
    // bevy's default main-texture usages include COPY_SRC, which the
    // dst-in-shader blend modes need for framebuffer snapshots.
    let color_texture: &wgpu::Texture = target.main_texture();

    // Entity tie-break: `puppets` is a HashMap, so equal z would flicker.
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

    // One `render_into` per renderer per view: each call spends one of the
    // submit's camera slots. So z orders only within a model; models are
    // ordered by their backmost puppet.
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

        gpu.renderer.update_camera(view_proj);
        let ModelGpu {
            renderer,
            submission,
            ..
        } = gpu;
        if let Err(e) = renderer.render_into(
            submission,
            &lists,
            encoder,
            catchlight_wgpu::RenderTarget {
                view: color_view,
                texture: Some(color_texture),
                width: w,
                height: h,
            },
            targets,
            None,
        ) {
            tracing::error!("catchlight render error: {e}");
        }
    }

    // Safe per view: only queries added since the last resolve are recorded.
    for (key, gpu) in gpus.iter_mut() {
        if key.format == view_format {
            gpu.renderer.resolve_gpu_queries(encoder);
        }
    }
}
