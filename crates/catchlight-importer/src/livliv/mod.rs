//! LivLiv format 4 package validation and conversion.
//!
//! The upstream contract is pinned to [`UPSTREAM_REVISION`]. Format 3 is
//! refused explicitly: its omitted rights information cannot be inferred.
//! Package data is never executable and resource paths never resolve URLs.
//! Source metadata and compatibility findings must travel with a conversion.

mod convert;
mod package;
pub use convert::{Conversion, Options, Report};

pub use package::Package;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Revision used for schema validation and conversion semantics.
pub const UPSTREAM_REVISION: &str = "d999dea88981124168f9e9ab948eda8d2f23c453";

type Result<T> = std::result::Result<T, Error>;

/// A rejected package or conversion, with the failing resource named.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid converted model: {0}")]
    Model(#[from] catchlight_core::model::ModelError),
    #[error("invalid converted id: {0}")]
    Id(#[from] catchlight_core::id::IdError),
    #[error("invalid LivLiv package: {0}")]
    Invalid(String),
    #[error("LivLiv IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("LivLiv ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("LivLiv JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("LivLiv image: {0}")]
    Image(#[from] image::ImageError),
}

/// Validated manifest and encoded resources. Accessors do not permit mutation
/// after validation; callers cannot bypass the resource and reference checks.
#[derive(Debug)]
pub struct ValidatedPackage {
    package: Package,
    manifest: Value,
    model: Manifest,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    canvas: [u32; 2],
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    expressions: Vec<Expression>,
    layers: Vec<Layer>,
}
#[derive(Debug, Deserialize)]
struct Expression {
    id: String,
    hotkey: Option<String>,
}
#[derive(Debug, Deserialize)]
struct Layer {
    id: String,
    texture: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    #[serde(default)]
    frames: Vec<String>,
    #[serde(default)]
    visemes: Vec<String>,
    mask: Option<String>,
    talk: Option<String>,
    brow_tex: Option<String>,
    clip: Option<String>,
    expression: Option<String>,
    role: Option<String>,
    #[serde(default)]
    bones: Vec<String>,
    #[serde(default)]
    blush: BTreeMap<String, f32>,
    #[serde(default)]
    show: BTreeMap<String, f32>,
    #[serde(default)]
    hide: BTreeMap<String, f32>,
}

impl Package {
    /// Validate the pinned v4 schema, references, resource presence and image
    /// dimensions before any conversion. All referenced images are decoded,
    /// including alternate mouth/expression frames. Total decoded RGBA is
    /// capped at 512 MiB; each image is capped by its layer before decoding.
    pub fn validate(self) -> Result<ValidatedPackage> {
        let bytes = self.get("model.json")?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(Error::Invalid("model.json exceeds 4 MiB".into()));
        }
        let manifest: Value = serde_json::from_slice(bytes)?;
        if manifest["format"] != "livliv-model" || manifest["version"] != 4 {
            return Err(Error::Invalid(
                "only livliv-model version 4 is supported; re-export legacy packages".into(),
            ));
        }
        validate_schema(&manifest, include_str!("model-v4.json"), "model.json")?;
        if let Some(required) = manifest["requires"].as_array().filter(|r| !r.is_empty()) {
            return Err(Error::Invalid(format!(
                "unsupported required features: {required:?}"
            )));
        }
        let model: Manifest = serde_json::from_value(manifest.clone())?;
        let ids = unique(model.layers.iter().map(|l| l.id.as_str()), "layer id")?;
        let expressions = unique(
            model.expressions.iter().map(|e| e.id.as_str()),
            "expression id",
        )?;
        unique(
            model.expressions.iter().filter_map(|e| e.hotkey.as_deref()),
            "expression hotkey",
        )?;
        let chains = manifest["rig"]["chains"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let chain_ids = unique(chains.iter().filter_map(|c| c["id"].as_str()), "chain id")?;
        for chain in chains {
            if let Some(bias) = chain["bias"].as_object() {
                for id in bias.keys() {
                    reference(&expressions, id, "chain bias")?;
                }
            }
        }
        let mut dimensions = BTreeMap::new();
        let mut decoded = 0u64;
        for layer in &model.layers {
            if layer.x as u64 + layer.w as u64 > model.canvas[0] as u64
                || layer.y as u64 + layer.h as u64 > model.canvas[1] as u64
            {
                return Err(Error::Invalid(format!(
                    "layer {} lies outside canvas",
                    layer.id
                )));
            }
            if let Some(id) = &layer.clip {
                reference(&ids, id, "clip")?;
                if id == &layer.id {
                    return Err(Error::Invalid(format!("self clip: {id}")));
                }
            }
            if let Some(id) = &layer.expression {
                reference(&expressions, id, "expression")?;
            }
            for id in layer
                .blush
                .keys()
                .chain(layer.show.keys())
                .chain(layer.hide.keys())
            {
                reference(&expressions, id, "opacity expression")?;
            }
            unique(layer.bones.iter().map(String::as_str), "bone reference")?;
            for id in &layer.bones {
                reference(&chain_ids, id, "bone")?;
            }
            if layer.frames.len() != layer.visemes.len()
                || (layer.role.as_deref() == Some("mouth") && layer.frames.len() < 3)
            {
                return Err(Error::Invalid(format!(
                    "layer {} has inconsistent mouth frames/visemes",
                    layer.id
                )));
            }
            unique(layer.visemes.iter().map(String::as_str), "viseme")?;
            for name in std::iter::once(&layer.texture)
                .chain(&layer.frames)
                .chain(layer.mask.iter())
                .chain(layer.talk.iter())
                .chain(layer.brow_tex.iter())
            {
                let dims = (layer.w, layer.h);
                if let Some(previous) = dimensions.insert(name.clone(), dims) {
                    if previous != dims {
                        return Err(Error::Invalid(format!("conflicting dimensions for {name}")));
                    }
                    continue;
                }
                decoded = decoded.saturating_add(layer.w as u64 * layer.h as u64 * 4);
                if decoded > 512 * 1024 * 1024 {
                    return Err(Error::Invalid("decoded images exceed 512 MiB".into()));
                }
                decode_image(self.get(name)?, name, dims)?;
            }
        }
        for (kind, name) in &model.files {
            let bytes = self.get(name)?;
            if kind == "keyforms" {
                if bytes.len() > 32 * 1024 * 1024 {
                    return Err(Error::Invalid("keyforms exceed 32 MiB".into()));
                }
                let keys: Value = serde_json::from_slice(bytes)?;
                validate_schema(&keys, include_str!("keyforms-v1.json"), name)?;
                validate_keyforms(&keys, &ids)?;
            }
        }
        Ok(ValidatedPackage {
            package: self,
            manifest,
            model,
        })
    }
}

impl ValidatedPackage {
    /// Original manifest, including author, license, watermark and extensions.
    pub fn manifest(&self) -> &Value {
        &self.manifest
    }
    /// Character name as plain text.
    pub fn name(&self) -> &str {
        &self.model.name
    }
    /// Number of source layers, before any part splitting.
    pub fn layer_count(&self) -> usize {
        self.model.layers.len()
    }
    /// Exact source resource bytes after path and existence validation.
    pub fn resource(&self, name: &str) -> Result<&[u8]> {
        self.package.get(name)
    }
}

fn validate_schema(value: &Value, schema: &str, name: &str) -> Result<()> {
    let schema: Value = serde_json::from_str(schema)?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|e| Error::Invalid(format!("bundled schema: {e}")))?;
    validator
        .validate(value)
        .map_err(|e| Error::Invalid(format!("{name} {}: {e}", e.instance_path())))
}

fn unique<'a>(values: impl Iterator<Item = &'a str>, kind: &str) -> Result<BTreeSet<&'a str>> {
    let mut found = BTreeSet::new();
    for value in values {
        if !found.insert(value) {
            return Err(Error::Invalid(format!("duplicate {kind}: {value}")));
        }
    }
    Ok(found)
}
fn reference(ids: &BTreeSet<&str>, id: &str, kind: &str) -> Result<()> {
    if !ids.contains(id) {
        return Err(Error::Invalid(format!("unknown {kind}: {id}")));
    }
    Ok(())
}

fn decode_image(bytes: &[u8], name: &str, dims: (u32, u32)) -> Result<image::DynamicImage> {
    let format = image::guess_format(bytes)?;
    let expected = if name.ends_with(".png") {
        image::ImageFormat::Png
    } else if name.ends_with(".webp") {
        image::ImageFormat::WebP
    } else {
        return Err(Error::Invalid(format!("unsupported image: {name}")));
    };
    if format != expected {
        return Err(Error::Invalid(format!(
            "image extension disagrees with content: {name}"
        )));
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(dims.0);
    limits.max_image_height = Some(dims.1);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| Error::Invalid(format!("{name}: {e}")))?;
    if image.width() != dims.0 || image.height() != dims.1 {
        return Err(Error::Invalid(format!(
            "image dimensions disagree with layer: {name}"
        )));
    }
    Ok(image)
}

fn validate_keyforms(keys: &Value, layers: &BTreeSet<&str>) -> Result<()> {
    use base64::Engine;
    let cells = keys["cols"]
        .as_u64()
        .unwrap_or(u64::MAX)
        .saturating_mul(keys["rows"].as_u64().unwrap_or(u64::MAX));
    if cells > 2_097_152 {
        return Err(Error::Invalid(
            "keyform lattice exceeds 2097152 points".into(),
        ));
    }
    let field = |group: &str, value: &Value, bytes_per_cell: u64| -> Result<()> {
        if !matches!(group, "head" | "hair" | "torso") && !layers.contains(group) {
            return Err(Error::Invalid(format!("unknown keyform group: {group}")));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value.as_str().unwrap_or_default())
            .map_err(|e| Error::Invalid(format!("keyform {group}: {e}")))?;
        if bytes.len() as u64 != cells * bytes_per_cell {
            return Err(Error::Invalid(format!(
                "keyform {group}: incorrect field length"
            )));
        }
        Ok(())
    };
    if let Some(entries) = keys["mask"].as_object() {
        for (group, value) in entries {
            field(group, value, 1)?;
        }
    }
    if let Some(poses) = keys["keys"].as_array() {
        for pose in poses {
            if let Some(entries) = pose["fields"].as_object() {
                for (group, value) in entries {
                    field(group, value, 4)?;
                }
            }
            if let Some(same) = pose["same"].as_array() {
                for group in same {
                    let group = group.as_str().unwrap_or_default();
                    if pose["fields"]["head"].is_null() {
                        return Err(Error::Invalid("keyform same requires a head field".into()));
                    }
                    field(group, &pose["fields"]["head"], 4)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
