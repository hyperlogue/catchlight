pub(crate) mod alpha_crop;
pub use alpha_crop::TexturePrepCache;
pub(crate) mod convert;
pub(crate) mod error;
pub(crate) mod from_clp;
pub(crate) mod schema;
pub(crate) mod to_clp;

#[cfg(test)]
mod import_tests;

pub use error::ImportError;
pub use from_clp::{from_clp, from_clp_cached, from_clp_with_budget};
pub use to_clp::from_inx_model_to_clp;

use crate::formats::InxModel;
use crate::puppet::Puppet;

pub fn parse(bytes: &[u8]) -> Result<Puppet, ImportError> {
    let model = InxModel::parse(std::io::Cursor::new(bytes))?;
    from_inx_model(&model)
}

pub fn parse_inp(bytes: &[u8]) -> Result<Puppet, ImportError> {
    let model = crate::formats::parse_inp(std::io::Cursor::new(bytes))?;
    from_inx_model(&model)
}

/// Import a puppet, cropping each texture to its opaque bounding box (see
/// [`alpha_crop`]).
pub fn from_inx_model(model: &InxModel) -> Result<Puppet, ImportError> {
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
/// Picking the count: inochi2d rigs are authored at ~1 texture px per world
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
) -> Result<Puppet, ImportError> {
    convert::schema_to_puppet(&model.payload, model.textures.clone(), texture_halvings)
}
