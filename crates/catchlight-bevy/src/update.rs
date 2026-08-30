use bevy::prelude::*;

use crate::asset::CatchlightModel;
use crate::components::CatchlightPuppet;

/// Main-world system: bake every puppet whose model is ready and tick it.
///
/// A puppet is rebaked when its handle now names a different model (a swap) or
/// when the asset's *value* was replaced under the same handle (a hot reload,
/// or an `Assets::insert` over the id) — neither of which the model's own
/// generation counter can report, because a fresh model starts back at zero.
/// An in-place edit through `Assets::get_mut` does move the generation, and
/// `Puppet::tick` rebakes on that by itself.
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
        // The entity's world placement is the puppet's root, so a moved,
        // rotated or scaled entity moves the whole rig. Transform propagation
        // runs earlier in PostUpdate, so this reads *this* frame's placement.
        let root = global_transform.to_matrix();
        if let Some(puppet) = puppet.puppet_mut() {
            puppet.tick_with_root(model, root, dt);
        }
    }
}
