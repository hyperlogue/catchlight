use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::{sync_world::RenderEntity, Extract};
use catchlight_core::Model;

use crate::asset::CatchlightModel;
use crate::components::{CatchlightCamera, CatchlightPuppet};
use crate::plugin::CatchlightSettings;
use crate::prepare::{CatchlightRenderInner, CatchlightRenderState, PuppetKey};

/// Render-world half of a `CatchlightPuppet`, retained across frames.
#[derive(Component)]
pub struct ExtractedPuppet {
    pub model: Arc<Model>,
    /// Informational only: staleness is decided by `model`'s `Arc` identity.
    pub model_id: AssetId<CatchlightModel>,
    /// World-space z; higher renders in front.
    pub z: f32,
    pub visible: bool,
}

impl bevy::render::sync_component::SyncComponent for CatchlightPuppet {
    type Target = ExtractedPuppet;
}

/// Refreshes render caches here because only extract may read the live
/// puppet: under pipelined rendering the next tick overwrites its deforms.
// Invariant: the render-state Mutex is only poisoned on panic, treated as fatal.
#[allow(clippy::unwrap_used)]
pub(crate) fn extract_puppets(
    mut commands: Commands,
    mut existing: Query<&mut ExtractedPuppet>,
    state: Res<CatchlightRenderState>,
    settings: Extract<Res<CatchlightSettings>>,
    models: Extract<Res<Assets<CatchlightModel>>>,
    query: Extract<
        Query<(
            &CatchlightPuppet,
            &GlobalTransform,
            &InheritedVisibility,
            &RenderEntity,
        )>,
    >,
) {
    let mut inner = state.inner.lock().unwrap();
    inner.options = settings.prepare;
    let formats = inner.formats();

    for (puppet_component, transform, visibility, render_entity) in query.iter() {
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
                let CatchlightRenderInner { gpus, puppets, .. } = &mut *inner;
                let Some(state) = puppets.get_mut(&key) else {
                    continue;
                };
                let Some(model_gpu) = gpus.get_mut(&state.model) else {
                    continue;
                };
                // Swapped models; `prepare_puppets` rebinds it this frame.
                if !Arc::ptr_eq(&model_gpu.model, asset.shared()) {
                    state.collected = false;
                    continue;
                }
                match model_gpu.cache.refresh_puppet(
                    &mut model_gpu.renderer,
                    &model_gpu.model,
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
        match existing.get_mut(render_entity.id()) {
            Ok(mut extracted) => {
                if !Arc::ptr_eq(&extracted.model, asset.shared()) {
                    extracted.model = asset.shared().clone();
                }
                extracted.model_id = model_id;
                extracted.z = z;
                extracted.visible = visible;
            }
            Err(_) => {
                commands.entity(render_entity.id()).insert(ExtractedPuppet {
                    model: asset.shared().clone(),
                    model_id,
                    z,
                    visible,
                });
            }
        }
    }
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
