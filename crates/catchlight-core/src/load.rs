//! Loading a model file into a [`Puppet`], dispatched by format.
//!
//! catchlight's first-class on-disk format is **`.clp`** (the editable source of
//! truth). The legacy `.inx` / `.inp` formats are kept only as a one-time import
//! path — convert a rig to `.clp` with `cargo xtask import` and load that. These
//! functions are byte-based (no filesystem), so they work on wasm too; callers
//! read the bytes and tag the format from the file extension.

use std::io::Cursor;
use std::path::Path;

use crate::formats::{clp, InxModel};
use crate::importer::{from_inx_model_downsampled, parse_inp};
use crate::puppet::Puppet;
use crate::{from_clp, ImportError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFormat {
    /// catchlight's editable source-of-truth format.
    Clp,
    /// Legacy export (deprecated load path — import to `.clp` instead).
    Inx,
    /// Legacy puppet (deprecated load path — import to `.clp` instead).
    Inp,
}

impl ModelFormat {
    /// Infer the format from a path's extension (case-insensitive). `None` for
    /// any other extension.
    pub fn from_path(path: &Path) -> Option<ModelFormat> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "clp" => Some(ModelFormat::Clp),
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
        if bytes.starts_with(&clp::MAGIC[..]) {
            Some(ModelFormat::Clp)
        } else if bytes.starts_with(INX_MAGIC) {
            Some(ModelFormat::Inx)
        } else {
            None
        }
    }
}

/// Build a [`Puppet`] from model-file `bytes` of the given `format`, downsampling
/// each texture by `texture_halvings` power-of-two steps (0 = full resolution).
pub fn load_model(
    bytes: &[u8],
    format: ModelFormat,
    texture_halvings: u32,
) -> Result<Puppet, ImportError> {
    match format {
        ModelFormat::Clp => {
            let file = clp::decode(bytes)?;
            from_clp(&file, texture_halvings)
        }
        ModelFormat::Inx => {
            tracing::warn!(
                "loading a .inx directly is deprecated; convert it to .clp with `cargo xtask import`"
            );
            let model = InxModel::parse(Cursor::new(bytes))?;
            from_inx_model_downsampled(&model, texture_halvings)
        }
        ModelFormat::Inp => {
            tracing::warn!(
                "loading a .inp directly is deprecated; convert it to .clp with `cargo xtask import`"
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
        assert_eq!(ModelFormat::sniff(&clp::MAGIC[..]), Some(ModelFormat::Clp));
        assert_eq!(ModelFormat::sniff(b"TRNSRTS\0junk"), Some(ModelFormat::Inx));
        assert_eq!(ModelFormat::sniff(b"nope"), None);
        assert_eq!(ModelFormat::sniff(&[]), None);
    }
}
