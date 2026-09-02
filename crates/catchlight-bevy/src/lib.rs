//! Bevy integration for catchlight.
//!
//! Adds `CatchlightPlugin`. Load a model as a `CatchlightModel` asset, spawn a
//! `CatchlightPuppet` component holding its `Handle` on an entity with a
//! `Transform` and a `GlobalTransform`, and it renders on every
//! `CatchlightCamera` in the scene, atomic at the entity's z.
//!
//! **Who owns what.** The three types the runtime is split into land in three
//! places, and the split is the reason the integration is shaped this way:
//!
//! - the **model** is an asset (`CatchlightModel`), shared: two entities
//!   holding one handle animate one model, loaded and decoded once;
//! - the **puppet** is a component (`CatchlightPuppet`), owned by its entity,
//!   ticked in the main world against the asset, with the entity's
//!   `GlobalTransform` as the model's root;
//! - the **render cache** is render-world state, one per (model, view
//!   format), prepared from the model and refreshed from every puppet of it.
//!
//! Bevy's stages carry catchlight's: **extract** is
//! `RenderCache::refresh_puppet` (the main world is paused there, which is the
//! only moment the live puppet may be read), and **prepare** is
//! `RenderCache::prepare` (the uploads that survive a frame, kept out of the
//! sync point). A puppet that appears on one frame therefore draws from the
//! next.
//!
//! **A cache is per model, not per entity.** A model's textures and meshes do
//! not depend on how it is posed, so fifty entities animating one rig hold one
//! renderer and one cache between them: one decode and one upload of every
//! texture, against fifty copies of the same thing before. What an entity owns
//! is a `DeformSet` — its own slice of that renderer's deform atlas, two floats
//! per vertex — and the render list collected from its puppet. Every draw of a
//! frame lands in one submit and `queue.write_buffer` batches at submit start,
//! so without those slices the second puppet's upload would overwrite the
//! first's before either drew.
//!
//! **A renderer draws once per frame, so z orders within a model.** Every
//! puppet of a model goes into one `render_lists_ext`; a second call on the
//! same renderer inside one submit would reset the frame cursors under the
//! draws already recorded. Models are therefore ordered by their backmost
//! puppet and drawn one group at a time: two models whose puppets interleave
//! in z do not interleave on screen.
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
