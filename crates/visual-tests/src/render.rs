use crate::config::{Camera, Config, ParamSetting};
use anyhow::{Context, Result};
use catchlight_core::{load_model, ModelFormat, Puppet};
use catchlight_wgpu::{collect_drawables, create_orthographic_camera_at, RenderList};
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

pub struct CachedPuppet {
    pub puppet: Puppet,
    /// Pristine clone captured at load time. Cloned-into `puppet` before
    /// each config so SimplePhysics persistent state (anchor_initialized,
    /// d_angle, spring_vel, pendulum) starts identical regardless of any
    /// previous config's settle outcome — otherwise the harness is
    /// ordering-dependent.
    pub pristine: Puppet,
    pub param_defaults: Vec<(u32, glam::Vec2)>,
    pub uploaded: bool,
}

pub fn load_puppet(path: &Path, texture_halvings: u32) -> Result<(Puppet, Vec<(u32, glam::Vec2)>)> {
    let bytes = std::fs::read(path).with_context(|| format!("opening {}", path.display()))?;
    let format = ModelFormat::from_path(path)
        .with_context(|| format!("unrecognized model extension: {}", path.display()))?;
    let puppet = load_model(&bytes, format, texture_halvings)?;
    let defaults: Vec<(u32, glam::Vec2)> =
        puppet.params().iter().map(|p| (p.id, p.defaults)).collect();
    Ok((puppet, defaults))
}

/// Restore `cached` to its pristine pose, apply `params`, settle the physics,
/// upload the resulting deforms, and collect the frame's drawables —
/// everything a render needs except the render pass itself. `world` is the
/// puppet's root transform: identity for a lone puppet, a translation when
/// several share one frame.
pub fn prepare_puppet(
    ctx: &mut RenderContext,
    cached: &mut CachedPuppet,
    params: &[ParamSetting],
    world: glam::Mat4,
) -> Result<RenderList> {
    if !cached.uploaded {
        ctx.renderer.upload_puppet(&cached.puppet)?;
        cached.uploaded = true;
    }
    cached.puppet.clone_from(&cached.pristine);
    cached.puppet.reset_dynamic_state();
    cached.puppet.reset_deforms();
    for &(uuid, value) in &cached.param_defaults {
        cached.puppet.set_param_value(uuid, value);
    }
    for p in params {
        let name = p.name.as_str();
        if !cached
            .puppet
            .set_param_value_by_name(name, glam::Vec2::new(p.x, p.y))
        {
            anyhow::bail!("visual test parameter '{name}' not found");
        }
    }
    for _ in 0..SETTLE_STEPS {
        cached
            .puppet
            .tick(&mut ctx.transforms, world, SUBSTEP_SECONDS);
    }
    ctx.renderer.sync_deforms(&cached.puppet);
    Ok(collect_drawables(&cached.puppet, &ctx.transforms))
}

/// The view-projection `camera` describes for a `width` x `height` viewport.
pub fn camera_matrix(width: u32, height: u32, camera: &Camera) -> glam::Mat4 {
    let aspect = width as f32 / height as f32;
    let camera_height = height as f32 / camera.zoom.max(1e-3);
    create_orthographic_camera_at(camera_height, aspect, glam::Vec2::new(camera.x, camera.y))
}

pub fn render_one_to_rgba(
    ctx: &mut RenderContext,
    cached: &mut CachedPuppet,
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
