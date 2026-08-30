use crate::config::{default_models, Config, ModelSpec, Thresholds};
use crate::diff::{diff_images, Metrics};
use crate::render::{
    camera_matrix, prepare_puppet, render_one_to_rgba, CachedModel, RenderContext, CLEAR_COLOR,
};
use crate::{baseline_path, baselines_root, failures_root, repo_root};
use anyhow::{anyhow, Context, Result};
use catchlight_wgpu::{
    create_headless_context, read_texture_to_rgba, CompositePool, FramebufferSnapshotPool,
    Pipelines, StencilTarget, WgpuRenderer,
};
use image::RgbaImage;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// All wgpu state, model caches, and per-config render lives behind the
/// `inner` mutex. wgpu Device/Queue are Send+Sync, but the per-frame
/// instance/uniform buffers in `WgpuRenderer` cannot be shared across
/// concurrent renders without aliasing (see AGENTS.md). Serializing every
/// trial through this lock sidesteps that entirely.
pub struct SharedHarness {
    inner: Mutex<HarnessInner>,
}

/// Color format of every render target here, matching the `Pipelines` the
/// harness builds.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct HarnessInner {
    pipelines: Arc<Pipelines>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Render context per puppet (each carries its own renderer so
    /// two render caches never overwrite each other's mesh and texture slots,
    /// and so two puppets of one model in a single frame do not overwrite
    /// each other's instance buffer). Keyed by model stem for the
    /// single-puppet path, and by stem + frame position for multi-puppet
    /// frames. Populated lazily on first use.
    contexts: HashMap<String, ContextSlot>,
    models: HashMap<String, ModelSpec>,
    thresholds: Thresholds,
}

struct ContextSlot {
    spec: ModelSpec,
    ctx: RenderContext,
    cached: CachedModel,
}

impl SharedHarness {
    pub fn new() -> Result<Self> {
        let root = repo_root();
        let models = default_models(&root);
        Self::with_models(models)
    }

    pub fn with_models(models: Vec<ModelSpec>) -> Result<Self> {
        let (device, queue) = pollster::block_on(create_headless_context())
            .map_err(|e| anyhow!("create_headless_context: {e}"))?;
        let pipelines = Arc::new(Pipelines::new(&device, TARGET_FORMAT));
        let mut models_map = HashMap::new();
        for m in models {
            models_map.insert(m.stem.clone(), m);
        }
        Ok(Self {
            inner: Mutex::new(HarnessInner {
                pipelines,
                device,
                queue,
                contexts: HashMap::new(),
                models: models_map,
                thresholds: Thresholds::default(),
            }),
        })
    }

    pub fn set_thresholds(&self, t: Thresholds) -> Result<()> {
        let mut inner = self.lock()?;
        inner.thresholds = t;
        Ok(())
    }

    pub fn thresholds(&self) -> Result<Thresholds> {
        let inner = self.lock()?;
        Ok(inner.thresholds)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HarnessInner>> {
        self.inner
            .lock()
            .map_err(|e| anyhow!("harness mutex poisoned: {e}"))
    }
}

impl HarnessInner {
    fn ensure_slot(&mut self, slot_key: &str, model_stem: &str) -> Result<&mut ContextSlot> {
        if !self.contexts.contains_key(slot_key) {
            let spec = self
                .models
                .get(model_stem)
                .cloned()
                .ok_or_else(|| anyhow!("unknown model '{model_stem}'"))?;
            let cached = CachedModel::load(&spec.path, spec.texture_halvings)?;
            let renderer = WgpuRenderer::from_pipelines(
                self.device.clone(),
                self.queue.clone(),
                self.pipelines.clone(),
            );
            let ctx = RenderContext::with_renderer(renderer, spec.width, spec.height)
                .map_err(|error| anyhow!("create render context: {error}"))?;
            self.contexts
                .insert(slot_key.to_string(), ContextSlot { spec, ctx, cached });
        }
        self.contexts
            .get_mut(slot_key)
            .ok_or_else(|| anyhow!("slot disappeared for '{slot_key}'"))
    }

    fn render_pixels(&mut self, config: &Config) -> Result<(Vec<u8>, u32, u32)> {
        if !config.frame_puppets.is_empty() {
            return self.render_pixels_multi(config);
        }
        let slot = self.ensure_slot(&config.model_stem, &config.model_stem)?;
        let pixels = render_one_to_rgba(&mut slot.ctx, &mut slot.cached, config)?;
        Ok((pixels, slot.spec.width, slot.spec.height))
    }

    /// Draw every puppet of a multi-puppet config into one frame, the way an
    /// app with several puppets on screen does: one encoder and one color
    /// target, and one caller-owned `StencilTarget` / `CompositePool` /
    /// `FramebufferSnapshotPool` passed to every `render_list_ext` call.
    /// Only the first call clears; the rest load what the previous ones drew.
    ///
    /// The frame resources are built here rather than borrowed from a
    /// puppet's own `RenderContext` precisely because they are shared: the
    /// pools are recycled from puppet to puppet, so a stale mask or a
    /// composite slot handed out twice would bleed across puppets, and the
    /// baseline is what catches that.
    fn render_pixels_multi(&mut self, config: &Config) -> Result<(Vec<u8>, u32, u32)> {
        let first = config
            .frame_puppets
            .first()
            .ok_or_else(|| anyhow!("{}: no frame puppets", config.name))?;
        let (width, height) = {
            let spec = self
                .models
                .get(&first.model_stem)
                .ok_or_else(|| anyhow!("unknown model '{}'", first.model_stem))?;
            (spec.width, spec.height)
        };

        let stencil =
            StencilTarget::new_for_pipelines(&self.pipelines, &self.device, width, height);
        let mut composites = CompositePool::new(width, height);
        let mut snapshots = FramebufferSnapshotPool::new(width, height);
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("multi-puppet-frame-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let camera = camera_matrix(width, height, &config.camera);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("multi-puppet-frame-encoder"),
            });

        for (i, puppet) in config.frame_puppets.iter().enumerate() {
            let world = glam::Mat4::from_translation(glam::Vec3::new(puppet.x, puppet.y, 0.0));
            let key = format!("{}#{i}", puppet.model_stem);
            let slot = self.ensure_slot(&key, &puppet.model_stem)?;
            let render_list = prepare_puppet(&mut slot.ctx, &mut slot.cached, &[], world)?;
            slot.ctx.renderer.begin_camera_submit();
            slot.ctx.renderer.update_camera(camera);
            slot.ctx
                .renderer
                .render_list_ext(
                    &render_list,
                    &mut encoder,
                    &view,
                    &stencil,
                    &mut composites,
                    Some(&target),
                    Some(&mut snapshots),
                    width,
                    height,
                    (i == 0).then_some(CLEAR_COLOR),
                )
                .map_err(|error| anyhow!("render {} puppet {i}: {error}", config.name))?;
        }
        self.queue.submit(std::iter::once(encoder.finish()));

        let pixels = pollster::block_on(read_texture_to_rgba(
            &self.device,
            &self.queue,
            &target,
            width,
            height,
        ))
        .map_err(|error| anyhow!("read {}: {error}", config.name))?;
        Ok((pixels, width, height))
    }
}

#[derive(Debug)]
pub enum RunOutcome {
    Pass(Metrics),
    Fail {
        metrics: Metrics,
        expected: PathBuf,
        actual: PathBuf,
        diff: PathBuf,
        summary: PathBuf,
    },
}

/// Render `config`, diff against the baseline on disk, persist failure
/// artifacts under `tmp/visual-test-failures/<config.name>/` when the metrics
/// exceed the configured thresholds.
pub fn run_one(harness: &SharedHarness, config: &Config) -> Result<RunOutcome> {
    let mut inner = harness.lock()?;
    let thresholds = inner.thresholds;
    let (pixels, width, height) = inner.render_pixels(config)?;
    drop(inner);

    let actual_img = RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| anyhow!("RgbaImage::from_raw size mismatch"))?;

    let root = repo_root();
    let baseline = baseline_path(&baselines_root(&root), &config.model_stem, &config.name);
    if !baseline.exists() {
        return Err(anyhow!(
            "no baseline at {} — run `cargo run -p visual-tests --release -- update` first",
            baseline.display()
        ));
    }
    let expected_img = image::open(&baseline)
        .with_context(|| format!("loading baseline {}", baseline.display()))?
        .to_rgba8();
    if expected_img.dimensions() != actual_img.dimensions() {
        let fail_dir = failures_root(&root).join(&config.name);
        std::fs::create_dir_all(&fail_dir)?;
        let actual_path = fail_dir.join("actual.png");
        actual_img.save(&actual_path)?;
        return Err(anyhow!(
            "size mismatch: baseline {:?}, actual {:?}; saved actual to {}",
            expected_img.dimensions(),
            actual_img.dimensions(),
            actual_path.display(),
        ));
    }

    let out = diff_images(&expected_img, &actual_img);
    let m = out.metrics;

    if metrics_within(&m, &thresholds) {
        return Ok(RunOutcome::Pass(m));
    }

    let fail_dir = failures_root(&root).join(&config.name);
    std::fs::create_dir_all(&fail_dir)?;
    let expected_out = fail_dir.join("expected.png");
    let actual_out = fail_dir.join("actual.png");
    let diff_out = fail_dir.join("diff.png");
    let summary_out = fail_dir.join("summary.txt");

    expected_img.save(&expected_out)?;
    actual_img.save(&actual_out)?;
    out.overlay.save(&diff_out)?;
    let summary = format!(
        "config: {}\nmodel: {}\nmean: {:.4}\np99: {}\nmax: {}\npct_above_threshold: {:.4}%\nthresholds: mean<={:.4}, p99<={}, max<={}, pct<={:.4}%\n",
        config.name,
        config.model_stem,
        m.mean,
        m.p99,
        m.max,
        m.pct_above_threshold,
        thresholds.mean,
        thresholds.p99,
        thresholds.max,
        thresholds.pct_above_threshold,
    );
    std::fs::write(&summary_out, summary)?;

    Ok(RunOutcome::Fail {
        metrics: m,
        expected: expected_out,
        actual: actual_out,
        diff: diff_out,
        summary: summary_out,
    })
}

fn metrics_within(m: &Metrics, t: &Thresholds) -> bool {
    m.mean <= t.mean
        && m.p99 <= t.p99
        && m.max <= t.max
        && m.pct_above_threshold <= t.pct_above_threshold
}

/// Render every config and overwrite its baseline. Used by the `update`
/// subcommand of the binary; the regression test never calls this.
pub fn update_all(harness: &SharedHarness, configs: &[Config]) -> Result<()> {
    let root = repo_root();
    let baselines = baselines_root(&root);
    let total = configs.len();
    for (i, config) in configs.iter().enumerate() {
        let mut inner = harness.lock()?;
        let (pixels, width, height) = inner.render_pixels(config)?;
        drop(inner);

        let dest = baseline_path(&baselines, &config.model_stem, &config.name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let img = RgbaImage::from_raw(width, height, pixels)
            .ok_or_else(|| anyhow!("RgbaImage::from_raw size mismatch"))?;
        img.save(&dest)
            .with_context(|| format!("writing {}", dest.display()))?;
        println!(
            "[{:>3}/{}] {} -> {}",
            i + 1,
            total,
            config.name,
            dest.display()
        );
    }
    Ok(())
}

/// Convenience for the binary's `list` subcommand.
pub fn list_configs(configs: &[Config]) {
    for c in configs {
        let cam = c.camera;
        let p_summary = if c.params.is_empty() {
            "default".into()
        } else {
            c.params
                .iter()
                .map(|p| format!("{}=({:.2},{:.2})", p.name, p.x, p.y))
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "{:<60}  model={:<24}  cam=(x={:.1},y={:.1},zoom={:.3})  params={}",
            c.name, c.model_stem, cam.x, cam.y, cam.zoom, p_summary
        );
    }
}
