use crate::inx::InxParseError;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("failed to parse .inx container: {0}")]
    InxContainer(#[from] InxParseError),

    #[error("invalid .clm file: {0}")]
    Model(#[from] catchlight_core::model::ModelError),

    /// An Id this crate minted was not an Id. Unreachable: every Id it mints
    /// is `root` / `node-<i>` / `param-<i>[.x|.y]` / `tex-<i>`, all inside the
    /// charset. Carried rather than unwrapped because the minting rule and the
    /// charset live in two crates, and a change to either should surface as an
    /// error rather than a panic.
    #[error("generated an invalid id: {0}")]
    Id(#[from] catchlight_core::id::IdError),

    #[error("failed to decode payload JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid deform shape: {0}")]
    DeformShape(#[from] catchlight_core::deform::DeformShapeError),

    #[error(transparent)]
    LoadLimit(#[from] catchlight_core::load_budget::LoadLimitError),

    #[error("missing required field: {0}")]
    MissingField(&'static str),

    #[error("invalid value for field {field}: expected {expected}, got {got}")]
    InvalidFieldType {
        field: &'static str,
        expected: &'static str,
        got: String,
    },

    #[error("unknown node type: {0}")]
    UnknownNodeType(String),

    #[error("unknown blend mode: {0}")]
    UnknownBlendMode(String),

    #[error("unknown interpolation mode: {0}")]
    UnknownInterpolationMode(String),

    #[error("malformed payload: {0}")]
    MalformedPayload(String),

    #[error("texture decode failed: {0}")]
    TextureDecode(String),

    /// A mesh inochi2d could not have drawn either: an index naming a vertex
    /// the mesh does not have, or a UV array that does not pair with the
    /// vertices. There is no rendering to preserve and nothing to repair
    /// without inventing geometry, so the import stops and names the node.
    #[error("node {id} ({name:?}) carries a mesh inochi2d could not draw: {detail}")]
    MalformedMesh {
        id: String,
        name: String,
        detail: String,
    },

    /// A finite param range whose minimum sits above its maximum. What the
    /// source runtime does with one is unclear, and guessing would move the
    /// rig, so the import refuses it by name rather than pick a reading.
    #[error("param {id} ({name:?}) is authored backwards: min {min} is above max {max}")]
    InvertedParamRange {
        id: String,
        name: String,
        min: f32,
        max: f32,
    },

    /// A part naming a texture slot the rig does not carry. There is no
    /// defined rendering to preserve and no repair that would not guess, so
    /// the import stops and names the part. (`uint.max` never lands here: it
    /// is the source runtime's "no texture" sentinel, repaired to none.)
    #[error("part {id} ({name:?}) draws texture slot {slot}, but the rig carries {count}")]
    TextureOutOfRange {
        id: String,
        name: String,
        slot: i64,
        count: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_messages_name_their_context() {
        let e = ImportError::MissingField("uuid");
        assert!(e.to_string().contains("uuid"));

        let e = ImportError::InvalidFieldType {
            field: "zsort",
            expected: "number",
            got: "array".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("zsort") && msg.contains("number") && msg.contains("array"));

        let e = ImportError::LoadLimit(catchlight_core::load_budget::LoadLimitError {
            resource: "texture",
            limit: 64_000_000,
            got: 99_999_999,
        });
        let msg = e.to_string();
        assert!(msg.contains("texture") && msg.contains("64000000") && msg.contains("99999999"));
    }
}
