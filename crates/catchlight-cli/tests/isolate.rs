#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `isolate` end to end: what it hides, what it keeps clipping, and what the
//! straight-alpha PNG holds.
//!
//! Needs a GPU, and like the render suite it fails rather than skips when no
//! adapter is there.

mod common;

use catchlight_core::formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding};
use catchlight_core::{
    MaskMode, Model, ModelNode, ModelNodeKind, ModelPart, ModelTexture, NodeId, SeededHex, TexId,
};
use image::RgbaImage;

/// The fixture's two quads: half-width 60, centres 160 apart. This rect holds
/// both with room to spare, and at scale 0.5 it is 2 world units per pixel.
const RECT: &str = "-160,-80,320,160";
const SCALE: &str = "0.5";
/// Where each quad's centre lands in that frame.
const DRIVEN_PX: (u32, u32) = (40, 40);
const SWEPT_PX: (u32, u32) = (120, 40);
/// The fixture's two texture colours.
const DRIVEN_RGB: [u8; 3] = [210, 90, 70];
const SWEPT_RGB: [u8; 3] = [70, 150, 210];

fn png(path: &std::path::Path) -> RgbaImage {
    image::open(path).expect("the png decodes").to_rgba8()
}

fn at(image: &RgbaImage, (x, y): (u32, u32)) -> [u8; 4] {
    image.get_pixel(x, y).0
}

#[test]
fn only_the_kept_part_is_drawn_and_the_size_follows_the_scale() {
    let dir = common::tmp("isolate-keep");
    let out = dir.join("swept.png");
    let (code, stdout, stderr) = common::run(&[
        "isolate",
        common::fixture("two_param_grid").to_str().unwrap(),
        "--keep",
        "node-2",
        "--rect",
        RECT,
        "--scale",
        SCALE,
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("1 part kept"), "{stdout}");

    let image = png(&out);
    // round(320 * 0.5) x round(160 * 0.5).
    assert_eq!(image.dimensions(), (160, 80));
    assert_eq!(at(&image, DRIVEN_PX)[3], 0, "the part not kept is gone");
    let swept = at(&image, SWEPT_PX);
    assert_eq!(swept[3], 255, "the kept part is opaque");
    assert_eq!(&swept[..3], &SWEPT_RGB, "and carries its own texture");
}

/// The unpremultiply check. `sweep` at its far key fades `driven` to a
/// quarter, and a straight-alpha PNG keeps the colour and drops the alpha —
/// premultiplied it would read as a quarter of the colour, and undoing that
/// in byte space instead of linear would saturate it toward white.
#[test]
fn a_faded_part_keeps_its_colour_and_loses_only_alpha() {
    let dir = common::tmp("isolate-faded");
    let out = dir.join("faded.png");
    let (code, _, stderr) = common::run(&[
        "isolate",
        common::fixture("two_param_grid").to_str().unwrap(),
        "--keep",
        "node-1",
        "--set",
        "param-2=10",
        "--rect",
        RECT,
        "--scale",
        SCALE,
        "--out",
        out.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");

    let pixel = at(&png(&out), DRIVEN_PX);
    // 0.25 opacity, so alpha 64 of 255.
    assert!(
        pixel[3].abs_diff(64) <= 1,
        "alpha should be a quarter: {pixel:?}"
    );
    for (channel, want) in pixel[..3].iter().zip(&DRIVEN_RGB) {
        assert!(
            channel.abs_diff(*want) <= 2,
            "straight alpha keeps the texture colour: {pixel:?} against {DRIVEN_RGB:?}"
        );
    }
}

#[test]
fn an_id_that_names_no_part_is_refused() {
    let dir = common::tmp("isolate-unknown");
    for (args, wanted) in [
        (vec!["--keep", "node-404"], "node-404"),
        // A group is not a Part, so keeping one is the same refusal.
        (vec!["--keep", "root"], "root"),
    ] {
        let (code, _, stderr) = run_with(&dir, &args);
        assert_eq!(code, 2, "an error exits 2");
        assert!(stderr.contains(wanted), "{stderr}");
    }

    let (code, _, stderr) = run_with(&dir, &["--keep", "node-1", "--set", "param-404=1"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("param-404"), "{stderr}");
}

fn run_with(dir: &std::path::Path, extra: &[&str]) -> (i32, String, String) {
    let out = dir.join("out.png");
    let model = common::fixture("two_param_grid");
    let mut args = vec!["isolate", model.to_str().unwrap()];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--rect", RECT, "--out", out.to_str().unwrap()]);
    common::run(&args)
}

/// A mask source that is not kept is disabled, and a disabled source still
/// stencils — which is the whole reason `--strip-masks` exists. The kept part
/// is half covered by such a source, so its uncovered half is missing until
/// the mask comes off.
#[test]
fn a_disabled_mask_source_still_clips_until_its_mask_is_stripped() {
    let dir = common::tmp("isolate-masks");
    let model = dir.join("masked.clm");
    let (kept, masker) = write_masked_model(&model);

    // World -40..40 across 80 pixels: the covered half and the bare half.
    let covered = (20, 40);
    let bare = (60, 40);

    let with_mask = dir.join("with-mask.png");
    let (code, _, stderr) = common::run(&[
        "isolate",
        model.to_str().unwrap(),
        "--keep",
        kept.as_str(),
        "--rect",
        "-40,-40,80,80",
        "--out",
        with_mask.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let image = png(&with_mask);
    assert_eq!(image.dimensions(), (80, 80));
    assert_eq!(at(&image, covered)[3], 255, "the masked-in half draws");
    assert_eq!(
        at(&image, bare)[3],
        0,
        "the half outside the mask is clipped away even though the source is disabled"
    );

    let stripped = dir.join("stripped.png");
    let (code, stdout, stderr) = common::run(&[
        "isolate",
        model.to_str().unwrap(),
        "--keep",
        kept.as_str(),
        "--strip-masks",
        masker.as_str(),
        "--rect",
        "-40,-40,80,80",
        "--out",
        stripped.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("1 mask stripped"), "{stdout}");
    let image = png(&stripped);
    assert_eq!(at(&image, covered)[3], 255);
    assert_eq!(
        at(&image, bare)[3],
        255,
        "the whole part draws once stripped"
    );

    // An Id that masks nothing is not an error: a script lists a whole rig's
    // sources without working out which ones bite.
    let (code, stdout, stderr) = common::run(&[
        "isolate",
        model.to_str().unwrap(),
        "--keep",
        kept.as_str(),
        "--strip-masks",
        "node-404",
        "--rect",
        "-40,-40,80,80",
        "--out",
        dir.join("nothing.png").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("0 masks stripped"), "{stdout}");
}

/// Two quads: one 80 wide on the origin, and a 40-wide one over its left
/// half that masks it. Returns `(kept, masker)`.
fn write_masked_model(path: &std::path::Path) -> (NodeId, NodeId) {
    let mut hex = SeededHex::new(11);
    let mut model = Model::new();
    let root = model.root().expect("a fresh model has a root").clone();

    let kept = model
        .add_node(
            &root,
            ModelNode::new("kept", ModelNodeKind::Part(ModelPart::new(quad(40.0, 0.0)))),
            &mut hex,
        )
        .expect("add the kept part");
    let masker = model
        .add_node(
            &root,
            ModelNode::new(
                "masker",
                ModelNodeKind::Part(ModelPart::new(quad(20.0, -20.0))),
            ),
            &mut hex,
        )
        .expect("add the mask source");

    for (node, id) in [(&kept, "tex-0"), (&masker, "tex-1")] {
        model
            .add_texture_with_id(
                TexId::new(id).expect("a texture id"),
                node,
                solid_texture([200, 120, 60, 255]),
            )
            .expect("add a texture");
    }
    model
        .mask_add(&kept, &masker, MaskMode::Mask)
        .expect("mask the kept part");

    std::fs::write(path, model.to_clm_bytes().expect("encode")).expect("write the model");
    (kept, masker)
}

/// A quad `half` wide and tall, centred at `x`.
fn quad(half: f32, x: f32) -> ClmMesh {
    ClmMesh {
        verts: vec![
            x - half,
            -half,
            x + half,
            -half,
            x + half,
            half,
            x - half,
            half,
        ],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    }
}

fn solid_texture(rgba: [u8; 4]) -> ModelTexture {
    let image = RgbaImage::from_pixel(8, 8, image::Rgba(rgba));
    let mut data = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut data, image::ImageFormat::Png)
        .expect("encode png");
    ModelTexture {
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data: data.into_inner().into(),
    }
}
