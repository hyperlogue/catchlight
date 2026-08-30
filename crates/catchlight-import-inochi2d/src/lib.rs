//! One-time import of inochi2d `.inx` / `.inp` puppets into a catchlight
//! [`Model`](catchlight_core::Model).
//!
//! This crate depends on `catchlight-core`; nothing in core depends on it.
//! `catchlight_core::load_model` reads `.clm` and nothing else — an `.inx` is
//! converted once, with `cargo xtask import`, and the `.clm` is what ships.
//!
//! Two halves. [`inx`] is the container reader: `TRNSRTS\0` framing, the JSON
//! payload, the texture table (PNG, TGA, or BC7 DDS decoded to PNG) and the
//! opaque vendor sections. [`from_inx_model_to_legacy`] is the reflection:
//! the inochi2d document read into the shape catchlight stores.
//!
//! **One reflection.** `.inx` is authored **Y-down with lower `zsort` in
//! front**; catchlight is **Y-up with higher `z_order` in front**. There is a
//! single place that conversion happens, and it negates exactly this set and
//! no more:
//!
//! - transform translation Y, rotation X, rotation Z (`reflect_transform_y`)
//! - mesh vertex Y and mesh origin Y (`reflect_mesh_y`); UVs are texture
//!   space and stay as authored
//! - the source `zsort` into `z_order` (`reflect_z`, which maps `0.0` to
//!   `0.0`, not `-0.0`)
//! - the Y-bearing binding outputs: `TransformTY`, `TransformRX`,
//!   `TransformRZ`, `Deform` offsets' Y, and `ZOrder`
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
//! **The reader is total.** On anything short of a corrupt container, import
//! produces a model: a reference that does not resolve is dropped, a field of
//! the wrong JSON type falls back to its default, and an unmodelled node type
//! becomes a group — `import_tests.rs` pins which half each case takes.
//! `.inx` is untrusted input, so the node walk carries its own stack rather
//! than recursing, and every length the container declares is checked against
//! a ceiling before anything is allocated for it.
//!
//! **Textures are carried verbatim.** The bytes an `.inx` stores land in the
//! model unchanged; decoding, the alpha crop and the UV remap belong to
//! [`catchlight_core::texture`], which every model goes through whether it
//! was imported or authored.

pub(crate) mod error;
pub mod inx;
pub(crate) mod read;
pub(crate) mod schema;
pub(crate) mod to_legacy;

#[cfg(test)]
mod import_tests;

pub use error::ImportError;
pub use inx::{parse_inp, InpModel, InpParseError, InxModel, InxParseError, VendorData};
pub use to_legacy::from_inx_model_to_legacy;
