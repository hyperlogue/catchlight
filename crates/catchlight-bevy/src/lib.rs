//! Bevy integration for catchlight.
//!
//! Adds `CatchlightPlugin` which plugs a puppet renderer into Bevy's
//! render graph. Spawn a `CatchlightPuppet` component on an entity with a
//! `Transform` and a `GlobalTransform` and it will render on every
//! `CatchlightCamera` in the scene, atomic at the entity's z.

mod camera_controls;
mod components;
mod extract;
mod node;
mod plugin;
mod prepare;
mod update;

pub use camera_controls::{CameraControls, CameraControlsPlugin};
pub use catchlight_core::Puppet;
pub use components::{CatchlightCamera, CatchlightPuppet, PuppetDynamicState};
pub use plugin::CatchlightPlugin;
pub use prepare::CatchlightRenderState;
