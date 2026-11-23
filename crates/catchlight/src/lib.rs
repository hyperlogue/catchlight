//! 2.5D character animation through mesh deformation.
//!
//! Catchlight simulates three-dimensional movement by deforming two-dimensional
//! parts. Try the [browser demo](https://hyperlogue.github.io/catchlight/).
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
//! let pose = Pose::new();
//! puppet.apply_pose(&pose);
//! puppet.tick(&model, 1.0 / 60.0);
//! # Ok(())
//! # }
//! ```
//!
//! The `wgpu` feature exposes the rendering backend as `catchlight::wgpu`.

pub use catchlight_core::*;

/// GPU rendering backend. Requires the `wgpu` feature.
#[cfg(feature = "wgpu")]
pub use catchlight_wgpu as wgpu;

#[cfg(feature = "bevy")]
pub use catchlight_bevy as bevy;
