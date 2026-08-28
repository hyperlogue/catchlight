use std::sync::{Arc, RwLock};

use bevy::prelude::*;
use catchlight_core::{GlobalTransforms, Puppet};
use catchlight_wgpu::{DrawableCollector, RenderList};

/// Wraps a `catchlight_core::Puppet` as a Bevy component.
///
/// The puppet lives behind `Arc<RwLock<>>` so:
/// - Main-world `update_puppets` mutates it (and `state`) via the write lock.
/// - Extract reads it once (main world paused) to snapshot the per-frame
///   `RenderList` + combined deforms into an `ExtractedPuppet`; the render
///   world consumes only that snapshot, never the live lock. This lets the
///   next frame's deform compute overlap rendering under pipelined rendering.
///
/// The per-frame `RenderList` lives in `state` so `update_puppets` writes
/// it and `extract_puppets` reads it.
///
/// `SyncToRenderWorld` is required via `SyncComponentPlugin` in
/// `CatchlightPlugin` (not `#[require]` — registering the same direct
/// requirement twice panics), so `extract_puppets` can reach the paired
/// `RenderEntity` and removal of this component tears down the
/// render-world entity.
///
/// `Visibility` is required so the entity gets an `InheritedVisibility`
/// (computed by bevy's visibility propagation); `extract_puppets` reads
/// it to honour `Visibility::Hidden` and hidden ancestors.
///
/// Deliberately not `Clone`: cloning onto a second entity would alias the
/// `Arc`s, double-ticking one puppet and racing its root transform. Spawn
/// each entity with its own `CatchlightPuppet::new`.
#[derive(Component)]
#[require(Visibility)]
pub struct CatchlightPuppet {
    pub puppet: Arc<RwLock<Puppet>>,
    pub state: Arc<RwLock<PuppetDynamicState>>,
}

#[derive(Default)]
pub struct PuppetDynamicState {
    pub transforms: GlobalTransforms,
    pub render_list: Option<RenderList>,
    pub drawable_collector: DrawableCollector,
}

impl CatchlightPuppet {
    pub fn new(puppet: Puppet) -> Self {
        Self {
            puppet: Arc::new(RwLock::new(puppet)),
            state: Arc::new(RwLock::new(PuppetDynamicState::default())),
        }
    }
}

/// Marker component on cameras that should render puppets.
#[derive(Component, Default, Clone, Copy)]
pub struct CatchlightCamera;
