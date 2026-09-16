//! Rendering owns one context/cache and one private model. Request runtimes
//! start independently; acquired deform sets prevent stale-generation aliasing
//! across Puppets. Files become visible atomically; manifest completion follows
//! durability of every required artifact, including traces and sidecars.
use super::{
    artifacts::*,
    frame::{self, FrameRuntime},
    overlay,
    spec::{bad, ResolvedRequest, ResolvedSpec},
    terminal,
};
use crate::Error;
use catchlight_core::{Model, ModelFormat};
use catchlight_wgpu::{PrepareOptions, RenderCache, RenderContext, RenderList};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Clone)]
pub enum Destination {
    Terminal,
    Png(PathBuf),
    Directory(PathBuf),
}
#[derive(Debug, Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub fn install(&self) -> Result<(), Error> {
        let token = self.clone();
        ctrlc::set_handler(move || token.cancel())
            .map_err(|e| bad(format!("cancellation handler: {e}")))
    }
    fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(bad("render cancelled"))
        } else {
            Ok(())
        }
    }
}
pub struct LoadedModel {
    pub model: Model,
    pub hash: Option<String>,
}
pub fn load(path: &Path, hash_input: bool) -> Result<LoadedModel, Error> {
    let format = ModelFormat::from_path(path).ok_or_else(|| Error::NotAClm {
        path: path.to_path_buf(),
    })?;
    // Bound the encoded input before allocating it, not just while decoding.
    // Metadata rejects regular oversized files immediately; the bounded read
    // also covers files that grow after the check or do not report a size.
    let limit = catchlight_core::load_budget::LoadLimits::default().encoded_bytes;
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let reported_size = file.metadata().map_err(|e| Error::io(path, e))?.len();
    super::spec::check_limit("model_bytes", reported_size, limit, "use a smaller model")?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::io(path, e))?;
    super::spec::check_limit(
        "model_bytes",
        bytes.len() as u64,
        limit,
        "use a smaller model",
    )?;
    let model = catchlight_core::load_model(&bytes, format).map_err(|source| Error::NotAModel {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(LoadedModel {
        model,
        hash: hash_input.then(|| hash(&bytes)),
    })
}
pub fn hash(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// An output alias must never replace the model that was inspected.
pub fn check_output_path(input: &Path, output: &Path) -> Result<(), Error> {
    if let (Ok(input), Ok(output)) = (input.canonicalize(), output.canonicalize()) {
        if input == output {
            return Err(bad("PNG output must not replace the input model"));
        }
    }
    Ok(())
}

pub struct Execution {
    pub listings: Vec<String>,
    pub manifest: Option<RunManifest>,
}
pub fn run(
    original: &Model,
    plan: ResolvedSpec,
    destination: Destination,
    input: Option<InputIdentity>,
    cancel: &Cancellation,
) -> Result<Execution, Error> {
    let directory = match &destination {
        Destination::Directory(path) => Some(path.clone()),
        _ => None,
    };
    if directory.is_none()
        && (plan.requests.len() != 1
            || plan
                .requests
                .values()
                .any(|r| r.animation.is_some() || r.overlay.is_some() || !r.geometry.is_empty()))
    {
        return Err(bad(
            "multiple images, animation, overlays and geometry require --out-dir",
        ));
    }
    if let Some(dir) = &directory {
        for file in std::iter::once("run.json").chain(
            plan.requests
                .values()
                .flat_map(|r| r.outputs.iter().map(String::as_str)),
        ) {
            if dir.join(file).symlink_metadata().is_ok() {
                return Err(bad(format!(
                    "output already exists: {file}; use a new output directory"
                )));
            }
        }
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    }
    let mut manifest = match (&directory, input) {
        (Some(_), Some(input)) => Some(RunManifest::new(plan.clone(), input)),
        (Some(_), None) => return Err(bad("directory export needs input identity")),
        _ => None,
    };
    persist_manifest(&directory, &manifest)?;
    let mut active_request = None;
    let result = (|| {
        cancel.check()?;
        let first = plan
            .requests
            .values()
            .next()
            .ok_or_else(|| bad("empty render plan"))?;
        let mut ctx = pollster::block_on(RenderContext::new(
            first.framing.size[0],
            first.framing.size[1],
        ))
        .map_err(|e| Error::gpu("gpu init", e))?;
        if let Some(run) = &mut manifest {
            run.renderer = Some(RendererIdentity {
                backend: format!("{:?}", ctx.renderer.device.adapter_info().backend),
                format: "rgba8unorm-srgb".into(),
            });
        }
        let mut model = original.clone();
        let mut previous_masks = Vec::new();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
            .map_err(|e| Error::gpu("prepare", e))?;
        let mut listings = Vec::new();
        for (name, request) in &plan.requests {
            active_request = Some(name.clone());
            cancel.check()?;
            frame::configure_masks(&mut model, original, &mut previous_masks, request)?;
            let mut runtime = FrameRuntime::new(&model, request, || cancel.is_cancelled())?;
            let mut trace = directory
                .as_ref()
                .filter(|_| request.animation.is_some())
                .map(|dir| tempfile::NamedTempFile::new_in(dir).map_err(|e| Error::io(dir, e)))
                .transpose()?;
            let deform_set = ctx.renderer.acquire_deform_set();
            let request_result = (|| {
                let count = request.animation.as_ref().map_or(1, |a| a.frames.count);
                let every = request.animation.as_ref().map_or(1, |a| a.frames.every);
                for index in 0..count {
                    cancel.check()?;
                    if index > 0 {
                        runtime.advance(&model);
                    }
                    effective_pose(&model, &runtime.puppet)?;
                    if let Some(trace) = &mut trace {
                        let record = TraceFrame::observe(name, request, &model, &runtime)?;
                        serde_json::to_writer(trace.as_file_mut(), &record)
                            .map_err(|e| bad(format!("trace encoding: {e}")))?;
                        trace
                            .write_all(b"\n")
                            .map_err(|e| bad(format!("trace write: {e}")))?;
                    }
                    if index % every != 0 {
                        continue;
                    }
                    let mut list = RenderList::default();
                    cache
                        .refresh_puppet(
                            &mut ctx.renderer,
                            &model,
                            &runtime.puppet,
                            deform_set,
                            &mut list,
                        )
                        .map_err(|e| Error::gpu("refresh", e))?;
                    cache.retain_part_colors(&mut list, |id| frame::color_retained(request, id));
                    let listing = super::listing::listing(&list);
                    if index == 0 {
                        listings.push(listing);
                    }
                    let mut pixels = draw(&mut ctx, &list, request)?;
                    let clean = png(&pixels, request.framing.size)?;
                    let frame = request.animation.as_ref().map(|_| index);
                    let stem = frame.map_or_else(|| name.clone(), |n| format!("{name}--{n:06}"));
                    match &destination {
                        Destination::Png(path) => atomic_write(path, &clean)?,
                        Destination::Terminal => {
                            if terminal::supported() {
                                terminal::present(&clean, std::io::stdout())
                                    .map_err(|e| bad(format!("terminal image: {e}")))?;
                            } else {
                                println!("Use --out image.png or --out-dir directory to save this render.");
                            }
                        }
                        Destination::Directory(dir) => {
                            let filename = format!("{stem}.png");
                            atomic_write(&dir.join(&filename), &clean)?;
                            record(&mut manifest, name, filename, frame, OutputKind::Image);
                            if let Some(overlay) = &request.overlay {
                                overlay::draw(
                                    &mut pixels,
                                    &model,
                                    &runtime.puppet,
                                    &request.framing,
                                    &overlay.mesh,
                                )?;
                                let filename = format!("{stem}--mesh.png");
                                atomic_write(
                                    &dir.join(&filename),
                                    &png(&pixels, request.framing.size)?,
                                )?;
                                record(&mut manifest, name, filename, frame, OutputKind::Mesh);
                            }
                            if !request.geometry.is_empty() {
                                let geometry =
                                    GeometryOutput::observe(name, request, &model, &runtime)?;
                                let filename = format!("{stem}--geometry.json");
                                atomic_json(&dir.join(&filename), &geometry)?;
                                record(&mut manifest, name, filename, frame, OutputKind::Geometry);
                            }
                        }
                    }
                }
                Ok::<_, Error>(())
            })();
            ctx.renderer.release_deform_set(deform_set);
            request_result?;
            if let (Some(trace), Some(dir)) = (trace, directory.as_ref()) {
                trace.as_file().sync_all().map_err(|e| Error::io(dir, e))?;
                let filename = format!("{name}--trace.jsonl");
                trace
                    .persist(dir.join(&filename))
                    .map_err(|e| Error::io(dir, e.error))?;
                #[cfg(unix)]
                std::fs::File::open(dir)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| Error::io(dir, e))?;
                record(&mut manifest, name, filename, None, OutputKind::Trace);
            }
            if let Some(run) = &mut manifest {
                if let Some(result) = run.requests.get_mut(name) {
                    result.complete = true;
                    if request.animation.is_none() {
                        result.pose = Some(effective_pose(&model, &runtime.puppet)?);
                    }
                }
            }
            persist_manifest(&directory, &manifest)?;
        }
        cancel.check()?;
        Ok::<_, Error>(listings)
    })();
    match result {
        Ok(listings) => {
            if let Some(run) = &mut manifest {
                run.complete = true;
            }
            persist_manifest(&directory, &manifest)?;
            Ok(Execution { listings, manifest })
        }
        Err(error) => {
            if let Some(run) = &mut manifest {
                run.failure = Some(RunFailure {
                    kind: if cancel.is_cancelled() {
                        "cancelled"
                    } else {
                        "render_failed"
                    }
                    .into(),
                    request: active_request,
                });
            }
            persist_manifest(&directory, &manifest)?;
            Err(error)
        }
    }
}
fn record(
    manifest: &mut Option<RunManifest>,
    request: &str,
    file: String,
    frame: Option<u32>,
    kind: OutputKind,
) {
    if let Some(result) = manifest.as_mut().and_then(|r| r.requests.get_mut(request)) {
        result.outputs.push(OutputRecord { file, frame, kind });
    }
}
fn persist_manifest(dir: &Option<PathBuf>, manifest: &Option<RunManifest>) -> Result<(), Error> {
    if let (Some(dir), Some(manifest)) = (dir, manifest) {
        atomic_json(&dir.join("run.json"), manifest)?;
    }
    Ok(())
}
fn draw(
    ctx: &mut RenderContext,
    list: &RenderList,
    request: &ResolvedRequest,
) -> Result<Vec<u8>, Error> {
    let bg = &request.background;
    let alpha = f64::from(bg.alpha);
    let clear = wgpu::Color {
        r: overlay::linear(f64::from(bg.srgb[0])) * alpha,
        g: overlay::linear(f64::from(bg.srgb[1])) * alpha,
        b: overlay::linear(f64::from(bg.srgb[2])) * alpha,
        a: alpha,
    };
    let mut pixels = ctx
        .render_rgba(
            list,
            catchlight_wgpu::Framing {
                center: glam::Vec2::from_array(request.framing.center),
                height: request.framing.height,
            },
            request.framing.size[0],
            request.framing.size[1],
            Some(clear),
        )
        .map_err(|e| Error::gpu("render", e))?;
    catchlight_core::texture::unpremultiply_linear_from_srgb_inplace(&mut pixels);
    Ok(pixels)
}
