//! Import invariants (`.inx` → catchlight).
//!
//! One path: `to_legacy.rs` turns a parsed `.inx` into the arena document a
//! [`Model`](crate::Model) is built from, which `cargo xtask import` then
//! writes as a `.clm`. Nothing reads a `.inx` into a runtime any more.
//!
//! `.inx` is authored **Y-down with lower `zsort` in front**; catchlight is
//! **Y-up with higher `z_order` in front**. The reader negates exactly this
//! set and no more:
//!
//! - transform translation Y, rotation X, rotation Z (`reflect_transform_y`)
//! - mesh vertex Y and mesh origin Y (`reflect_mesh_y`); UVs are texture
//!   space and stay as authored
//! - the source `zsort` into `z_order` (`reflect_z`, which maps `0.0` to
//!   `0.0`, not `-0.0`)
//! - the Y-bearing binding outputs: `TransformTY`, `TransformRX`,
//!   `TransformRZ`, `Deform` offsets' Y, and `ZOrder` (`reflect_binding_outputs`)
//!
//! Rotation Y and scale are **not** reflected, and neither are the non-Y
//! transform components.
//!
//! `the_import_reflects_exactly_the_y_bearing_fields` (`to_legacy.rs`) guards
//! this on every checkout: it runs a hand-authored model through the reader
//! and asserts the absolute authored→imported values, with a non-reflected
//! control beside every reflected field — so a missing negation and a doubled
//! one both fail. The synthetic INX must therefore be authored in the
//! **source** convention (Y-down, lower-zsort-in-front).
//!
//! **The reader is total.** A reference that does not resolve is dropped, a
//! field of the wrong JSON type falls back to its default, and an unmodelled
//! node type becomes a group — `import_tests.rs` pins which half each case
//! takes. `.inx` is untrusted input, so the node walk carries its own stack
//! rather than recursing.
//!
//! **Texture strategy is `alpha_crop.rs` and nothing else.** It crops each
//! texture to the aligned bounding box of its *opaque* texels plus a 16-texel
//! transparent mip skirt, keeping texture ids 1:1 with the source table so only
//! part UVs are rewritten. `atlas.rs` is gone.

pub(crate) mod alpha_crop;
pub use alpha_crop::{prepare_textures, PreppedTexture, TexturePrepCache, UvCrop};
pub(crate) mod error;
pub(crate) mod schema;
pub(crate) mod to_legacy;

#[cfg(test)]
mod import_tests;

pub use error::ImportError;
pub use to_legacy::from_inx_model_to_legacy;
