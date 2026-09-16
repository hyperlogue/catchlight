//! Authoring tools over a [`catchlight_core::Model`].
//!
//! The model itself lives in `catchlight-core`; what is left here operates on
//! one: [`WorkingMesh`] (constrained triangulation, contour automesh, alpha
//! culling) and [`Manifest`] (a hand-written description of a model assembled
//! from loose textures). Both reach the model through an extension trait —
//! [`ModelMeshExt`] and [`ModelManifestExt`] — because the type they extend is
//! defined in another crate. [`fit_strand`] is the third:
//! pure geometry over one mesh, measuring where a strand of hair hangs from
//! and how long each of its links is.
//! [`ModelHistory`] retains bounded branching snapshots for authoring sessions;
//! the caller supplies publication revisions and owns locking and notifications.
//!
//! Pure and wasm-safe: no GPU, no async, no filesystem.

mod history;
mod manifest;
mod mesh;
mod mesh_draft;
mod recording;
mod strand;

pub use history::*;
pub use manifest::*;
pub use mesh::*;
pub use mesh_draft::MeshDraft;
pub use recording::{RecordProperties, Recording};
pub use strand::*;

/// Picking and selection bounds against the evaluated puppet.
pub mod picking;
