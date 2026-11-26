use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::view::ViewTarget;
use catchlight_core::Model;
use catchlight_wgpu::{
    DeformSet, FrameStats, FrameTargets, PipelineOptions, Pipelines, PrepareOptions, RenderCache,
    RenderList, Submission, WgpuRenderer,
};

use crate::asset::CatchlightModel;
use crate::extract::{ExtractedCatchlightCamera, ExtractedPuppet};

/// The render world's caches, renderers and per-puppet state.
#[derive(Resource, Default)]
pub struct CatchlightRenderState {
    pub(crate) gpus: HashMap<ModelKey, ModelGpu>,
    pub(crate) puppets: HashMap<PuppetKey, PuppetGpu>,
    pub(crate) formats: HashMap<wgpu::TextureFormat, FormatResources>,
    pub(crate) options: PrepareOptions,
    pub(crate) missing_format_warned: bool,
    pub(crate) failures: PrepareFailures,
    live_scratch: HashSet<Entity>,
    live_models: HashSet<ModelKey>,
}

impl CatchlightRenderState {
    /// Summed top-level GPU pass time in nanoseconds, lagging 2-3 frames.
    /// Call from a render-world system in `Cleanup`, after the graph submits.
    pub fn drain_gpu_frame_ns(&mut self) -> Option<u64> {
        let mut total_ns: u64 = 0;
        let mut got_any = false;
        for gpu in self.gpus.values_mut() {
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
    #[must_use]
    pub fn collected_drawables(&self, render_entity: Entity) -> Option<RenderList> {
        self.puppets
            .iter()
            .find(|(key, puppet)| key.entity == render_entity && puppet.collected)
            .map(|(_, puppet)| puppet.render_list.clone())
    }

    /// The whole model's tally, since a renderer is shared by every puppet of
    /// one model. Keyed by the paired `RenderEntity`.
    #[must_use]
    pub fn frame_stats(&self, render_entity: Entity) -> Option<FrameStats> {
        Some(self.gpu_of(render_entity)?.renderer.frame_stats())
    }

    #[doc(hidden)]
    #[must_use]
    pub fn camera_views_issued(&self, render_entity: Entity) -> Option<u32> {
        Some(self.gpu_of(render_entity)?.submission.views_issued())
    }

    #[must_use]
    pub fn resident_caches(&self) -> usize {
        self.gpus.len()
    }

    #[must_use]
    pub fn resident_puppets(&self) -> usize {
        self.puppets.len()
    }

    fn gpu_of(&self, render_entity: Entity) -> Option<&ModelGpu> {
        let puppet = self
            .puppets
            .iter()
            .find(|(key, _)| key.entity == render_entity)
            .map(|(_, puppet)| puppet)?;
        self.gpus.get(&puppet.model)
    }

    pub(crate) fn formats(&self) -> Vec<wgpu::TextureFormat> {
        self.formats.keys().copied().collect()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrepareAttempt {
    identity: u64,
    generation: u64,
    options: PrepareOptions,
}

/// Failed prepares, so one is retried, and logged, only once the model or
/// the options change rather than on every frame.
#[derive(Default)]
pub(crate) struct PrepareFailures(HashMap<ModelKey, PrepareAttempt>);

impl PrepareFailures {
    fn attempt(model: &Model, options: PrepareOptions) -> PrepareAttempt {
        PrepareAttempt {
            identity: model.identity(),
            generation: model.generation(),
            options,
        }
    }

    pub(crate) fn failed(&self, key: ModelKey, model: &Model, options: PrepareOptions) -> bool {
        self.0.get(&key) == Some(&Self::attempt(model, options))
    }

    pub(crate) fn prepare(
        &mut self,
        key: ModelKey,
        renderer: &mut WgpuRenderer,
        model: &Model,
        options: PrepareOptions,
    ) -> Option<RenderCache> {
        match RenderCache::prepare(renderer, model, options) {
            Ok(cache) => {
                self.0.remove(&key);
                Some(cache)
            }
            Err(error) => {
                tracing::error!("catchlight: preparing the render cache failed: {error}");
                self.0.insert(key, Self::attempt(model, options));
                None
            }
        }
    }
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
    /// Issued by `model`'s renderer.
    pub deform_set: DeformSet,
    pub render_list: RenderList,
    pub collected: bool,
}

pub(crate) fn format_resources<'a>(
    formats: &'a mut HashMap<wgpu::TextureFormat, FormatResources>,
    format: wgpu::TextureFormat,
    device: &wgpu::Device,
) -> &'a mut FormatResources {
    formats.entry(format).or_insert_with(|| {
        let pipelines = Arc::new(Pipelines::new(device, format, PipelineOptions::default()));
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

fn release(renderer: &mut WgpuRenderer, set: DeformSet) {
    if let Err(error) = renderer.release_deform_set(set) {
        tracing::error!("catchlight: releasing a deform set failed: {error}");
    }
}

/// Runs in prepare rather than extract to keep uploads out of the sync point,
/// so a new puppet draws one frame late.
pub(crate) fn prepare_puppets(
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut state: ResMut<CatchlightRenderState>,
    extracted: Query<(Entity, &ExtractedPuppet)>,
    views: Query<&ViewTarget, With<ExtractedCatchlightCamera>>,
) {
    let state = &mut *state;
    let options = state.options;

    for resources in state.formats.values_mut() {
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
            let pipelines = format_resources(&mut state.formats, format, device.wgpu_device())
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
            let CatchlightRenderState {
                gpus,
                puppets,
                failures,
                ..
            } = &mut *state;
            let model_gpu = match gpus.entry(model_key) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    if failures.failed(model_key, &ex.model, options) {
                        continue;
                    }
                    let mut renderer = WgpuRenderer::from_pipelines(
                        device.wgpu_device().clone(),
                        (**queue.0).clone(),
                        format_pipelines.clone(),
                    );
                    let Some(cache) =
                        failures.prepare(model_key, &mut renderer, &ex.model, options)
                    else {
                        continue;
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
            if model_gpu.options != options
                && !failures.failed(model_key, &model_gpu.model, options)
                && let Some(cache) = failures.prepare(
                    model_key,
                    &mut model_gpu.renderer,
                    &model_gpu.model,
                    options,
                )
            {
                model_gpu.cache = cache;
                model_gpu.options = options;
                for puppet in puppets.values_mut() {
                    if puppet.model == model_key {
                        puppet.collected = false;
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
                        release(&mut old_gpu.renderer, released);
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

    let CatchlightRenderState {
        gpus,
        puppets,
        live_scratch,
        live_models,
        failures,
        ..
    } = &mut *state;
    live_scratch.clear();
    live_scratch.extend(extracted.iter().map(|(e, _)| e));
    live_models.clear();
    puppets.retain(|key, puppet| {
        let live = live_scratch.contains(&key.entity) && view_formats.contains(&key.format);
        if live {
            live_models.insert(puppet.model);
        } else {
            if let Some(gpu) = gpus.get_mut(&puppet.model) {
                release(&mut gpu.renderer, puppet.deform_set);
            }
        }
        live
    });
    gpus.retain(|key, _| live_models.contains(key));
    failures.0.retain(|key, _| {
        view_formats.contains(&key.format)
            && extracted.iter().any(|(_, ex)| ex.model_id == key.model)
    });

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
