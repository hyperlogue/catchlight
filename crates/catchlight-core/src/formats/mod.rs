//! On-disk model formats.
//!
//! `.clm` is the editable source of truth; `.inx` / `.inp` are a one-time
//! import path only (`load_model` warns when you load one directly).
//! [`container`] frames opaque sections and owns the version word; [`clm`]
//! gives them meaning (`Structure` CBOR + verbatim `Textures`) and holds the
//! value types a [`Model`](crate::Model) is made of. [`legacy`] is the arena
//! document the importer still produces and the legacy runtime still consumes.

pub mod clm;
pub mod container;
mod inx;
pub mod legacy;
mod utils;

pub use inx::*;
