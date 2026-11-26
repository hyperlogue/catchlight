use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::render::{
    renderer::{RenderContext, ViewQuery},
    view::{ExtractedView, ViewTarget},
};

use crate::extract::{ExtractedCatchlightCamera, ExtractedPuppet};
use crate::prepare::{CatchlightRenderState, ModelGpu, ModelKey};

pub(crate) fn catchlight_2d_pass(
    mut state: ResMut<CatchlightRenderState>,
    extracted: Query<&ExtractedPuppet>,
    view: ViewQuery<(
        Entity,
        &'static ExtractedView,
        &'static ViewTarget,
        Option<&'static RenderLayers>,
        &'static ExtractedCatchlightCamera,
    )>,
    mut render_context: RenderContext,
) {
    let (view_entity, view, target, layers, _marked) = view.into_inner();
    let layers = layers.unwrap_or_default();

    // 2D cameras leave clip_from_world None; bevy derives it the same way.
    let view_proj = view
        .clip_from_world
        .unwrap_or_else(|| view.clip_from_view * view.world_from_view.to_matrix().inverse());

    let view_format = target.main_texture_format();

    let CatchlightRenderState {
        gpus,
        puppets,
        formats,
        missing_format_warned,
        ..
    } = &mut *state;

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

    let encoder = render_context.command_encoder();
    // bevy's default main-texture usages include COPY_SRC, which the
    // dst-in-shader blend modes need for framebuffer snapshots. The main
    // texture is the camera target's size, and the viewport a rectangle of it.
    let viewport = view.viewport;
    let color_target = catchlight_wgpu::RenderTarget::copyable(target.main_texture_view())
        .with_viewport(viewport.x, viewport.y, viewport.z, viewport.w);

    // Entity tie-break: `puppets` is a HashMap, so equal z would flicker.
    let mut order: Vec<(ModelKey, f32, Entity)> = puppets
        .iter()
        .filter_map(|(key, puppet)| {
            if key.format != view_format || !puppet.collected {
                return None;
            }
            let ex = extracted.get(key.entity).ok()?;
            (ex.visible && layers.intersects(&ex.layers)).then_some((
                puppet.model,
                ex.z,
                key.entity,
            ))
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
        if let Err(e) =
            renderer.render_into(submission, &lists, encoder, color_target, targets, None)
        {
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
