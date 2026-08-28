use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::renderer::{RenderAdapter, RenderDevice, RenderQueue};
use bevy::render::view::ViewTarget;
use catchlight_wgpu::{
    CompositePool, FramebufferSnapshotPool, Pipelines, StencilTarget, WgpuRenderer,
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
        for renderer in inner.renderers.values_mut() {
            if let Some(frame) = renderer.process_gpu_frame() {
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
    pub renderers: HashMap<RendererKey, WgpuRenderer>,
    pub(crate) formats: HashMap<wgpu::TextureFormat, FormatResources>,
    pub size: (u32, u32),
    pub(crate) missing_format_warned: bool,
    pub(crate) camera_overflow_warned: bool,
    // Reused across frames so GC doesn't allocate a fresh HashSet every tick.
    live_scratch: HashSet<Entity>,
}

/// Prepare system: upload new puppets and sync deforms for existing ones.
// Invariant: render-state Mutex and per-puppet RwLocks are only poisoned on panic, treated as fatal.
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
            let renderer = match inner.renderers.entry(key) {
                std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    let mut renderer = WgpuRenderer::from_pipelines(
                        device.wgpu_device().clone(),
                        (**queue.0).clone(),
                        pipelines.clone(),
                    );
                    let puppet = ex.puppet.read().unwrap();
                    if let Err(err) = renderer.upload_puppet(&puppet) {
                        tracing::error!("catchlight upload_puppet failed: {}", err);
                        continue;
                    }
                    v.insert(renderer)
                }
            };

            // Native extraction freezes deforms before pipelined rendering.
            // Wasm prepare runs before rendering on the same thread, so it
            // reads the live puppet and avoids the snapshot copy.
            if ex.visible {
                #[cfg(not(target_arch = "wasm32"))]
                renderer.sync_deforms_snapshot(&ex.deforms);
                #[cfg(target_arch = "wasm32")]
                {
                    let puppet = ex.puppet.read().unwrap();
                    renderer.sync_deforms(&puppet);
                }
            }
        }
    }

    // GC renderers whose render entities are gone (entity despawned, or
    // `CatchlightPuppet` removed — SyncComponentPlugin despawns the
    // render-world entity, so the puppet drops out of `extracted`).
    let CatchlightRenderInner {
        renderers,
        live_scratch,
        ..
    } = &mut *inner;
    live_scratch.clear();
    live_scratch.extend(extracted.iter().map(|(e, _)| e));
    renderers
        .retain(|key, _| live_scratch.contains(&key.entity) && view_formats.contains(&key.format));

    // Close the previous frame's GPU-profiler queries — exactly once per
    // frame, not once per CatchlightCamera. end_frame maps the timestamp
    // read buffer, so it must run after that frame's encoder was submitted:
    // last frame's render graph (which submits) ran before this Prepare, and
    // this frame's render node has not recorded queries yet. Doing it here
    // keeps the per-view render node to recording + resolving only. GPU
    // timing for the overlay therefore trails by one frame.
    for renderer in renderers.values_mut() {
        renderer.end_gpu_frame();
        renderer.begin_camera_submit();
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
