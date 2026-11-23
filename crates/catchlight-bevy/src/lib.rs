//! Bevy integration for catchlight.
//!
//! `bevy_render` and `catchlight-wgpu` must resolve to a single `wgpu`; a split
//! tree fails as a type mismatch between two `wgpu::Device`s. glam is pinned
//! to bevy's version for the same reason.

mod asset;
mod camera_controls;
mod components;
mod extract;
mod node;
mod plugin;
mod prepare;
mod update;

pub use asset::{model_from_bytes, CatchlightModel, CatchlightModelLoader, ModelAssetError};
pub use camera_controls::{CameraControls, CameraControlsPlugin};
pub use catchlight_core::{Model, Pose, Puppet};
pub use components::{CatchlightCamera, CatchlightPuppet};
pub use extract::ExtractedPuppet;
pub use plugin::{CatchlightPlugin, CatchlightSettings};
pub use prepare::CatchlightRenderState;
