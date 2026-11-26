//! Visual regression harness: renders a curated param/camera matrix and diffs
//! each render against a committed baseline PNG.
//!
//! `cargo test -p visual-tests` compares against
//! `tests/baselines/<model>/<config>.png`; failures are written under
//! `tmp/visual-test-failures/<config>/`.
//!
//! To **update baselines** after an intended change, run
//! `cargo run -p visual-tests --release -- update` (add `--filter SUBSTR` to
//! narrow, or `list` to see the matrix) and commit the PNGs.

pub mod config;
mod diff;
pub mod harness;
mod lfs;
pub mod render;

pub use config::{
    build_matrix, default_models, Camera, Config, FramePuppet, ModelSpec, ParamSetting, Thresholds,
};
pub use diff::Metrics;
pub use harness::{list_configs, run_one, shared_context, update_all, RunOutcome, SharedHarness};
pub use render::{render_one_to_rgba, RenderContext};

use std::path::{Path, PathBuf};

pub fn baseline_path(baselines_root: &Path, model_stem: &str, config_name: &str) -> PathBuf {
    baselines_root
        .join(model_stem)
        .join(format!("{config_name}.png"))
}

/// Fixed at build time, so the binary finds the checkout from any working
/// directory; the crate is never published.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn baselines_root(repo_root: &Path) -> PathBuf {
    repo_root.join("tests").join("baselines")
}

pub fn failures_root(repo_root: &Path) -> PathBuf {
    repo_root.join("tmp").join("visual-test-failures")
}

pub fn generate_configs() -> Vec<Config> {
    default_models(&repo_root())
        .iter()
        .flat_map(build_matrix)
        .collect()
}
