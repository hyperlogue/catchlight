//! Authoring tools over a [`catchlight_core::Model`]. Pure and wasm-safe:
//! no GPU, no async, no filesystem.

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
pub use recording::{
    RecordProperties, Recording, RecordingCell, RecordingError, RecordingKeyInsert, RecordingValue,
    RecordingWrite,
};
pub use strand::*;

pub mod picking;
