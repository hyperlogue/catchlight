//! 2.5D character animation through mesh deformation.
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

pub use catchlight_core::*;

/// The official `catchlight.animations` extension and its player.
pub use catchlight_extension_animations as animations;

/// The official `catchlight.expressions` extension: emotions and visemes.
pub use catchlight_extension_expressions as expressions;

/// The official `catchlight.humanoid` extension: standard controls mapped
/// onto params.
pub use catchlight_extension_humanoid as humanoid;

/// GPU rendering backend. Requires the `wgpu` feature.
#[cfg(feature = "wgpu")]
pub use catchlight_wgpu as wgpu;

#[cfg(feature = "bevy")]
pub use catchlight_bevy as bevy;
