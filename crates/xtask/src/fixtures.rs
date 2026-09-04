//! Generators for the committed test fixtures under `tests/models/`.
//!
//! These models are authored by hand rather than imported, so they exist only
//! as code here plus the encoded `.clm` checked into the repo. A generator
//! authors the `.clm` *document* and `generate` reads it into a [`Model`] on
//! the way to the file, so a fixture is refused here for exactly what a `.clm`
//! off disk is refused for. Nodes are numbered by [`push`] and params and
//! textures by their position, so a fixture's Ids are the ones an import
//! mints. Regenerating is only needed when a fixture's shape changes; the
//! visual baselines under `tests/baselines/` are rendered from the committed
//! bytes.
//!
//! Regenerate one with `cargo xtask gen-fixture <name>`.
//! `committed_fixtures_still_match_their_generators` pins each committed
//! file's *structure* (never byte equality) to its generator here, so drift
//! names itself. `cargo xtask` also has `import <model.inx|.inp>
//! [-o <model.clm>]`.
//!
//! The synthetic `.inx` fixtures are built instead by
//! `scripts/build_minimal_inx.py`, a `uv` inline-script (`uv` and `python3`
//! are in the dev shell).

use anyhow::{anyhow, Context, Result};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use catchlight_core::components::BlendMode;
use catchlight_core::formats::clm::{
    ClmBinding, ClmBindingValues, ClmCell, ClmCells, ClmComposite, ClmDocument, ClmFile,
    ClmIndices, ClmMesh, ClmNode, ClmNodeKind, ClmParam, ClmPart, ClmSlot, ClmSlotPair, ClmTexture,
    ClmTransform, ClmWeld, TextureAlpha, TextureEncoding,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::{Model, NodeId, ParamId, SlotId, TexId};

/// Builds a fixture's structure document and its texture table.
type Build = fn() -> (ClmDocument, Vec<ClmTexture>);

/// Every fixture this command can (re)build, by output stem.
const FIXTURES: &[(&str, Build)] = &[
    ("composite_blit_uniforms", composite_blit_uniforms),
    ("mip_checker", mip_checker),
    ("welded_seam", welded_seam),
];

pub fn names() -> impl Iterator<Item = &'static str> {
    FIXTURES.iter().map(|(name, _)| *name)
}

/// Build `<name>` and overwrite `tests/models/<name>.clm`. Returns the path
/// written, relative to the workspace root.
pub fn generate(name: &str) -> Result<PathBuf> {
    let build = FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, build)| build)
        .ok_or_else(|| anyhow!("unknown fixture: {name}"))?;
    let (doc, textures) = build();
    let encoded = Model::from_clm_file(&ClmFile { doc, textures })
        .and_then(|m| m.to_clm_bytes())
        .context("encoding .clm")?;

    let relative = Path::new("tests")
        .join("models")
        .join(format!("{name}.clm"));
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

/// The Id a node at position `i` carries: `root` for the first, `node-<i>`
/// after it — the same rule the inochi2d import mints by, so a fixture and an
/// imported model name things the same way.
fn nid(i: usize) -> NodeId {
    let id = if i == 0 {
        "root".to_string()
    } else {
        format!("node-{i}")
    };
    NodeId::new(id).expect("a generated node id")
}

fn pid(i: usize) -> ParamId {
    ParamId::new(format!("param-{i}")).expect("a generated param id")
}

fn tid(i: usize) -> TexId {
    TexId::new(format!("tex-{i}")).expect("a generated texture id")
}

/// Append `node` under the node at position `parent`, give it the Id its own
/// position mints, and hand that position back for its children to name.
///
/// Appending in parent-before-child order is what makes the document
/// topological, which is what the `.clm` reader requires.
fn push(nodes: &mut Vec<ClmNode>, parent: Option<usize>, mut node: ClmNode) -> usize {
    let i = nodes.len();
    node.id = nid(i);
    node.parent = parent.map(nid);
    nodes.push(node);
    i
}

/// Number a fixture's textures by position, the way an import does.
fn textures(pngs: Vec<Vec<u8>>) -> Vec<ClmTexture> {
    pngs.into_iter()
        .enumerate()
        .map(|(i, data)| ClmTexture {
            id: tid(i),
            encoding: TextureEncoding::Png,
            // Authored straight, so the texture prep premultiplies it the way
            // a real texture arrives.
            alpha: TextureAlpha::Straight,
            data,
        })
        .collect()
}

// --- mip_checker -----------------------------------------------------------

/// Side of the checkerboard texture, in texels. A power of two, so every mip
/// level halves exactly and the box filter never hits the odd-dimension
/// edge-clamp in `mip_downsample.wgsl`.
const CHECKER_TEXELS: u32 = 512;
/// The two quads: node name, half-width, and world x, in model units.
/// Rendered at the model's `default_zoom` of 0.25 they cover 128 and 64
/// pixels, so the same [`CHECKER_TEXELS`]-wide texture lands at 4 and 8 texels
/// per pixel; the x offsets keep them from overlapping.
const CHECKER_QUADS: [(&str, f32, f32); 2] = [
    ("minified_4x", 256.0, -320.0),
    ("minified_8x", 128.0, 320.0),
];

/// The mip regression model: one 1-texel checkerboard — the highest spatial
/// frequency an RGBA8 texture can carry — drawn on two quads minified 4x and
/// 8x. Nothing else in the suite samples a mip level at all.
///
/// Every 2x2 block of a 1-texel checker averages to exactly half, so a
/// correctly box-filtered chain makes both quads flat mid-gray: the 4x quad
/// off level 2, the 8x quad off level 3, so a chain that stops short shows up
/// as a difference between the two. A chain that isn't generated at all
/// leaves the sampler minifying level 0 into moire, and one downsampled by
/// point-sampling instead of box-filtering collapses to flat white or flat
/// black.
///
/// The averaging also has to land in linear space, which is what the
/// `Rgba8UnormSrgb` decode on load and encode on store buy: black and white
/// averaged in linear encode to sRGB ~187, averaged as gamma-encoded bytes
/// they would come out ~128 — far enough apart that the baseline pins which
/// one happened.
fn mip_checker() -> (ClmDocument, Vec<ClmTexture>) {
    let mut nodes = Vec::new();
    let root = push(&mut nodes, None, group_node("root"));
    for (name, half, x) in CHECKER_QUADS {
        push(
            &mut nodes,
            Some(root),
            ClmNode {
                name: name.into(),
                transform: ClmTransform {
                    translation: [x, 0.0, 0.0],
                    ..identity_transform()
                },
                ..part_node(quad_part(0, half))
            },
        );
    }

    let doc = ClmDocument {
        nodes,
        ..ClmDocument::default()
    };
    (doc, textures(vec![checker_texture(CHECKER_TEXELS)]))
}

/// A `size`x`size` opaque checkerboard with 1-texel cells: the highest
/// spatial frequency an RGBA8 texture can carry, and the only pattern whose
/// correctly filtered mip levels are a single known value.
fn checker_texture(size: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(size, size, |x, y| {
        if (x + y) % 2 == 0 {
            image::Rgba([255, 255, 255, 255])
        } else {
            image::Rgba([0, 0, 0, 255])
        }
    });
    flat_image(img)
}

/// One quad spanning `[-half, half]` on both axes, its texture mapped over
/// the whole face. UVs put v = 0 at the top row so the texture sits upright
/// in the Y-up world, same convention as [`grid_part`].
fn quad_part(albedo: usize, half: f32) -> ClmPart {
    ClmPart {
        mesh: ClmMesh {
            #[rustfmt::skip]
            verts: vec![
                -half, -half,
                 half, -half,
                 half,  half,
                -half,  half,
            ],
            #[rustfmt::skip]
            uvs: vec![
                0.0, 1.0,
                1.0, 1.0,
                1.0, 0.0,
                0.0, 0.0,
            ],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        },
        albedo: Some(tid(albedo)),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        tint: [1.0, 1.0, 1.0],
        screen_tint: [0.0, 0.0, 0.0],
        masks: Vec::new(),
        mask_threshold: 0.5,
        slots: Vec::new(),
    }
}

// --- composite_blit_uniforms -----------------------------------------------

/// Half-width of one cell's back quad, in model units, and how far its
/// translucent front quad is offset from it.
const CELL_HALF: f32 = 45.0;
const CELL_FRONT_OFFSET: f32 = 25.0;
/// One cell: where it sits, and the composite property it takes off the
/// identity. Everything it doesn't vary stays at the identity the rest of the
/// suite already uses.
struct BlitCell {
    name: &'static str,
    x: f32,
    opacity: f32,
    tint: [f32; 3],
    screen_tint: [f32; 3],
}

const BLIT_CELLS: [BlitCell; 3] = [
    BlitCell {
        name: "opacity",
        x: -180.0,
        opacity: 0.5,
        tint: [1.0; 3],
        screen_tint: [0.0; 3],
    },
    BlitCell {
        name: "tint",
        x: -60.0,
        opacity: 1.0,
        tint: [1.0, 0.45, 0.2],
        screen_tint: [0.0; 3],
    },
    BlitCell {
        name: "screen_tint",
        x: 60.0,
        opacity: 1.0,
        tint: [1.0; 3],
        screen_tint: [0.0, 0.35, 0.7],
    },
];
/// The nested cell's outer and inner composite opacities.
const NESTED_CELL_X: f32 = 180.0;
const NESTED_OUTER_OPACITY: f32 = 0.6;
const NESTED_INNER_OPACITY: f32 = 0.7;

/// The composite-blit regression model: four unmasked Normal composites over a
/// gradient backdrop, each blitted by the plain `blit.wgsl` path with one of
/// its uniforms off the identity — opacity, tint, screen tint, and an inner
/// composite blitted into an outer one's slot rather than into the main view.
///
/// The rest of the suite reaches that shader only at opacity 1, white tint and
/// zero screen tint, so its uniform math is unpinned: squaring the opacity
/// multiply, or dropping it entirely, leaves every other baseline green. Each
/// cell holds an opaque quad under a half-alpha one, so the blit is exercised
/// over three source alphas at once (opaque, blended, translucent-only) —
/// which is what the screen-tint term, scaled by the sampled alpha, needs.
fn composite_blit_uniforms() -> (ClmDocument, Vec<ClmTexture>) {
    let mut nodes = Vec::new();
    let root = push(&mut nodes, None, group_node("root"));
    push(
        &mut nodes,
        Some(root),
        ClmNode {
            name: "backdrop".into(),
            z_order: -10.0,
            ..part_node(quad_part(0, 256.0))
        },
    );

    for c in BLIT_CELLS {
        let cell = push(
            &mut nodes,
            Some(root),
            composite_node(c.name, c.x, c.opacity, c.tint, c.screen_tint),
        );
        push(&mut nodes, Some(cell), cell_quad("back", 1, 0.0));
        push(
            &mut nodes,
            Some(cell),
            cell_quad("front", 2, CELL_FRONT_OFFSET),
        );
    }
    // The nested cell: an inner composite that has to blit into the outer
    // composite's slot, then travel out through the outer's own blit.
    let outer = push(
        &mut nodes,
        Some(root),
        composite_node(
            "nested_outer",
            NESTED_CELL_X,
            NESTED_OUTER_OPACITY,
            [1.0; 3],
            [0.0; 3],
        ),
    );
    push(&mut nodes, Some(outer), cell_quad("back", 1, 0.0));
    let inner = push(
        &mut nodes,
        Some(outer),
        composite_node(
            "nested_inner",
            0.0,
            NESTED_INNER_OPACITY,
            [1.0; 3],
            [0.0; 3],
        ),
    );
    push(
        &mut nodes,
        Some(inner),
        cell_quad("front", 2, CELL_FRONT_OFFSET),
    );

    let doc = ClmDocument {
        nodes,
        ..ClmDocument::default()
    };
    (
        doc,
        textures(vec![
            gradient_texture(),
            solid_texture([235, 140, 60]),
            flat_texture([60, 130, 220, 128]),
        ]),
    )
}

/// An unmasked Normal composite parented to the root at world `x`.
fn composite_node(
    name: &str,
    x: f32,
    opacity: f32,
    tint: [f32; 3],
    screen_tint: [f32; 3],
) -> ClmNode {
    ClmNode {
        name: name.into(),
        transform: ClmTransform {
            translation: [x, 0.0, 0.0],
            ..identity_transform()
        },
        kind: ClmNodeKind::Composite(ClmComposite {
            opacity,
            blend_mode: BlendMode::Normal,
            tint,
            screen_tint,
            masks: Vec::new(),
            mask_threshold: 0.5,
            propagate_meshgroup: true,
        }),
        ..blank_node()
    }
}

/// One quad inside a cell, offset diagonally by `offset` so the cell ends up
/// with an opaque region, an overlap, and a translucent-only region.
fn cell_quad(name: &str, albedo: usize, offset: f32) -> ClmNode {
    ClmNode {
        name: name.into(),
        transform: ClmTransform {
            translation: [offset, -offset, 0.0],
            ..identity_transform()
        },
        ..part_node(quad_part(albedo, CELL_HALF))
    }
}

/// A 64x64 opaque two-axis gradient. A composite blitted over it differs
/// everywhere it covers when its opacity or tint is wrong, not just at the
/// edges the way a flat backdrop would.
fn gradient_texture() -> Vec<u8> {
    const SIZE: u32 = 64;
    let scale = |v: u32| (v * 255 / (SIZE - 1)) as u8;
    flat_image(image::RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        image::Rgba([scale(x), scale(y), scale(SIZE - 1 - x), 255])
    }))
}

// --- welded_seam -----------------------------------------------------------

const COLS: usize = 3;
const ROWS: usize = 3;
/// Half the width of each grid, in model units.
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
fn welded_seam() -> (ClmDocument, Vec<ClmTexture>) {
    let mut nodes = Vec::new();
    let root = push(&mut nodes, None, group_node("root"));
    let upper = push(
        &mut nodes,
        Some(root),
        ClmNode {
            name: "upper".into(),
            ..part_node(grid_part(0, Growth::Up))
        },
    );
    let lower = push(
        &mut nodes,
        Some(root),
        ClmNode {
            name: "lower".into(),
            ..part_node(grid_part(1, Growth::Down))
        },
    );

    let n_verts = COLS * ROWS;
    let pull = ClmParam {
        id: pid(0),
        name: "pull".into(),
        min: 0.0,
        max: 1.0,
        default: 0.0,
        key_positions: vec![0.0, 1.0],
    };
    let deform = ClmBinding {
        params: vec![pid(0)],
        node: nid(upper),
        interpolate_mode: InterpolateMode::Linear,
        values: ClmBindingValues::Deform(ClmCells {
            // Both ends of the axis are authored: the rest pose has to be
            // an explicit zero cell, or interpolation has nothing to
            // interpolate from.
            cells: vec![
                ClmCell {
                    x: 0,
                    y: 0,
                    value: vec![0.0; 2 * n_verts],
                },
                ClmCell {
                    x: 1,
                    y: 0,
                    value: PULL.repeat(n_verts),
                },
            ],
        }),
    };

    let doc = ClmDocument {
        nodes,
        params: vec![pull],
        bindings: vec![deform],
        // Both grids order their seam row first, so slot `s<i>` names the
        // coincident vertex on each side.
        welds: vec![ClmWeld {
            a: nid(upper),
            b: nid(lower),
            pairs: (0..COLS)
                .map(|i| ClmSlotPair {
                    a: slot_id(i),
                    b: slot_id(i),
                    weight: SEAM_WEIGHTS[i],
                })
                .collect(),
        }],
        ..ClmDocument::default()
    };
    (
        doc,
        textures(vec![
            solid_texture([230, 140, 60]),
            solid_texture([50, 160, 170]),
        ]),
    )
}

/// The slot Ids both parts carry, one per column of the shared seam row —
/// the same Id on each side, which is how the one weld pairs them.
fn slot_id(i: usize) -> SlotId {
    SlotId::new(format!("s{i}")).expect("a generated slot id")
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
fn grid_part(albedo: usize, growth: Growth) -> ClmPart {
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

    ClmPart {
        mesh: ClmMesh {
            verts,
            uvs,
            indices: ClmIndices::U16(indices),
            origin: [0.0, 0.0],
        },
        albedo: Some(tid(albedo)),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        tint: [1.0, 1.0, 1.0],
        screen_tint: [0.0, 0.0, 0.0],
        masks: Vec::new(),
        mask_threshold: 0.5,
        // The welded row: slot `s<i>` is filled by the seam vertex `i`, which
        // both grids order first.
        slots: (0..COLS)
            .map(|i| ClmSlot {
                id: slot_id(i),
                vertex: Some(i as u32),
            })
            .collect(),
    }
}

/// A 64x64 opaque single-colour PNG. Small and deterministic; the fixtures
/// only need each Part to be visually distinguishable.
fn solid_texture(rgb: [u8; 3]) -> Vec<u8> {
    flat_texture([rgb[0], rgb[1], rgb[2], 255])
}

/// A 64x64 single-colour PNG, alpha included.
fn flat_texture(rgba: [u8; 4]) -> Vec<u8> {
    flat_image(image::RgbaImage::from_pixel(64, 64, image::Rgba(rgba)))
}

/// Encode an authored image as a fixture texture's PNG bytes.
fn flat_image(img: image::RgbaImage) -> Vec<u8> {
    encode_png(&img)
}

/// Encode to PNG the same way for every fixture: max compression, no
/// pre-filter, so regenerating an unchanged fixture is a byte-for-byte no-op.
fn encode_png(img: &image::RgbaImage) -> Vec<u8> {
    use image::ImageEncoder;

    let mut data = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        Cursor::new(&mut data),
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(
        img,
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    data
}

fn identity_transform() -> ClmTransform {
    ClmTransform {
        translation: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0, 1.0],
    }
}

/// A default node wrapping `part`; [`push`] fills in the Id and the parent,
/// and callers fill in the name.
fn part_node(part: ClmPart) -> ClmNode {
    ClmNode {
        kind: ClmNodeKind::Part(part),
        ..blank_node()
    }
}

fn group_node(name: &str) -> ClmNode {
    ClmNode {
        name: name.into(),
        ..blank_node()
    }
}

/// The fields every fixture node shares. The Id and the parent are
/// placeholders [`push`] overwrites.
fn blank_node() -> ClmNode {
    ClmNode {
        id: nid(0),
        parent: None,
        name: String::new(),
        enabled: true,
        z_order: 0.0,
        transform: identity_transform(),
        lock_to_root: false,
        kind: ClmNodeKind::Group,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode `tests/models/<name>.clm` back to the document its generator
    /// authors.
    fn committed(name: &str) -> ClmFile {
        let path = workspace_root()
            .join("tests/models")
            .join(format!("{name}.clm"));
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        Model::from_clm_bytes(&bytes)
            .unwrap()
            .to_clm_file()
            .unwrap()
    }

    /// Every committed `.clm` is what its visual baselines were rendered
    /// from, so each has to keep matching the generator it came from — a
    /// fixture whose generator drifts silently renders a different model than
    /// the one the baseline pins. Structure only, never byte-equality against
    /// a fresh encode, which any harmless encoder or PNG change would break:
    /// the committed file is read into a Model and written back out as a
    /// document, and *that* is compared with the document the generator
    /// authors. The texture table is excluded for the same reason — its
    /// bytes are a PNG encoder's output. See
    /// `committed_mip_checker_texture_is_a_one_texel_checkerboard` for the
    /// one fixture whose texture content is the point.
    #[test]
    fn committed_fixtures_still_match_their_generators() {
        for (name, build) in FIXTURES {
            let (generated, _) = build();
            assert_eq!(
                committed(name).doc,
                generated,
                "tests/models/{name}.clm has drifted from its generator; \
                 re-run `cargo xtask gen-fixture {name}`"
            );
        }
    }

    /// The document comparison above covers structure but not the texture
    /// table, and the whole point of this fixture is the texture: minifying a
    /// 1-texel checkerboard is what makes a broken mip chain visible. Decode
    /// it and check the frequency survived.
    #[test]
    fn committed_mip_checker_texture_is_a_one_texel_checkerboard() {
        let file = committed("mip_checker");
        let [texture] = &file.textures[..] else {
            panic!("mip_checker carries exactly one texture");
        };
        let img = image::load_from_memory(&texture.data).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (CHECKER_TEXELS, CHECKER_TEXELS));
        for y in 0..CHECKER_TEXELS {
            for x in 0..CHECKER_TEXELS {
                let expected = if (x + y) % 2 == 0 { 255 } else { 0 };
                assert_eq!(
                    img.get_pixel(x, y).0,
                    [expected, expected, expected, 255],
                    "texel ({x}, {y}) breaks the 1-texel checkerboard"
                );
            }
        }
    }

    /// Spells out the properties the weld regression renders actually depend
    /// on, so losing one names itself instead of surfacing as a whole-document
    /// mismatch in the test above.
    #[test]
    fn committed_welded_seam_keeps_its_weld_structure() {
        let committed = committed("welded_seam").doc;

        // The properties the weld regression renders actually depend on,
        // spelled out so losing one names itself.
        let parts = committed
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, ClmNodeKind::Part(_)))
            .count();
        assert_eq!(parts, 2, "two welded grid Parts");
        let weld = committed.welds.first().unwrap();
        let weights: Vec<f32> = weld.pairs.iter().map(|p| p.weight).collect();
        assert_eq!(weights, SEAM_WEIGHTS, "all three weight regimes");
        // Each pair names the slot both parts fill with the coincident vertex.
        for (i, p) in weld.pairs.iter().enumerate() {
            assert_eq!(p.a, slot_id(i));
            assert_eq!(p.b, slot_id(i));
        }
        assert_eq!(committed.params.len(), 1);
        assert_eq!(committed.params[0].name, "pull");
    }
}
