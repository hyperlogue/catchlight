use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::renderer::{RenderAdapter, RenderDevice, RenderQueue};
use bevy::render::view::ViewTarget;
use catchlight_core::Model;
use catchlight_wgpu::{
    CompositePool, DeformSet, FrameStats, FramebufferSnapshotPool, Pipelines, PrepareOptions,
    RenderCache, RenderList, StencilTarget, WgpuRenderer,
};

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
            .puppets
            .iter()
            .find(|(key, puppet)| key.entity == render_entity && puppet.collected)
            .map(|(_, puppet)| puppet.render_list.clone())
    }

    /// What the render node last recorded for the renderer one render-world
    /// entity draws through: the tallies `catchlight-wgpu` keeps for the most
    /// recent `render_lists_ext` call. Zeroed until it has drawn once.
    ///
    /// A renderer is shared by every puppet of one model, so this is the
    /// whole frame's tally for that model, not this entity's share of it.
    /// Like [`Self::collected_drawables`], the key is the paired
    /// `RenderEntity`, and this is for tests and debug overlays.
    pub fn frame_stats(&self, render_entity: Entity) -> Option<FrameStats> {
        let inner = self.inner.lock().ok()?;
        let puppet = inner
            .puppets
            .iter()
            .find(|(key, _)| key.entity == render_entity)
            .map(|(_, puppet)| puppet)?;
        Some(inner.gpus.get(&puppet.model)?.renderer.frame_stats())
    }

    /// How many render caches are resident: one per **model** per view
    /// format, however many puppets animate it. Two puppets of one model
    /// hold one between them.
    pub fn resident_caches(&self) -> usize {
        self.inner.lock().map(|inner| inner.gpus.len()).unwrap_or(0)
    }

    /// How many puppets have render-world state: one per entity per view
    /// format. Each is a deform set and a render list against the cache its
    /// model holds.
    pub fn resident_puppets(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.puppets.len())
            .unwrap_or(0)
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

/// A model's GPU copy, keyed by the model itself rather than by any entity.
///
/// The model is held by **pointer identity**, not by `AssetId`: an id survives
/// its asset's *value* being replaced — a hot reload, an `Assets::insert` —
/// and the replacement is a different model whose generation counter starts
/// over, so neither the id nor the generation would notice. A different `Arc`
/// always means a different model. `ModelGpu` holds that `Arc`, so the address
/// this key carries cannot be reused while the entry lives.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ModelKey {
    model: usize,
    pub format: wgpu::TextureFormat,
}

impl ModelKey {
    fn new(model: &Arc<Model>, format: wgpu::TextureFormat) -> Self {
        Self {
            model: Arc::as_ptr(model) as usize,
            format,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PuppetKey {
    pub entity: Entity,
    pub format: wgpu::TextureFormat,
}

/// One model's GPU state for one view format: the renderer that holds it and
/// the render cache naming its slots. **Every** puppet of that model draws
/// through it.
///
/// **One cache, one renderer, N puppets.** The first half is
/// `catchlight-wgpu`'s own rule: a cache's slots name state inside the
/// renderer that prepared it, so two caches on one renderer would overwrite
/// each other's mesh and texture slots. The second half is what makes 50
/// puppets of one rig affordable: a model's textures and meshes do not depend
/// on how it is posed, so N puppets share one decode and one upload of both.
/// What a puppet owns is in [`PuppetGpu`].
pub(crate) struct ModelGpu {
    pub renderer: WgpuRenderer,
    pub cache: RenderCache,
    /// The model the cache was prepared from. Holding it keeps the `Arc` alive
    /// for as long as [`ModelKey`] names its address.
    pub model: Arc<Model>,
    /// What the cache was prepared with. Changing `CatchlightSettings`
    /// re-prepares, so a texture budget is live rather than fixed at load.
    pub options: PrepareOptions,
}

/// One puppet's per-frame state against the [`ModelGpu`] it draws through: its
/// slice of that renderer's deform atlas, and the drawables the last extract
/// collected.
///
/// A deform set is the whole of what a second puppet of one model costs on the
/// GPU — two floats per vertex — against the textures and meshes it no longer
/// duplicates.
pub(crate) struct PuppetGpu {
    /// Which model's GPU state this puppet draws through. A puppet whose
    /// entity swapped models points at a different key from the next prepare.
    pub model: ModelKey,
    /// This puppet's slice of `model`'s renderer deform atlas. Released back
    /// to that renderer when the puppet goes.
    pub deform_set: DeformSet,
    /// Refilled by extraction, drawn by the render node. Empty until the first
    /// extract after the cache was prepared.
    pub render_list: RenderList,
    /// Whether `render_list` has been collected against the current cache.
    pub collected: bool,
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
    /// Per-model GPU state: one renderer and one cache per model per view
    /// format, shared by every puppet of it.
    pub gpus: HashMap<ModelKey, ModelGpu>,
    /// Per-puppet state against those caches, keyed by render-world entity
    /// and view format.
    pub puppets: HashMap<PuppetKey, PuppetGpu>,
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
    /// to find the state an entity has.
    pub(crate) fn formats(&self) -> Vec<wgpu::TextureFormat> {
        self.formats.keys().copied().collect()
    }
}

/// Prepare system: build a renderer and a render cache for every *model* that
/// does not have one yet, give every puppet of it a deform set, and drop what
/// no live entity names any more.
///
/// Preparing here rather than at extract is what keeps the GPU uploads out of
/// the frame's sync point, at the cost of one frame: a puppet that appears on
/// frame N gets its cache at the end of frame N and its first collected
/// drawables at frame N+1's extract, so it draws from N+1 on. An entity that
/// swaps models pays the same frame again: extract finds its `PuppetGpu`
/// pointing at the model it left, stops collecting, and this rebinds it.
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
        for (format, format_pipelines) in &pipelines {
            let model_key = ModelKey::new(&ex.model, *format);
            let CatchlightRenderInner { gpus, puppets, .. } = &mut *inner;
            let model_gpu = match gpus.entry(model_key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let mut renderer = WgpuRenderer::from_pipelines(
                        device.wgpu_device().clone(),
                        (**queue.0).clone(),
                        format_pipelines.clone(),
                    );
                    let cache = match RenderCache::prepare(&mut renderer, &ex.model, options) {
                        Ok(cache) => cache,
                        Err(error) => {
                            tracing::error!(
                                "catchlight: preparing the render cache failed: {error}"
                            );
                            continue;
                        }
                    };
                    entry.insert(ModelGpu {
                        renderer,
                        cache,
                        model: ex.model.clone(),
                        options,
                    })
                }
            };

            // A texture budget is live rather than fixed at load, so a
            // settings change re-prepares the shared cache. Every puppet of
            // it then holds a list whose slots name the build that went, so
            // none may draw until the next extract has collected again.
            if model_gpu.options != options {
                match RenderCache::prepare(&mut model_gpu.renderer, &model_gpu.model, options) {
                    Ok(cache) => {
                        model_gpu.cache = cache;
                        model_gpu.options = options;
                        for puppet in puppets.values_mut() {
                            if puppet.model == model_key {
                                puppet.collected = false;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::error!("catchlight: re-preparing with new options: {error}");
                    }
                }
            }

            let puppet_key = PuppetKey {
                entity,
                format: *format,
            };
            match puppets.get_mut(&puppet_key) {
                // Bound to the model it still animates: nothing to do.
                Some(puppet) if puppet.model == model_key => {}
                // The entity swapped models. Its set belongs to the renderer
                // it left, so hand that one back before taking a new one.
                Some(puppet) => {
                    let old = std::mem::replace(&mut puppet.model, model_key);
                    puppet.collected = false;
                    puppet.render_list = RenderList::default();
                    let released = puppet.deform_set;
                    puppet.deform_set = model_gpu.renderer.acquire_deform_set();
                    if let Some(old_gpu) = gpus.get_mut(&old) {
                        old_gpu.renderer.release_deform_set(released);
                    }
                }
                None => {
                    puppets.insert(
                        puppet_key,
                        PuppetGpu {
                            model: model_key,
                            deform_set: model_gpu.renderer.acquire_deform_set(),
                            render_list: RenderList::default(),
                            collected: false,
                        },
                    );
                }
            }
        }
    }

    // GC puppets whose render entities are gone (entity despawned, or
    // `CatchlightPuppet` removed — SyncComponentPlugin despawns the
    // render-world entity, so the puppet drops out of `extracted`), then the
    // model caches no surviving puppet draws through.
    let CatchlightRenderInner {
        gpus,
        puppets,
        live_scratch,
        ..
    } = &mut *inner;
    live_scratch.clear();
    live_scratch.extend(extracted.iter().map(|(e, _)| e));
    puppets.retain(|key, puppet| {
        let live = live_scratch.contains(&key.entity) && view_formats.contains(&key.format);
        if !live {
            if let Some(gpu) = gpus.get_mut(&puppet.model) {
                gpu.renderer.release_deform_set(puppet.deform_set);
            }
        }
        live
    });
    gpus.retain(|key, _| puppets.values().any(|puppet| puppet.model == *key));

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
