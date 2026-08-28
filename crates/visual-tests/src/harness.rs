use crate::config::{default_models, Config, ModelSpec, Thresholds};
use crate::diff::{diff_images, Metrics};
use crate::render::{load_puppet, render_one_to_rgba, CachedPuppet, RenderContext};
use crate::{baseline_path, baselines_root, failures_root, repo_root};
use anyhow::{anyhow, Context, Result};
use catchlight_wgpu::{create_headless_context, Pipelines, WgpuRenderer};
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

struct HarnessInner {
    pipelines: Arc<Pipelines>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Per-model render context (each carries its own renderer so
    /// `MeshId`s from different puppets don't collide in
    /// `WgpuRenderer.mesh_buffers`). Populated lazily on first use.
    contexts: HashMap<String, ContextSlot>,
    models: HashMap<String, ModelSpec>,
    thresholds: Thresholds,
}

struct ContextSlot {
    spec: ModelSpec,
    ctx: RenderContext,
    cached: CachedPuppet,
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
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let pipelines = Arc::new(Pipelines::new(&device, format));
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
    fn ensure_slot(&mut self, model_stem: &str) -> Result<&mut ContextSlot> {
        if !self.contexts.contains_key(model_stem) {
            let spec = self
                .models
                .get(model_stem)
                .cloned()
                .ok_or_else(|| anyhow!("unknown model '{model_stem}'"))?;
            let (puppet, defaults) = load_puppet(&spec.path, spec.texture_halvings)?;
            let renderer = WgpuRenderer::from_pipelines(
                self.device.clone(),
                self.queue.clone(),
                self.pipelines.clone(),
            );
            let ctx = RenderContext::with_renderer(renderer, spec.width, spec.height)
                .map_err(|error| anyhow!("create render context: {error}"))?;
            let pristine = puppet.clone();
            let cached = CachedPuppet {
                puppet,
                pristine,
                param_defaults: defaults,
                uploaded: false,
            };
            self.contexts
                .insert(model_stem.to_string(), ContextSlot { spec, ctx, cached });
        }
        self.contexts
            .get_mut(model_stem)
            .ok_or_else(|| anyhow!("slot disappeared for '{model_stem}'"))
    }

    fn render_pixels(&mut self, config: &Config) -> Result<(Vec<u8>, u32, u32)> {
        let slot = self.ensure_slot(&config.model_stem)?;
        let pixels = render_one_to_rgba(&mut slot.ctx, &mut slot.cached, config)?;
        Ok((pixels, slot.spec.width, slot.spec.height))
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
