use bevy::prelude::*;

use crate::asset::CatchlightModel;
use crate::components::CatchlightPuppet;

/// Rebakes on asset replacement explicitly: a fresh model's generation
/// counter starts back at zero, so `Puppet::tick` cannot see it.
pub(crate) fn update_puppets(
    time: Res<Time>,
    models: Res<Assets<CatchlightModel>>,
    mut asset_events: MessageReader<AssetEvent<CatchlightModel>>,
    mut puppets: Query<(&mut CatchlightPuppet, &GlobalTransform)>,
) {
    let mut replaced: Vec<AssetId<CatchlightModel>> = Vec::new();
    for event in asset_events.read() {
        if let AssetEvent::Modified { id } | AssetEvent::LoadedWithDependencies { id } = event {
            replaced.push(*id);
        }
    }

    let dt = time.delta_secs();
    for (mut puppet, global_transform) in &mut puppets {
        let id = puppet.model().id();
        let Some(model) = models.get(id) else {
            continue;
        };
        let model = model.model();
        if puppet.needs_bake(id) || replaced.contains(&id) {
            puppet.bake(model, id);
        }
        puppet.tick(model, global_transform.to_matrix(), dt);
    }
}
