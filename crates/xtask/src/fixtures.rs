//! Generators for the committed test fixtures under `tests/models/`.
//!
//! These models are authored by hand rather than imported, so they exist only
//! as code here plus the encoded `.clp` checked into the repo. Regenerating is
//! only needed when a fixture's shape changes; the visual baselines under
//! `tests/baselines/` are rendered from the committed bytes.

use anyhow::{anyhow, Context, Result};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use catchlight_core::components::BlendMode;
use catchlight_core::formats::clp::{
    self, ClpBinding, ClpBindingValues, ClpCell, ClpCells, ClpDocument, ClpIndices, ClpMesh,
    ClpNode, ClpNodeKind, ClpParam, ClpPart, ClpPhysics, ClpTexture, ClpTransform, ClpWeld,
    ClpWeldPair, TextureAlpha, TextureEncoding,
};
use catchlight_core::params::InterpolateMode;

/// Builds a fixture's structure document and its texture table.
type Build = fn() -> (ClpDocument, Vec<ClpTexture>);

/// Every fixture this command can (re)build, by output stem.
const FIXTURES: &[(&str, Build)] = &[("welded_seam", welded_seam)];

pub fn names() -> impl Iterator<Item = &'static str> {
    FIXTURES.iter().map(|(name, _)| *name)
}

/// Build `<name>` and overwrite `tests/models/<name>.clp`. Returns the path
/// written, relative to the workspace root.
pub fn generate(name: &str) -> Result<PathBuf> {
    let build = FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, build)| build)
        .ok_or_else(|| anyhow!("unknown fixture: {name}"))?;
    let (doc, textures) = build();
    let encoded = clp::encode(&doc, &textures).context("encoding .clp")?;

    let relative = Path::new("tests")
        .join("models")
        .join(format!("{name}.clp"));
    let out = workspace_root().join(&relative);
    std::fs::write(&out, &encoded).with_context(|| format!("writing {}", out.display()))?;
    Ok(relative)
}

fn workspace_root() -> PathBuf {
    // crates/xtask/ -> crates/ -> workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

// --- welded_seam -----------------------------------------------------------

const COLS: usize = 3;
const ROWS: usize = 3;
/// Half the width of each grid, in puppet units.
const HALF_W: f32 = 150.0;
/// Height of each grid, measured from the shared seam.
const PART_H: f32 = 120.0;
/// Offset `pull` = 1 applies to every vertex of the upper part.
const PULL: [f32; 2] = [60.0, 40.0];
/// A's share of the meeting point, left to right along the seam.
const SEAM_WEIGHTS: [f32; COLS] = [1.0, 0.5, 0.0];

/// The weld regression model: two 3x3-grid Parts stacked vertically with a
/// coincident seam at y = 0 and a `pull` param that deform-shifts the whole
/// upper part by [`PULL`]. The seam is welded per vertex with weights
/// 1.0 / 0.5 / 0.0 left to right, so a single render at pull = 1 shows all
/// three regimes at once: the lower part stretching up to follow the upper,
/// both meeting midway, and the upper's corner staying pinned to the lower.
fn welded_seam() -> (ClpDocument, Vec<ClpTexture>) {
    let root = ClpNode {
        parent: None,
        name: "root".into(),
        enabled: true,
        zsort: 0.0,
        transform: identity_transform(),
        lock_to_root: false,
        kind: ClpNodeKind::Empty,
    };
    let upper = ClpNode {
        parent: Some(0),
        name: "upper".into(),
        ..part_node(grid_part(0, Growth::Up))
    };
    let lower = ClpNode {
        parent: Some(0),
        name: "lower".into(),
        ..part_node(grid_part(1, Growth::Down))
    };

    let n_verts = COLS * ROWS;
    let pull = ClpParam {
        name: "pull".into(),
        is_vec2: false,
        min: [0.0, 0.0],
        max: [1.0, 0.0],
        defaults: [0.0, 0.0],
        axis_points_x: vec![0.0, 1.0],
        axis_points_y: vec![0.0],
        bindings: vec![ClpBinding {
            node: 1,
            interpolate_mode: InterpolateMode::Linear,
            values: ClpBindingValues::Deform(ClpCells {
                // Both ends of the axis are authored: the rest pose has to be
                // an explicit zero cell, or interpolation has nothing to
                // interpolate from.
                cells: vec![
                    ClpCell {
                        x: 0,
                        y: 0,
                        value: vec![0.0; 2 * n_verts],
                    },
                    ClpCell {
                        x: 1,
                        y: 0,
                        value: PULL.repeat(n_verts),
                    },
                ],
            }),
        }],
    };

    let doc = ClpDocument {
        physics: ClpPhysics::default(),
        nodes: vec![root, upper, lower],
        params: vec![pull],
        welds: vec![ClpWeld {
            a: 1,
            b: 2,
            // Both grids order their seam row first, so vertex i on one side
            // is the coincident vertex i on the other.
            pairs: (0..COLS)
                .map(|i| ClpWeldPair {
                    a_vert: i as u32,
                    b_vert: i as u32,
                    weight: SEAM_WEIGHTS[i],
                })
                .collect(),
        }],
    };
    let textures = vec![solid_texture([230, 140, 60]), solid_texture([50, 160, 170])];
    (doc, textures)
}

/// Which way a grid marches away from the seam at y = 0.
#[derive(Clone, Copy)]
enum Growth {
    Up,
    Down,
}

/// A 3x3-vertex grid spanning x in `[-HALF_W, HALF_W]` and `PART_H` tall, with
/// its seam row at y = 0 and the remaining rows marching away in `growth` — so
/// both parts' seam vertices land at indices 0..COLS, which is what the weld
/// pairs reference. UVs put v = 0 at the part's topmost row, so both textures
/// sit upright in world space.
fn grid_part(albedo: u32, growth: Growth) -> ClpPart {
    let y_top = match growth {
        Growth::Up => PART_H,
        Growth::Down => 0.0,
    };
    let row_step = PART_H / (ROWS - 1) as f32;
    let col_step = 2.0 * HALF_W / (COLS - 1) as f32;

    let mut verts = Vec::with_capacity(2 * ROWS * COLS);
    let mut uvs = Vec::with_capacity(2 * ROWS * COLS);
    for r in 0..ROWS {
        // Written as a signed distance from the seam rather than a scaled
        // offset so row 0 lands on +0.0 for both parts, not -0.0.
        let from_seam = r as f32 * row_step;
        let y = match growth {
            Growth::Up => from_seam,
            Growth::Down => 0.0 - from_seam,
        };
        for c in 0..COLS {
            let x = -HALF_W + c as f32 * col_step;
            verts.extend_from_slice(&[x, y]);
            uvs.extend_from_slice(&[(x + HALF_W) / (2.0 * HALF_W), (y_top - y) / PART_H]);
        }
    }

    let mut indices = Vec::with_capacity(6 * (ROWS - 1) * (COLS - 1));
    for r in 0..ROWS - 1 {
        for c in 0..COLS - 1 {
            let v00 = (r * COLS + c) as u16;
            let (v01, v10, v11) = (v00 + 1, v00 + COLS as u16, v00 + COLS as u16 + 1);
            indices.extend_from_slice(&[v00, v01, v11, v00, v11, v10]);
        }
    }

    ClpPart {
        mesh: ClpMesh {
            verts,
            uvs,
            indices: ClpIndices::U16(indices),
            origin: [0.0, 0.0],
        },
        albedo,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        tint: [1.0, 1.0, 1.0],
        screen_tint: [0.0, 0.0, 0.0],
        masks: Vec::new(),
        mask_threshold: 0.5,
    }
}

/// A 64x64 opaque single-colour PNG. Small and deterministic; the fixtures
/// only need each Part to be visually distinguishable.
fn solid_texture(rgb: [u8; 3]) -> ClpTexture {
    use image::ImageEncoder;

    const SIZE: u32 = 64;
    let pixel = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
    let img = image::RgbaImage::from_pixel(SIZE, SIZE, pixel);
    let mut data = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        Cursor::new(&mut data),
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(&img, SIZE, SIZE, image::ExtendedColorType::Rgba8)
    .unwrap();
    ClpTexture {
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data,
    }
}

fn identity_transform() -> ClpTransform {
    ClpTransform {
        translation: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0, 1.0],
    }
}

/// A default node wrapping `part`; callers fill in `parent` and `name`.
fn part_node(part: ClpPart) -> ClpNode {
    ClpNode {
        parent: None,
        name: String::new(),
        enabled: true,
        zsort: 0.0,
        transform: identity_transform(),
        lock_to_root: false,
        kind: ClpNodeKind::Part(part),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed `.clp` is what the visual baselines were rendered from,
    /// so it has to keep matching the generator above. Structure only — never
    /// byte-equality against a fresh encode, which any harmless encoder or PNG
    /// change would break.
    #[test]
    fn committed_welded_seam_still_matches_the_generator() {
        let path = workspace_root().join("tests/models/welded_seam.clp");
        let bytes = std::fs::read(&path).unwrap();
        let committed = clp::decode(&bytes).unwrap().doc;

        // The properties the weld regression renders actually depend on,
        // spelled out so losing one names itself.
        let parts = committed
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, ClpNodeKind::Part(_)))
            .count();
        assert_eq!(parts, 2, "two welded grid Parts");
        let weld = committed.welds.first().unwrap();
        let weights: Vec<f32> = weld.pairs.iter().map(|p| p.weight).collect();
        assert_eq!(weights, SEAM_WEIGHTS, "all three weight regimes");
        assert_eq!(committed.params.len(), 1);
        assert_eq!(committed.params[0].name, "pull");

        // Then the whole document, to catch anything the list above misses.
        let (generated, _) = welded_seam();
        assert_eq!(
            committed, generated,
            "tests/models/welded_seam.clp has drifted from the generator above; \
             re-run `cargo xtask gen-fixture welded_seam`"
        );
    }
}
