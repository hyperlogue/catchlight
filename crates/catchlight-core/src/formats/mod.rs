//! On-disk puppet formats.
//!
//! `.clp` is the editable source of truth; `.inx` / `.inp` are a one-time
//! import path only (`load_model` warns when you load one directly).
//! [`container`] frames opaque sections and owns the version word; [`clp`]
//! gives them meaning (`Structure` CBOR + verbatim `Textures`).

pub mod clp;
pub mod container;
mod inx;
mod utils;

pub use inx::*;
