//! Loading a model file into a [`Model`].
//!
//! **The load path is `.clm` bytes -> [`Model`], and there is no other.**
//! `.clm` is catchlight's own format and the only one this crate reads: an
//! inochi2d `.inx` / `.inp` is converted once, by `cargo xtask import`
//! (`catchlight-import-inochi2d`), and the `.clm` is what ships. Nothing here
//! knows that format exists.
//!
//! The file is only ever read into a Model — that is where the format's Ids,
//! scalar params and slots are checked — and whatever animates or draws it is
//! derived from there: [`crate::Puppet::new`] bakes the runtime's dense arena,
//! and a render cache is prepared alongside it. So there is one reader to
//! keep honest.
//!
//! These functions are byte-based (no filesystem), so they work on wasm too;
//! callers read the bytes and tag the format from the file extension or the
//! magic.

use std::path::Path;

use crate::formats::clm;
use crate::model::{Model, ModelError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    /// catchlight's editable source-of-truth format — the only one there is.
    Clm,
}

impl ModelFormat {
    /// Infer the format from a path's extension (case-insensitive). `None` for
    /// any other extension, including `.inx` / `.inp`: those are imported, not
    /// loaded.
    pub fn from_path(path: &Path) -> Option<ModelFormat> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "clm" => Some(ModelFormat::Clm),
            _ => None,
        }
    }

    /// Detect the format from a file's leading magic bytes, for callers that
    /// only have bytes and no path (a `fetch()` in the wasm host, a hand-in
    /// from JS).
    pub fn sniff(bytes: &[u8]) -> Option<ModelFormat> {
        bytes
            .starts_with(&clm::MAGIC[..])
            .then_some(ModelFormat::Clm)
    }
}

/// Read model-file `bytes` of the given `format` into a [`Model`].
///
/// Textures arrive source-encoded, exactly as the file stores them — a render
/// cache is what decodes and downsamples them, so there is no texture budget
/// here.
pub fn load_model(bytes: &[u8], format: ModelFormat) -> Result<Model, ModelError> {
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
