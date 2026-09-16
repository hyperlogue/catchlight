//! Portable JSON records and durable atomic artifacts. Records contain relative
//! output names and input hashes, never input paths or adapter/device identity.
use super::frame::{color_retained, FrameRuntime};
use super::spec::{bad, Framing, Physics, ResolvedRequest, ResolvedSpec};
use crate::Error;
use catchlight_core::geometry::EvaluatedGeometry;
use catchlight_core::{Model, ModelNodeKind, ParamId, Puppet};
use image::ImageEncoder;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeometryOutput {
    #[schemars(range(min = 1, max = 1))]
    pub schema: u32,
    pub request: String,
    /// Export index, or null for static requests.
    pub frame: Option<u32>,
    /// Complete effective controls from the frame rendered, after drivers.
    pub pose: BTreeMap<String, f32>,
    pub framing: Framing,
    pub nodes: BTreeMap<String, MeshGeometry>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeshGeometry {
    /// Authored mesh coordinates, in original vertex order.
    pub rest: Vec<[f32; 2]>,
    /// rest - origin + combined_deform, in original vertex order.
    pub local: Vec<[f32; 2]>,
    /// Evaluated XYZ, including inherited deformation and welds.
    pub world: Vec<[f32; 3]>,
    pub origin: [f32; 2],
    /// Column-major 4x4 matrix mapping local XYZ to world XYZ.
    pub local_to_world: [[f32; 4]; 4],
    /// Original UV array; may be empty for untextured geometry.
    pub uvs: Vec<[f32; 2]>,
    pub triangles: Vec<[u32; 3]>,
}
impl GeometryOutput {
    pub fn observe(
        name: &str,
        request: &ResolvedRequest,
        model: &Model,
        runtime: &FrameRuntime,
    ) -> Result<Self, Error> {
        let geometry =
            EvaluatedGeometry::new(model, &runtime.puppet).map_err(|e| bad(e.to_string()))?;
        let mut nodes = BTreeMap::new();
        for id in &request.geometry {
            let mesh = geometry.mesh(id).map_err(|e| bad(e.to_string()))?;
            let mut output = MeshGeometry {
                rest: Vec::new(),
                local: Vec::new(),
                world: Vec::new(),
                origin: mesh.origin().to_array(),
                local_to_world: mesh.local_to_world().to_cols_array_2d(),
                uvs: mesh.uvs().iter().map(|v| v.to_array()).collect(),
                triangles: mesh.triangles().map(|(_, t)| t).collect(),
            };
            if !mesh.local_to_world().is_finite() {
                return Err(bad(format!("non-finite transform for {id}")));
            }
            for vertex in mesh.vertices() {
                if !vertex.world.is_finite() || !vertex.local.is_finite() {
                    return Err(bad(format!(
                        "non-finite evaluated vertex {} in {id}",
                        vertex.index
                    )));
                }
                output.rest.push(vertex.rest.to_array());
                output.local.push(vertex.local.to_array());
                output.world.push(vertex.world.to_array());
            }
            nodes.insert(id.to_string(), output);
        }
        Ok(Self {
            schema: 1,
            request: name.into(),
            frame: request.animation.as_ref().map(|_| runtime.index),
            pose: effective_pose(model, &runtime.puppet)?,
            framing: request.framing.clone(),
            nodes,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TraceFrame {
    #[schemars(range(min = 1, max = 1))]
    pub schema: u32,
    pub request: String,
    pub frame: u32,
    /// Actual f32 clip/export timestep, in seconds.
    pub timestep: f32,
    /// Export index times effective timestep; distinct from wrapped clip time.
    pub elapsed: f64,
    pub clip: ClipSample,
    pub requested: BTreeMap<String, f32>,
    pub effective: BTreeMap<String, f32>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipSample {
    pub name: String,
    /// Index in this request's installed clip list, currently 0.
    pub index: usize,
    /// Native f32 playback clock in seconds after loop/clamp processing.
    pub time: f32,
    /// Actual sampled fractional clip frame, observed from Puppet.
    pub frame: f32,
    pub looping: bool,
}
impl TraceFrame {
    pub fn observe(
        name: &str,
        request: &ResolvedRequest,
        model: &Model,
        runtime: &FrameRuntime,
    ) -> Result<Self, Error> {
        let sample = runtime
            .puppet
            .animation_sample()
            .ok_or_else(|| bad("animation has no evaluated sample"))?;
        let mut requested = BTreeMap::new();
        let mut effective = BTreeMap::new();
        for id in &request.trace_params {
            let default = model
                .param(id)
                .ok_or_else(|| bad(format!("unknown trace parameter {id}")))?
                .default;
            let a = runtime.puppet.param_value_posed(id).unwrap_or(default);
            let b = runtime.puppet.param_value(id).unwrap_or(default);
            if !a.is_finite() || !b.is_finite() {
                return Err(bad(format!("non-finite trace parameter {id}")));
            }
            requested.insert(id.to_string(), a);
            effective.insert(id.to_string(), b);
        }
        if !sample.time.is_finite() || !sample.frame.is_finite() {
            return Err(bad("non-finite animation sample"));
        }
        Ok(Self {
            schema: 1,
            request: name.into(),
            frame: runtime.index,
            timestep: runtime.timestep(),
            elapsed: f64::from(runtime.index) * f64::from(runtime.timestep()),
            clip: ClipSample {
                name: sample.name.into(),
                index: sample.index,
                time: sample.time,
                frame: sample.frame,
                looping: sample.looping,
            },
            requested,
            effective,
        })
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundsOutput {
    #[schemars(range(min = 1, max = 1))]
    pub schema: u32,
    pub space: String,
    pub pose: BTreeMap<String, f32>,
    pub physics: Physics,
    pub frame: Option<u32>,
    pub overall: Option<Bounds>,
    pub parts: BTreeMap<String, PartBounds>,
}
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub min: [f32; 2],
    pub max: [f32; 2],
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartBounds {
    pub bounds: Option<Bounds>,
    pub included: bool,
}
impl BoundsOutput {
    pub fn observe(
        request: &ResolvedRequest,
        model: &Model,
        runtime: &FrameRuntime,
    ) -> Result<Self, Error> {
        let view =
            EvaluatedGeometry::new(model, &runtime.puppet).map_err(|e| bad(e.to_string()))?;
        let mut parts = BTreeMap::new();
        let mut overall: Option<catchlight_core::geometry::Bounds2> = None;
        for id in model.nodes_in_order() {
            if !matches!(
                model.node(&id).map(|n| &n.kind),
                Some(ModelNodeKind::Part(_))
            ) {
                continue;
            }
            let bounds = view
                .mesh(&id)
                .map_err(|e| bad(e.to_string()))?
                .world_bounds()
                .map_err(|e| bad(e.to_string()))?;
            let mut included = color_retained(request, &id);
            let mut node = model.node(&id);
            while let Some(n) = node {
                included &= n.enabled;
                node = n.parent().and_then(|id| model.node(id));
            }
            if included {
                if let Some(b) = bounds {
                    overall = Some(overall.map_or(b, |v| v.union(b)));
                }
            }
            parts.insert(
                id.to_string(),
                PartBounds {
                    bounds: bounds.map(|b| Bounds {
                        min: b.min.to_array(),
                        max: b.max.to_array(),
                    }),
                    included,
                },
            );
        }
        Ok(Self {
            schema: 1,
            space: "world".into(),
            pose: effective_pose(model, &runtime.puppet)?,
            physics: request.physics.clone(),
            frame: request.animation.as_ref().map(|_| runtime.index),
            overall: overall.map(|b| Bounds {
                min: b.min.to_array(),
                max: b.max.to_array(),
            }),
            parts,
        })
    }
}
pub(super) fn effective_pose(
    model: &Model,
    puppet: &Puppet,
) -> Result<BTreeMap<String, f32>, Error> {
    effective_values(model, puppet)
        .map(|entry| entry.map(|(id, value)| (id.to_string(), value)))
        .collect()
}

pub(super) fn validate_effective_pose(model: &Model, puppet: &Puppet) -> Result<(), Error> {
    effective_values(model, puppet).try_for_each(|entry| entry.map(|_| ()))
}

fn effective_values<'a>(
    model: &'a Model,
    puppet: &'a Puppet,
) -> impl Iterator<Item = Result<(&'a ParamId, f32), Error>> + 'a {
    model.param_ids().iter().map(|id| {
        let default = model
            .param(id)
            .ok_or_else(|| bad("parameter disappeared"))?
            .default;
        let value = puppet.param_value(id).unwrap_or(default);
        if !value.is_finite() {
            return Err(bad(format!("non-finite effective parameter {id}")));
        }
        Ok((id, value))
    })
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunManifest {
    #[schemars(range(min = 1, max = 1))]
    pub schema: u32,
    pub tool: ToolIdentity,
    pub input: InputIdentity,
    pub renderer: Option<RendererIdentity>,
    pub complete: bool,
    pub failure: Option<RunFailure>,
    pub plan: ResolvedSpec,
    pub requests: BTreeMap<String, RequestResult>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub version: String,
    pub build: String,
    pub clm_format: u16,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputIdentity {
    pub model_sha256: String,
    pub spec_sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RendererIdentity {
    pub backend: String,
    pub format: String,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunFailure {
    pub kind: String,
    pub request: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestResult {
    pub complete: bool,
    pub outputs: Vec<OutputRecord>,
    pub pose: Option<BTreeMap<String, f32>>,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputRecord {
    pub file: String,
    pub frame: Option<u32>,
    pub kind: OutputKind,
}
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Image,
    Mesh,
    Geometry,
    Trace,
}
impl RunManifest {
    pub fn new(plan: ResolvedSpec, input: InputIdentity) -> Self {
        let requests = plan
            .requests
            .keys()
            .map(|name| (name.clone(), RequestResult::default()))
            .collect();
        Self {
            schema: 1,
            tool: ToolIdentity {
                version: env!("CARGO_PKG_VERSION").into(),
                build: option_env!("CATCHLIGHT_BUILD_REVISION")
                    .unwrap_or("unversioned")
                    .into(),
                clm_format: catchlight_core::formats::clm::FORMAT_VERSION,
            },
            input,
            renderer: None,
            complete: false,
            failure: None,
            plan,
            requests,
        }
    }
}

pub fn json_schema(kind: &str) -> Result<serde_json::Value, Error> {
    let schema = match kind {
        "geometry" => schemars::schema_for!(GeometryOutput),
        "trace" => schemars::schema_for!(TraceFrame),
        "run" => schemars::schema_for!(RunManifest),
        _ => schemars::schema_for!(BoundsOutput),
    };
    let mut schema = schema.to_value();
    schema["examples"] = serde_json::json!([schema_example(kind)?]);
    Ok(schema)
}

/// Complete discovery examples share one synthetic model and the production
/// normalizer/evaluator. This performs no filesystem or GPU work. The run
/// example illustrates completed records; schema discovery creates no images.
pub(super) fn schema_example(kind: &str) -> Result<serde_json::Value, Error> {
    use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
    use catchlight_core::{
        BindingKey, BindingTarget, ModelNode, ModelParam, ModelPart, Name, NodeId, ParamId,
        ScalarTarget,
    };
    let fail = |e: &dyn std::fmt::Display| bad(format!("schema example: {e}"));
    let mut model = Model::new();
    let root = model
        .root()
        .cloned()
        .ok_or_else(|| bad("schema example has no root"))?;
    let node = NodeId::new("panel-a").map_err(|e| fail(&e))?;
    let param = ParamId::new("drive").map_err(|e| fail(&e))?;
    model
        .add_node_with_id(
            node.clone(),
            &root,
            ModelNode::new(
                "Panel",
                ModelNodeKind::Part(ModelPart::new(ClmMesh {
                    verts: vec![0.0, 0.0, 100.0, 0.0, 0.0, 100.0],
                    uvs: vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                    indices: ClmIndices::U16(vec![0, 1, 2]),
                    origin: [0.0, 0.0],
                })),
            ),
        )
        .map_err(|e| fail(&e))?;
    model
        .add_param_with_id(
            param.clone(),
            ModelParam::new(Name::truncated("Drive"), 0.0, 1.0, 0.0),
        )
        .map_err(|e| fail(&e))?;
    let key = BindingKey::new(param, node, BindingTarget::Scalar(ScalarTarget::Tx));
    model
        .set_binding_key(&key, [0, 0], 0.0)
        .map_err(|e| fail(&e))?;
    model
        .set_binding_key(&key, [1, 0], 12.0)
        .map_err(|e| fail(&e))?;
    let authored = serde_json::json!({
        "schema":1,
        "requests":{
            "detail":{"rect":[-10,-10,120,120],"scale":2,"physics":{"mode":"off"},
                      "pose":{"drive":0.5},"geometry":["panel-a"]},
            "pulse":{"extends":"detail","animation":{"source":"spec","name":"pulse"},
                     "trace_params":["drive"]}
        },
        "animations":{"pulse":{"name":"Pulse","timestep":0.016666667,"length":2,
            "lead_in":-1,"lead_out":-1,"lanes":[{"param":"drive","interpolation":"Linear",
                "keyframes":[{"frame":0,"value":0},{"frame":1,"value":1}]}]}}
    });
    let spec_bytes = serde_json::to_vec(&authored).map_err(|e| fail(&e))?;
    let plan = super::spec::Document::parse(&spec_bytes)?.resolve(&model)?;
    if kind == "resolved" {
        return serde_json::to_value(plan).map_err(|e| fail(&e));
    }
    let detail = &plan.requests["detail"];
    let runtime = FrameRuntime::new(&model, detail, || false)?;
    match kind {
        "geometry" => {
            serde_json::to_value(GeometryOutput::observe("detail", detail, &model, &runtime)?)
        }
        "bounds" => serde_json::to_value(BoundsOutput::observe(detail, &model, &runtime)?),
        "trace" => {
            let pulse = &plan.requests["pulse"];
            let mut runtime = FrameRuntime::new(&model, pulse, || false)?;
            runtime.advance(&model);
            serde_json::to_value(TraceFrame::observe("pulse", pulse, &model, &runtime)?)
        }
        "run" => {
            let mut manifest = RunManifest::new(
                plan,
                InputIdentity {
                    model_sha256: super::execute::hash(
                        &model.to_clm_bytes().map_err(|e| fail(&e))?,
                    ),
                    spec_sha256: Some(super::execute::hash(&spec_bytes)),
                },
            );
            manifest.complete = true;
            manifest.renderer = Some(RendererIdentity {
                backend: "Vulkan".into(),
                format: "rgba8unorm-srgb".into(),
            });
            for (name, request) in &manifest.plan.requests {
                let frames: Vec<_> = request.animation.as_ref().map_or_else(
                    || vec![None],
                    |a| a.frames.captured_indices().map(Some).collect(),
                );
                let mut outputs = Vec::new();
                for frame in frames {
                    let base = frame.map_or_else(|| name.clone(), |i| format!("{name}--{i:06}"));
                    outputs.extend([
                        OutputRecord {
                            file: format!("{base}.png"),
                            frame,
                            kind: OutputKind::Image,
                        },
                        OutputRecord {
                            file: format!("{base}--geometry.json"),
                            frame,
                            kind: OutputKind::Geometry,
                        },
                    ]);
                }
                if request.animation.is_some() {
                    outputs.push(OutputRecord {
                        file: format!("{name}--trace.jsonl"),
                        frame: None,
                        kind: OutputKind::Trace,
                    });
                }
                manifest.requests.insert(
                    name.clone(),
                    RequestResult {
                        complete: true,
                        outputs,
                        pose: if request.animation.is_none() {
                            Some(effective_pose(&model, &runtime.puppet)?)
                        } else {
                            None
                        },
                    },
                );
            }
            serde_json::to_value(manifest)
        }
        _ => return Err(bad("unknown output schema example")),
    }
    .map_err(|e| fail(&e))
}

/// Commit a single completed artifact beside its destination. Existing PNGs
/// may be explicitly replaced; directory runs preflight all planned filenames.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    atomic(path, |file| {
        file.write_all(bytes).map_err(|e| Error::io(path, e))
    })
}
fn atomic(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> Result<(), Error>,
) -> Result<(), Error> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(|e| Error::io(path, e))?;
    write(temp.as_file_mut())?;
    temp.as_file().sync_all().map_err(|e| Error::io(path, e))?;
    temp.persist(path).map_err(|e| Error::io(path, e.error))?;
    #[cfg(unix)]
    std::fs::File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::io(path, e))?;
    Ok(())
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    // Stream large geometry and manifest records into the atomic destination;
    // do not duplicate their full serialized size in a second memory buffer.
    atomic(path, |file| {
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, value)
            .map_err(|e| bad(format!("JSON encoding: {e}")))?;
        writer.flush().map_err(|e| Error::io(path, e))
    })
}
pub fn png(pixels: &[u8], size: [u32; 2]) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(pixels, size[0], size[1], image::ExtendedColorType::Rgba8)
        .map_err(|e| bad(format!("PNG encoding: {e}")))?;
    Ok(bytes)
}
