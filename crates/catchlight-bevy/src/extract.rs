use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::{sync_world::RenderEntity, Extract};
use catchlight_core::Model;

use crate::asset::CatchlightModel;
use crate::components::{CatchlightCamera, CatchlightPuppet};
use crate::plugin::CatchlightSettings;
use crate::prepare::{CatchlightRenderState, RendererKey};

/// Render-world half of a `CatchlightPuppet`, retained across frames and
/// refilled in place by `extract_puppets`.
///
/// It carries what the render world needs and cannot get from a cache: the
/// model to prepare from, and the entity's placement in the scene. The
/// drawables and the deforms are not here — those go straight into the render
/// cache during extraction, while the main world is paused.
#[derive(Component)]
pub struct ExtractedPuppet {
    /// The model this entity animates. An `Arc`, so extraction copies a
    /// pointer rather than a rig.
    pub model: Arc<Model>,
    /// Which asset it came from. For a reader asking what a render entity
    /// draws — staleness is decided by the model's own identity, not by this,
    /// because an id outlives the value behind it.
    pub model_id: AssetId<CatchlightModel>,
    /// World-space z of the entity, used to order overlapping puppets
    /// deterministically (higher z renders in front).
    pub z: f32,
    /// Hierarchy visibility (`Visibility::Hidden` or a hidden ancestor
    /// clears this). The render node skips drawing when false; the cache is
    /// kept resident so unhiding doesn't re-upload the model.
    pub visible: bool,
}

// bevy 0.19's `SyncComponentPlugin` needs this `SyncComponent` impl: it
// registers `SyncToRenderWorld` as required on `CatchlightPuppet` (so each gets
// a paired `RenderEntity`), and on removal strips `Target` from the render
// entity — dropping `ExtractedPuppet` makes `prepare`'s GC release the cache
// instead of drawing the frozen render list forever.
impl bevy::render::sync_component::SyncComponent for CatchlightPuppet {
    type Target = ExtractedPuppet;
}

/// Extract system. Bevy's Extract schedule: main world frozen, we hand the
/// render world what this frame needs.
///
/// This is where a puppet reaches its render cache. Bevy's `extract` is
/// catchlight's [`RenderCache::refresh`](catchlight_wgpu::RenderCache::refresh):
/// the frame's deforms are uploaded and the drawables collected here, against
/// the live puppet, because **the main world is paused and nothing else may
/// read it**. Under bevy's pipelined rendering the next main-world frame runs
/// while the render world draws, and a tick overwrites the puppet's combined
/// deforms in place — so a later read would race. Doing it here also means the
/// render world never holds a puppet, so nothing needs a frozen per-frame copy
/// of the frame's deforms.
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
    // Copied in here so `prepare_puppets`, which has no main-world access,
    // builds caches the way the app asked for.
    inner.options = settings.prepare;
    let options = inner.options;
    let formats = inner.formats();

    for (puppet_component, transform, visibility, render_entity) in query.iter() {
        let Some(puppet) = puppet_component.puppet() else {
            // The model has not loaded yet, so there is nothing to draw and
            // nothing for `prepare_puppets` to build a cache from.
            continue;
        };
        let model_id = puppet_component.model().id();
        let Some(asset) = models.get(model_id) else {
            continue;
        };
        let visible = visibility.get();
        // A hidden puppet skips the upload and the collect: nothing draws it,
        // and the frame it is unhidden extracts `visible = true` first, so it
        // catches up right here.
        if visible {
            for format in &formats {
                let key = RendererKey {
                    entity: render_entity.id(),
                    format: *format,
                };
                if let Some(gpu) = inner.gpus.get_mut(&key) {
                    gpu.refresh(asset.shared(), puppet, options);
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

/// Marker on render-world cameras that should render puppets.
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
