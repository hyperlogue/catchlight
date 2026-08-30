use bevy::asset::AssetApp;
use bevy::core_pipeline::{Core2d, Core2dSystems};
use bevy::prelude::*;
use bevy::render::sync_component::SyncComponentPlugin;
use bevy::render::{ExtractSchedule, Render, RenderApp, RenderSystems};
use bevy::transform::TransformSystems;
use catchlight_wgpu::PrepareOptions;

use crate::asset::{CatchlightModel, CatchlightModelLoader};
use crate::components::{CatchlightCamera, CatchlightPuppet};
use crate::extract::{extract_cameras, extract_puppets};
use crate::node::catchlight_2d_pass;
use crate::prepare::{prepare_puppets, CatchlightRenderState};
use crate::update::update_puppets;

/// How a model reaches the GPU. A render-side budget, not a model property:
/// changing it re-prepares every cache on the next frame.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatchlightSettings {
    /// Texture downsampling and the decode memo. See
    /// [`PrepareOptions`](catchlight_wgpu::PrepareOptions).
    pub prepare: PrepareOptions,
}

/// Add this plugin to enable catchlight puppet rendering.
pub struct CatchlightPlugin;

impl Plugin for CatchlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<CatchlightModel>()
            .init_asset_loader::<CatchlightModelLoader>()
            .init_resource::<CatchlightSettings>();
        // After transform propagation: update_puppets bakes
        // `GlobalTransform.to_matrix()` into the puppet's root matrix, and
        // propagation runs in PostUpdate — running earlier would bake a
        // one-frame-stale root (disagreeing with the z extract reads).
        app.add_systems(
            PostUpdate,
            update_puppets.after(TransformSystems::Propagate),
        );
        // Registers `SyncToRenderWorld` as a required component and, when
        // `CatchlightPuppet` is removed from a live entity, despawns its
        // render-world counterpart — otherwise the retained
        // `ExtractedPuppet` would keep drawing the frozen render list.
        app.add_plugins(SyncComponentPlugin::<CatchlightPuppet>::default());
        // Catchlight pipelines are sample_count=1. Camera's required Msaa
        // defaults to Sample4, which makes the ViewTarget multisampled and
        // the puppet draws silently blank. #[require(Msaa::Off)] cannot
        // replace an already-present Msaa, so overwrite on add.
        app.add_observer(|add: On<Add, CatchlightCamera>, mut commands: Commands| {
            commands.entity(add.entity).insert(Msaa::Off);
        });

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // bevy 0.19 replaced the render graph with the Core2d render schedule:
        // the pass is a system run after the built-in main pass (so puppets
        // composite over sprites/ui) and before post-process.
        render_app
            .init_resource::<CatchlightRenderState>()
            .add_systems(ExtractSchedule, (extract_puppets, extract_cameras))
            .add_systems(Render, prepare_puppets.in_set(RenderSystems::Prepare))
            .add_systems(
                Core2d,
                catchlight_2d_pass
                    .after(Core2dSystems::MainPass)
                    .before(Core2dSystems::EarlyPostProcess),
            );
    }
}
