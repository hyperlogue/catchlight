//! GPU-free render planning. One named request schema serves JSON and CLI
//! shortcuts. Inheritance replaces whole settings; JSON null resets a setting
//! to its context-dependent default. Each named request remains an independent
//! execution, including requests used as bases.
//!
//! Validation precedes GPU creation and artifact writes. All input keys are
//! unique, every model reference is checked, and output names/work are bounded.
//! Animations remain core `ClmAnimation` values and use core validation; this
//! module does not introduce a second animation evaluator.

mod animation;
mod json;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use catchlight_core::{Model, ModelNodeKind, NodeId, ParamId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Error;

pub use animation::NativeAnimation;
pub use json::{decode_json, read_json};

/// Schema adapter for the charset checked by core's NodeId/ParamId.
struct ModelIdSchema;
impl JsonSchema for ModelIdSchema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ModelId".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type":"string", "pattern":"^[A-Za-z0-9_][A-Za-z0-9_./-]*$"})
    }
}

pub const MAX_JSON_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_REQUESTS: usize = 4096;
pub const MAX_RESOLVED_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ANIMATIONS: usize = 4096;
pub const MAX_UPDATES: u64 = 100_000;
pub const MAX_DIMENSION: u32 = 8192;
pub const MAX_PIXELS: u64 = 16_777_216;
pub const MAX_RUN_PIXELS: u64 = 2_000_000_000;
pub const MAX_OUTPUTS: u64 = 100_000;
pub const MAX_GEOMETRY_VERTICES: u64 = 10_000_000;
pub const MAX_TRACE_VALUES: u64 = 10_000_000;
pub const MAX_OVERLAY_WORK: u64 = 250_000_000;
pub const MAX_NAME_BYTES: usize = 100;
pub const DEFAULT_RECT: [f32; 4] = [-1500.0, -2500.0, 3000.0, 5000.0];
pub const DEFAULT_SCALE: f32 = 0.32;

/// Resource limits apply to the complete expanded run, before allocation.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Maximum encoded model-file bytes, shared with core's loading budget.
    pub model_bytes: u64,
    pub spec_bytes: u64,
    /// Bound expanded inheritance and fully resolved JSON independently.
    pub resolved_bytes: u64,
    pub requests: usize,
    pub animations: usize,
    pub simulation_updates: u64,
    pub image_dimension: u32,
    pub image_pixels: u64,
    pub run_pixels: u64,
    pub output_files: u64,
    pub geometry_vertices: u64,
    pub trace_values: u64,
    /// Conservative triangle-edge × image-span × line-width estimate.
    pub overlay_work: u64,
    pub name_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            model_bytes: catchlight_core::load_budget::LoadLimits::default().encoded_bytes,
            spec_bytes: MAX_JSON_BYTES,
            resolved_bytes: MAX_RESOLVED_BYTES,
            requests: MAX_REQUESTS,
            animations: MAX_ANIMATIONS,
            simulation_updates: MAX_UPDATES,
            image_dimension: MAX_DIMENSION,
            image_pixels: MAX_PIXELS,
            run_pixels: MAX_RUN_PIXELS,
            output_files: MAX_OUTPUTS,
            geometry_vertices: MAX_GEOMETRY_VERTICES,
            trace_values: MAX_TRACE_VALUES,
            overlay_work: MAX_OVERLAY_WORK,
            name_bytes: MAX_NAME_BYTES,
        }
    }
}

/// Missing inherits; null resets; a value replaces the entire parent setting.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Setting<T> {
    #[default]
    Inherit,
    Reset,
    Value(T),
}
impl<T> Setting<T> {
    fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }
    fn override_with(&mut self, child: &Self)
    where
        T: Clone,
    {
        if !child.is_inherit() {
            *self = child.clone();
        }
    }
    fn value(self) -> Option<T> {
        match self {
            Self::Value(v) => Some(v),
            Self::Inherit | Self::Reset => None,
        }
    }
}
impl<T: Serialize> Serialize for Setting<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Value(v) => v.serialize(serializer),
            _ => serializer.serialize_none(),
        }
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Setting<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Option::<T>::deserialize(deserializer)?.map_or(Self::Reset, Self::Value))
    }
}
impl<T: JsonSchema> JsonSchema for Setting<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        Option::<T>::schema_name()
    }
    fn schema_id() -> std::borrow::Cow<'static, str> {
        Option::<T>::schema_id()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        Option::<T>::json_schema(generator)
    }
    fn inline_schema() -> bool {
        true
    }
}

/// JSON authoring document. Request names become portable output basenames.
/// Execution order is lexicographic; map insertion order has no meaning.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// Format version, currently exactly 1.
    #[schemars(range(min = 1, max = 1))]
    pub schema: u32,
    /// Nonempty map. Names use 1..100 ASCII letters/digits/_/-, start with
    /// a letter/digit and exclude the reserved generated-file separator --.
    #[schemars(transform = named_requests)]
    pub requests: BTreeMap<String, Request>,
    /// Optional spec-local native clips; no external file references.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(transform = named_animations)]
    pub animations: BTreeMap<String, NativeAnimation>,
}

/// A request's top-level settings. Omitted fields inherit; null resets the
/// whole setting. In particular pose, frames, physics and arrays never merge.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Name of another request, including a forward reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// World-space minimum x, minimum y, width, height. Y is up. Default
    /// [-1500,-2500,3000,5000]; width and height must be positive.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    #[schemars(transform = rect_constraints)]
    pub rect: Setting<[f32; 4]>,
    /// Uniform output pixels per world unit, positive. Default 0.32.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    #[schemars(transform = positive_number)]
    pub scale: Setting<f32>,
    /// Straight sRGB color and alpha, each 0..1. Default opaque white.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    pub background: Setting<Background>,
    /// Static default settled; animated default simulate/fresh/zero warm-up.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    pub physics: Setting<Physics>,
    /// Fixed parameter overrides. Unspecified params take model defaults;
    /// animation lanes and then physics resolve after these base controls.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    #[schemars(with = "Setting<BTreeMap<ModelIdSchema, f32>>")]
    pub pose: Setting<BTreeMap<ParamId, f32>>,
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    pub animation: Setting<AnimationRef>,
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    pub frames: Setting<Frames>,
    /// Animated requests only. Trace records every update, including frame 0.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    #[schemars(with = "Setting<Vec<ModelIdSchema>>")]
    pub trace_params: Setting<Vec<ParamId>>,
    /// Retain color from exact Parts; omitted means all, [] means none.
    /// Mask contributions and authored enabled state are preserved.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    #[schemars(with = "Setting<Vec<ModelIdSchema>>")]
    pub only_parts: Setting<Vec<NodeId>>,
    /// Hide exact Part colors while preserving their mask contributions.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    #[schemars(with = "Setting<Vec<ModelIdSchema>>")]
    pub hide_color: Setting<Vec<NodeId>>,
    /// Remove these explicit mask edges from the private render model.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    pub strip_masks: Setting<Vec<MaskEdge>>,
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    pub overlay: Setting<Overlay>,
    /// Meshed node Ids to export in authored vertex/triangle order.
    #[serde(default, skip_serializing_if = "Setting::is_inherit")]
    #[schemars(with = "Setting<Vec<ModelIdSchema>>")]
    pub geometry: Setting<Vec<NodeId>>,
}
impl Request {
    fn inherit_from(&self, mut base: Self) -> Self {
        macro_rules! fields { ($($field:ident),+ $(,)?) => { $(base.$field.override_with(&self.$field);)+ }; }
        fields!(
            rect,
            scale,
            background,
            physics,
            pose,
            animation,
            frames,
            trace_params,
            only_parts,
            hide_color,
            strip_masks,
            overlay,
            geometry
        );
        base.extends = None;
        base
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Background {
    /// Straight sRGB components in 0..1.
    #[schemars(transform = unit_components)]
    pub srgb: [f32; 3],
    #[schemars(range(min = 0, max = 1))]
    pub alpha: f32,
}
impl Default for Background {
    fn default() -> Self {
        Self {
            srgb: [1.0; 3],
            alpha: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Physics {
    Off {},
    /// Static requests only.
    Settled {},
    /// Animation requests only, holding frame-zero controls during warm-up.
    Simulate {
        #[serde(default)]
        initial: Initial,
        #[serde(default)]
        #[schemars(range(max = 100000))]
        warmup_frames: u32,
    },
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Initial {
    #[default]
    Fresh,
    Settled,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AnimationRef {
    pub source: AnimationSource,
    /// Spec map key, or an unambiguous model clip display name.
    pub name: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AnimationSource {
    Spec,
    Model,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Frames {
    /// Number of export frame indices including frame 0. Default clip length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 100001))]
    pub count: Option<u32>,
    /// Capture indices divisible by every. Simulation still updates all frames.
    #[serde(default = "one")]
    #[schemars(range(min = 1))]
    pub every: u32,
}
impl Default for Frames {
    fn default() -> Self {
        Self {
            count: None,
            every: 1,
        }
    }
}
fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct MaskEdge {
    #[schemars(with = "ModelIdSchema")]
    pub node: NodeId,
    #[schemars(with = "ModelIdSchema")]
    pub source: NodeId,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    pub mesh: MeshOverlay,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MeshOverlay {
    #[schemars(with = "Vec<ModelIdSchema>", length(min = 1))]
    pub parts: Vec<NodeId>,
    pub positions: Positions,
    /// Straight sRGB RGBA components in 0..1, blended in linear light.
    #[schemars(transform = unit_components)]
    pub color: [f32; 4],
    /// Image-pixel line width, in (0,32].
    #[schemars(range(max = 32), transform = positive_number)]
    pub width_px: f32,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Positions {
    Rest,
    Posed,
}

/// Integer output rounding preserves center and uniform scale, not both
/// requested edges. Coordinates refer to image edges; centers are at halves.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Framing {
    pub rect: [f32; 4],
    pub effective_rect: [f64; 4],
    pub scale: f32,
    pub size: [u32; 2],
    pub center: [f32; 2],
    /// Effective height in world units for the renderer's existing camera.
    pub height: f32,
    /// Rows mapping [world_x, world_y, 1] to top-left/Y-down pixel edges.
    pub world_to_pixel: [[f64; 3]; 2],
}
impl Framing {
    pub fn resolve(rect: [f32; 4], scale: f32) -> Result<Self, Error> {
        let [x, y, w, h] = rect.map(f64::from);
        let s = f64::from(scale);
        if !rect.iter().all(|v| v.is_finite()) || w <= 0.0 || h <= 0.0 || !s.is_finite() || s <= 0.0
        {
            return Err(bad("rect requires finite coordinates and positive dimensions; scale must be finite and positive"));
        }
        let dimensions = [w * s, h * s].map(|v| v.round().max(1.0));
        if dimensions.iter().any(|v| *v > f64::from(MAX_DIMENSION)) {
            return Err(limit(
                "image_dimension",
                u64::from(MAX_DIMENSION),
                dimensions[0].max(dimensions[1]) as u64,
                "reduce rect or scale",
            ));
        }
        let size = dimensions.map(|v| v as u32);
        let pixels = u64::from(size[0]) * u64::from(size[1]);
        check_limit("image_pixels", pixels, MAX_PIXELS, "reduce rect or scale")?;
        // The camera consumes f32 coordinates. Record the same representable
        // center in the mapping rather than a different f64-only midpoint.
        let center = [x + w * 0.5, y + h * 0.5].map(|v| f64::from(v as f32));
        let span = [f64::from(size[0]) / s, f64::from(size[1]) / s];
        let effective_rect = [
            center[0] - span[0] * 0.5,
            center[1] - span[1] * 0.5,
            span[0],
            span[1],
        ];
        if center
            .iter()
            .chain(span.iter())
            .chain(effective_rect.iter())
            .any(|v| !(*v as f32).is_finite())
            || (span[1] as f32) <= 0.0
        {
            return Err(bad(
                "resolved framing is outside the renderer's finite f32 coordinate range",
            ));
        }
        Ok(Self {
            rect,
            effective_rect,
            scale,
            size,
            center: center.map(|v| v as f32),
            height: span[1] as f32,
            world_to_pixel: [
                [s, 0.0, f64::from(size[0]) * 0.5 - center[0] * s],
                [0.0, -s, f64::from(size[1]) * 0.5 + center[1] * s],
            ],
        })
    }

    /// Compatibility form: W H camera_height, centered at the origin.
    pub fn legacy(width: u32, height: u32, camera_height: f32) -> Result<Self, Error> {
        if width == 0 || height == 0 || !camera_height.is_finite() || camera_height <= 0.0 {
            return Err(bad(
                "legacy width, height and camera_height must be finite and positive",
            ));
        }
        let w = camera_height * width as f32 / height as f32;
        Self::resolve(
            [-w * 0.5, -camera_height * 0.5, w, camera_height],
            height as f32 / camera_height,
        )
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSpec {
    pub schema: u32,
    pub limits: Limits,
    pub requests: BTreeMap<String, ResolvedRequest>,
    /// Planned simulation ticks across requests, excluding zero-dt frame 0.
    pub simulation_updates: u64,
    /// All clean and overlaid image pixels across captured frames.
    pub output_pixels: u64,
    /// Artifact count including run.json.
    pub output_files: u64,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRequest {
    pub framing: Framing,
    pub background: Background,
    pub physics: Physics,
    /// Complete model-default-plus-override base controls, before animation/physics.
    #[schemars(with = "BTreeMap<ModelIdSchema, f32>")]
    pub pose: BTreeMap<ParamId, f32>,
    pub animation: Option<ResolvedAnimation>,
    #[schemars(with = "Vec<ModelIdSchema>")]
    pub trace_params: Vec<ParamId>,
    #[schemars(with = "Option<Vec<ModelIdSchema>>")]
    pub only_parts: Option<Vec<NodeId>>,
    #[schemars(with = "Vec<ModelIdSchema>")]
    pub hide_color: Vec<NodeId>,
    pub strip_masks: Vec<MaskEdge>,
    pub overlay: Option<Overlay>,
    #[schemars(with = "Vec<ModelIdSchema>")]
    pub geometry: Vec<NodeId>,
    /// Flat relative output names, including trace/overlays/geometry if needed.
    pub outputs: Vec<String>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedAnimation {
    pub selection: AnimationRef,
    /// Exact native clip selected by source/name, not another file reference.
    pub clip: NativeAnimation,
    pub frames: ResolvedFrames,
}
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFrames {
    pub count: u32,
    pub every: u32,
}
impl ResolvedFrames {
    pub fn captured_indices(&self) -> impl Iterator<Item = u32> {
        (0..self.count).step_by(self.every as usize)
    }
    pub fn captures(&self) -> u64 {
        u64::from((self.count - 1) / self.every + 1)
    }
}

impl Document {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        check_limit(
            "spec_bytes",
            bytes.len() as u64,
            MAX_JSON_BYTES,
            "split the spec",
        )?;
        let value: serde_json::Value = decode_json(bytes)?;
        if let Some(requests) = value.get("requests").and_then(serde_json::Value::as_object) {
            check_limit(
                "requests",
                requests.len() as u64,
                MAX_REQUESTS as u64,
                "split the spec",
            )?;
        }
        if let Some(animations) = value
            .get("animations")
            .and_then(serde_json::Value::as_object)
        {
            check_limit(
                "animations",
                animations.len() as u64,
                MAX_ANIMATIONS as u64,
                "split the spec",
            )?;
        }
        let doc: Self =
            serde_json::from_value(value).map_err(|e| bad(format!("invalid render spec: {e}")))?;
        if doc.schema != 1 {
            return Err(bad("unsupported render schema; expected 1"));
        }
        Ok(doc)
    }
    pub fn read(path: &Path) -> Result<Self, Error> {
        Self::parse(&read_json(path)?)
    }
    pub fn single(request: Request) -> Self {
        Self {
            schema: 1,
            requests: BTreeMap::from([("default".into(), request)]),
            animations: BTreeMap::new(),
        }
    }

    /// Resolve one discovery request without charging the work of unrelated
    /// requests. Its inheritance still resolves against the complete document.
    pub fn resolve_one(&self, model: &Model, name: &str) -> Result<ResolvedRequest, Error> {
        check_limit(
            "requests",
            self.requests.len() as u64,
            MAX_REQUESTS as u64,
            "split the spec",
        )?;
        let mut inherited = self.resolve_inheritance()?;
        let request = inherited
            .remove(name)
            .ok_or_else(|| bad(format!("unknown request {name}")))?;
        let single = Self {
            schema: self.schema,
            requests: BTreeMap::from([(name.to_owned(), request)]),
            animations: self.animations.clone(),
        };
        single
            .resolve(model)?
            .requests
            .remove(name)
            .ok_or_else(|| bad("resolved request disappeared"))
    }

    pub fn resolve(&self, model: &Model) -> Result<ResolvedSpec, Error> {
        if self.schema != 1 {
            return Err(bad("unsupported render schema; expected 1"));
        }
        if self.requests.is_empty() {
            return Err(bad("requests must contain at least one named request"));
        }
        check_limit(
            "requests",
            self.requests.len() as u64,
            MAX_REQUESTS as u64,
            "split the spec",
        )?;
        check_limit(
            "animations",
            self.animations.len() as u64,
            MAX_ANIMATIONS as u64,
            "split the spec",
        )?;
        let mut folded = BTreeSet::new();
        for name in self.requests.keys() {
            validate_name(name)?;
            if !folded.insert(name.to_ascii_lowercase()) {
                return Err(bad(format!(
                    "case-insensitive output collision for request {name}"
                )));
            }
        }
        for (name, clip) in &self.animations {
            validate_name(name)?;
            animation::validate(model, &clip.0)
                .map_err(|e| bad(format!("animation {name}: {e}")))?;
        }
        let inherited = self.resolve_inheritance()?;
        let mut out = ResolvedSpec {
            schema: 1,
            limits: Limits::default(),
            requests: BTreeMap::new(),
            simulation_updates: 0,
            output_pixels: 0,
            output_files: 1,
        };
        let mut filenames = BTreeSet::from(["run.json".to_string()]);
        let mut resolved_bytes = 0;
        let mut geometry_vertices = 0_u64;
        let mut trace_values = 0_u64;
        let mut overlay_work = 0_u64;
        for (name, request) in inherited {
            let mut request =
                self.resolve_request(model, request)
                    .map_err(|source| Error::RenderRequest {
                        request: name.clone(),
                        source: Box::new(source),
                    })?;
            let captures = request
                .animation
                .as_ref()
                .map_or(1, |a| a.frames.captures());
            let updates = request
                .animation
                .as_ref()
                .map_or(0, |a| u64::from(a.frames.count - 1));
            let warmup = match request.physics {
                Physics::Simulate { warmup_frames, .. } => u64::from(warmup_frames),
                _ => 0,
            };
            out.simulation_updates += updates + warmup;
            check_limit(
                "simulation_updates",
                out.simulation_updates,
                MAX_UPDATES,
                "reduce frames/warm-up or split the spec",
            )?;
            out.output_pixels += captures
                * u64::from(request.framing.size[0])
                * u64::from(request.framing.size[1])
                * (1 + u64::from(request.overlay.is_some()));
            check_limit(
                "run_pixels",
                out.output_pixels,
                MAX_RUN_PIXELS,
                "reduce scale/captures or split the spec",
            )?;
            let output_count = captures
                * (1 + u64::from(request.overlay.is_some())
                    + u64::from(!request.geometry.is_empty()))
                + u64::from(request.animation.is_some());
            out.output_files += output_count;
            check_limit(
                "output_files",
                out.output_files,
                MAX_OUTPUTS,
                "reduce captures or split the spec",
            )?;
            let vertices: u64 = request
                .geometry
                .iter()
                .filter_map(|id| model.node_mesh(id))
                .map(|mesh| mesh.vertex_count() as u64)
                .sum();
            geometry_vertices = geometry_vertices.saturating_add(vertices.saturating_mul(captures));
            check_limit(
                "geometry_vertices",
                geometry_vertices,
                MAX_GEOMETRY_VERTICES,
                "reduce geometry selections/captures or split the spec",
            )?;
            if let Some(animation) = &request.animation {
                trace_values = trace_values.saturating_add(
                    u64::from(animation.frames.count)
                        .saturating_mul(request.trace_params.len() as u64)
                        .saturating_mul(2),
                );
                check_limit(
                    "trace_values",
                    trace_values,
                    MAX_TRACE_VALUES,
                    "reduce trace params/frames or split the spec",
                )?;
            }
            if let Some(overlay) = &request.overlay {
                let triangles: u64 = overlay
                    .mesh
                    .parts
                    .iter()
                    .filter_map(|id| model.node_mesh(id))
                    .map(|mesh| mesh.triangle_count() as u64)
                    .sum();
                let span = u64::from(request.framing.size[0]) + u64::from(request.framing.size[1]);
                overlay_work = overlay_work.saturating_add(
                    triangles
                        .saturating_mul(3)
                        .saturating_mul(span)
                        .saturating_mul((overlay.mesh.width_px + 2.0).ceil() as u64)
                        .saturating_mul(captures),
                );
                check_limit(
                    "overlay_work",
                    overlay_work,
                    MAX_OVERLAY_WORK,
                    "reduce overlay selections, scale or captures",
                )?;
            }
            request.outputs = plan_outputs(&name, &request);
            for filename in &request.outputs {
                if !filenames.insert(filename.to_ascii_lowercase()) {
                    return Err(bad(format!("output filename collision: {filename}")));
                }
            }
            resolved_bytes += encoded_len(&request)?;
            check_limit(
                "resolved_bytes",
                resolved_bytes,
                MAX_RESOLVED_BYTES,
                "reduce repeated clip/settings data or split the spec",
            )?;
            out.requests.insert(name, request);
        }
        Ok(out)
    }

    // Iterative traversal bounds stack use even when all 4096 requests form a
    // chain. Cache each expanded base, so a shared ancestor resolves once.
    fn resolve_inheritance(&self) -> Result<BTreeMap<String, Request>, Error> {
        let mut resolved = BTreeMap::<String, Request>::new();
        let mut expanded_bytes = 0;
        for name in self.requests.keys() {
            let mut chain = Vec::new();
            let mut visited = BTreeSet::new();
            let mut current = name.as_str();
            while !resolved.contains_key(current) {
                if !visited.insert(current) {
                    return Err(bad(format!("inheritance cycle at request {current}")));
                }
                let request = self
                    .requests
                    .get(current)
                    .ok_or_else(|| bad(format!("unknown inherited request {current}")))?;
                chain.push(current);
                if let Some(base) = &request.extends {
                    current = base;
                } else {
                    break;
                }
            }
            while let Some(name) = chain.pop() {
                let child = &self.requests[name];
                let base = child
                    .extends
                    .as_ref()
                    .and_then(|n| resolved.get(n))
                    .cloned()
                    .unwrap_or_default();
                let expanded = child.inherit_from(base);
                expanded_bytes += encoded_len(&expanded)?;
                check_limit(
                    "resolved_bytes",
                    expanded_bytes,
                    MAX_RESOLVED_BYTES,
                    "reduce inherited settings or split the spec",
                )?;
                resolved.insert(name.to_string(), expanded);
            }
        }
        Ok(resolved)
    }

    fn resolve_request(&self, model: &Model, request: Request) -> Result<ResolvedRequest, Error> {
        let framing = Framing::resolve(
            request.rect.value().unwrap_or(DEFAULT_RECT),
            request.scale.value().unwrap_or(DEFAULT_SCALE),
        )?;
        let background = request.background.value().unwrap_or_default();
        if !unit(background.alpha) || !background.srgb.iter().all(|v| unit(*v)) {
            return Err(bad("background components must be finite in 0..1"));
        }
        let overrides = request.pose.value().unwrap_or_default();
        validate_pose(model, &overrides)?;
        let mut pose: BTreeMap<_, _> = model
            .param_ids()
            .iter()
            .filter_map(|id| model.param(id).map(|p| (id.clone(), p.default)))
            .collect();
        pose.extend(overrides);
        let animation = request
            .animation
            .value()
            .map(|selection| {
                let clip = match selection.source {
                    AnimationSource::Spec => self
                        .animations
                        .get(&selection.name)
                        .cloned()
                        .ok_or_else(|| bad(format!("unknown spec animation {}", selection.name)))?,
                    AnimationSource::Model => {
                        let mut matches = model
                            .animations()
                            .iter()
                            .filter(|a| a.name == selection.name);
                        let clip = matches.next().ok_or_else(|| {
                            bad(format!("unknown model animation {}", selection.name))
                        })?;
                        if matches.next().is_some() {
                            return Err(bad(format!(
                                "ambiguous model animation {}",
                                selection.name
                            )));
                        }
                        NativeAnimation(clip.clone())
                    }
                };
                animation::validate(model, &clip.0)?;
                let frames = request.frames.clone().value().unwrap_or_default();
                let count = frames.count.unwrap_or(clip.0.length as u32);
                if count == 0 || frames.every == 0 {
                    return Err(bad("frame count and every must be positive"));
                }
                check_limit(
                    "simulation_updates",
                    u64::from(count - 1),
                    MAX_UPDATES,
                    "reduce frame count",
                )?;
                Ok(ResolvedAnimation {
                    selection,
                    clip,
                    frames: ResolvedFrames {
                        count,
                        every: frames.every,
                    },
                })
            })
            .transpose()?;
        if animation.is_none()
            && (request.frames.value().is_some() || request.trace_params.clone().value().is_some())
        {
            return Err(bad("frames and trace_params require an animation"));
        }
        let physics = request.physics.value().unwrap_or_else(|| {
            if animation.is_some() {
                Physics::Simulate {
                    initial: Initial::Fresh,
                    warmup_frames: 0,
                }
            } else {
                Physics::Settled {}
            }
        });
        match (&physics, animation.is_some()) {
            (Physics::Simulate { .. }, false) => return Err(bad("physics simulate requires an animation")),
            (Physics::Settled {}, true) => return Err(bad("animated requests use physics off or simulate; settled is a simulate initial setting")),
            _ => {}
        }
        let trace_params = request.trace_params.value().unwrap_or_default();
        unique("trace_params", &trace_params)?;
        for id in &trace_params {
            if model.param(id).is_none() {
                return Err(bad(format!("unknown trace parameter {id}")));
            }
        }
        let only_parts = request.only_parts.value();
        let hide_color = request.hide_color.value().unwrap_or_default();
        if let Some(parts) = &only_parts {
            unique("only_parts", parts)?;
        }
        unique("hide_color", &hide_color)?;
        for node in only_parts.iter().flatten().chain(&hide_color) {
            part(model, node)?;
        }
        let strip_masks = request.strip_masks.value().unwrap_or_default();
        unique("strip_masks", &strip_masks)?;
        for edge in &strip_masks {
            part(model, &edge.node)?;
            let Some(ModelNodeKind::Part(p)) = model.node(&edge.node).map(|n| &n.kind) else {
                return Err(bad(format!("{} is not a Part", edge.node)));
            };
            if !p.masks().iter().any(|m| m.source() == &edge.source) {
                return Err(bad(format!(
                    "no mask edge {} -> {}",
                    edge.node, edge.source
                )));
            }
        }
        let geometry = request.geometry.value().unwrap_or_default();
        unique("geometry", &geometry)?;
        let overlay = request.overlay.value();
        for node in geometry
            .iter()
            .chain(overlay.iter().flat_map(|o| &o.mesh.parts))
        {
            if model.node_mesh(node).is_none() {
                return Err(bad(format!("{node} is not a meshed node")));
            }
        }
        if let Some(o) = &overlay {
            unique("overlay.mesh.parts", &o.mesh.parts)?;
            if o.mesh.parts.is_empty()
                || !o.mesh.color.iter().all(|v| unit(*v))
                || !o.mesh.width_px.is_finite()
                || o.mesh.width_px <= 0.0
                || o.mesh.width_px > 32.0
            {
                return Err(bad(
                    "mesh overlay needs nonempty meshes, RGBA in 0..1, and width_px in (0,32]",
                ));
            }
        }
        Ok(ResolvedRequest {
            framing,
            background,
            physics,
            pose,
            animation,
            trace_params,
            only_parts,
            hide_color,
            strip_masks,
            overlay,
            geometry,
            outputs: Vec::new(),
        })
    }
}

fn plan_outputs(name: &str, request: &ResolvedRequest) -> Vec<String> {
    let mut out = Vec::new();
    let mut image = |stem: String| {
        out.push(format!("{stem}.png"));
        if request.overlay.is_some() {
            out.push(format!("{stem}--mesh.png"));
        }
        if !request.geometry.is_empty() {
            out.push(format!("{stem}--geometry.json"));
        }
    };
    if let Some(animation) = &request.animation {
        for n in animation.frames.captured_indices() {
            image(format!("{name}--{n:06}"));
        }
        out.push(format!("{name}--trace.jsonl"));
    } else {
        image(name.to_string());
    }
    out
}

fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || name.contains("--")
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(bad(format!("name {name:?} must use 1..{MAX_NAME_BYTES} ASCII letters/digits/_/-, start with a letter/digit and exclude --")));
    }
    Ok(())
}
fn unique<T: Ord>(field: &str, ids: &[T]) -> Result<(), Error> {
    if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(bad(format!("duplicate selection in {field}")));
    }
    Ok(())
}
pub fn part(model: &Model, node: &NodeId) -> Result<(), Error> {
    if !matches!(
        model.node(node).map(|n| &n.kind),
        Some(ModelNodeKind::Part(_))
    ) {
        return Err(bad(format!("{node} is not a Part")));
    }
    Ok(())
}
pub fn validate_pose(model: &Model, pose: &BTreeMap<ParamId, f32>) -> Result<(), Error> {
    for (id, value) in pose {
        let param = model
            .param(id)
            .ok_or_else(|| bad(format!("unknown parameter {id}")))?;
        if !value.is_finite() || *value < param.min || *value > param.max {
            return Err(bad(format!(
                "parameter {id} must be finite in {}..{}",
                param.min, param.max
            )));
        }
    }
    Ok(())
}
pub fn parse_set(text: &str) -> Result<(ParamId, f32), Error> {
    let (id, value) = text
        .rsplit_once('=')
        .ok_or_else(|| bad("--set takes param=value"))?;
    let id = ParamId::new(id).map_err(|e| bad(e.to_string()))?;
    let value = value
        .parse::<f32>()
        .map_err(|_| bad("--set takes a numeric value"))?;
    if !value.is_finite() {
        return Err(bad("--set takes a finite numeric value"));
    }
    Ok((id, value))
}
pub fn parse_rect(text: &str) -> Result<[f32; 4], Error> {
    let v = text
        .split(',')
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| bad("--rect takes x0,y0,w,h"))?;
    v.try_into().map_err(|_| bad("--rect takes x0,y0,w,h"))
}
// Count serialization without allocating another expanded JSON buffer.
fn encoded_len(value: &impl Serialize) -> Result<u64, Error> {
    struct Counter(u64);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value).map_err(|e| bad(e.to_string()))?;
    Ok(counter.0)
}
fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
pub(super) fn bad(message: impl Into<String>) -> Error {
    Error::Render(message.into())
}
fn limit(budget: &'static str, limit: u64, requested: u64, hint: &'static str) -> Error {
    Error::RenderLimit {
        budget,
        limit,
        requested,
        hint,
    }
}
pub(super) fn check_limit(
    budget: &'static str,
    requested: u64,
    max: u64,
    hint: &'static str,
) -> Result<(), Error> {
    if requested > max {
        Err(limit(budget, max, requested, hint))
    } else {
        Ok(())
    }
}

/// Schema is generated from the same Rust types used for parsing and output.
/// Semantic checks requiring model state run in Document::resolve.
pub fn schema(resolved: bool) -> serde_json::Value {
    let schema = if resolved {
        schemars::schema_for!(ResolvedSpec)
    } else {
        schemars::schema_for!(Document)
    };
    let mut value = schema.to_value();
    value["x-catchlight-limits"] = serde_json::json!(Limits::default());
    if !resolved {
        // An installed binary carries complete examples alongside its schema.
        if let Ok(example) = serde_json::from_str::<serde_json::Value>(include_str!(
            "../../tests/fixtures/render-spec.json"
        )) {
            value["examples"] = serde_json::json!([example]);
        }
    }
    value
}

fn named_requests(schema: &mut schemars::Schema) {
    named_map(schema, MAX_REQUESTS);
    schema.insert("minProperties".into(), serde_json::json!(1));
}
fn named_animations(schema: &mut schemars::Schema) {
    named_map(schema, MAX_ANIMATIONS);
}
fn named_map(schema: &mut schemars::Schema, max: usize) {
    schema.insert("maxProperties".into(), serde_json::json!(max));
    schema.insert(
        "propertyNames".into(),
        serde_json::json!({
            "type":"string", "minLength":1, "maxLength":MAX_NAME_BYTES,
            "pattern":"^[A-Za-z0-9][A-Za-z0-9_-]*$", "not":{"pattern":"--"}
        }),
    );
}
fn positive_number(schema: &mut schemars::Schema) {
    schema.insert("exclusiveMinimum".into(), serde_json::json!(0));
}
fn unit_components(schema: &mut schemars::Schema) {
    let object = schema.ensure_object();
    if let Some(items) = object
        .get_mut("items")
        .and_then(serde_json::Value::as_object_mut)
    {
        items.insert("minimum".into(), serde_json::json!(0));
        items.insert("maximum".into(), serde_json::json!(1));
    }
}
fn rect_constraints(schema: &mut schemars::Schema) {
    fn array(value: &mut serde_json::Value) {
        if let Some(object) = value.as_object_mut() {
            if object.get("type").is_some_and(|t| {
                t == "array"
                    || t.as_array()
                        .is_some_and(|types| types.iter().any(|t| t == "array"))
            }) {
                object.insert(
                    "prefixItems".into(),
                    serde_json::json!([
                        {"type":"number"},{"type":"number"},
                        {"type":"number","exclusiveMinimum":0},
                        {"type":"number","exclusiveMinimum":0}
                    ]),
                );
            }
            if let Some(branches) = object
                .get_mut("anyOf")
                .and_then(serde_json::Value::as_array_mut)
            {
                for branch in branches {
                    array(branch);
                }
            }
        }
    }
    let mut value = schema.clone().to_value();
    array(&mut value);
    if let Ok(updated) = schemars::Schema::try_from(value) {
        *schema = updated;
    }
}
