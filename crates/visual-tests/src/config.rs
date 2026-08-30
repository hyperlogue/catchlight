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

/// One puppet's placement inside a multi-puppet frame: which model, and where
/// it sits in world space (applied as that puppet's root transform, the way an
/// app positions two characters on one stage).
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
    /// Every puppet this frame draws, in order, when it draws more than one.
    /// Empty is the single-puppet default: `model_stem` alone at the origin.
    /// A multi-puppet frame renders each entry through its own renderer but
    /// one shared stencil target and composite pool, and renders every puppet
    /// at its param defaults — `params` applies to the single-puppet path
    /// only.
    pub frame_puppets: Vec<FramePuppet>,
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
        // Every blend mode the renderer implements, one disk per cell over a
        // 2D-gradient backdrop. A regression in any blend-state branch or
        // blend shader localizes to one cell of the diff heatmap.
        ModelSpec {
            stem: "blend_modes".into(),
            path: repro.join("blend_modes.clm"),
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
        // Four unmasked Normal composites over a gradient, each blitted by
        // the plain `blit.wgsl` path with one uniform off the identity:
        // opacity, tint, screen tint, and an inner composite blitting into an
        // outer one's slot (`cargo xtask gen-fixture
        // composite_blit_uniforms`). Every other model reaches that shader
        // only at opacity 1 / white tint / zero screen tint, so without this
        // its uniform math is unpinned — squaring the opacity multiply leaves
        // the rest of the suite green. A regression localizes to one cell of
        // the diff heatmap.
        ModelSpec {
            stem: "composite_blit_uniforms".into(),
            path: repro.join("composite_blit_uniforms.clm"),
            width: 512,
            height: 512,
            default_zoom: 1.0,
            texture_halvings: 0,
        },
        // A 1-texel checkerboard on two quads, minified 4x and 8x
        // (`cargo xtask gen-fixture mip_checker`) — the only configs in the
        // suite that sample a mip level at all. A correct box-filtered chain
        // resolves both quads to the same flat mid-gray; a missing chain
        // moires, a point-sampled one flattens to an extreme, and one that
        // stops short splits the two quads apart. `default_zoom` 0.25 is what
        // sets those sampling rates: it zooms out until the quads' 512 texels
        // land on 128 and 64 pixels.
        ModelSpec {
            stem: "mip_checker".into(),
            path: repro.join("mip_checker.clm"),
            width: 512,
            height: 512,
            default_zoom: 0.25,
            texture_halvings: 0,
        },
        // Two grid Parts with a welded seam, per-vertex weights 1 / 0.5 / 0
        // left-to-right (`cargo xtask gen-fixture welded_seam`). The `pull`
        // param deform-shifts the top part; the seam must stay closed with
        // each weight regime visible.
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
        // Four model-widths across, for multi-puppet frames: three puppets
        // spaced further than one width apart all fit, side by side and
        // non-overlapping. A power-of-two zoom keeps world-to-pixel exact in
        // f32, so puppets placed a whole number of pixels apart really land
        // there.
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

/// One hand-authored regression pose. Each entry exercises a distinct part of
/// the render path; the set is deliberately small (every config is a committed
/// baseline PNG in LFS) rather than an auto-generated sweep of every param.
struct Curated {
    label: &'static str,
    params: &'static [(&'static str, f32, f32)],
    /// Must name an entry from `camera_presets()`.
    camera_preset: &'static str,
    /// `(model stem, world x, world y)` per puppet when this pose draws more
    /// than one into a single frame; empty for the single-puppet default.
    /// Every stem must name an entry in `default_models()`.
    frame_puppets: &'static [(&'static str, f32, f32)],
}

fn curated_configs(stem: &str) -> Vec<Curated> {
    match stem {
        "welded_seam" => vec![
            // Rest: the seam is coincident, so welding must be a no-op.
            Curated {
                label: "default",
                params: &[],
                camera_preset: "default",
                frame_puppets: &[],
            },
            // Deformed: without welds the top part slides off whole; welded,
            // the seam blends per weight (B follows / midway / A pinned).
            Curated {
                label: "pull",
                params: &[("pull", 1.0, 0.0)],
                camera_preset: "default",
                frame_puppets: &[],
            },
        ],
        "composite_masks" => vec![
            Curated {
                label: "default",
                params: &[],
                camera_preset: "default",
                frame_puppets: &[],
            },
            // Three puppets in one frame, sharing one caller-owned
            // StencilTarget and CompositePool across three `render_list_ext`
            // calls — the arrangement an app with several puppets on screen
            // uses, and the one where a mask left in the stencil buffer or a
            // recycled composite slot would bleed from one puppet into the
            // next. `composite_masks` appears twice (its masks and its
            // composite blits are what such a bleed would corrupt) with a
            // dst-in-shader model between them, which snapshots the shared
            // framebuffer mid-frame. Each puppet must look exactly as it does
            // rendered alone.
            Curated {
                label: "multi_puppet_shared_pool",
                params: &[],
                camera_preset: "wide_stage",
                // +/-640 world units is exactly +/-160 pixels at this
                // camera, so the two `composite_masks` instances sit a whole
                // number of pixels apart and have to rasterize identically —
                // one drawn into fresh pool slots, the other into recycled
                // ones.
                frame_puppets: &[
                    ("composite_masks", -640.0, 0.0),
                    ("blend_modes_composite", 0.0, 0.0),
                    ("composite_masks", 640.0, 0.0),
                ],
            },
        ],
        // The repro models have no params: a single rest render isolates the
        // basic alpha/mask/blend/compositing pipeline.
        _ => vec![Curated {
            label: "default",
            params: &[],
            camera_preset: "default",
            frame_puppets: &[],
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
                frame_puppets: c
                    .frame_puppets
                    .iter()
                    .map(|(stem, x, y)| FramePuppet {
                        model_stem: (*stem).to_string(),
                        x: *x,
                        y: *y,
                    })
                    .collect(),
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

    // A multi-puppet pose that names a model the harness doesn't install
    // fails at render time with "unknown model"; catch it here instead. The
    // params check pins the documented split: the multi-puppet path renders
    // every puppet at its defaults, so a param set on such a pose would be
    // silently dropped.
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
