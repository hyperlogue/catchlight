//! CLI shortcuts lower to the same authoring request as JSON. Model-dependent
//! mask-source shortcuts become explicit edges before ordinary validation.

use super::spec::{
    self, AnimationRef, AnimationSource, Background, Document, Frames, MaskEdge,
    MotionResponseMode, Request, Setting, Video, VideoFormat,
};
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
pub enum MotionResponsesArg {
    Off,
    On,
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
    /// Repeatable humanoid control values over the model's control mappings.
    #[arg(long, allow_hyphen_values = true, value_name = "name=value")]
    pub control: Vec<String>,
    /// Repeatable weights, 0..1, of the model's expressions.
    #[arg(long, value_name = "name=weight")]
    pub expression: Vec<String>,
    /// Opaque white (default) or transparent black.
    #[arg(long, value_enum)]
    pub background: Option<BackgroundArg>,
    /// Enable or disable motion responses (default on).
    #[arg(long, value_enum)]
    pub motion_responses: Option<MotionResponsesArg>,
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
        if let Some(motion_responses) = self.motion_responses {
            request.motion_responses = Setting::Value(match motion_responses {
                MotionResponsesArg::Off => MotionResponseMode::Off {},
                MotionResponsesArg::On => MotionResponseMode::default(),
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
        for (flag, values, setting) in [
            ("control", &self.control, &mut request.controls),
            ("expression", &self.expression, &mut request.expressions),
        ] {
            let mut named = BTreeMap::new();
            for text in values {
                let (name, value) = spec::parse_named(flag, text)?;
                if named.insert(name.clone(), value).is_some() {
                    return Err(spec::bad(format!("duplicate --{flag} {name}")));
                }
            }
            if !named.is_empty() {
                *setting = Setting::Value(named);
            }
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

/// Playback shortcuts for one model clip. Seconds select the export frames
/// whose start time lies in [from, to), on the clip's own timestep.
#[derive(Debug, Clone, Default, Args)]
#[group(id = "clip_settings", multiple = true)]
pub struct ClipArgs {
    /// Play this model clip, by display name. Motion responses run by default.
    #[arg(long)]
    pub animation: Option<String>,
    /// First captured playback time, in seconds. Earlier frames still simulate.
    #[arg(long, requires = "animation")]
    pub from: Option<f64>,
    /// End of capture, exclusive, in seconds. Default the clip length.
    #[arg(long, requires = "animation")]
    pub to: Option<f64>,
    /// Capture every Nth frame; a video plays at 1 / (timestep × N) fps.
    #[arg(long, requires = "animation")]
    pub every: Option<u32>,
}
impl ClipArgs {
    fn apply(&self, model: &Model, request: &mut Request) -> Result<(), Error> {
        let Some(name) = &self.animation else {
            return Ok(());
        };
        let clip = spec::model_clip(model, name)?;
        let step = f64::from(clip.timestep);
        if !step.is_finite() || step <= 0.0 {
            return Err(spec::bad(format!(
                "animation {name} has no positive timestep"
            )));
        }
        // Tolerate decimal seconds that land a hair past a frame boundary.
        let frame = |seconds: f64, flag: &str| {
            if !seconds.is_finite() || seconds < 0.0 {
                return Err(spec::bad(format!(
                    "--{flag} takes finite nonnegative seconds"
                )));
            }
            let f = (seconds / step - 1e-6).ceil().max(0.0);
            if f > f64::from(u32::MAX) {
                return Err(spec::bad(format!("--{flag} is too far into the clip")));
            }
            Ok(f as u32)
        };
        let start = self
            .from
            .map(|s| frame(s, "from"))
            .transpose()?
            .unwrap_or(0);
        let count = self.to.map(|s| frame(s, "to")).transpose()?;
        if count.is_some_and(|c| c <= start) {
            return Err(spec::bad("--to must leave at least one frame after --from"));
        }
        request.animation = Setting::Value(AnimationRef {
            source: AnimationSource::Model,
            name: name.clone(),
        });
        request.frames = Setting::Value(Frames {
            start,
            count,
            every: self.every.unwrap_or(1),
        });
        Ok(())
    }
}

/// Unified render entry points: every image flag lowers to the spec normalizer.
#[derive(Debug, Args)]
pub struct RenderArgs {
    /// Input .clm model. Schema discovery needs no file.
    #[arg(required_unless_present = "schema")]
    pub file: Option<PathBuf>,
    /// Named render requests JSON; rendering requires --out-dir.
    #[arg(long, conflicts_with_all = ["image_settings", "clip_settings", "out"])]
    pub spec: Option<PathBuf>,
    /// Expand settings and validate model references/work without GPU or writes.
    #[arg(long, conflicts_with_all = ["out", "out_dir"])]
    pub validate: bool,
    /// Write one PNG atomically, or with --animation one
    /// .mp4/.webm/.mkv/.gif encoded by ffmpeg.
    #[arg(short, long, conflicts_with = "out_dir")]
    pub out: Option<PathBuf>,
    /// Write flat named artifacts and a run manifest.
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
    /// Discover authoring or resolved planning schema, without a model or GPU.
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "spec",
        conflicts_with_all = ["file", "spec", "validate", "image_settings", "clip_settings", "out", "out_dir"])]
    pub schema: Option<SchemaKind>,
    #[command(flatten)]
    pub image: ImageArgs,
    #[command(flatten)]
    pub clip: ClipArgs,
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
            }?;
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
        } else {
            let mut request = self.image.request(&loaded.model)?;
            self.clip.apply(&loaded.model, &mut request)?;
            let format = self.out.as_deref().and_then(VideoFormat::from_extension);
            match (format, self.clip.animation.is_some()) {
                (Some(format), true) => {
                    request.video = Setting::Value(Video {
                        format,
                        keep_frames: false,
                    });
                }
                (Some(_), false) => {
                    return Err(spec::bad("video output requires --animation"));
                }
                (None, true) if self.out.is_some() => {
                    return Err(spec::bad(
                        "--animation writes .mp4, .webm, .mkv or .gif with --out, or frames with --out-dir",
                    ));
                }
                _ => {}
            }
            (Document::single(request), None)
        };
        let plan = document.resolve(&loaded.model)?;
        if self.validate {
            return print_json(&plan);
        }
        let output = self.out;
        if let Some(output) = &output {
            super::execute::check_output_path(file, output)?;
        }
        let destination = match (&output, &self.out_dir) {
            (Some(path), _) if VideoFormat::from_extension(path).is_some() => {
                super::execute::Destination::Video(path.clone())
            }
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
            return print_json(&super::artifacts::json_schema("bounds")?);
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
        let mut runtime = super::frame::FrameRuntime::new(&loaded.model, &request)?;
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
