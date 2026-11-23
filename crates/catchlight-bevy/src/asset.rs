//! A catchlight [`Model`] as a bevy asset.

use std::fmt;
use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext};
use bevy::prelude::*;
use catchlight_core::{Model, ModelFormat};

/// A loaded [`Model`], shared by every puppet animating it.
#[derive(Asset, TypePath, Clone)]
pub struct CatchlightModel {
    model: Arc<Model>,
}

impl CatchlightModel {
    pub fn new(model: Model) -> Self {
        Self {
            model: Arc::new(model),
        }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Copy-on-write, so the frame in flight keeps the model it started with.
    pub fn model_mut(&mut self) -> &mut Model {
        Arc::make_mut(&mut self.model)
    }

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

/// Loads a `.clm` into a [`CatchlightModel`].
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
        &["clm"]
    }
}

pub fn model_from_bytes(bytes: &[u8], format: ModelFormat) -> Result<Model, ModelAssetError> {
    catchlight_core::load_model(bytes, format).map_err(ModelAssetError::parse)
}

#[derive(Debug)]
pub enum ModelAssetError {
    Read(std::io::Error),
    UnknownFormat,
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
