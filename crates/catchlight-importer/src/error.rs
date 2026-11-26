use crate::inx::InxParseError;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("failed to parse .inx container: {0}")]
    InxContainer(#[from] InxParseError),

    #[error("invalid .clm file: {0}")]
    Model(#[from] catchlight_core::model::ModelError),

    /// Unreachable today; carried rather than unwrapped because the minting
    /// rule and the Id charset live in different crates.
    #[error("generated an invalid id: {0}")]
    Id(#[from] catchlight_core::id::IdError),

    #[error(transparent)]
    LoadLimit(#[from] catchlight_core::load_budget::LoadLimitError),

    #[error("missing required field: {0}")]
    MissingField(&'static str),

    #[error("unknown blend mode: {0}")]
    UnknownBlendMode(String),

    #[error("malformed payload: {0}")]
    MalformedPayload(String),

    #[error("node {id} ({name:?}) carries a mesh inochi2d could not draw: {detail}")]
    MalformedMesh {
        id: String,
        name: String,
        detail: String,
    },

    #[error("param {id} ({name:?}) is authored backwards: min {min} is above max {max}")]
    InvertedParamRange {
        id: String,
        name: String,
        min: f32,
        max: f32,
    },

    #[error("part {id} ({name:?}) draws texture slot {slot}, but the rig carries {count}")]
    TextureOutOfRange {
        id: String,
        name: String,
        slot: i64,
        count: usize,
    },
}
