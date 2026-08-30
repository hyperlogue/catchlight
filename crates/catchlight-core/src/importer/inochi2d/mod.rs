//! Import invariants (`.inx` → catchlight).
//!
//! There are **two independent reflection paths** and they must stay in step:
//!
//! - `convert.rs` — `.inx` → `LegacyPuppet` (the legacy direct load).
//! - `to_legacy.rs` — `.inx` → legacy document → `.clm` (what
//!   `cargo xtask import` runs).
//!
//! `.inx` is authored **Y-down with lower `zsort` in front**; catchlight is
//! **Y-up with higher `z_order` in front**. Both paths must negate exactly
//! the same set:
//!
//! - transform translation Y, rotation X, rotation Z (`reflect_transform_y`)
//! - mesh vertex Y and mesh origin Y (`reflect_mesh_y` / the loop in
//!   `convert_mesh`); UVs are texture space and stay as authored
//! - the source `zsort` into `z_order` (`reflect_z`, which maps `0.0` to
//!   `0.0`, not `-0.0`)
//! - the Y-bearing binding outputs: `TransformTY`, `TransformRX`,
//!   `TransformRZ`, `Deform` offsets' Y, and `ZOrder` (`reflect_binding_outputs`)
//!
//! Rotation Y and scale are **not** reflected, and neither are the non-Y
//! transform components.
//!
//! **Change one path, change the other.**
//! `synthetic_model_reflects_identically_on_both_paths` (`from_legacy.rs`) guards
//! this on every checkout: it runs a hand-authored model through both paths,
//! asserts they agree field for field, *and* asserts the absolute
//! authored→runtime values with a non-reflected control beside every reflected
//! field — agreement alone would pass if both paths forgot the same negation.
//! The synthetic INX must therefore be authored in the **source** convention
//! (Y-down, lower-zsort-in-front). `reference_legacy_build_matches_inx_puppet`
//! runs the same comparison over the full private model, and is `#[ignore]`d
//! unless that model is present.
//!
//! **Texture strategy is `alpha_crop.rs` and nothing else.** It crops each
//! texture to the aligned bounding box of its *opaque* texels plus a 16-texel
//! transparent mip skirt, keeping texture ids 1:1 with the source table so only
//! part UVs are rewritten. `atlas.rs` is gone.

pub(crate) mod alpha_crop;
pub use alpha_crop::TexturePrepCache;
pub(crate) mod convert;
pub(crate) mod error;
pub(crate) mod from_legacy;
pub(crate) mod schema;
pub(crate) mod to_legacy;

#[cfg(test)]
mod import_tests;

pub use error::ImportError;
pub use from_legacy::{from_legacy, from_legacy_cached, from_legacy_with_budget};
pub use to_legacy::from_inx_model_to_legacy;

use crate::formats::InxModel;
use crate::legacy_puppet::LegacyPuppet;

pub fn parse(bytes: &[u8]) -> Result<LegacyPuppet, ImportError> {
    let model = InxModel::parse(std::io::Cursor::new(bytes))?;
    from_inx_model(&model)
}

pub fn parse_inp(bytes: &[u8]) -> Result<LegacyPuppet, ImportError> {
    let model = crate::formats::parse_inp(std::io::Cursor::new(bytes))?;
    from_inx_model(&model)
}

/// Import a puppet, cropping each texture to its opaque bounding box (see
/// [`alpha_crop`]).
pub fn from_inx_model(model: &InxModel) -> Result<LegacyPuppet, ImportError> {
    from_inx_model_downsampled(model, 0)
}

/// Import with every texture downsampled by `texture_halvings` power-of-two
/// steps (the same linear-space box filter renderers use for mips, so the
/// result is texel-identical to starting the mip chain that many levels in).
/// Halving happens per texture right after decode, so peak memory stays near
/// the downsampled total — this matters on wasm, where linear memory never
/// shrinks and a full-resolution transient becomes the tab's permanent
/// footprint.
///
/// Picking the count: inochi2d models are authored at ~1 texture px per world
/// unit, so the on-screen sampling rate equals the camera zoom (device px
/// per world unit). A deployment whose maximum zoom stays at or below
/// `0.5^k` loses nothing at `k` halvings — the sampler never read the
/// dropped mip levels.
/// [`from_inx_model`] with each texture downsampled `texture_halvings`
/// power-of-two steps after decode (a memory escape hatch orthogonal to the
/// crop strategy).
pub fn from_inx_model_downsampled(
    model: &InxModel,
    texture_halvings: u32,
) -> Result<LegacyPuppet, ImportError> {
    convert::schema_to_puppet(&model.payload, model.textures.clone(), texture_halvings)
}
