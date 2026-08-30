use bevy::prelude::*;
use catchlight_core::{Keyframe, Model, Pose, Puppet, PuppetAnimation, PuppetLane};

use crate::asset::CatchlightModel;

/// An entity that animates a [`CatchlightModel`].
///
/// The component holds the [`Puppet`] — the pose, the drivers' state and the
/// evaluated frame — beside a handle on the model it animates. Two entities
/// holding the same handle animate **one** model with two puppets: posing one
/// never touches the model and never reaches the other.
///
/// The puppet is `None` until the asset finishes loading. `update_puppets`
/// bakes it on the first frame the model is available and ticks it from then
/// on, so a caller that wants to pose before the model has loaded hands the
/// pose to [`CatchlightPuppet::with_pose`] and it lands on the bake.
///
/// `SyncToRenderWorld` is required via `SyncComponentPlugin` in
/// [`crate::CatchlightPlugin`] (not `#[require]` — registering the same direct
/// requirement twice panics), so `extract_puppets` can reach the paired
/// `RenderEntity` and removing this component tears the render-world entity
/// down.
///
/// `Visibility` is required so the entity gets an `InheritedVisibility`
/// (computed by bevy's visibility propagation); extraction reads it to honour
/// `Visibility::Hidden` and hidden ancestors.
#[derive(Component)]
#[require(Visibility)]
pub struct CatchlightPuppet {
    model: Handle<CatchlightModel>,
    puppet: Option<Puppet>,
    /// A pose waiting for a bake: set before the model loaded, or carried
    /// across a model swap. Taken by the next bake.
    pending_pose: Option<Pose>,
    /// The asset the current puppet was baked from. A different id here is
    /// what makes a swapped handle rebase the puppet.
    baked_from: Option<AssetId<CatchlightModel>>,
}

impl CatchlightPuppet {
    /// Animate `model`. The puppet is baked on the first frame the asset is
    /// available.
    pub fn new(model: Handle<CatchlightModel>) -> Self {
        Self {
            model,
            puppet: None,
            pending_pose: None,
            baked_from: None,
        }
    }

    /// Start from `pose` instead of the model's defaults. The pose is keyed by
    /// `ParamId`, so it survives a later model swap the same way.
    #[must_use]
    pub fn with_pose(mut self, pose: Pose) -> Self {
        self.pending_pose = Some(pose);
        self
    }

    /// The model this entity animates.
    pub fn model(&self) -> &Handle<CatchlightModel> {
        &self.model
    }

    /// Animate a different model from the next frame on, carrying the pose.
    ///
    /// The puppet is rebuilt against the new model and the pose is re-applied
    /// **by `ParamId`**: a param the new model also has keeps its value, one it
    /// dropped is forgotten, one it added starts at its own default. This is a
    /// rebuild rather than [`Puppet::sync`] because two different models can
    /// hold the same generation counter, which is the one staleness the
    /// generation gate cannot see.
    pub fn set_model(&mut self, model: Handle<CatchlightModel>) {
        if self.model.id() == model.id() {
            return;
        }
        if self.pending_pose.is_none() {
            self.pending_pose = self.puppet.as_ref().map(Puppet::pose);
        }
        self.model = model;
    }

    /// The puppet, once the model has loaded and been baked.
    pub fn puppet(&self) -> Option<&Puppet> {
        self.puppet.as_ref()
    }

    /// The puppet, to pose it or to drive it. `None` until the model loads.
    pub fn puppet_mut(&mut self) -> Option<&mut Puppet> {
        self.puppet.as_mut()
    }

    /// Whether the model has loaded and the puppet is baked.
    pub fn is_baked(&self) -> bool {
        self.puppet.is_some()
    }

    /// The pose that will be animating next frame: the puppet's own, or the
    /// one waiting for a bake, or empty.
    pub fn pose(&self) -> Pose {
        match (&self.puppet, &self.pending_pose) {
            (Some(puppet), _) => puppet.pose(),
            (None, Some(pose)) => pose.clone(),
            (None, None) => Pose::new(),
        }
    }

    /// Whether the puppet has to be (re)baked against `id` before it ticks.
    pub(crate) fn needs_bake(&self, id: AssetId<CatchlightModel>) -> bool {
        self.puppet.is_none() || self.baked_from != Some(id)
    }

    /// Build the puppet against `model`, carrying whatever pose was already
    /// set. Called by `update_puppets`, which owns when it happens.
    pub(crate) fn bake(&mut self, model: &Model, id: AssetId<CatchlightModel>) {
        let carried = self
            .pending_pose
            .take()
            .or_else(|| self.puppet.as_ref().map(Puppet::pose));
        let mut puppet = Puppet::new(model);
        puppet.set_animations(animations_of(model));
        // A freshly baked model renders settled rather than swinging into
        // place. `settle_physics` leaves the puppet unposed, so the carried
        // pose goes on after it.
        puppet.settle_physics(model);
        if let Some(pose) = carried {
            puppet.apply_pose(&pose);
        }
        self.puppet = Some(puppet);
        self.baked_from = Some(id);
    }
}

/// The model's own animation clips, in the form a puppet plays.
///
/// A conversion rather than a shared type because a `.clm` clip is wire data
/// and a [`PuppetAnimation`] is play state; core should own this once the two
/// stop being separate types.
fn animations_of(model: &Model) -> Vec<PuppetAnimation> {
    model
        .animations()
        .iter()
        .map(|clip| PuppetAnimation {
            name: clip.name.clone(),
            timestep: clip.timestep,
            length: clip.length,
            lead_in: clip.lead_in,
            lead_out: clip.lead_out,
            lanes: clip
                .lanes
                .iter()
                .map(|lane| PuppetLane {
                    param: lane.param.clone(),
                    interpolation: lane.interpolation,
                    keyframes: lane
                        .keyframes
                        .iter()
                        .map(|k| Keyframe {
                            frame: k.frame,
                            value: k.value,
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

/// Marker component on cameras that should render puppets.
#[derive(Component, Default, Clone, Copy)]
pub struct CatchlightCamera;
