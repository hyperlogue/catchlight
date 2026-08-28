use bevy::prelude::*;
use catchlight_wgpu::RenderList;

use crate::components::CatchlightPuppet;

/// Main-world system. Runs the documented 7-step puppet lifecycle per
/// entity and produces a `RenderList`.
// Invariant: per-puppet RwLocks are only poisoned on panic, treated as fatal.
#[allow(clippy::unwrap_used)]
pub(crate) fn update_puppets(time: Res<Time>, q: Query<(&CatchlightPuppet, &GlobalTransform)>) {
    let dt = time.delta_secs();
    for (cp, global_transform) in &q {
        let root = global_transform.to_matrix();

        let mut puppet = cp.puppet.write().unwrap();
        let mut state = cp.state.write().unwrap();

        let state = &mut *state;
        puppet.tick(&mut state.transforms, root, dt);
        let crate::components::PuppetDynamicState {
            transforms,
            render_list,
            drawable_collector,
        } = state;
        let render_list = render_list.get_or_insert_with(RenderList::default);
        drawable_collector.collect_into(&puppet, transforms, render_list);
    }
}
