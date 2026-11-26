use bevy::prelude::*;
use catchlight_core::{Mat4, Model, Pose, Puppet};
use catchlight_extension_animations::{AnimationPlayer, Animations, LiveInputs, Rig};

use crate::asset::CatchlightModel;

/// An entity that animates a [`CatchlightModel`].
///
/// The puppet is `None` until the asset loads; to pose before then, use
/// [`CatchlightPuppet::with_pose`]. Its player holds the model's clips from
/// the same bake on, and advances before every tick through the model's
/// control mappings, expressions and visemes, with no inputs of its own.
// `SyncToRenderWorld` comes from `SyncComponentPlugin`, not `#[require]`:
// registering the same direct requirement twice panics.
#[derive(Component)]
#[require(Visibility)]
pub struct CatchlightPuppet {
    model: Handle<CatchlightModel>,
    puppet: Option<Puppet>,
    player: AnimationPlayer,
    pending_pose: Option<Pose>,
    baked_from: Option<AssetId<CatchlightModel>>,
}

impl CatchlightPuppet {
    #[must_use]
    pub fn new(model: Handle<CatchlightModel>) -> Self {
        Self {
            model,
            puppet: None,
            player: AnimationPlayer::default(),
            pending_pose: None,
            baked_from: None,
        }
    }

    #[must_use]
    pub fn with_pose(mut self, pose: Pose) -> Self {
        self.pending_pose = Some(pose);
        self
    }

    #[must_use]
    pub const fn model(&self) -> &Handle<CatchlightModel> {
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

    #[must_use]
    pub const fn puppet(&self) -> Option<&Puppet> {
        self.puppet.as_ref()
    }

    pub const fn puppet_mut(&mut self) -> Option<&mut Puppet> {
        self.puppet.as_mut()
    }

    #[must_use]
    pub const fn player(&self) -> &AnimationPlayer {
        &self.player
    }

    pub const fn player_mut(&mut self) -> &mut AnimationPlayer {
        &mut self.player
    }

    /// Both halves at once, for a caller that advances the player itself.
    pub const fn puppet_and_player_mut(&mut self) -> (Option<&mut Puppet>, &mut AnimationPlayer) {
        (self.puppet.as_mut(), &mut self.player)
    }

    #[must_use]
    pub const fn is_baked(&self) -> bool {
        self.puppet.is_some()
    }

    #[must_use]
    pub fn pose(&self) -> Pose {
        match (&self.puppet, &self.pending_pose) {
            (Some(puppet), _) => puppet.pose(),
            (None, Some(pose)) => pose.clone(),
            (None, None) => Pose::new(),
        }
    }

    /// A replaced asset keeps its id but brings a new model identity; an
    /// edit keeps both and reaches the puppet through [`Puppet::sync`].
    pub(crate) fn needs_bake(&self, id: AssetId<CatchlightModel>, model: &Model) -> bool {
        self.baked_from != Some(id)
            || self
                .puppet
                .as_ref()
                .is_none_or(|puppet| puppet.model_identity() != model.identity())
    }

    pub(crate) fn bake(&mut self, model: &Model, id: AssetId<CatchlightModel>) {
        let carried = self
            .pending_pose
            .take()
            .or_else(|| self.puppet.as_ref().map(Puppet::pose));
        let mut puppet = Puppet::new(model);
        self.player = model
            .extension::<Animations>()
            .map(AnimationPlayer::from_animations)
            .unwrap_or_default();
        if let Some(pose) = carried {
            puppet.apply_pose(&pose);
        }
        // A loaded or replaced asset starts at equilibrium in its carried pose.
        puppet.reset_motion_responses(model);
        self.puppet = Some(puppet);
        self.baked_from = Some(id);
    }

    pub(crate) fn tick(&mut self, model: &Model, root: Mat4, dt: f32) {
        if let Some(puppet) = &mut self.puppet {
            self.player
                .advance_with(puppet, dt, Rig::of(model), &LiveInputs::default());
            puppet.tick_with_root(model, root, dt);
        }
    }
}

#[derive(Component, Default, Clone, Copy)]
pub struct CatchlightCamera;
