//! 2.5D character animation through mesh deformation.
//!
//! Catchlight simulates three-dimensional movement by deforming two-dimensional
//! parts. Start with a `.clm` model, create a [`Puppet`], and evaluate its pose
//! each frame. Try the [browser demo](https://hyperlogue.github.io/catchlight/)
//! or visit the [project](https://github.com/hyperlogue/catchlight).
//!
//! # Load and evaluate
//!
//! ```no_run
//! use catchlight::{load_model, ModelFormat, Pose, Puppet};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bytes = std::fs::read("character.clm")?;
//! let model = load_model(&bytes, ModelFormat::Clm)?;
//! let mut puppet = Puppet::new(&model);
//!
//! // Each frame, apply the desired parameter values and advance the puppet.
//! let pose = Pose::new();
//! puppet.apply_pose(&pose);
//! puppet.tick(&model, 1.0 / 60.0);
//! # Ok(())
//! # }
//! ```
//!
//! Evaluation produces a posed puppet; drawing it requires a renderer.
//!
//! # Features
//!
//! - **Default:** the `catchlight-core` API, re-exported at the crate root.
//!   No GPU backend or game engine is enabled by this crate.
//! - **`wgpu`:** the rendering backend, exposed as `catchlight::wgpu`.
//! - **`bevy`:** Bevy integration, exposed as `catchlight::bevy`; also enables
//!   `wgpu` so the renderer is available through the same entry point.
//!
//! ```toml
//! [dependencies]
//! catchlight = { version = "0.1.0", features = ["wgpu"] }
//! ```
//!
//! This crate contains only re-exports: its types are the same types used by
//! the underlying crates, so applications can mix either import style.
//! Authoring, format import, and command-line tools remain separate packages.

pub use catchlight_core::*;

/// GPU rendering backend. Requires the `wgpu` feature.
#[cfg(feature = "wgpu")]
pub use catchlight_wgpu as wgpu;

/// Bevy integration. Requires the `bevy` feature.
#[cfg(feature = "bevy")]
pub use catchlight_bevy as bevy;
