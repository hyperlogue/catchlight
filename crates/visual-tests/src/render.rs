use crate::config::{Camera, Config, ParamSetting};
use anyhow::{Context, Result};
use catchlight_core::{load_model_bytes, Model, ModelFormat, ParamId, Pose, Puppet};
use catchlight_wgpu::{
    collect, create_orthographic_camera_at, PrepareOptions, RenderCache, RenderList,
};
use std::collections::HashMap;
use std::path::Path;

pub use catchlight_wgpu::RenderContext;

const SUBSTEP_SECONDS: f32 = 0.01;
const SETTLE_STEPS: u32 = 500;

/// Opaque white, so a dropped or wrongly-blended pixel reads as a difference
/// rather than blending into the background.
pub const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// One model and everything derived from it that a render needs: the puppet
/// animating it and the renderer's cache of it.
pub struct CachedModel {
    pub model: Model,
    pub puppet: Puppet,
    /// Pristine clone captured at load time. Cloned-into `puppet` before
    /// each config so SimplePhysics persistent state (anchor_initialized,
    /// d_angle, spring_vel, pendulum) starts identical regardless of any
    /// previous config's settle outcome — otherwise the harness is
    /// ordering-dependent.
    pub pristine: Puppet,
    /// The model's params by [`catchlight_core::id::Name`], resolved once at
    /// load: a config names a param the way its author does, and nothing in a
    /// model is addressed by name past this point.
    pub param_ids: HashMap<String, ParamId>,
    /// Built on first use against the slot's own renderer, so a model that no
    /// config reaches never decodes a texture.
    pub cache: Option<RenderCache>,
    pub texture_halvings: u32,
}

impl CachedModel {
    /// Load `path` and bake a puppet for it. The render cache waits for a
    /// renderer.
    pub fn load(path: &Path, texture_halvings: u32) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("opening {}", path.display()))?;
        let format = ModelFormat::from_path(path)
            .with_context(|| format!("unrecognized model extension: {}", path.display()))?;
        let model = load_model_bytes(&bytes, format)?;
        let param_ids = model
            .param_ids()
            .iter()
            .filter_map(|id| Some((model.param(id)?.name.to_string(), id.clone())))
            .collect();
        let puppet = Puppet::new(&model);
        Ok(Self {
            pristine: puppet.clone(),
            model,
            puppet,
            param_ids,
            cache: None,
            texture_halvings,
        })
    }
}

/// The pose `params` describes, keyed by Id.
///
/// A config names a param the way a person does. The model's params are
/// scalar, so a config entry that was one 2-D param upstream lands on the
/// split pair `<name>.x` / `<name>.y`; one that is genuinely scalar takes `x`
/// and ignores `y`. A name that resolves to neither is a broken config and
/// says so rather than rendering the rest at rest.
fn pose_from(cached: &CachedModel, params: &[ParamSetting]) -> Result<Pose> {
    let mut pose = Pose::new();
    for p in params {
        let name = p.name.as_str();
        if let Some(id) = cached.param_ids.get(name) {
            pose.set(id.clone(), p.x);
            continue;
        }
        let x = cached.param_ids.get(&format!("{name}.x"));
        let y = cached.param_ids.get(&format!("{name}.y"));
        if x.is_none() && y.is_none() {
            anyhow::bail!("visual test parameter '{name}' not found");
        }
        if let Some(id) = x {
            pose.set(id.clone(), p.x);
        }
        if let Some(id) = y {
            pose.set(id.clone(), p.y);
        }
    }
    Ok(pose)
}

/// Restore `cached` to its pristine state, apply `params`, settle the physics,
/// refresh the render cache, and collect the frame's drawables — everything a
/// render needs except the render pass itself. `world` is the puppet's root
/// transform: identity for a lone puppet, a translation when several share
/// one frame.
pub fn prepare_puppet(
    ctx: &mut RenderContext,
    cached: &mut CachedModel,
    params: &[ParamSetting],
    world: glam::Mat4,
) -> Result<RenderList> {
    let pose = pose_from(cached, params)?;
    if cached.cache.is_none() {
        cached.cache = Some(RenderCache::prepare(
            &mut ctx.renderer,
            &cached.model,
            PrepareOptions {
                texture_halvings: cached.texture_halvings,
                memoize_textures: false,
            },
        )?);
    }
    cached.puppet.clone_from(&cached.pristine);
    // Every param the config leaves out goes back to its default, so one
    // config never inherits another's pose.
    cached.puppet.apply_pose(&pose);
    for _ in 0..SETTLE_STEPS {
        cached
            .puppet
            .tick_with_root(&cached.model, world, SUBSTEP_SECONDS);
    }
    let cache = cached
        .cache
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("render cache disappeared"))?;
    cache.refresh(&mut ctx.renderer, &cached.model, &cached.puppet)?;
    Ok(collect(cache, &cached.puppet))
}

/// The view-projection `camera` describes for a `width` x `height` viewport.
pub fn camera_matrix(width: u32, height: u32, camera: &Camera) -> glam::Mat4 {
    let aspect = width as f32 / height as f32;
    let camera_height = height as f32 / camera.zoom.max(1e-3);
    create_orthographic_camera_at(camera_height, aspect, glam::Vec2::new(camera.x, camera.y))
}

pub fn render_one_to_rgba(
    ctx: &mut RenderContext,
    cached: &mut CachedModel,
    config: &Config,
) -> Result<Vec<u8>> {
    let render_list = prepare_puppet(ctx, cached, &config.params, glam::Mat4::IDENTITY)?;
    let camera = camera_matrix(ctx.width, ctx.height, &config.camera);
    ctx.renderer.update_camera(camera);
    ctx.render(&render_list, Some(CLEAR_COLOR))
        .map_err(|error| anyhow::anyhow!("render visual test: {error}"))?;
    ctx.read_rgba()
        .map_err(|error| anyhow::anyhow!("read visual test: {error}"))
}
