use bevy::prelude::*;
use bevy::render::{sync_world::RenderEntity, Extract};

use crate::components::{CatchlightCamera, CatchlightPuppet};
use std::sync::{Arc, RwLock};

use catchlight_core::Puppet;
use catchlight_wgpu::{DeformSnapshot, RenderList};

/// Render-world snapshot of a `CatchlightPuppet`, retained across frames
/// and refilled in place by `extract_puppets` (see
/// `DeformSnapshot::refill_from_puppet`).
///
/// Everything the render world needs for *this* frame is frozen here at
/// extract time (while the main world is paused): the drawable list and
/// the combined deforms. The render world therefore never reads the live
/// puppet during the frame, so the next main-world frame can recompute
/// deforms — which overwrite the live `DeformStack::combined` buffers in
/// place — concurrently with rendering (bevy pipelined rendering). The
/// `Arc<RwLock<Puppet>>` is retained only for the one-time `upload_puppet`
/// (static meshes + textures, read once when the renderer is created).
#[derive(Component)]
pub struct ExtractedPuppet {
    pub puppet: Arc<RwLock<Puppet>>,
    pub render_list: Option<RenderList>,
    // On wasm prepare reads the live puppet instead (no pipelined
    // rendering), so the snapshot is only filled and read on native.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub deforms: DeformSnapshot,
    /// World-space z of the entity, used to order overlapping puppets
    /// deterministically (higher z renders in front).
    pub z: f32,
    /// Hierarchy visibility (`Visibility::Hidden` or a hidden ancestor
    /// clears this). The render node skips drawing when false; the
    /// renderer is kept resident so unhiding doesn't re-upload the
    /// puppet.
    pub visible: bool,
}

// bevy 0.19's `SyncComponentPlugin` needs this `SyncComponent` impl: it
// registers `SyncToRenderWorld` as required on `CatchlightPuppet` (so each gets
// a paired `RenderEntity`), and on removal strips `Target` from the render
// entity — dropping `ExtractedPuppet` makes `prepare`'s GC release the renderer
// instead of drawing the frozen render list forever.
impl bevy::render::sync_component::SyncComponent for CatchlightPuppet {
    type Target = ExtractedPuppet;
}

/// Extract system. Bevy's Extract schedule: main-world frozen, we copy
/// what the render world needs into the paired `RenderEntity`.
///
/// The render world *retains* the `ExtractedPuppet` across frames, so we
/// refill it in place (reusing its `DeformSnapshot` Vec allocations) rather
/// than inserting a fresh one each frame — the snapshot is the only
/// per-frame allocation otherwise, and it's ~one Vec per active deform node.
// Invariant: per-puppet RwLocks are only poisoned on panic, treated as fatal.
#[allow(clippy::unwrap_used)]
pub(crate) fn extract_puppets(
    mut commands: Commands,
    mut existing: Query<&mut ExtractedPuppet>,
    query: Extract<
        Query<(
            &CatchlightPuppet,
            &GlobalTransform,
            &InheritedVisibility,
            &RenderEntity,
        )>,
    >,
) {
    for (cp, transform, visibility, render_entity) in query.iter() {
        // Safe to read without racing update_puppets: extract runs with the
        // main world paused. Snapshot the per-frame state so the render
        // world owns it and the next main-world frame can mutate the puppet
        // concurrently.
        let z = transform.translation().z;
        let visible = visibility.get();
        match existing.get_mut(render_entity.id()) {
            Ok(mut ex) => {
                // Native: refill the deform snapshot for the render thread.
                // Wasm: prepare reads the live puppet (no pipelining), so
                // skip the snapshot copy — `deforms` stays empty/unused.
                // Hidden puppets skip the copy too: nothing draws them,
                // and the unhide frame extracts visible=true first, so
                // the refill (generation-gated) catches up right here.
                #[cfg(not(target_arch = "wasm32"))]
                if visible {
                    ex.deforms.refill_from_puppet(&cp.puppet.read().unwrap());
                }
                ex.render_list
                    .clone_from(&cp.state.read().unwrap().render_list);
                ex.z = z;
                ex.visible = visible;
            }
            Err(_) => {
                let render_list = cp.state.read().unwrap().render_list.clone();
                #[cfg(not(target_arch = "wasm32"))]
                let deforms = DeformSnapshot::from_puppet(&cp.puppet.read().unwrap());
                #[cfg(target_arch = "wasm32")]
                let deforms = DeformSnapshot::default();
                commands.entity(render_entity.id()).insert(ExtractedPuppet {
                    puppet: cp.puppet.clone(),
                    render_list,
                    deforms,
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
