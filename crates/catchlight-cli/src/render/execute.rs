//! Acquired deform sets prevent stale-generation aliasing across Puppets. The
//! manifest is written only after every required artifact is durable.
use super::{
    artifacts::*,
    frame::{self, FrameRuntime},
    overlay,
    spec::{Framing, ResolvedRequest, ResolvedSpec, bad},
    terminal,
    video::{self, VideoSink},
};
use crate::Error;
use catchlight_core::{Model, ModelFormat};
use catchlight_wgpu::{PrepareOptions, RenderCache, RenderContext, RenderList};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone)]
pub enum Destination {
    Terminal,
    Png(PathBuf),
    Video(PathBuf),
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
    /// Routes Ctrl-C to this token for the rest of the process; a second
    /// Ctrl-C exits at once, for work that never reaches a cancellation check.
    /// A process can install only one handler, so this belongs in `main`.
    pub fn install(&self) -> Result<(), Error> {
        let token = self.clone();
        ctrlc::set_handler(move || {
            if token.is_cancelled() {
                std::process::exit(130);
            }
            token.cancel();
        })
        .map_err(Error::CancellationHandler)
    }
    fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
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
    // The bounded read also covers files that grow after the metadata check.
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
    let model = catchlight_core::load_model(&bytes, format)
        .map_err(|source| crate::file::not_a_model(path, &bytes, source))?;
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
    if let Destination::Video(_) = &destination {
        if plan.requests.len() != 1
            || plan
                .requests
                .values()
                .any(|r| r.video.is_none() || r.overlay.is_some() || !r.geometry.is_empty())
        {
            return Err(bad(
                "video output takes one animated request without overlays or geometry",
            ));
        }
    } else if directory.is_none()
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
    if plan.requests.values().any(|r| r.video.is_some())
        && let Err(error) = video::preflight()
    {
        if let Some(run) = &mut manifest {
            run.failure = Some(RunFailure {
                kind: "render_failed".into(),
                request: None,
            });
        }
        persist_failed_manifest(&directory, &manifest);
        return Err(error);
    }
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
            let mut runtime = FrameRuntime::new(&model, request)?;
            let mut trace = directory
                .as_ref()
                .filter(|_| request.animation.is_some())
                .map(|dir| crate::file::temp_beside(&dir.join(format!("{name}--trace.jsonl")), ""))
                .transpose()?;
            let video_path = request.video.as_ref().and_then(|spec| match &destination {
                Destination::Directory(dir) => {
                    Some(dir.join(format!("{name}.{}", spec.format.extension())))
                }
                Destination::Video(path) => Some(path.clone()),
                _ => None,
            });
            let mut sink = match (&request.video, request.frame_interval(), video_path) {
                (Some(spec), Some(interval), Some(path)) => Some(VideoSink::start(
                    &path,
                    spec.format,
                    request.framing.size,
                    interval,
                    &request.background,
                )?),
                _ => None,
            };
            let clean_frames = request.video.as_ref().is_none_or(|v| v.keep_frames);
            let deform_set = ctx.renderer.acquire_deform_set();
            let request_result = (|| {
                let count = request.animation.as_ref().map_or(1, |a| a.frames.count);
                let mut listed = false;
                for index in 0..count {
                    cancel.check()?;
                    if index > 0 {
                        runtime.advance(&model);
                    }
                    validate_effective_pose(&model, &runtime.puppet)?;
                    if let Some(trace) = &mut trace {
                        let record = TraceFrame::observe(name, request, &model, &runtime)?;
                        serde_json::to_writer(trace.as_file_mut(), &record)
                            .map_err(std::io::Error::from)
                            .and_then(|()| trace.write_all(b"\n"))
                            .map_err(|e| Error::io(trace.path(), e))?;
                    }
                    if request
                        .animation
                        .as_ref()
                        .is_some_and(|a| !a.frames.is_captured(index))
                    {
                        continue;
                    }
                    let framing = request.framing_at(index)?;
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
                    if request.only_parts.is_some() || !request.hide_color.is_empty() {
                        cache
                            .retain_part_colors(&mut list, |id| frame::color_retained(request, id));
                    }
                    if !listed {
                        listings.push(super::listing::listing(&list));
                        listed = true;
                    }
                    let mut pixels = draw(&mut ctx, &list, request, &framing)?;
                    if let Some(sink) = &mut sink {
                        sink.push(&pixels)?;
                    }
                    let frame = request.animation.as_ref().map(|_| index);
                    let stem = frame.map_or_else(|| name.clone(), |n| format!("{name}--{n:06}"));
                    match &destination {
                        Destination::Png(path) => atomic_write(path, &png(&pixels, framing.size)?)?,
                        Destination::Video(_) => {}
                        Destination::Terminal => {
                            if terminal::supported() {
                                terminal::present(&png(&pixels, framing.size)?, std::io::stdout())
                                    .map_err(Error::Stdout)?;
                            } else {
                                eprintln!(
                                    "Use --out image.png or --out-dir directory to save this render."
                                );
                            }
                        }
                        Destination::Directory(dir) => {
                            if clean_frames {
                                let filename = format!("{stem}.png");
                                atomic_write(&dir.join(&filename), &png(&pixels, framing.size)?)?;
                                record(&mut manifest, name, filename, frame, OutputKind::Image);
                            }
                            if let Some(overlay) = &request.overlay {
                                overlay::draw(
                                    &mut pixels,
                                    &model,
                                    &runtime.puppet,
                                    &framing,
                                    &overlay.mesh,
                                )?;
                                let filename = format!("{stem}--mesh.png");
                                atomic_write(&dir.join(&filename), &png(&pixels, framing.size)?)?;
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
            if let Some(sink) = sink {
                cancel.check()?;
                sink.finish()?;
                if let (Some(spec), Destination::Directory(_)) = (&request.video, &destination) {
                    let filename = format!("{name}.{}", spec.format.extension());
                    record(&mut manifest, name, filename, None, OutputKind::Video);
                }
            }
            if let (Some(trace), Some(dir)) = (trace, directory.as_ref()) {
                let filename = format!("{name}--trace.jsonl");
                crate::file::persist(trace.into_temp_path(), &dir.join(&filename))?;
                record(&mut manifest, name, filename, None, OutputKind::Trace);
            }
            if let Some(run) = &mut manifest
                && let Some(result) = run.requests.get_mut(name)
            {
                result.complete = true;
                if request.animation.is_none() {
                    result.pose = Some(effective_pose(&model, &runtime.puppet)?);
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
            persist_failed_manifest(&directory, &manifest);
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
/// A failed run's manifest is best effort: the disk that refused the render
/// often refuses run.json too, and the render's own error is the one to report.
fn persist_failed_manifest(dir: &Option<PathBuf>, manifest: &Option<RunManifest>) {
    if let Err(error) = persist_manifest(dir, manifest) {
        eprintln!("catchlight-cli: recording the failed run: {error}");
    }
}
fn draw(
    ctx: &mut RenderContext,
    list: &RenderList,
    request: &ResolvedRequest,
    framing: &Framing,
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
                center: glam::Vec2::from_array(framing.center),
                height: framing.height,
            },
            framing.size[0],
            framing.size[1],
            Some(clear),
        )
        .map_err(|e| Error::gpu("render", e))?;
    catchlight_core::texture::unpremultiply_linear_from_srgb_inplace(&mut pixels);
    Ok(pixels)
}
