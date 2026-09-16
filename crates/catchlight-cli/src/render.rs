//! Reproducible renders from one `.clm`: named static/animated requests share
//! one GPU context/cache while each request owns a fresh continuing Puppet.
//! `spec` normalizes CLI and JSON before GPU work; `frame` also drives GPU-free
//! bounds; `artifacts` defines discoverable output schemas and atomic writes.
//! Selection changes only Part color draws and explicit private mask edges.
//! Neither rendering nor inspection changes the input model file.
pub mod args;
pub mod artifacts;
pub mod execute;
pub mod frame;
mod listing;
mod overlay;
pub mod spec;
mod terminal;

use crate::Error;
use std::path::{Path, PathBuf};
pub const DEFAULT_WIDTH: u32 = 960;
pub const DEFAULT_HEIGHT: u32 = 1600;
pub const DEFAULT_CAMERA_HEIGHT: f32 = 5000.0;

/// Compatibility library entry point, lowered to the same validated request
/// and execution as the retained positional CLI invocation.
pub struct Rendered {
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    pub listing: String,
}
impl std::fmt::Display for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}\nwrote {} ({}x{})",
            self.listing,
            self.out.display(),
            self.width,
            self.height
        )
    }
}
pub fn run(
    path: &Path,
    out: &Path,
    width: u32,
    height: u32,
    camera_height: f32,
) -> Result<Rendered, Error> {
    execute::check_output_path(path, out)?;
    let framing = spec::Framing::legacy(width, height, camera_height)?;
    let model = execute::load(path, false)?.model;
    let request = spec::Request {
        rect: spec::Setting::Value(framing.rect),
        scale: spec::Setting::Value(framing.scale),
        ..Default::default()
    };
    let plan = spec::Document::single(request).resolve(&model)?;
    let result = execute::run(
        &model,
        plan,
        execute::Destination::Png(out.to_path_buf()),
        None,
        &execute::Cancellation::default(),
    )?;
    Ok(Rendered {
        out: out.to_path_buf(),
        width: framing.size[0],
        height: framing.size[1],
        listing: result.listings.join("\n"),
    })
}
