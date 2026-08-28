//! Visual regression harness: render a curated matrix of param/camera configs
//! and diff each render against a committed baseline PNG.
//!
//! Cases are hand-authored in [`config`] (`default_models` × `curated_configs`
//! × `camera_presets`, named `<model>__<label>__cam_<preset>`; a test asserts
//! the names are unique, because two configs sharing a name would silently
//! share one baseline PNG). `cargo test -p visual-tests` renders each config
//! and diffs it against `tests/baselines/<model>/<config>.png` under the
//! thresholds in `Thresholds::default`; failures land as
//! expected/actual/diff/summary under `tmp/visual-test-failures/<config>/`.
//!
//! To **update baselines** after an intended change, run
//! `cargo run -p visual-tests --release -- update` (add `--filter SUBSTR` to
//! narrow, or `list` to see the matrix) and commit the PNGs. `update_all` is
//! the only writer; the regression test never calls it.

pub mod config;
mod diff;
pub mod harness;
pub mod render;

pub use config::{
    build_matrix, default_models, Camera, Config, FramePuppet, ModelSpec, ParamSetting, Thresholds,
};
pub use diff::Metrics;
pub use harness::{list_configs, run_one, update_all, RunOutcome, SharedHarness};
pub use render::{render_one_to_rgba, RenderContext};

use std::path::{Path, PathBuf};

pub fn baseline_path(baselines_root: &Path, model_stem: &str, config_name: &str) -> PathBuf {
    baselines_root
        .join(model_stem)
        .join(format!("{config_name}.png"))
}

/// Locate the repository root from the crate's `CARGO_MANIFEST_DIR`. We're in
/// `crates/visual-tests`, so the workspace root is two levels up. Falls back
/// to the current dir for unusual launch contexts.
pub fn repo_root() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok();
    if let Some(m) = manifest {
        let p = PathBuf::from(m);
        if let Some(parent) = p.parent().and_then(|p| p.parent()) {
            return parent.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn baselines_root(repo_root: &Path) -> PathBuf {
    repo_root.join("tests").join("baselines")
}

pub fn failures_root(repo_root: &Path) -> PathBuf {
    repo_root.join("tmp").join("visual-test-failures")
}

/// Generate the curated config list across every default model. Used by both
/// the binary `update` flow and the libtest_mimic harness. Deterministic and
/// hand-authored — no model files are read here.
pub fn generate_configs() -> Vec<Config> {
    default_models(&repo_root())
        .iter()
        .flat_map(build_matrix)
        .collect()
}
