use crate::config::{Camera, Config, ParamSetting};
use anyhow::{Context, Result};
use catchlight_core::formats::{clm::ClmError, container::ContainerError};
use catchlight_core::{load_model, Model, ModelError, ModelFormat, ParamId, Pose, Puppet};
use catchlight_wgpu::{
    collect, create_orthographic_camera_at, PrepareOptions, RenderCache, RenderList,
};
use std::collections::HashMap;
use std::path::Path;

pub use catchlight_wgpu::RenderContext;

/// Seconds per tick: fixed 60 Hz, never a clock, so renders are deterministic.
pub const FRAME_SECONDS: f32 = 1.0 / 60.0;

/// Opaque, so a dropped or wrongly-blended pixel shows up as a difference.
pub const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

pub struct CachedModel {
    pub model: Model,
    pub puppet: Puppet,
    pub pristine: Puppet,
    pub param_ids: HashMap<String, ParamId>,
    /// Built lazily so a model no config reaches never decodes a texture.
    pub cache: Option<RenderCache>,
    pub texture_halvings: u32,
}

impl CachedModel {
    pub fn load(path: &Path, texture_halvings: u32) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("opening {}", path.display()))?;
        let format = ModelFormat::from_path(path)
            .with_context(|| format!("unrecognized model extension: {}", path.display()))?;
        let model = load_model(&bytes, format).map_err(|error| match error {
            ModelError::Clm(ClmError::Container(ContainerError::BadMagic))
                if crate::lfs::is_pointer(&bytes) =>
            {
                crate::lfs::pointer_error(path)
            }
            error => error.into(),
        })?;
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

/// A config entry for a 2-D param maps to the scalar pair `<name>.x` /
/// `<name>.y`; a scalar param takes `x` and ignores `y`. Unknown names error.
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

/// `world` is the puppet's root transform.
pub fn prepare_puppet(
    ctx: &mut RenderContext,
    cached: &mut CachedModel,
    params: &[ParamSetting],
    ticks_after_pose: u32,
    world: glam::Mat4,
) -> Result<RenderList> {
    let pose = pose_from(cached, params)?;
    if cached.cache.is_none() {
        cached.cache = Some(RenderCache::prepare(
            &mut ctx.renderer,
            &cached.model,
            PrepareOptions {
                texture_halvings: cached.texture_halvings,
                ..PrepareOptions::default()
            },
        )?);
    }
    cached.puppet.clone_from(&cached.pristine);
    // Static: pose then settle. Transient: settle at defaults, then pose, so
    // the following ticks capture the motion the pose change starts.
    if ticks_after_pose == 0 {
        cached.puppet.apply_pose(&pose);
        cached.puppet.reset_motion_responses(&cached.model);
        cached.puppet.tick_with_root(&cached.model, world, 0.0);
    } else {
        cached.puppet.reset_motion_responses(&cached.model);
        cached.puppet.apply_pose(&pose);
        for _ in 0..ticks_after_pose {
            cached
                .puppet
                .tick_with_root(&cached.model, world, FRAME_SECONDS);
        }
    }
    let cache = cached
        .cache
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("render cache disappeared"))?;
    cache.refresh(&mut ctx.renderer, &cached.model, &cached.puppet)?;
    Ok(collect(cache, &cached.puppet))
}

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
    let render_list = prepare_puppet(
        ctx,
        cached,
        &config.params,
        config.ticks_after_pose,
        glam::Mat4::IDENTITY,
    )?;
    let camera = camera_matrix(ctx.width, ctx.height, &config.camera);
    ctx.renderer.update_camera(camera);
    ctx.render(&render_list, Some(CLEAR_COLOR))
        .map_err(|error| anyhow::anyhow!("render visual test: {error}"))?;
    ctx.read_rgba()
        .map_err(|error| anyhow::anyhow!("read visual test: {error}"))
}
