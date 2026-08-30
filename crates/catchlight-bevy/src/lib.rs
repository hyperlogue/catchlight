//! Bevy integration for catchlight.
//!
//! Adds `CatchlightPlugin` which plugs a puppet renderer into Bevy's
//! render graph. Spawn a `CatchlightPuppet` component on an entity with a
//! `Transform` and a `GlobalTransform` and it will render on every
//! `CatchlightCamera` in the scene, atomic at the entity's z.
//!
//! **One wgpu across the whole workspace.** `catchlight-bevy` hands bevy's
//! render world a `Device`, `Queue` and `Arc<Pipelines>` built by
//! `catchlight-wgpu` (`prepare.rs`, via `device.wgpu_device()`), so
//! `bevy_render` and `catchlight-wgpu` must resolve to a **single** wgpu —
//! 29.0.3 today, shared with `eframe` and `wgpu-profiler`. `bevy = "0.19"` is
//! really a proxy for "the bevy built against wgpu 29". If the tree ever
//! splits, the failure is a type mismatch between two identically named
//! `wgpu::Device`s at the plugin boundary, which reads as nonsense: check
//! `Cargo.lock` for a duplicate `wgpu`.
//!
//! **glam is pinned to bevy's 0.32**, not the crates.io latest, for the same
//! reason — a newer glam re-splits the tree. `cargo deny`'s
//! `multiple-versions` is at `warn` because these duplicates are the decision,
//! not a defect.

mod camera_controls;
mod components;
mod extract;
mod node;
mod plugin;
mod prepare;
mod update;

pub use camera_controls::{CameraControls, CameraControlsPlugin};
pub use catchlight_core::LegacyPuppet;
pub use components::{CatchlightCamera, CatchlightPuppet, PuppetDynamicState};
pub use plugin::CatchlightPlugin;
pub use prepare::CatchlightRenderState;
