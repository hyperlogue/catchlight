//! Loading a model file into a [`Model`], dispatched by format.
//!
//! catchlight's first-class on-disk format is **`.clm`** (the editable source
//! of truth). The legacy `.inx` / `.inp` formats are kept only as a one-time
//! import path — convert a model to `.clm` with `cargo xtask import` and load
//! that. These functions are byte-based (no filesystem), so they work on wasm
//! too; callers read the bytes and tag the format from the file extension.
//!
//! **The load path is `.clm` bytes -> [`Model`].** The file is only ever read
//! into a Model — that is where the format's Ids, scalar params and seams are
//! checked — and whatever animates or draws it is derived from there:
//! [`crate::Puppet::new`] bakes the runtime's dense arena, and a render cache
//! is prepared alongside it. Nothing reads a model file into anything else, so
//! there is one reader to keep honest.

use std::io::Cursor;
use std::path::Path;

use crate::formats::clm;
use crate::formats::inx::InxModel;
use crate::importer::from_inx_model_to_legacy;
use crate::model::Model;
use crate::ImportError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    /// catchlight's editable source-of-truth format.
    Clm,
    /// Legacy export (deprecated load path — import to `.clm` instead).
    Inx,
    /// Legacy puppet (deprecated load path — import to `.clm` instead).
    Inp,
}

impl ModelFormat {
    /// Infer the format from a path's extension (case-insensitive). `None` for
    /// any other extension.
    pub fn from_path(path: &Path) -> Option<ModelFormat> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "clm" => Some(ModelFormat::Clm),
            "inx" => Some(ModelFormat::Inx),
            "inp" => Some(ModelFormat::Inp),
            _ => None,
        }
    }

    /// Detect the format from a file's leading magic bytes, for callers that
    /// only have bytes and no path (a `fetch()` in the wasm host, a hand-in from
    /// JS). `.inx` and `.inp` share the `TRNSRTS\0` container magic, so
    /// both report as [`ModelFormat::Inx`] (the parse path is identical).
    pub fn sniff(bytes: &[u8]) -> Option<ModelFormat> {
        const INX_MAGIC: &[u8] = b"TRNSRTS\0";
        if bytes.starts_with(&clm::MAGIC[..]) {
            Some(ModelFormat::Clm)
        } else if bytes.starts_with(INX_MAGIC) {
            Some(ModelFormat::Inx)
        } else {
            None
        }
    }
}

/// Read model-file `bytes` of the given `format` into a [`Model`].
///
/// `.clm` is read directly; a legacy `.inx` / `.inp` goes through the
/// importer's arena document, which is the one path either still has.
/// Textures arrive source-encoded, exactly as the file stores them — a render
/// cache is what decodes and downsamples them, so there is no texture budget
/// here.
pub fn load_model(bytes: &[u8], format: ModelFormat) -> Result<Model, ImportError> {
    let inx = match format {
        ModelFormat::Clm => return Ok(Model::from_clm_bytes(bytes)?),
        ModelFormat::Inx => InxModel::parse(Cursor::new(bytes))?,
        ModelFormat::Inp => crate::formats::inx::parse_inp(Cursor::new(bytes))?,
    };
    tracing::warn!(
        "loading a .inx/.inp directly is deprecated; convert it to .clm with `cargo xtask import`"
    );
    Ok(Model::from_legacy(&from_inx_model_to_legacy(&inx)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_detects_magics() {
        assert_eq!(ModelFormat::sniff(&clm::MAGIC[..]), Some(ModelFormat::Clm));
        assert_eq!(ModelFormat::sniff(b"TRNSRTS\0junk"), Some(ModelFormat::Inx));
        assert_eq!(ModelFormat::sniff(b"nope"), None);
        assert_eq!(ModelFormat::sniff(&[]), None);
    }
}
