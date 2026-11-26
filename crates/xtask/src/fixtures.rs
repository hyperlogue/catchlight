//! Generators for the hand-authored test fixtures under `tests/models/`.
//!
//! Regenerate one with `cargo xtask gen-fixture <name>`; only needed when a
//! fixture's structure changes. Nodes, params and textures are numbered by
//! position so fixture Ids match what an import mints.

use anyhow::{Context, Result, anyhow};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use catchlight_core::components::BlendMode;
use catchlight_core::formats::clm::{
    ClmBinding, ClmBindingValues, ClmCell, ClmCells, ClmComposite, ClmFile, ClmIndices, ClmMesh,
    ClmNode, ClmNodeKind, ClmParam, ClmPart, ClmSlot, ClmSlotPair, ClmSpine, ClmStructure,
    ClmTexture, ClmTransform, ClmWeld, TextureAlpha, TextureEncoding,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::{
    Model, MotionResponse, NodeId, ParamId, ResponseInput, ResponseSource, SlotId, TexId,
};
use catchlight_editor_core::fit_strand;

type Build = fn() -> (ClmStructure, Vec<ClmTexture>);

const FIXTURES: &[(&str, Build)] = &[
    ("composite_blit_uniforms", composite_blit_uniforms),
    ("mip_checker", mip_checker),
    ("part_uniforms", part_uniforms),
    ("strand_response", strand_response),
    ("two_param_grid", two_param_grid),
    ("welded_seam", welded_seam),
];

pub fn names() -> impl Iterator<Item = &'static str> {
    FIXTURES.iter().map(|(name, _)| *name)
}

pub fn generate(name: &str) -> Result<PathBuf> {
    let build = FIXTURES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, build)| build)
        .ok_or_else(|| anyhow!("unknown fixture: {name}"))?;
    let (doc, textures) = build();
    let encoded = Model::from_clm_file(&ClmFile {
        doc,
        textures,
        extensions: Vec::new(),
        unknown_sections: Vec::new(),
    })
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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// Matches the inochi2d import's Id rule.
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

/// Parent-before-child appending keeps the structure topological, which the
/// `.clm` reader requires.
fn push(nodes: &mut Vec<ClmNode>, parent: Option<usize>, mut node: ClmNode) -> usize {
    let i = nodes.len();
    node.id = nid(i);
    node.parent = parent.map(nid);
    nodes.push(node);
    i
}

fn textures(pngs: Vec<Vec<u8>>) -> Vec<ClmTexture> {
    pngs.into_iter()
        .enumerate()
        .map(|(i, data)| ClmTexture {
            id: tid(i),
            encoding: TextureEncoding::Png,
            // Premultiplied by texture prep, as a real texture arrives.
            alpha: TextureAlpha::Straight,
            data,
        })
        .collect()
}

// --- two_param_grid --------------------------------------------------------

const GRID_X: usize = 0;
const GRID_Y: usize = 1;
const SWEEP: usize = 2;

/// Every default sits at key position 0, so the all-defaults pose is cell `(0, 0)`.
const PARAMS: [(&str, f32, f32, f32, &[f32]); 3] = [
    ("grid_x", 0.0, 1.0, 0.0, &[0.0, 0.5, 1.0]),
    // Not 0..1, so key positions and values cannot be confused.
    ("grid_y", -2.0, 2.0, -2.0, &[0.0, 1.0]),
    ("sweep", 0.0, 10.0, 0.0, &[0.0, 1.0]),
];

/// Per grid step, in model units.
const GRID_STEP: [f32; 2] = [10.0, 20.0];
const SWEEP_SHIFT_X: f32 = 25.0;
const SWEEP_Z: f32 = 3.0;
const SWEEP_OPACITY: f32 = 0.25;

/// Model units.
const GRID_QUAD_HALF: f32 = 60.0;
const GRID_QUAD_GAP: f32 = 160.0;

/// Covers every field a pose dump reports, including a two-param joint grid
/// that one-param sweeps cannot fully reach.
fn two_param_grid() -> (ClmStructure, Vec<ClmTexture>) {
    let mut nodes = Vec::new();
    let root = push(&mut nodes, None, group_node("root"));
    let driven = push(
        &mut nodes,
        Some(root),
        ClmNode {
            name: "driven".into(),
            transform: ClmTransform {
                translation: [-GRID_QUAD_GAP / 2.0, 0.0, 0.0],
                ..identity_transform()
            },
            ..part_node(quad_part(0, GRID_QUAD_HALF))
        },
    );
    let swept = push(
        &mut nodes,
        Some(root),
        ClmNode {
            name: "swept".into(),
            transform: ClmTransform {
                translation: [GRID_QUAD_GAP / 2.0, 0.0, 0.0],
                ..identity_transform()
            },
            ..part_node(quad_part(1, GRID_QUAD_HALF))
        },
    );

    let params = PARAMS
        .iter()
        .enumerate()
        .map(|(i, (name, min, max, default, _))| ClmParam {
            id: pid(i),
            name: (*name).into(),
            min: *min,
            max: *max,
            default: *default,
        })
        .collect();

    // Author every cell: the fill only derives cells between authored ones.
    let (w, h) = (PARAMS[GRID_X].4.len(), PARAMS[GRID_Y].4.len());
    let joint = ClmBinding {
        params: vec![pid(GRID_X), pid(GRID_Y)],
        key_positions: vec![PARAMS[GRID_X].4.to_vec(), PARAMS[GRID_Y].4.to_vec()],
        node: nid(driven),
        interpolate_mode: InterpolateMode::Linear,
        values: ClmBindingValues::Deform(ClmCells {
            cells: (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| ClmCell {
                    x: x as u32,
                    y: y as u32,
                    value: [GRID_STEP[0] * x as f32, GRID_STEP[1] * y as f32]
                        .repeat(GRID_QUAD_VERTS),
                })
                .collect(),
        }),
    };

    let doc = ClmStructure {
        nodes,
        params,
        bindings: vec![
            joint,
            sweep_binding(
                nid(swept),
                ClmBindingValues::TransformTX,
                0.0,
                SWEEP_SHIFT_X,
            ),
            sweep_binding(nid(swept), ClmBindingValues::ZOrder, 0.0, SWEEP_Z),
            // Opacity folds multiplicatively, so its identity is 1, not 0.
            sweep_binding(nid(driven), ClmBindingValues::Opacity, 1.0, SWEEP_OPACITY),
        ],
        ..ClmStructure::default()
    };
    (
        doc,
        textures(vec![
            solid_texture([210, 90, 70]),
            solid_texture([70, 150, 210]),
        ]),
    )
}

const GRID_QUAD_VERTS: usize = 4;

fn sweep_binding(
    node: NodeId,
    wrap: fn(ClmCells<f32>) -> ClmBindingValues,
    rest: f32,
    far: f32,
) -> ClmBinding {
    ClmBinding {
        params: vec![pid(SWEEP)],
        key_positions: vec![PARAMS[SWEEP].4.to_vec()],
        node,
        interpolate_mode: InterpolateMode::Linear,
        values: wrap(ClmCells {
            cells: vec![
                ClmCell {
                    x: 0,
                    y: 0,
                    value: rest,
                },
                ClmCell {
                    x: 1,
                    y: 0,
                    value: far,
                },
            ],
        }),
    }
}

// --- mip_checker -----------------------------------------------------------

/// Texels. A power of two so every mip halves exactly and avoids the
/// odd-dimension edge clamp in `mip_downsample.wgsl`.
const CHECKER_TEXELS: u32 = 512;
/// Node name, half-width, world x (model units). At zoom 0.25 these cover
/// 128 and 64 pixels: 4 and 8 texels per pixel.
const CHECKER_QUADS: [(&str, f32, f32); 2] = [
    ("minified_4x", 256.0, -320.0),
    ("minified_8x", 128.0, 320.0),
];

/// A correct box-filtered chain renders both quads flat mid-gray (level 2 and
/// level 3). Averaging must happen in linear space: linear gives sRGB ~187,
/// gamma-space averaging ~128.
fn mip_checker() -> (ClmStructure, Vec<ClmTexture>) {
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

    let doc = ClmStructure {
        nodes,
        ..ClmStructure::default()
    };
    (doc, textures(vec![checker_texture(CHECKER_TEXELS)]))
}

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

/// v = 0 at the top row so the texture is upright in the Y-up world.
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

/// Model units.
const CELL_HALF: f32 = 45.0;
const CELL_FRONT_OFFSET: f32 = 25.0;
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
const NESTED_CELL_X: f32 = 180.0;
const NESTED_OUTER_OPACITY: f32 = 0.6;
const NESTED_INNER_OPACITY: f32 = 0.7;

/// Each cell takes one `blit.wgsl` uniform off the identity, which no other
/// fixture does. An opaque quad under a half-alpha one exercises three source
/// alphas, which the alpha-scaled screen-tint term needs.
fn composite_blit_uniforms() -> (ClmStructure, Vec<ClmTexture>) {
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

    let doc = ClmStructure {
        nodes,
        ..ClmStructure::default()
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

/// The same cells as `composite_blit_uniforms`, on the parts themselves: each
/// takes one `basic.wgsl` uniform off the identity.
fn part_uniforms() -> (ClmStructure, Vec<ClmTexture>) {
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
        for (name, albedo, offset, z_order) in
            [("back", 1, 0.0, 0.0), ("front", 2, CELL_FRONT_OFFSET, 1.0)]
        {
            push(
                &mut nodes,
                Some(root),
                ClmNode {
                    name: format!("{}_{name}", c.name).as_str().into(),
                    z_order,
                    transform: ClmTransform {
                        translation: [c.x + offset, -offset, 0.0],
                        ..identity_transform()
                    },
                    ..part_node(ClmPart {
                        opacity: c.opacity,
                        tint: c.tint,
                        screen_tint: c.screen_tint,
                        ..quad_part(albedo, CELL_HALF)
                    })
                },
            );
        }
    }
    let doc = ClmStructure {
        nodes,
        ..ClmStructure::default()
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

/// A gradient, so a wrong opacity or tint differs everywhere, not just at edges.
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
/// Model units.
const HALF_W: f32 = 150.0;
const PART_H: f32 = 120.0;
const PULL: [f32; 2] = [60.0, 40.0];
const SEAM_WEIGHTS: [f32; COLS] = [1.0, 0.5, 0.0];

/// Weights 1.0 / 0.5 / 0.0 left to right show all three weld regimes in one
/// render at pull = 1.
fn welded_seam() -> (ClmStructure, Vec<ClmTexture>) {
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
    };
    let deform = ClmBinding {
        params: vec![pid(0)],
        key_positions: vec![vec![0.0, 1.0]],
        node: nid(upper),
        interpolate_mode: InterpolateMode::Linear,
        values: ClmBindingValues::Deform(ClmCells {
            // The rest pose must be an explicit zero cell to interpolate from.
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

    let doc = ClmStructure {
        nodes,
        params: vec![pull],
        bindings: vec![deform],
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
        ..ClmStructure::default()
    };
    (
        doc,
        textures(vec![
            solid_texture([230, 140, 60]),
            solid_texture([50, 160, 170]),
        ]),
    )
}

fn slot_id(i: usize) -> SlotId {
    SlotId::new(format!("s{i}")).expect("a generated slot id")
}

#[derive(Clone, Copy)]
enum Growth {
    Up,
    Down,
}

/// Seam row first, so seam vertices are indices 0..COLS on both parts; the
/// welds reference that. v = 0 at the topmost row.
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
        // Signed distance so row 0 is +0.0 on both parts, not -0.0.
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
        slots: (0..COLS)
            .map(|i| ClmSlot {
                id: slot_id(i),
                vertex: Some(i as u32),
            })
            .collect(),
    }
}

// --- strand_response ----------------------------------------------------------

/// Model pixels. Two columns is the narrowest strip a bend can bow; nine rows
/// let each link's crease ramp across its own third of the art.
const STRAND_HALF_W: f32 = 12.0;
const STRAND_TOP: f32 = 80.0;
const STRAND_BOTTOM: f32 = -80.0;
const STRAND_ROWS: usize = 9;

const STRAND_LINKS: u32 = 3;
/// Model pixels.
const TURN_SHIFT_X: f32 = 120.0;
const TURN: usize = STRAND_LINKS as usize;

fn strand_response() -> (ClmStructure, Vec<ClmTexture>) {
    let mesh = strand_mesh();
    let fit = fit_strand(&mesh, STRAND_LINKS, None).expect("the strip is a fittable strand");

    let mut nodes = Vec::new();
    let root = push(&mut nodes, None, group_node("root"));
    let head = push(&mut nodes, Some(root), group_node("Head"));
    let spine = push(
        &mut nodes,
        Some(head),
        ClmNode {
            name: "Strand spine".into(),
            transform: ClmTransform {
                translation: [
                    fit.root[0] - mesh.origin[0],
                    fit.root[1] - mesh.origin[1],
                    0.0,
                ],
                ..identity_transform()
            },
            kind: ClmNodeKind::Spine(ClmSpine {
                joints: (0..fit.lengths.len())
                    .map(|i| {
                        let s: f32 = fit.lengths[..=i].iter().sum();
                        [fit.axis[0] * s, fit.axis[1] * s]
                    })
                    .collect(),
                targets: (0..fit.lengths.len()).map(|i| Some(pid(i))).collect(),
            }),
            ..blank_node()
        },
    );
    push(
        &mut nodes,
        Some(spine),
        ClmNode {
            name: "Strand".into(),
            transform: ClmTransform {
                translation: [
                    mesh.origin[0] - fit.root[0],
                    mesh.origin[1] - fit.root[1],
                    0.0,
                ],
                ..identity_transform()
            },
            ..part_node(strand_part(&mesh))
        },
    );

    let mut parent = head;
    for (i, period_s) in [0.32, 0.42, 0.54].into_iter().enumerate() {
        let source = if i == 0 {
            ResponseSource::PoseParam { param: pid(TURN) }
        } else {
            ResponseSource::Parent
        };
        parent = push(
            &mut nodes,
            Some(parent),
            ClmNode {
                name: format!("Bend response {}", i + 1),
                kind: ClmNodeKind::MotionResponse(MotionResponse {
                    inputs: vec![ResponseInput {
                        source,
                        gain: if i == 0 { 0.24 } else { 0.85 },
                    }],
                    output_param: Some(pid(i)),
                    period_s,
                    damping_ratio: 0.72,
                }),
                ..blank_node()
            },
        );
    }

    let mut params: Vec<ClmParam> = (0..STRAND_LINKS as usize).map(bend_param).collect();
    params.push(ClmParam {
        id: pid(TURN),
        name: "turn".into(),
        min: -1.0,
        max: 1.0,
        default: 0.0,
    });

    let bindings = vec![ClmBinding {
        params: vec![pid(TURN)],
        key_positions: vec![vec![0.0, 0.5, 1.0]],
        node: nid(head),
        interpolate_mode: InterpolateMode::Linear,
        values: ClmBindingValues::TransformTX(ClmCells {
            cells: vec![
                ClmCell {
                    x: 0,
                    y: 0,
                    value: -TURN_SHIFT_X,
                },
                ClmCell {
                    x: 1,
                    y: 0,
                    value: 0.0,
                },
                ClmCell {
                    x: 2,
                    y: 0,
                    value: TURN_SHIFT_X,
                },
            ],
        }),
    }];

    let doc = ClmStructure {
        nodes,
        params,
        bindings,
        ..ClmStructure::default()
    };
    (doc, textures(vec![strand_texture()]))
}

/// A spine reads a param's value, not its range position, so the range is
/// the bend in half turns.
fn bend_param(link: usize) -> ClmParam {
    ClmParam {
        id: pid(link),
        name: format!("bend {}", link + 1),
        min: -1.0,
        max: 1.0,
        default: 0.0,
    }
}

/// Top row first so the fit's root is the first vertex pair; v = 0 on top.
fn strand_mesh() -> ClmMesh {
    let mut verts = Vec::with_capacity(4 * STRAND_ROWS);
    let mut uvs = Vec::with_capacity(4 * STRAND_ROWS);
    for r in 0..STRAND_ROWS {
        let t = r as f32 / (STRAND_ROWS - 1) as f32;
        let y = STRAND_TOP + (STRAND_BOTTOM - STRAND_TOP) * t;
        verts.extend_from_slice(&[-STRAND_HALF_W, y, STRAND_HALF_W, y]);
        uvs.extend_from_slice(&[0.0, t, 1.0, t]);
    }

    let mut indices = Vec::with_capacity(6 * (STRAND_ROWS - 1));
    for r in 0..STRAND_ROWS - 1 {
        let v = (r * 2) as u16;
        indices.extend_from_slice(&[v, v + 1, v + 3, v, v + 3, v + 2]);
    }

    ClmMesh {
        verts,
        uvs,
        indices: ClmIndices::U16(indices),
        origin: [0.0, 0.0],
    }
}

fn strand_part(mesh: &ClmMesh) -> ClmPart {
    ClmPart {
        mesh: mesh.clone(),
        albedo: Some(tid(0)),
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        tint: [1.0, 1.0, 1.0],
        screen_tint: [0.0, 0.0, 0.0],
        masks: Vec::new(),
        mask_threshold: 0.5,
        slots: Vec::new(),
    }
}

/// Bands, shading and an edge column make the bend's sign and link legible,
/// which a solid colour (indistinguishable from a translation) would not.
fn strand_texture() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(STRAND_TEX_W, STRAND_TEX_H, |x, y| {
        if x < STRAND_TEX_EDGE {
            return image::Rgba([35, 25, 55, 255]);
        }
        let along = y as f32 / (STRAND_TEX_H - 1) as f32;
        let fade = 1.0 - 0.5 * along;
        let band = if (y / STRAND_TEX_BAND).is_multiple_of(2) {
            1.0
        } else {
            0.6
        };
        let shade = |c: f32| (c * fade * band) as u8;
        image::Rgba([shade(240.0), shade(175.0), shade(95.0), 255])
    });
    flat_image(img)
}

/// Texels.
const STRAND_TEX_W: u32 = 32;
const STRAND_TEX_H: u32 = 256;
const STRAND_TEX_BAND: u32 = 32;
const STRAND_TEX_EDGE: u32 = 3;

fn solid_texture(rgb: [u8; 3]) -> Vec<u8> {
    flat_texture([rgb[0], rgb[1], rgb[2], 255])
}

fn flat_texture(rgba: [u8; 4]) -> Vec<u8> {
    flat_image(image::RgbaImage::from_pixel(64, 64, image::Rgba(rgba)))
}

fn flat_image(img: image::RgbaImage) -> Vec<u8> {
    encode_png(&img)
}

/// Fixed settings so regenerating an unchanged fixture is a byte-for-byte no-op.
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

    /// Structure only, never byte equality: a harmless encoder or PNG change
    /// would break that. Texture tables are excluded for the same reason.
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

    #[test]
    fn committed_welded_seam_keeps_its_weld_structure() {
        let committed = committed("welded_seam").doc;

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
