use crate::config::Config;
use anyhow::{Context, Result};
use catchlight_core::{load_model, ModelFormat, Puppet};
use catchlight_wgpu::{collect_drawables, create_orthographic_camera_at};
use std::path::Path;

pub use catchlight_wgpu::RenderContext;

const SUBSTEP_SECONDS: f32 = 0.01;
const SETTLE_STEPS: u32 = 500;

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
    let defaults: Vec<(u32, glam::Vec2)> = puppet
        .params()
        .iter()
        .map(|p| (p.id, p.defaults))
        .collect();
    Ok((puppet, defaults))
}

pub fn render_one_to_rgba(
    ctx: &mut RenderContext,
    cached: &mut CachedPuppet,
    config: &Config,
) -> Result<Vec<u8>> {
    if !cached.uploaded {
        ctx.renderer.upload_puppet(&cached.puppet)?;
        cached.uploaded = true;
    }
    cached.puppet.clone_from(&cached.pristine);
    let params: Vec<(&str, glam::Vec2)> = config
        .params
        .iter()
        .map(|p| (p.name.as_str(), glam::Vec2::new(p.x, p.y)))
        .collect();
    cached.puppet.reset_dynamic_state();
    cached.puppet.reset_deforms();
    for &(uuid, value) in &cached.param_defaults {
        cached.puppet.set_param_value(uuid, value);
    }
    for &(name, value) in &params {
        if !cached.puppet.set_param_value_by_name(name, value) {
            anyhow::bail!("visual test parameter '{name}' not found");
        }
    }
    for _ in 0..SETTLE_STEPS {
        cached
            .puppet
            .tick(&mut ctx.transforms, glam::Mat4::IDENTITY, SUBSTEP_SECONDS);
    }
    ctx.renderer.sync_deforms(&cached.puppet);
    let aspect = ctx.width as f32 / ctx.height as f32;
    let camera_height = ctx.height as f32 / config.camera.zoom.max(1e-3);
    let camera = create_orthographic_camera_at(
        camera_height,
        aspect,
        glam::Vec2::new(config.camera.x, config.camera.y),
    );
    ctx.renderer.update_camera(camera);

    let render_list = collect_drawables(&cached.puppet, &ctx.transforms);
    let clear = Some(wgpu::Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    });
    ctx.render(&render_list, clear)
        .map_err(|error| anyhow::anyhow!("render visual test: {error}"))?;
    ctx.read_rgba()
        .map_err(|error| anyhow::anyhow!("read visual test: {error}"))
}
