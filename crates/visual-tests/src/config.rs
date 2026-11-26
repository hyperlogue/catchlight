use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub stem: String,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub default_zoom: f32,
    /// Matches production texture downsampling so baselines gate shipped
    /// fidelity.
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

/// Position is in world units, applied as the puppet's root transform.
#[derive(Debug, Clone)]
pub struct FramePuppet {
    pub model_stem: String,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub model_stem: String,
    pub params: Vec<ParamSetting>,
    pub camera: Camera,
    /// Empty means `model_stem` alone at the origin. Multi-puppet frames
    /// ignore `params` and render every puppet at its defaults.
    pub frame_puppets: Vec<FramePuppet>,
    /// Ticks of `render::FRAME_SECONDS` after applying `params`. Zero renders
    /// the settled pose; above zero settles at defaults first, then poses, so
    /// the baseline pins motion. Single-puppet only.
    pub ticks_after_pose: u32,
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

impl Thresholds {
    /// Baselines are rendered on lavapipe. A hardware rasterizer differs from
    /// it only in pixels along part edges, which the defaults do not allow.
    #[must_use]
    pub const fn hardware() -> Self {
        Self {
            mean: 0.75,
            p99: 8,
            max: 64,
            pct_above_threshold: 1.0,
        }
    }
}

#[must_use]
pub fn default_models(repo_root: &Path) -> Vec<ModelSpec> {
    let repro = repo_root.join("tests").join("models");
    vec![
        ModelSpec {
            stem: "lan".into(),
            path: repro.join("lan.clm"),
            width: 512,
            height: 512,
            default_zoom: 512.0 / 2253.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "disk_masked_by_disk".into(),
            path: repro.join("disk_masked_by_disk.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "multiply_blend".into(),
            path: repro.join("multiply_blend.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "quad_over_bg".into(),
            path: repro.join("quad_over_bg.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "single_alpha_quad".into(),
            path: repro.join("single_alpha_quad.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // One disk per blend mode, so a regression localizes to one cell.
        ModelSpec {
            stem: "blend_modes".into(),
            path: repro.join("blend_modes.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // Pins the composite-child snapshot path blend_modes cannot reach.
        ModelSpec {
            stem: "blend_modes_composite".into(),
            path: repro.join("blend_modes_composite.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "composite_masks".into(),
            path: repro.join("composite_masks.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // The only model that reaches `blit.wgsl` with non-identity uniforms.
        ModelSpec {
            stem: "composite_blit_uniforms".into(),
            path: repro.join("composite_blit_uniforms.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // The only model whose parts carry a tint and a screen tint.
        ModelSpec {
            stem: "part_uniforms".into(),
            path: repro.join("part_uniforms.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // The only configs that sample mips. Zoom 0.25 maps the quads' 512
        // texels to 128 and 64 pixels (4x and 8x minification).
        ModelSpec {
            stem: "mip_checker".into(),
            path: repro.join("mip_checker.clm"),
            width: 512,
            height: 512,
            default_zoom: 0.25,
            texture_halvings: 0,
        },
        // Three cascaded responses drive separate bends on the fitted strip.
        ModelSpec {
            stem: "strand_response".into(),
            path: repro.join("strand_response.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.25,
            texture_halvings: 0,
        },
        ModelSpec {
            stem: "welded_seam".into(),
            path: repro.join("welded_seam.clm"),
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
            "lan_hair",
            Camera {
                x: 0.0,
                y: 600.0,
                zoom: 512.0 / 1100.0,
            },
        ),
        (
            "lan_face",
            Camera {
                x: -3.0,
                y: 812.0,
                zoom: 512.0 / 696.0,
            },
        ),
        (
            "default",
            Camera {
                x: 0.0,
                y: 0.0,
                zoom: default_zoom,
            },
        ),
        (
            "zoom_face",
            Camera {
                x: 0.0,
                y: 1400.0,
                zoom: default_zoom * 5.0,
            },
        ),
        // Power-of-two zoom keeps world-to-pixel exact in f32.
        (
            "wide_stage",
            Camera {
                x: 0.0,
                y: 0.0,
                zoom: default_zoom / 4.0,
            },
        ),
    ]
}

struct Curated {
    label: &'static str,
    params: &'static [(&'static str, f32, f32)],
    /// Must name an entry from `camera_presets()`.
    camera_preset: &'static str,
    /// `(model stem, world x, world y)`; stems must be in `default_models()`.
    frame_puppets: &'static [(&'static str, f32, f32)],
    ticks_after_pose: u32,
}

fn curated_configs(stem: &str) -> Vec<Curated> {
    match stem {
        "lan" => vec![
            Curated {
                label: "roll_left",
                params: &[("head_roll", -30.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "roll_right",
                params: &[("head_roll", 30.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "roll_left",
                params: &[("head_roll", -30.0, 0.0)],
                camera_preset: "lan_hair",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "roll_right",
                params: &[("head_roll", 30.0, 0.0)],
                camera_preset: "lan_hair",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "rest",
                params: &[],
                camera_preset: "default",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "face",
                params: &[],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "turn_left",
                params: &[("head_yaw", 30.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "turn_right",
                params: &[("head_yaw", -30.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "pitch_down",
                params: &[("head_pitch", -30.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "pitch_up",
                params: &[("head_pitch", 30.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "blink",
                params: &[("eye_left_open", 0.1, 0.0), ("eye_right_open", 0.1, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "half_happy",
                params: &[("mouth_open", 0.5, 0.0), ("mouth_expression", 1.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "half_sad",
                params: &[("mouth_open", 0.5, 0.0), ("mouth_expression", -1.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "full_open",
                params: &[("mouth_open", 1.0, 0.0)],
                camera_preset: "lan_face",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
        ],
        "welded_seam" => vec![
            Curated {
                label: "default",
                params: &[],
                camera_preset: "default",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            Curated {
                label: "pull",
                params: &[("pull", 1.0, 0.0)],
                camera_preset: "default",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
        ],
        "strand_response" => vec![
            Curated {
                label: "rest",
                params: &[],
                camera_preset: "default",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            // At 50 ms the root has moved 120 px and the tip still trails it.
            Curated {
                label: "swing",
                params: &[("turn", 1.0, 0.0)],
                camera_preset: "default",
                frame_puppets: &[],
                ticks_after_pose: 3,
            },
        ],
        "composite_masks" => vec![
            Curated {
                label: "default",
                params: &[],
                camera_preset: "default",
                frame_puppets: &[],
                ticks_after_pose: 0,
            },
            // Catches stencil/composite state bleeding between puppets that
            // share frame resources; each must match its solo render.
            Curated {
                label: "multi_puppet_shared_pool",
                params: &[],
                camera_preset: "wide_stage",
                // +/-640 world units is exactly +/-160 px at this camera, so
                // both `composite_masks` instances must rasterize identically.
                frame_puppets: &[
                    ("composite_masks", -640.0, 0.0),
                    ("blend_modes_composite", 0.0, 0.0),
                    ("composite_masks", 640.0, 0.0),
                ],
                ticks_after_pose: 0,
            },
        ],
        _ => vec![Curated {
            label: "default",
            params: &[],
            camera_preset: "default",
            frame_puppets: &[],
            ticks_after_pose: 0,
        }],
    }
}

#[must_use]
pub fn build_matrix(model: &ModelSpec) -> Vec<Config> {
    let presets = camera_presets(model.default_zoom);
    curated_configs(&model.stem)
        .into_iter()
        .map(|c| {
            let camera = presets
                .iter()
                .find(|(name, _)| *name == c.camera_preset)
                .map_or(
                    Camera {
                        x: 0.0,
                        y: 0.0,
                        zoom: model.default_zoom,
                    },
                    |(_, cam)| *cam,
                );
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
                frame_puppets: c
                    .frame_puppets
                    .iter()
                    .map(|(stem, x, y)| FramePuppet {
                        model_stem: (*stem).to_string(),
                        x: *x,
                        y: *y,
                    })
                    .collect(),
                ticks_after_pose: c.ticks_after_pose,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Duplicate names would silently share one baseline PNG.
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

    #[test]
    fn curated_frame_puppets_resolve() {
        let models = default_models(Path::new("."));
        let stems: Vec<&str> = models.iter().map(|m| m.stem.as_str()).collect();
        for m in &models {
            for c in build_matrix(m) {
                for p in &c.frame_puppets {
                    assert!(
                        stems.contains(&p.model_stem.as_str()),
                        "{} names unknown model '{}'",
                        c.name,
                        p.model_stem
                    );
                }
                assert!(
                    c.frame_puppets.is_empty() || c.params.is_empty(),
                    "{} sets params on a multi-puppet pose, which ignores them",
                    c.name
                );
            }
        }
    }

    // Otherwise the baseline would pin a settled frame while claiming motion.
    #[test]
    fn curated_transients_have_a_pose_to_be_a_transient_of() {
        for m in default_models(Path::new(".")) {
            for c in build_matrix(&m) {
                if c.ticks_after_pose == 0 {
                    continue;
                }
                assert!(
                    !c.params.is_empty(),
                    "{} ticks a transient but changes no param",
                    c.name
                );
                assert!(
                    c.frame_puppets.is_empty(),
                    "{} ticks a transient on a multi-puppet pose, which ignores params",
                    c.name
                );
            }
        }
    }

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
