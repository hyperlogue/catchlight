use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::renderer::{RenderAdapter, RenderDevice, RenderQueue};
use bevy::render::view::ViewTarget;
use catchlight_core::{Model, Puppet};
use catchlight_wgpu::{
    CompositePool, FrameStats, FramebufferSnapshotPool, Pipelines, PrepareOptions, RenderCache,
    RenderList, StencilTarget, WgpuRenderer,
};

use crate::asset::CatchlightModel;
use crate::extract::{ExtractedCatchlightCamera, ExtractedPuppet};

/// Single render-world resource that owns every piece of mutable
/// catchlight state the render node needs. Guarded by a Mutex so the
/// node can mutate via `&Res<..>` — render graph nodes don't have
/// mutable resource access.
#[derive(Resource, Default)]
pub struct CatchlightRenderState {
    pub(crate) inner: Mutex<CatchlightRenderInner>,
}

impl CatchlightRenderState {
    /// Drain each renderer's most recently *finished* GPU timer-query
    /// frame and return the summed top-level pass time in nanoseconds.
    /// `None` when no frame has completed since the last call — query
    /// results resolve 2-3 frames behind the displayed frame, so this
    /// is a lagging indicator by design. Intended for overlays/perf
    /// HUDs, called from a render-world system in `Cleanup` (after the
    /// graph has submitted).
    pub fn drain_gpu_frame_ns(&self) -> Option<u64> {
        let Ok(mut inner) = self.inner.lock() else {
            return None;
        };
        let mut total_ns: u64 = 0;
        let mut got_any = false;
        for gpu in inner.gpus.values_mut() {
            if let Some(frame) = gpu.renderer.process_gpu_frame() {
                got_any = true;
                let frame_ns: u64 = frame
                    .iter()
                    .filter_map(|r| r.time.as_ref())
                    .map(|t| ((t.end - t.start) * 1e9).max(0.0) as u64)
                    .sum();
                total_ns = total_ns.saturating_add(frame_ns);
            }
        }
        got_any.then_some(total_ns)
    }

    /// The drawables the last extraction collected for one render-world
    /// entity, in the first view format it has GPU state for.
    ///
    /// `None` until a cache has been prepared *and* collected into — the one
    /// frame after a puppet appears — and `None` for a main-world entity: the
    /// key is the paired `RenderEntity`. A clone, meant for tests and
    /// debugging rather than for the frame.
    pub fn collected_drawables(&self, render_entity: Entity) -> Option<RenderList> {
        let inner = self.inner.lock().ok()?;
        inner
            .gpus
            .iter()
            .find(|(key, gpu)| key.entity == render_entity && gpu.collected)
            .map(|(_, gpu)| gpu.render_list.clone())
    }

    /// What the render node last recorded for one render-world entity: the
    /// tallies `catchlight-wgpu` keeps for the most recent `render_list_ext`
    /// call on that puppet's renderer. Zeroed until it has drawn once.
    ///
    /// Like [`Self::collected_drawables`], the key is the paired
    /// `RenderEntity`, and this is for tests and debug overlays.
    pub fn frame_stats(&self, render_entity: Entity) -> Option<FrameStats> {
        let inner = self.inner.lock().ok()?;
        inner
            .gpus
            .iter()
            .find(|(key, _)| key.entity == render_entity)
            .map(|(_, gpu)| gpu.renderer.frame_stats())
    }

    /// How many render caches are resident: one per puppet per view format.
    /// Two puppets of one model hold two, which is what a renderer's single
    /// deform buffer forces; see the module doc of `crate::lib`.
    pub fn resident_caches(&self) -> usize {
        self.inner.lock().map(|inner| inner.gpus.len()).unwrap_or(0)
    }
}

/// Pipelines plus the offscreen targets whose texture format is coupled
/// to `Pipelines::surface_format` (composite slots, framebuffer
/// snapshots, the WebGL alpha-mask stencil fallback).
pub(crate) struct FormatResources {
    pub pipelines: Arc<Pipelines>,
    pub stencil: StencilTarget,
    pub composites: CompositePool,
    pub snapshots: FramebufferSnapshotPool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RendererKey {
    pub entity: Entity,
    pub format: wgpu::TextureFormat,
}

/// One puppet's GPU state for one view format: the renderer that holds it, the
/// render cache naming its slots, and the drawables the last extract collected.
///
/// **One cache, one renderer, one puppet.** The first half is
/// `catchlight-wgpu`'s own rule: a cache's slots name state inside the renderer
/// that prepared it. The second half is bevy's: a renderer holds exactly one
/// puppet's deforms, because a deform lives at a byte range decided by its mesh
/// slot and every draw in a frame is recorded into one submit, so a second
/// puppet's upload would overwrite the first's before either draws. Two
/// entities animating one model therefore share the `Model` (one asset, one
/// `Arc`, one decode) and hold a cache each. Sharing the *GPU* copy needs a
/// per-puppet deform region in `catchlight-wgpu`, which is a renderer change,
/// not a bevy one.
pub(crate) struct PuppetGpu {
    pub renderer: WgpuRenderer,
    pub cache: RenderCache,
    /// The asset the cache was prepared from; a different id means the entity
    /// swapped models and the cache has to be rebuilt.
    pub model: AssetId<CatchlightModel>,
    /// What the cache was prepared with. Changing `CatchlightSettings`
    /// re-prepares, so a texture budget is live rather than fixed at load.
    pub options: PrepareOptions,
    /// Refilled by extraction, drawn by the render node. Empty until the first
    /// extract after this cache was prepared.
    pub render_list: RenderList,
    /// Whether `render_list` has been collected against the current cache.
    pub collected: bool,
}

impl PuppetGpu {
    /// Rebuild the cache when the entity swapped models, then push this
    /// frame's deforms at the GPU and collect the drawables to draw.
    ///
    /// Runs at extract time, with the main world paused, which is what makes
    /// reading the live puppet safe: bevy's pipelined rendering may run the
    /// next main-world frame while the render world draws, and that frame
    /// overwrites the puppet's combined-deform buffers in place.
    pub(crate) fn refresh(
        &mut self,
        model: &Model,
        model_id: AssetId<CatchlightModel>,
        puppet: &Puppet,
        options: PrepareOptions,
    ) {
        if self.model != model_id || self.options != options {
            match RenderCache::prepare(&mut self.renderer, model, options) {
                Ok(cache) => {
                    self.cache = cache;
                    self.model = model_id;
                    self.options = options;
                    self.collected = false;
                }
                Err(error) => {
                    tracing::error!("catchlight: preparing a swapped model failed: {error}");
                    return;
                }
            }
        }
        if let Err(error) = self.cache.refresh(&mut self.renderer, model, puppet) {
            tracing::error!("catchlight: refreshing the render cache failed: {error}");
            return;
        }
        self.cache.collect_into(puppet, &mut self.render_list);
        self.collected = true;
    }
}

/// Get-or-build the resources for one `ViewTarget` main-texture format
/// (`Rgba8UnormSrgb` for ordinary cameras, `Rgba16Float` for `hdr: true`).
/// Building compiles ~18 pipelines (~20 ms) — once per format ever seen.
pub(crate) fn format_resources<'a>(
    formats: &'a mut HashMap<wgpu::TextureFormat, FormatResources>,
    format: wgpu::TextureFormat,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
) -> &'a mut FormatResources {
    formats.entry(format).or_insert_with(|| {
        let pipelines = Arc::new(Pipelines::new_autodetect(adapter, device, format));
        // 1x1 placeholders: the render node resizes them to each view's
        // target before drawing — a render-to-texture or second-window
        // camera has its own size, so sizing from any single window here
        // would be wrong for the rest.
        FormatResources {
            stencil: StencilTarget::new_for_pipelines(&pipelines, device, 1, 1),
            composites: CompositePool::new(1, 1),
            snapshots: FramebufferSnapshotPool::new(1, 1),
            pipelines,
        }
    })
}

fn unique_formats(
    formats: impl IntoIterator<Item = wgpu::TextureFormat>,
) -> Vec<wgpu::TextureFormat> {
    let mut unique = Vec::new();
    for format in formats {
        if !unique.contains(&format) {
            unique.push(format);
        }
    }
    unique
}

#[derive(Default)]
pub(crate) struct CatchlightRenderInner {
    /// Per-puppet GPU state, keyed by render-world entity and view format.
    pub gpus: HashMap<RendererKey, PuppetGpu>,
    pub(crate) formats: HashMap<wgpu::TextureFormat, FormatResources>,
    pub size: (u32, u32),
    /// What the main world's `CatchlightSettings` asked for, copied in at
    /// extract so both extraction and preparation build caches the same way.
    pub(crate) options: PrepareOptions,
    pub(crate) missing_format_warned: bool,
    pub(crate) camera_overflow_warned: bool,
    // Reused across frames so GC doesn't allocate a fresh HashSet every tick.
    live_scratch: HashSet<Entity>,
}

impl CatchlightRenderInner {
    /// The view formats pipelines have been built for. Extraction walks these
    /// to find the caches an entity has.
    pub(crate) fn formats(&self) -> Vec<wgpu::TextureFormat> {
        self.formats.keys().copied().collect()
    }
}

/// Prepare system: build a renderer and a render cache for every puppet that
/// does not have one yet, and drop the ones whose entity is gone.
///
/// Preparing here rather than at extract is what keeps the GPU uploads out of
/// the frame's sync point, at the cost of one frame: a puppet that appears on
/// frame N gets its cache at the end of frame N and its first collected
/// drawables at frame N+1's extract, so it draws from N+1 on.
// Invariant: the render-state Mutex is only poisoned on panic, treated as fatal.
#[allow(clippy::unwrap_used)]
pub(crate) fn prepare_puppets(
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    adapter: Res<RenderAdapter>,
    state: Res<CatchlightRenderState>,
    extracted: Query<(Entity, &ExtractedPuppet)>,
    views: Query<&ViewTarget, With<ExtractedCatchlightCamera>>,
) {
    let mut inner = state.inner.lock().unwrap();
    let options = inner.options;

    let view_formats = unique_formats(views.iter().map(ViewTarget::main_texture_format));
    let pipelines: Vec<_> = view_formats
        .iter()
        .map(|&format| {
            let pipelines =
                format_resources(&mut inner.formats, format, &adapter.0, device.wgpu_device())
                    .pipelines
                    .clone();
            (format, pipelines)
        })
        .collect();

    for (entity, ex) in &extracted {
        for (format, pipelines) in &pipelines {
            let key = RendererKey {
                entity,
                format: *format,
            };
            if inner.gpus.contains_key(&key) {
                continue;
            }
            let mut renderer = WgpuRenderer::from_pipelines(
                device.wgpu_device().clone(),
                (**queue.0).clone(),
                pipelines.clone(),
            );
            let cache = match RenderCache::prepare(&mut renderer, &ex.model, options) {
                Ok(cache) => cache,
                Err(error) => {
                    tracing::error!("catchlight: preparing the render cache failed: {error}");
                    continue;
                }
            };
            inner.gpus.insert(
                key,
                PuppetGpu {
                    renderer,
                    cache,
                    model: ex.model_id,
                    options,
                    render_list: RenderList::default(),
                    collected: false,
                },
            );
        }
    }

    // GC renderers whose render entities are gone (entity despawned, or
    // `CatchlightPuppet` removed — SyncComponentPlugin despawns the
    // render-world entity, so the puppet drops out of `extracted`).
    let CatchlightRenderInner {
        gpus, live_scratch, ..
    } = &mut *inner;
    live_scratch.clear();
    live_scratch.extend(extracted.iter().map(|(e, _)| e));
    gpus.retain(|key, _| live_scratch.contains(&key.entity) && view_formats.contains(&key.format));

    // Close the previous frame's GPU-profiler queries — exactly once per
    // frame, not once per CatchlightCamera. end_frame maps the timestamp
    // read buffer, so it must run after that frame's encoder was submitted:
    // last frame's render graph (which submits) ran before this Prepare, and
    // this frame's render node has not recorded queries yet. Doing it here
    // keeps the per-view render node to recording + resolving only. GPU
    // timing for the overlay therefore trails by one frame.
    for gpu in gpus.values_mut() {
        gpu.renderer.end_gpu_frame();
        gpu.renderer.begin_camera_submit();
    }
}

#[cfg(test)]
mod tests {
    use super::unique_formats;

    #[test]
    fn keeps_every_distinct_marked_view_format() {
        let formats = unique_formats([
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ]);

        assert_eq!(
            formats,
            [
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureFormat::Rgba16Float,
            ]
        );
    }
}
