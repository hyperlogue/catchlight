//! A catchlight [`Model`] as a bevy asset.
//!
//! Invariants this module enforces:
//!
//! - **The asset owns an `Arc<Model>`, never a bare `Model`.** The render
//!   world needs the model every frame — a [`crate::CatchlightPuppet`]'s cache
//!   is prepared and refreshed from it — and extracting it must not copy
//!   megabytes of textures. Cloning the `Arc` is what makes that free, and it
//!   is also what lets one loaded model back many puppets.
//! - **Loading never poses and never animates.** A file becomes a `Model` and
//!   nothing else; the pose, the drivers and the evaluated frame belong to the
//!   `Puppet` on the entity.
//! - **The format is decided by the path, then by the file's magic.** An asset
//!   path with no recognised extension still loads if the bytes name
//!   themselves, which is what a `.clm` served from an opaque URL needs.

use std::fmt;
use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext};
use bevy::prelude::*;
use catchlight_core::formats::InxModel;
use catchlight_core::importer::from_inx_model_to_legacy;
use catchlight_core::{Model, ModelFormat};

/// A loaded [`Model`], shared by every puppet animating it.
///
/// Clone it to hand the same model to another system; the clone shares the
/// model rather than copying it.
#[derive(Asset, TypePath, Clone)]
pub struct CatchlightModel {
    model: Arc<Model>,
}

impl CatchlightModel {
    /// Wrap a model that is already in memory — a fixture, an import, a model
    /// an editor just built.
    pub fn new(model: Model) -> Self {
        Self {
            model: Arc::new(model),
        }
    }

    /// The model itself. Read-only: an edit belongs to whoever owns the
    /// `Assets` entry, through [`Assets::get_mut`], and bumps the model's own
    /// generation so every puppet and cache rebakes.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The model behind its `Arc`, for a caller that has to hold it past the
    /// borrow of the asset — extraction into the render world, for one.
    /// Cloning it shares the model rather than copying it.
    pub fn shared(&self) -> &Arc<Model> {
        &self.model
    }
}

impl From<Model> for CatchlightModel {
    fn from(model: Model) -> Self {
        Self::new(model)
    }
}

impl fmt::Debug for CatchlightModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CatchlightModel")
            .field("generation", &self.model.generation())
            .field("nodes", &self.model.node_count())
            .field("params", &self.model.param_ids().len())
            .field("textures", &self.model.texture_ids().len())
            .finish_non_exhaustive()
    }
}

/// Loads `.clm` (and, for a one-time import, `.inx` / `.inp`) into a
/// [`CatchlightModel`].
#[derive(Default, TypePath)]
pub struct CatchlightModelLoader;

impl AssetLoader for CatchlightModelLoader {
    type Asset = CatchlightModel;
    type Settings = ();
    type Error = ModelAssetError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<CatchlightModel, ModelAssetError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(ModelAssetError::Read)?;
        let format = ModelFormat::from_path(load_context.path().path())
            .or_else(|| ModelFormat::sniff(&bytes))
            .ok_or(ModelAssetError::UnknownFormat)?;
        model_from_bytes(&bytes, format).map(CatchlightModel::new)
    }

    fn extensions(&self) -> &[&str] {
        &["clm", "inx", "inp"]
    }
}

/// Read model-file `bytes` of a known `format` into a [`Model`].
///
/// `.clm` is the one first-class path. A legacy `.inx` / `.inp` goes through
/// the importer's legacy document, which is the only route it still has —
/// convert one with `cargo xtask import` rather than shipping it as an asset.
pub fn model_from_bytes(bytes: &[u8], format: ModelFormat) -> Result<Model, ModelAssetError> {
    match format {
        ModelFormat::Clm => Model::from_clm_bytes(bytes).map_err(ModelAssetError::parse),
        ModelFormat::Inx | ModelFormat::Inp => {
            tracing::warn!(
                "loading a legacy inochi2d model as an asset is deprecated; \
                 convert it to .clm with `cargo xtask import`"
            );
            let inx =
                InxModel::parse(std::io::Cursor::new(bytes)).map_err(ModelAssetError::parse)?;
            let legacy = from_inx_model_to_legacy(&inx).map_err(ModelAssetError::parse)?;
            Model::from_legacy(&legacy).map_err(ModelAssetError::parse)
        }
    }
}

/// Why a model asset could not be loaded.
#[derive(Debug)]
pub enum ModelAssetError {
    /// The asset source could not be read.
    Read(std::io::Error),
    /// Neither the path's extension nor the file's leading bytes named a
    /// format catchlight can read.
    UnknownFormat,
    /// The bytes named a format but did not parse as one. The message is the
    /// reader's own, kept as text because the three readers report three
    /// unrelated error types and a caller can only log this.
    Parse(String),
}

impl ModelAssetError {
    fn parse(error: impl fmt::Display) -> Self {
        Self::Parse(error.to_string())
    }
}

impl fmt::Display for ModelAssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(f, "could not read the model asset: {error}"),
            Self::UnknownFormat => f.write_str(
                "not a catchlight model: the extension and the leading bytes name no known format",
            ),
            Self::Parse(message) => write!(f, "could not parse the model: {message}"),
        }
    }
}

impl std::error::Error for ModelAssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            _ => None,
        }
    }
}
