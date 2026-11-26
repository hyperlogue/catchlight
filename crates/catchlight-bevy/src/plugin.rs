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
use crate::prepare::{CatchlightRenderState, prepare_puppets};
use crate::update::update_puppets;

/// Changing it re-prepares every cache on the next frame.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatchlightSettings {
    pub prepare: PrepareOptions,
}

/// Order systems that set params or poses `.before(CatchlightSystems::Tick)`
/// in `PostUpdate` so this frame's tick sees them.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CatchlightSystems {
    Tick,
}

pub struct CatchlightPlugin;

impl Plugin for CatchlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<CatchlightModel>()
            .init_asset_loader::<CatchlightModelLoader>()
            .init_resource::<CatchlightSettings>();
        // Earlier than propagation would bake a one-frame-stale root.
        app.configure_sets(
            PostUpdate,
            CatchlightSystems::Tick.after(TransformSystems::Propagate),
        )
        .add_systems(PostUpdate, update_puppets.in_set(CatchlightSystems::Tick));
        app.add_plugins(SyncComponentPlugin::<CatchlightPuppet>::default());
        // Pipelines are sample_count=1 and MSAA draws silently blank;
        // #[require(Msaa::Off)] cannot replace an already-present Msaa.
        app.add_observer(|add: On<Add, CatchlightCamera>, mut commands: Commands| {
            commands.entity(add.entity).insert(Msaa::Off);
        });

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
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
