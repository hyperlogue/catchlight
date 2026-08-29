//! Authoring tools over a [`catchlight_core::Model`].
//!
//! The model itself lives in `catchlight-core`; what is left here operates on
//! one: [`WorkingMesh`] (constrained triangulation, contour automesh, alpha
//! culling) and [`Manifest`] (a hand-written description of a model assembled
//! from loose textures). Both reach the model through an extension trait —
//! [`ModelMeshExt`] and [`ModelManifestExt`] — because the type they extend is
//! defined in another crate.
//!
//! Pure and wasm-safe: no GPU, no async, no filesystem.

mod manifest;
mod mesh;

pub use manifest::*;
pub use mesh::*;
