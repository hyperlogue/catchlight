use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::renderer::{RenderAdapter, RenderDevice, RenderQueue};
use bevy::render::view::ViewTarget;
use catchlight_core::Model;
use catchlight_wgpu::{
    DeformSet, FrameStats, FrameTargets, Pipelines, PrepareOptions, RenderCache, RenderList,
    Submission, WgpuRenderer,
};

use crate::asset::CatchlightModel;
use crate::extract::{ExtractedCatchlightCamera, ExtractedPuppet};

/// Mutex-guarded because render graph nodes have no mutable resource access.
#[derive(Resource, Default)]
pub struct CatchlightRenderState {
    pub(crate) inner: Mutex<CatchlightRenderInner>,
}

impl CatchlightRenderState {
    /// Summed top-level GPU pass time in nanoseconds, lagging 2-3 frames.
    /// Call from a render-world system in `Cleanup`, after the graph submits.
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

    /// Keyed by the paired `RenderEntity`, not the main-world entity.
    pub fn collected_drawables(&self, render_entity: Entity) -> Option<RenderList> {
        let inner = self.inner.lock().ok()?;
        inner
            .puppets
            .iter()
            .find(|(key, puppet)| key.entity == render_entity && puppet.collected)
            .map(|(_, puppet)| puppet.render_list.clone())
    }

    /// The whole model's tally, since a renderer is shared by every puppet of
    /// one model. Keyed by the paired `RenderEntity`.
    pub fn frame_stats(&self, render_entity: Entity) -> Option<FrameStats> {
        let inner = self.inner.lock().ok()?;
        let puppet = inner
            .puppets
            .iter()
            .find(|(key, _)| key.entity == render_entity)
            .map(|(_, puppet)| puppet)?;
        Some(inner.gpus.get(&puppet.model)?.renderer.frame_stats())
    }

    #[doc(hidden)]
    pub fn camera_views_issued(&self, render_entity: Entity) -> Option<u32> {
        let inner = self.inner.lock().ok()?;
        let puppet = inner
            .puppets
            .iter()
            .find(|(key, _)| key.entity == render_entity)
            .map(|(_, puppet)| puppet)?;
        Some(inner.gpus.get(&puppet.model)?.submission.views_issued())
    }

    pub fn resident_caches(&self) -> usize {
        self.inner.lock().map(|inner| inner.gpus.len()).unwrap_or(0)
    }

    pub fn resident_puppets(&self) -> usize {
        self.inner
            .lock()
            .map(|inner| inner.puppets.len())
            .unwrap_or(0)
    }
}

pub(crate) struct FormatResources {
    pub pipelines: Arc<Pipelines>,
    /// Per view: two cameras of different sizes would otherwise rebuild one
    /// set of targets on every frame.
    pub targets: HashMap<Entity, FrameTargets>,
}

/// Keyed by asset id: an edit keeps the cache and rebuilds it in place. A
/// replaced value under the same id shows as a new `Model::identity`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ModelKey {
    pub model: AssetId<CatchlightModel>,
    pub format: wgpu::TextureFormat,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PuppetKey {
    pub entity: Entity,
    pub format: wgpu::TextureFormat,
}

/// One cache per renderer: two caches on one renderer would overwrite each
/// other's slots.
pub(crate) struct ModelGpu {
    pub renderer: WgpuRenderer,
    /// Must be replaced every frame: bevy owns the submit, so the renderer
    /// cannot see the frame boundary.
    pub submission: Submission,
    pub cache: RenderCache,
    /// The model state `cache` was last built or rebuilt from.
    pub model: Arc<Model>,
    pub options: PrepareOptions,
}

pub(crate) struct PuppetGpu {
    pub model: ModelKey,
    /// Must be released to `model`'s renderer, not another.
    pub deform_set: DeformSet,
    pub render_list: RenderList,
    pub collected: bool,
}

pub(crate) fn format_resources<'a>(
    formats: &'a mut HashMap<wgpu::TextureFormat, FormatResources>,
    format: wgpu::TextureFormat,
    adapter: &wgpu::Adapter,
    device: &wgpu::Device,
) -> &'a mut FormatResources {
    formats.entry(format).or_insert_with(|| {
        let pipelines = Arc::new(Pipelines::new_autodetect(adapter, device, format));
        FormatResources {
            targets: HashMap::new(),
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
    pub gpus: HashMap<ModelKey, ModelGpu>,
    pub puppets: HashMap<PuppetKey, PuppetGpu>,
    pub(crate) formats: HashMap<wgpu::TextureFormat, FormatResources>,
    pub size: (u32, u32),
    pub(crate) options: PrepareOptions,
    pub(crate) missing_format_warned: bool,
    live_scratch: HashSet<Entity>,
    live_models: HashSet<ModelKey>,
}

impl CatchlightRenderInner {
    pub(crate) fn formats(&self) -> Vec<wgpu::TextureFormat> {
        self.formats.keys().copied().collect()
    }
}

/// Runs in prepare rather than extract to keep uploads out of the sync point,
/// so a new puppet draws one frame late.
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

    for resources in inner.formats.values_mut() {
        let format = resources.pipelines.surface_format;
        resources.targets.retain(|entity, _| {
            views
                .get(*entity)
                .is_ok_and(|target| target.main_texture_format() == format)
        });
    }

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
            let model_key = ModelKey {
                model: ex.model_id,
                format: *format,
            };
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
                    let submission = renderer.submission();
                    entry.insert(ModelGpu {
                        renderer,
                        submission,
                        cache,
                        model: ex.model.clone(),
                        options,
                    })
                }
            };

            // Lists collected against the old cache name stale slots.
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
                Some(puppet) if puppet.model == model_key => {}
                // Swapped models: release the set to the renderer it left.
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

    let CatchlightRenderInner {
        gpus,
        puppets,
        live_scratch,
        live_models,
        ..
    } = &mut *inner;
    live_scratch.clear();
    live_scratch.extend(extracted.iter().map(|(e, _)| e));
    live_models.clear();
    puppets.retain(|key, puppet| {
        let live = live_scratch.contains(&key.entity) && view_formats.contains(&key.format);
        match live {
            true => {
                live_models.insert(puppet.model);
            }
            false => {
                if let Some(gpu) = gpus.get_mut(&puppet.model) {
                    gpu.renderer.release_deform_set(puppet.deform_set);
                }
            }
        }
        live
    });
    gpus.retain(|key, _| live_models.contains(key));

    // Once per frame, not per camera, and after last frame's submit:
    // end_gpu_frame maps the timestamp read buffer.
    for gpu in gpus.values_mut() {
        gpu.renderer.end_gpu_frame();
        // Reusing a token across frames runs the camera-slot ring out.
        gpu.submission = gpu.renderer.submission();
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
