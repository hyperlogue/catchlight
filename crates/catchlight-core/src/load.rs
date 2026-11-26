//! Loading a model file into a [`Model`].

use std::path::Path;

use crate::formats::clm;
use crate::model::{LoadError, Model};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    Clm,
}

impl ModelFormat {
    /// Case-insensitive. `.inx` / `.inp` are imported, not loaded.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "clm" => Some(Self::Clm),
            _ => None,
        }
    }

    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        bytes.starts_with(&clm::MAGIC[..]).then_some(Self::Clm)
    }
}

/// Textures stay source-encoded; a render cache decodes and downsamples them.
pub fn load_model(bytes: &[u8], format: ModelFormat) -> Result<Model, LoadError> {
    match format {
        ModelFormat::Clm => Model::from_clm_bytes(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_detects_the_clm_magic_and_nothing_else() {
        assert_eq!(ModelFormat::sniff(&clm::MAGIC[..]), Some(ModelFormat::Clm));
        assert_eq!(ModelFormat::sniff(b"TRNSRTS\0junk"), None);
        assert_eq!(ModelFormat::sniff(b"nope"), None);
        assert_eq!(ModelFormat::sniff(&[]), None);
    }

    #[test]
    fn only_clm_is_a_loadable_extension() {
        assert_eq!(
            ModelFormat::from_path(Path::new("a/b.clm")),
            Some(ModelFormat::Clm)
        );
        assert_eq!(
            ModelFormat::from_path(Path::new("a/b.CLM")),
            Some(ModelFormat::Clm)
        );
        assert_eq!(ModelFormat::from_path(Path::new("a/b.inx")), None);
        assert_eq!(ModelFormat::from_path(Path::new("a/b.inp")), None);
    }
}
