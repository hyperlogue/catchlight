//! CLI shortcuts lower to the same authoring request as JSON. Model-dependent
//! mask-source shortcuts become explicit edges before ordinary validation.

use super::spec::{self, Background, Document, MaskEdge, Physics, Request, Setting};
use crate::Error;
use catchlight_core::{Model, ModelNodeKind, NodeId};
use clap::{Args, ValueEnum};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SchemaKind {
    Spec,
    Resolved,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BackgroundArg {
    White,
    Transparent,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PhysicsArg {
    Off,
    Settled,
}

/// Per-image flags have identical defaults and validation to JSON requests.
#[derive(Debug, Clone, Default, Args)]
#[group(id = "image_settings", multiple = true)]
pub struct ImageArgs {
    /// Retain only these exact Part colors, preserving mask contributions.
    #[arg(long, value_delimiter = ',')]
    pub keep: Option<Vec<String>>,
    /// Remove matching mask edges on retained Parts. Unknown sources fail.
    #[arg(long, value_delimiter = ',')]
    pub strip_masks: Vec<String>,
    /// World-space minimum x,y,width,height; world Y is up.
    #[arg(long, allow_hyphen_values = true, value_name = "x0,y0,w,h")]
    pub rect: Option<String>,
    /// Positive output pixels per world unit, default 0.32.
    #[arg(long)]
    pub scale: Option<f32>,
    /// Repeatable param=value overrides over model defaults.
    #[arg(long, allow_hyphen_values = true, value_name = "param=value")]
    pub set: Vec<String>,
    /// Opaque white (default) or transparent black.
    #[arg(long, value_enum)]
    pub background: Option<BackgroundArg>,
    /// Static physics policy, default settled.
    #[arg(long, value_enum)]
    pub physics: Option<PhysicsArg>,
}
impl ImageArgs {
    pub fn request(&self, model: &Model) -> Result<Request, Error> {
        let mut request = Request::default();
        if let Some(rect) = &self.rect {
            request.rect = Setting::Value(spec::parse_rect(rect)?);
        }
        if let Some(scale) = self.scale {
            request.scale = Setting::Value(scale);
        }
        if let Some(background) = self.background {
            request.background = Setting::Value(match background {
                BackgroundArg::White => Background::default(),
                BackgroundArg::Transparent => Background {
                    srgb: [0.0; 3],
                    alpha: 0.0,
                },
            });
        }
        if let Some(physics) = self.physics {
            request.physics = Setting::Value(match physics {
                PhysicsArg::Off => Physics::Off {},
                PhysicsArg::Settled => Physics::Settled {},
            });
        }
        let mut pose = BTreeMap::new();
        for value in &self.set {
            let (id, value) = spec::parse_set(value)?;
            if pose.insert(id.clone(), value).is_some() {
                return Err(spec::bad(format!("duplicate --set parameter {id}")));
            }
        }
        if !pose.is_empty() {
            request.pose = Setting::Value(pose);
        }
        let only_parts = self
            .keep
            .as_ref()
            .map(|names| {
                names
                    .iter()
                    .map(|id| NodeId::new(id).map_err(|e| spec::bad(e.to_string())))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        if let Some(keep) = &only_parts {
            for id in keep {
                spec::part(model, id)?;
            }
        }
        let sources: BTreeSet<NodeId> = self
            .strip_masks
            .iter()
            .map(|s| {
                let id = NodeId::new(s).map_err(|e| spec::bad(e.to_string()))?;
                if !matches!(
                    model.node(&id).map(|n| &n.kind),
                    Some(ModelNodeKind::Part(_) | ModelNodeKind::Composite(_))
                ) {
                    return Err(spec::bad(format!("{id} is not a mask source")));
                }
                Ok(id)
            })
            .collect::<Result<_, _>>()?;
        if sources.len() != self.strip_masks.len() {
            return Err(spec::bad("duplicate --strip-masks source"));
        }
        let mut edges = Vec::new();
        for id in model.nodes_in_order() {
            if only_parts.as_ref().is_some_and(|keep| !keep.contains(&id)) {
                continue;
            }
            let Some(ModelNodeKind::Part(part)) = model.node(&id).map(|n| &n.kind) else {
                continue;
            };
            for mask in part.masks() {
                if sources.contains(mask.source()) {
                    edges.push(MaskEdge {
                        node: id.clone(),
                        source: mask.source().clone(),
                    });
                }
            }
        }
        edges.sort();
        edges.dedup();
        if !edges.is_empty() {
            request.strip_masks = Setting::Value(edges);
        }
        if let Some(only_parts) = only_parts {
            request.only_parts = Setting::Value(only_parts);
        }
        Ok(request)
    }
}

/// Render planning entry points. Positional rendering remains usable while
/// subsequent rendering slices share this normalizer and replace execution.
#[derive(Debug, Args)]
pub struct RenderArgs {
    /// Input .clm model. Schema discovery needs no file.
    #[arg(required_unless_present = "schema")]
    pub file: Option<PathBuf>,
    /// Compatibility output: model.clm out.png [width height camera_height].
    #[arg(conflicts_with_all = ["schema", "validate", "spec", "image_settings"])]
    pub legacy_out: Option<PathBuf>,
    #[arg(requires = "legacy_out")]
    pub width: Option<u32>,
    #[arg(requires = "width")]
    pub height: Option<u32>,
    #[arg(requires = "height")]
    pub camera_height: Option<f32>,
    /// Named render requests JSON. Planning currently requires --validate.
    #[arg(long, requires = "validate", conflicts_with = "image_settings")]
    pub spec: Option<PathBuf>,
    /// Expand settings and validate model references/work without GPU or writes.
    #[arg(long)]
    pub validate: bool,
    /// Discover authoring or resolved planning schema, without a model or GPU.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "spec",
        conflicts_with_all = ["file", "legacy_out", "spec", "validate", "image_settings"])]
    pub schema: Option<SchemaKind>,
    #[command(flatten)]
    pub image: ImageArgs,
}
impl RenderArgs {
    pub fn run(self) -> Result<(), Error> {
        if let Some(schema) = self.schema {
            println!(
                "{}",
                serde_json::to_string_pretty(&spec::schema(matches!(schema, SchemaKind::Resolved)))
                    .map_err(|e| spec::bad(e.to_string()))?
            );
            return Ok(());
        }
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| spec::bad("render requires a .clm model"))?;
        if let Some(out) = &self.legacy_out {
            let width = self.width.unwrap_or(super::DEFAULT_WIDTH);
            let height = self.height.unwrap_or(super::DEFAULT_HEIGHT);
            let camera_height = self.camera_height.unwrap_or(super::DEFAULT_CAMERA_HEIGHT);
            spec::Framing::legacy(width, height, camera_height)?;
            println!("{}", super::run(file, out, width, height, camera_height)?);
            return Ok(());
        }
        if !self.validate {
            return Err(spec::bad(
                "use --validate to inspect a request, or provide a positional PNG output",
            ));
        }
        let model = crate::file::load_model(file)?;
        let document = match &self.spec {
            Some(path) => Document::read(path)?,
            None => Document::single(self.image.request(&model)?),
        };
        let resolved = document.resolve(&model)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&resolved).map_err(|e| spec::bad(e.to_string()))?
        );
        Ok(())
    }
}
