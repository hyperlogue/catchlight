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
