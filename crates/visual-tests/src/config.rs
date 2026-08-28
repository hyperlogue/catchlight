use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub stem: String,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Default camera distance roughly framing the puppet body. Used as the
    /// "centered" preset and as the basis for derived presets.
    pub default_zoom: f32,
    /// Import-time texture downsampling, matching what production deploys
    /// for this model — the baselines then gate production fidelity, with
    /// `zoom_face` (zoom 1.1) as the realistic worst-case sampling rate.
    pub texture_halvings: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

#[derive(Debug, Clone)]
pub struct ParamSetting {
    pub name: String,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub model_stem: String,
    pub params: Vec<ParamSetting>,
    pub camera: Camera,
}

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    pub mean: f32,
    pub p99: u8,
    pub max: u8,
    pub pct_above_threshold: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            mean: 0.5,
            p99: 4,
            max: 32,
            pct_above_threshold: 0.05,
        }
    }
}

/// Default-installed model matrix. Can be substituted in tests by
/// constructing `ModelSpec`s directly.
pub fn default_models(repo_root: &Path) -> Vec<ModelSpec> {
    let repro = repo_root.join("tests").join("models");
    vec![
        ModelSpec {
            stem: "disk_masked_by_disk".into(),
            path: repro.join("disk_masked_by_disk.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "multiply_blend".into(),
            path: repro.join("multiply_blend.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "quad_over_bg".into(),
            path: repro.join("quad_over_bg.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "single_alpha_quad".into(),
            path: repro.join("single_alpha_quad.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // Every blend mode the renderer implements, one disk per cell over a
        // 2D-gradient backdrop. A regression in any blend-state branch or
        // blend shader localizes to one cell of the diff heatmap.
        ModelSpec {
            stem: "blend_modes".into(),
            path: repro.join("blend_modes.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // Dst-in-shader disks nested in a Composite: each must blend
        // against the composite's own buffer (gray pad / transparent),
        // never the gradient behind the composite — pins the
        // composite-child snapshot path that blend_modes (root drawables
        // only) can't reach.
        ModelSpec {
            stem: "blend_modes_composite".into(),
            path: repro.join("blend_modes_composite.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "composite_masks".into(),
            path: repro.join("composite_masks.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // Two grid Parts with a welded seam, per-vertex weights 1 / 0.5 / 0
        // left-to-right (scripts/build_welded_seam_clp.py). The `pull` param
        // deform-shifts the top part; the seam must stay closed with each
        // weight regime visible.
        ModelSpec {
            stem: "welded_seam".into(),
            path: repro.join("welded_seam.clp"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
    ]
}

fn camera_presets(default_zoom: f32) -> Vec<(&'static str, Camera)> {
    vec![
        (
            "default",
            Camera {
                x: 0.0,
                y: 0.0,
                zoom: default_zoom,
            },
        ),
        // Tight on the face, for poses whose interesting detail is in the
        // eyes / mouth / brows (blink mask path, gaze, expressions).
        (
            "zoom_face",
            Camera {
                x: 0.0,
                y: 1400.0,
                zoom: default_zoom * 5.0,
            },
        ),
    ]
}

/// One hand-authored regression pose. Each entry exercises a distinct part of
/// the render path; the set is deliberately small (every config is a committed
/// baseline PNG in LFS) rather than an auto-generated sweep of every param.
struct Curated {
    label: &'static str,
    params: &'static [(&'static str, f32, f32)],
    /// Must name an entry from `camera_presets()`.
    camera_preset: &'static str,
}

fn curated_configs(stem: &str) -> Vec<Curated> {
    match stem {
        "welded_seam" => vec![
            // Rest: the seam is coincident, so welding must be a no-op.
            Curated {
                label: "default",
                params: &[],
                camera_preset: "default",
            },
            // Deformed: without welds the top part slides off whole; welded,
            // the seam blends per weight (B follows / midway / A pinned).
            Curated {
                label: "pull",
                params: &[("pull", 1.0, 0.0)],
                camera_preset: "default",
            },
        ],
        // The repro models have no params: a single rest render isolates the
        // basic alpha/mask/blend/compositing pipeline.
        _ => vec![Curated {
            label: "default",
            params: &[],
            camera_preset: "default",
        }],
    }
}

/// Build the curated config list for one model. Deterministic and
/// hand-authored — there is no random sweep, because every config is a
/// committed baseline PNG.
pub fn build_matrix(model: &ModelSpec) -> Vec<Config> {
    let presets = camera_presets(model.default_zoom);
    curated_configs(&model.stem)
        .into_iter()
        .map(|c| {
            let camera = presets
                .iter()
                .find(|(name, _)| *name == c.camera_preset)
                .map(|(_, cam)| *cam)
                .unwrap_or(Camera {
                    x: 0.0,
                    y: 0.0,
                    zoom: model.default_zoom,
                });
            Config {
                name: format!("{}__{}__cam_{}", model.stem, c.label, c.camera_preset),
                model_stem: model.stem.clone(),
                params: c
                    .params
                    .iter()
                    .map(|(name, x, y)| ParamSetting {
                        name: (*name).to_string(),
                        x: *x,
                        y: *y,
                    })
                    .collect(),
                camera,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A duplicate name would make two configs share — and silently overwrite —
    // one baseline PNG, so the second pose would never actually be tested.
    #[test]
    fn config_names_are_unique() {
        let models = default_models(Path::new("."));
        let mut seen = std::collections::HashSet::new();
        for m in &models {
            for c in build_matrix(m) {
                assert!(
                    seen.insert(c.name.clone()),
                    "duplicate config name: {}",
                    c.name
                );
            }
        }
    }

    // Every curated pose must reference a camera preset that exists, or it
    // silently falls back to a default camera and the pose is framed wrong.
    #[test]
    fn curated_cameras_resolve() {
        for m in default_models(Path::new(".")) {
            let preset_names: Vec<&str> = camera_presets(m.default_zoom)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            for c in curated_configs(&m.stem) {
                assert!(
                    preset_names.contains(&c.camera_preset),
                    "{} pose '{}' names unknown camera '{}'",
                    m.stem,
                    c.label,
                    c.camera_preset
                );
            }
        }
    }
}
