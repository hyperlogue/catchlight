
//! The editable puppet model for the catchlight editor.
//!
//! [`EditModel`] mirrors a `.clp` document but keys nodes, params and textures
//! by stable generational ids instead of array indices, so insert, reparent and
//! delete are cheap and never renumber live references. Array indices exist only
//! at the file edge: [`EditModel::flatten`] assigns them in topological order on
//! save, [`EditModel::from_clp_file`] recovers ids on open.
//!
//! Pure and wasm-safe: no GPU, no async, no filesystem. The editor server, CLI
//! and (future) web GUI all build on this.

mod binding;
mod check;
mod flatten;
mod manifest;
mod mesh;
mod model;

pub use binding::*;
pub use check::*;
pub use manifest::*;
pub use mesh::*;
pub use model::*;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EditError {
    #[error("unknown node id")]
    UnknownNode,
    #[error("unknown param id")]
    UnknownParam,
    #[error("unknown texture id")]
    UnknownTexture,
    #[error(".clp node arena must contain exactly one root at index 0")]
    InvalidClpRoot,
    #[error(".clp node {node} parent index {parent} must name a preceding node")]
    InvalidClpParent { node: usize, parent: u32 },
    #[error("cannot reparent a node under itself or a descendant")]
    Cycle,
    #[error("the root node cannot be {0}")]
    Root(&'static str),
    #[error("binding cell is outside the param's axis grid")]
    CellOutOfRange,
    #[error("no such binding")]
    UnknownBinding,
    #[error("node is not a part")]
    NotAPart,
    #[error("node cannot have masks")]
    NotMaskable,
    #[error("a node cannot mask itself")]
    SelfMask,
    #[error("index out of range")]
    IndexOutOfRange,
    #[error("constraint edges may not cross")]
    ConstraintCross,
    #[error(".clp codec: {0}")]
    Clp(#[from] catchlight_core::formats::clp::ClpError),
    #[error(transparent)]
    LoadLimit(#[from] catchlight_core::LoadLimitError),
}
