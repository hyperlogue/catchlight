//! Loading a model file into a [`LegacyPuppet`], dispatched by format.
//!
//! catchlight's first-class on-disk format is **`.clm`** (the editable source
//! of truth). The legacy `.inx` / `.inp` formats are kept only as a one-time
//! import path — convert a model to `.clm` with `cargo xtask import` and load
//! that. These functions are byte-based (no filesystem), so they work on wasm
//! too; callers read the bytes and tag the format from the file extension.
//!
//! **The load path is `.clm` bytes -> [`Model`] -> legacy document -> puppet.**
//! The file is only ever read into a Model — that is where the format's Ids,
//! scalar params and seams are checked — and the runtime's dense arena is
//! built from the Model's own arena projection
//! (`crates/catchlight-core/src/model/legacy.rs`). Nothing reads a model file
//! into a puppet directly, so there is one reader to keep honest.

use std::io::Cursor;
use std::path::Path;

use crate::formats::{clm, InxModel};
use crate::importer::{from_inx_model_downsampled, parse_inp};
use crate::legacy_puppet::LegacyPuppet;
use crate::model::Model;
use crate::{from_legacy, ImportError};

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

/// Build a [`LegacyPuppet`] from model-file `bytes` of the given `format`, downsampling
/// each texture by `texture_halvings` power-of-two steps (0 = full resolution).
pub fn load_model(
    bytes: &[u8],
    format: ModelFormat,
    texture_halvings: u32,
) -> Result<LegacyPuppet, ImportError> {
    match format {
        ModelFormat::Clm => {
            let model = Model::from_clm_bytes(bytes)?;
            from_legacy(&model.to_legacy()?, texture_halvings)
        }
        ModelFormat::Inx => {
            tracing::warn!(
                "loading a .inx directly is deprecated; convert it to .clm with `cargo xtask import`"
            );
            let model = InxModel::parse(Cursor::new(bytes))?;
            from_inx_model_downsampled(&model, texture_halvings)
        }
        ModelFormat::Inp => {
            tracing::warn!(
                "loading a .inp directly is deprecated; convert it to .clm with `cargo xtask import`"
            );
            // .inp is unused in-tree; the importer path ignores texture_halvings.
            parse_inp(bytes)
        }
    }
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
