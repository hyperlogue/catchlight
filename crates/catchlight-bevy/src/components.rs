use bevy::prelude::*;
use catchlight_core::{Model, Pose, Puppet};

use crate::asset::CatchlightModel;

/// An entity that animates a [`CatchlightModel`].
///
/// The puppet is `None` until the asset loads; to pose before then, use
/// [`CatchlightPuppet::with_pose`].
// `SyncToRenderWorld` comes from `SyncComponentPlugin`, not `#[require]`:
// registering the same direct requirement twice panics.
#[derive(Component)]
#[require(Visibility)]
pub struct CatchlightPuppet {
    model: Handle<CatchlightModel>,
    puppet: Option<Puppet>,
    pending_pose: Option<Pose>,
    baked_from: Option<AssetId<CatchlightModel>>,
}

impl CatchlightPuppet {
    pub fn new(model: Handle<CatchlightModel>) -> Self {
        Self {
            model,
            puppet: None,
            pending_pose: None,
            baked_from: None,
        }
    }

    #[must_use]
    pub fn with_pose(mut self, pose: Pose) -> Self {
        self.pending_pose = Some(pose);
        self
    }

    pub fn model(&self) -> &Handle<CatchlightModel> {
        &self.model
    }

    /// The pose carries over by `ParamId`. Rebuilds rather than
    /// [`Puppet::sync`]s, because two models can share a generation counter.
    pub fn set_model(&mut self, model: Handle<CatchlightModel>) {
        if self.model.id() == model.id() {
            return;
        }
        if self.pending_pose.is_none() {
            self.pending_pose = self.puppet.as_ref().map(Puppet::pose);
        }
        self.model = model;
    }

    pub fn puppet(&self) -> Option<&Puppet> {
        self.puppet.as_ref()
    }

    pub fn puppet_mut(&mut self) -> Option<&mut Puppet> {
        self.puppet.as_mut()
    }

    pub fn is_baked(&self) -> bool {
        self.puppet.is_some()
    }

    pub fn pose(&self) -> Pose {
        match (&self.puppet, &self.pending_pose) {
            (Some(puppet), _) => puppet.pose(),
            (None, Some(pose)) => pose.clone(),
            (None, None) => Pose::new(),
        }
    }

    pub(crate) fn needs_bake(&self, id: AssetId<CatchlightModel>) -> bool {
        self.puppet.is_none() || self.baked_from != Some(id)
    }

    pub(crate) fn bake(&mut self, model: &Model, id: AssetId<CatchlightModel>) {
        let carried = self
            .pending_pose
            .take()
            .or_else(|| self.puppet.as_ref().map(Puppet::pose));
        let mut puppet = Puppet::new(model);
        puppet.set_animations_from(model);
        if let Some(pose) = carried {
            puppet.apply_pose(&pose);
        }
        // A loaded or replaced asset starts at equilibrium in its carried pose.
        puppet.reset_motion_responses(model);
        self.puppet = Some(puppet);
        self.baked_from = Some(id);
    }
}

#[derive(Component, Default, Clone, Copy)]
pub struct CatchlightCamera;
