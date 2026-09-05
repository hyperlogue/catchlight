//! One-time import of inochi2d `.inx` / `.inp` puppets into a catchlight
//! [`Model`](catchlight_core::Model).
//!
//! This crate depends on `catchlight-core`; nothing in core depends on it.
//! `catchlight_core::load_model` reads `.clm` and nothing else — an `.inx` is
//! converted once, with `cargo xtask import`, and the `.clm` is what ships.
//!
//! Three parts. [`inx`] is the container reader: `TRNSRTS\0` framing, the JSON
//! payload, the texture table (PNG, TGA, or BC7 DDS decoded to PNG) and the
//! opaque vendor sections. `reflect.rs` turns inochi2d's JSON into the values
//! catchlight stores. `to_clm.rs` assembles them into a `.clm` structure and
//! mints its Ids, and [`import_inx_model`] reads that back through
//! [`Model::from_clm_file`](catchlight_core::Model::from_clm_file) — so an
//! import that succeeds has been through the same reader a `.clm` off disk
//! goes through, and what it returns can be written and opened again.
//!
//! **Ids come from position, not from inochi2d.** `.inx` identifies
//! everything by a `uuid` that catchlight does not keep, so the import mints
//! `root`, `node-<i>`, `param-<i>` (`param-<i>.x` / `.y` for a 2-D param) and
//! `tex-<i>` from each thing's place in the flattening. Deterministic, so two
//! imports of one `.inx` agree about what an addon would be naming.
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
//! `the_import_reflects_exactly_the_y_bearing_fields` (`to_clm.rs`) guards
//! this on every checkout: it runs a hand-authored model through the reader
//! and asserts the absolute authored→imported values, with a non-reflected
//! control beside every reflected field — so a missing negation and a doubled
//! one both fail. The synthetic INX must therefore be authored in the
//! **source** convention (Y-down, lower-zsort-in-front).
//!
//! **The reader is tolerant of sloppiness.** A reference that does not resolve
//! is dropped, a field of the wrong JSON type falls back to its default, and an
//! unmodelled node type becomes a group — `import_tests.rs` pins which half
//! each case takes. `.inx` is untrusted input, so the node walk carries its own
//! stack rather than recursing, and every length the container declares is
//! checked against a ceiling before anything is allocated for it.
//!
//! **A malformed rig is repaired only when the repair provably cannot change
//! how inochi2d renders it; otherwise the import is refused with the node or
//! param named.** Guessing is the one thing an import may not do: the `.clm` it
//! writes is the source of truth afterwards, so a wrong-but-loadable model is
//! unrecoverable in a way a refusal is not. Every repair says what it changed
//! through `tracing::warn!`, naming the thing it changed.
//!
//! Repaired, because the source draws the same picture either way:
//!
//! - a param range that cannot be posed against — `min == max`, or a bound too
//!   large to be a number — widens to `min..min + 1`; the source param cannot
//!   move either (`usable_range`, `to_clm.rs`)
//! - a deform cell that disagrees with its node's mesh is zipped against it —
//!   offsets past the last vertex drive nothing, vertices past the last offset
//!   stay undeformed (`fit_deform_cells`, `reflect.rs`)
//! - a deform binding on a node with no mesh vertices is dropped; there is
//!   nothing for it to move (`convert_binding`)
//! - a part with no `textures` array takes `tex-0`, which is what the source
//!   runtime draws, or no texture at all when the rig carries none
//!   (`convert_part`)
//! - a texture no part draws is dropped from the file (`from_inx_model`)
//! - a mask whose source is not a part or a composite is dropped; catchlight
//!   draws neither a mesh group nor a plain node nor a pendulum, so a source
//!   that is one rasterized into no stencil in the source runtime either
//!   (`convert_masks`, `to_clm.rs`)
//! - a mesh group's UVs are dropped when they do not pair with its vertices;
//!   nothing samples them, which is the same reason they are not checked
//!   (`convert_mesh`, `reflect.rs`)
//!
//! Refused, because inochi2d's own rendering of them is undefined or unclear:
//!
//! - a mesh whose indices name vertices it does not have, or whose UVs do not
//!   pair with its vertices (`convert_mesh`, `reflect.rs`)
//! - a finite param range authored the wrong way round (`min > max`)
//!
//! **Textures are carried verbatim.** The bytes an `.inx` stores land in the
//! model unchanged; decoding, the alpha crop and the UV remap belong to
//! [`catchlight_core::texture`], which every model goes through whether it
//! was imported or authored. Ids are minted from the source's texture order,
//! and dropping an undrawn texture never renumbers the ones that stay.
//!
//! **Animations come across.** A source clip's lanes address a param by
//! `uuid` and an axis by index, which is the one thing about them that does
//! not survive the split of a 2-D source param into two scalar ones; the lane
//! lands on the param that axis became (`convert_animations`, `to_clm.rs`). A
//! lane is dropped, by the same rule as any other dangling reference, when its
//! `uuid` names no param or its axis names one the param does not have. An
//! interpolation catchlight does not model is **not** grounds for dropping
//! one: the lane keeps its keyframes and plays the nearest mode, warned.
//!
//! **What is dropped**, because catchlight does not model it: meta, groups,
//! automation, cameras, emissive and bump texture slots, `emissionStrength`,
//! and a source node's `uuid`.

pub(crate) mod error;
pub mod inx;
pub(crate) mod read;
pub(crate) mod reflect;
pub(crate) mod schema;
pub(crate) mod to_clm;

#[cfg(test)]
mod import_tests;

pub use error::ImportError;
pub use inx::{parse_inp, InpModel, InpParseError, InxModel, InxParseError, VendorData};
pub use to_clm::{from_inx_model, import_inx_bytes, import_inx_model};
