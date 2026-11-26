use std::sync::Arc;

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::render::{Extract, sync_world::RenderEntity};
use catchlight_core::{Model, Puppet};

use crate::asset::CatchlightModel;
use crate::components::{CatchlightCamera, CatchlightPuppet};
use crate::plugin::CatchlightSettings;
use crate::prepare::{CatchlightRenderState, ModelGpu, PuppetKey};

/// Render-world half of a `CatchlightPuppet`, retained across frames. `pub`
/// only because `SyncComponent::Target` names it; the module is private.
#[derive(Component)]
pub struct ExtractedPuppet {
    pub(crate) model: Arc<Model>,
    pub(crate) model_id: AssetId<CatchlightModel>,
    /// World-space z; higher renders in front.
    pub(crate) z: f32,
    pub(crate) visible: bool,
    /// A camera draws the puppet only when their layers intersect.
    pub(crate) layers: RenderLayers,
}

impl bevy::render::sync_component::SyncComponent for CatchlightPuppet {
    type Target = ExtractedPuppet;
}

type MainWorldPuppet = (
    &'static CatchlightPuppet,
    &'static GlobalTransform,
    &'static InheritedVisibility,
    Option<&'static RenderLayers>,
    &'static RenderEntity,
);

/// Refreshes render caches here because only extract may read the live
/// puppet: under pipelined rendering the next tick overwrites its deforms.
pub(crate) fn extract_puppets(
    mut commands: Commands,
    mut existing: Query<&mut ExtractedPuppet>,
    mut state: ResMut<CatchlightRenderState>,
    settings: Extract<Res<CatchlightSettings>>,
    models: Extract<Res<Assets<CatchlightModel>>>,
    query: Extract<Query<MainWorldPuppet>>,
) {
    let state = &mut *state;
    state.options = settings.prepare;
    let formats = state.formats();
    reprepare_replaced(state, &models);

    for (puppet_component, transform, visibility, layers, render_entity) in query.iter() {
        let Some(puppet) = puppet_component.puppet() else {
            continue;
        };
        let model_id = puppet_component.model().id();
        let Some(asset) = models.get(model_id) else {
            continue;
        };
        let visible = visibility.get();
        if visible {
            for format in &formats {
                let key = PuppetKey {
                    entity: render_entity.id(),
                    format: *format,
                };
                let CatchlightRenderState { gpus, puppets, .. } = &mut *state;
                let Some(state) = puppets.get_mut(&key) else {
                    continue;
                };
                let Some(model_gpu) = gpus.get_mut(&state.model) else {
                    continue;
                };
                let ModelGpu {
                    renderer,
                    cache,
                    model,
                    ..
                } = model_gpu;
                let Some(model) = refresh_model(model, asset.shared(), puppet) else {
                    state.collected = false;
                    continue;
                };
                match cache.refresh_puppet(
                    renderer,
                    model,
                    puppet,
                    state.deform_set,
                    &mut state.render_list,
                ) {
                    Ok(()) => state.collected = true,
                    Err(error) => {
                        tracing::error!("catchlight: refreshing the render cache failed: {error}");
                        state.collected = false;
                    }
                }
            }
        }

        let z = transform.translation().z;
        let layers = layers.cloned().unwrap_or_default();
        match existing.get_mut(render_entity.id()) {
            Ok(mut extracted) => {
                if !Arc::ptr_eq(&extracted.model, asset.shared()) {
                    extracted.model = asset.shared().clone();
                }
                extracted.model_id = model_id;
                extracted.z = z;
                extracted.visible = visible;
                extracted.layers = layers;
            }
            Err(_) => {
                commands.entity(render_entity.id()).insert(ExtractedPuppet {
                    model: asset.shared().clone(),
                    model_id,
                    z,
                    visible,
                    layers,
                });
            }
        }
    }
}

/// Re-prepares, on the same renderer and deform sets, each cache whose asset
/// now holds a different model, so a hot reload draws in the frame it lands.
fn reprepare_replaced(state: &mut CatchlightRenderState, models: &Assets<CatchlightModel>) {
    let CatchlightRenderState {
        gpus,
        puppets,
        options,
        failures,
        ..
    } = state;
    for (key, gpu) in gpus.iter_mut() {
        let Some(asset) = models.get(key.model) else {
            continue;
        };
        let model = asset.model();
        if gpu.model.identity() == model.identity() || failures.failed(*key, model, *options) {
            continue;
        }
        if let Some(cache) = failures.prepare(*key, &mut gpu.renderer, model, *options) {
            gpu.cache = cache;
            gpu.model = asset.shared().clone();
            gpu.options = *options;
        }
        for puppet in puppets.values_mut() {
            if puppet.model == *key {
                puppet.collected = false;
            }
        }
    }
}

/// The model state to refresh `puppet` against, adopting `current` when the
/// puppet has ticked against it. `None` when the cache must be re-prepared
/// first (the asset was replaced and preparing it failed) or the puppet
/// matches neither state.
fn refresh_model<'a>(
    cached: &'a mut Arc<Model>,
    current: &'a Arc<Model>,
    puppet: &Puppet,
) -> Option<&'a Model> {
    let ticked_against = |model: &Model| {
        puppet.model_identity() == model.identity()
            && puppet.baked_generation() == model.generation()
    };
    if cached.identity() != current.identity() {
        return None;
    }
    if ticked_against(current) {
        if !Arc::ptr_eq(cached, current) {
            *cached = current.clone();
        }
        return Some(current);
    }
    // Edited after this frame's tick: draw the state the puppet saw.
    ticked_against(cached).then_some(&**cached)
}

#[derive(Component, Default, Clone, Copy)]
pub(crate) struct ExtractedCatchlightCamera;

pub(crate) fn extract_cameras(
    mut commands: Commands,
    existing: Query<Entity, With<ExtractedCatchlightCamera>>,
    query: Extract<Query<&RenderEntity, With<CatchlightCamera>>>,
) {
    for entity in existing.iter() {
        if !query
            .iter()
            .any(|render_entity| render_entity.id() == entity)
        {
            commands
                .entity(entity)
                .remove::<ExtractedCatchlightCamera>();
        }
    }
    for render_entity in query.iter() {
        if existing.get(render_entity.id()).is_err() {
            commands
                .entity(render_entity.id())
                .insert(ExtractedCatchlightCamera);
        }
    }
}
