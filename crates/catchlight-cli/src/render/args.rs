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
    Geometry,
    Trace,
    Run,
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

/// Unified render entry points: every image flag lowers to the spec normalizer.
#[derive(Debug, Args)]
pub struct RenderArgs {
    /// Input .clm model. Schema discovery needs no file.
    #[arg(required_unless_present = "schema")]
    pub file: Option<PathBuf>,
    /// Compatibility output: model.clm out.png [width height camera_height].
    #[arg(conflicts_with_all = ["schema", "validate", "spec", "image_settings", "out", "out_dir"])]
    pub legacy_out: Option<PathBuf>,
    #[arg(requires = "legacy_out")]
    pub width: Option<u32>,
    #[arg(requires = "width")]
    pub height: Option<u32>,
    #[arg(requires = "height")]
    pub camera_height: Option<f32>,
    /// Named render requests JSON; rendering requires --out-dir.
    #[arg(long, conflicts_with_all = ["image_settings", "out"])]
    pub spec: Option<PathBuf>,
    /// Expand settings and validate model references/work without GPU or writes.
    #[arg(long, conflicts_with_all = ["out", "out_dir"])]
    pub validate: bool,
    /// Write one PNG atomically.
    #[arg(short, long, conflicts_with = "out_dir")]
    pub out: Option<PathBuf>,
    /// Write flat named artifacts and a run manifest.
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
    /// Discover authoring or resolved planning schema, without a model or GPU.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "spec",
        conflicts_with_all = ["file", "legacy_out", "spec", "validate", "image_settings", "out", "out_dir"])]
    pub schema: Option<SchemaKind>,
    #[command(flatten)]
    pub image: ImageArgs,
}
impl RenderArgs {
    pub fn run(self) -> Result<(), Error> {
        if let Some(kind) = self.schema {
            let schema = match kind {
                SchemaKind::Spec => spec::schema(false),
                SchemaKind::Resolved => spec::schema(true),
                SchemaKind::Geometry => super::artifacts::json_schema("geometry"),
                SchemaKind::Trace => super::artifacts::json_schema("trace"),
                SchemaKind::Run => super::artifacts::json_schema("run"),
            };
            print_json(&schema)?;
            return Ok(());
        }
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| spec::bad("render requires a .clm model"))?;
        if self.spec.is_some() && !self.validate && self.out_dir.is_none() {
            return Err(spec::bad(
                "render --spec requires --out-dir, or use --validate",
            ));
        }
        let directory = self.out_dir.is_some();
        let loaded = super::execute::load(file, directory)?;
        let (document, spec_hash) = if let Some(path) = &self.spec {
            let bytes = spec::read_json(path)?;
            (
                Document::parse(&bytes)?,
                directory.then(|| super::execute::hash(&bytes)),
            )
        } else if self.legacy_out.is_some() {
            let framing = spec::Framing::legacy(
                self.width.unwrap_or(super::DEFAULT_WIDTH),
                self.height.unwrap_or(super::DEFAULT_HEIGHT),
                self.camera_height.unwrap_or(super::DEFAULT_CAMERA_HEIGHT),
            )?;
            (
                Document::single(Request {
                    rect: Setting::Value(framing.rect),
                    scale: Setting::Value(framing.scale),
                    ..Default::default()
                }),
                None,
            )
        } else {
            (Document::single(self.image.request(&loaded.model)?), None)
        };
        let plan = document.resolve(&loaded.model)?;
        if self.validate {
            return print_json(&plan);
        }
        let output = self.out.or(self.legacy_out);
        if let Some(output) = &output {
            super::execute::check_output_path(file, output)?;
        }
        let destination = match (&output, &self.out_dir) {
            (Some(path), _) => super::execute::Destination::Png(path.clone()),
            (_, Some(path)) => super::execute::Destination::Directory(path.clone()),
            _ => super::execute::Destination::Terminal,
        };
        let input = loaded
            .hash
            .map(|model_sha256| super::artifacts::InputIdentity {
                model_sha256,
                spec_sha256: spec_hash,
            });
        let cancel = super::execute::Cancellation::default();
        cancel.install()?;
        let result = super::execute::run(&loaded.model, plan, destination, input, &cancel)?;
        for listing in result.listings {
            println!("{listing}");
        }
        if let Some(path) = output {
            println!("wrote {}", path.display());
        }
        if let Some(dir) = self.out_dir {
            println!("wrote {}", dir.join("run.json").display());
        }
        Ok(())
    }
}

/// Bounds observes the same initialized/ticked request without creating a GPU.
#[derive(Debug, Args)]
pub struct BoundsArgs {
    #[arg(required_unless_present = "schema")]
    pub file: Option<PathBuf>,
    /// Print the bounds output JSON Schema without loading a model.
    #[arg(long, conflicts_with_all = ["file", "spec", "request", "frame", "image_settings"])]
    pub schema: bool,
    #[arg(long, requires = "request", conflicts_with = "image_settings")]
    pub spec: Option<PathBuf>,
    /// Resolve exactly this named request, including inheritance.
    #[arg(long, requires = "spec")]
    pub request: Option<String>,
    /// Required export frame index when the selected request is animated.
    #[arg(long, requires = "request")]
    pub frame: Option<u32>,
    #[command(flatten)]
    pub image: ImageArgs,
}
impl BoundsArgs {
    pub fn run(self) -> Result<(), Error> {
        if self.schema {
            return print_json(&super::artifacts::json_schema("bounds"));
        }
        let file = self
            .file
            .as_ref()
            .ok_or_else(|| spec::bad("bounds requires a .clm model"))?;
        let loaded = super::execute::load(file, false)?;
        let document = match &self.spec {
            Some(path) => Document::read(path)?,
            None => Document::single(self.image.request(&loaded.model)?),
        };
        let name = self.request.as_deref().unwrap_or("default");
        let request = document.resolve_one(&loaded.model, name)?;
        let frame = match (&request.animation, self.frame) {
            (Some(animation), Some(frame)) if frame < animation.frames.count => frame,
            (Some(_), Some(_)) => {
                return Err(spec::bad("--frame is outside the request's export range"))
            }
            (Some(_), None) => return Err(spec::bad("animated bounds requires --frame")),
            (None, Some(_)) => return Err(spec::bad("--frame requires an animated request")),
            (None, None) => 0,
        };
        let cancel = super::execute::Cancellation::default();
        cancel.install()?;
        let mut runtime =
            super::frame::FrameRuntime::new(&loaded.model, &request, || cancel.is_cancelled())?;
        for _ in 0..frame {
            if cancel.is_cancelled() {
                return Err(spec::bad("bounds cancelled"));
            }
            runtime.advance(&loaded.model);
        }
        print_json(&super::artifacts::BoundsOutput::observe(
            &request,
            &loaded.model,
            &runtime,
        )?)
    }
}
fn print_json(value: &impl serde::Serialize) -> Result<(), Error> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|e| spec::bad(e.to_string()))?
    );
    Ok(())
}
