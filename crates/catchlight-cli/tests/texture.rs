#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `replace-texture` on copies of the committed fixtures.
//!
//! The replacement images here are deliberately not decodable — a PNG
//! signature with nothing behind it, a TGA footer with nothing in front of it.
//! Every one of these tests would fail if anything on the path opened the
//! image, which is exactly the property the command promises.

mod common;

use catchlight_cli::diff::diff;
use catchlight_cli::texture;
use catchlight_cli::Error;
use catchlight_core::formats::clm::{TextureAlpha, TextureEncoding};
use catchlight_core::Model;

use common::{copy_fixture, decode, read, tmp};

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const TGA_FOOTER: &[u8] = b"TRUEVISION-XFILE.\0";

#[test]
fn the_bytes_are_swapped_and_nothing_else_moves() {
    let dir = tmp("texture-swap");
    let file = copy_fixture("welded_seam", &dir);
    let base = decode(&file);

    let image = dir.join("new.png");
    let bytes = [PNG_MAGIC.as_slice(), b"a payload no decoder would accept"].concat();
    std::fs::write(&image, &bytes).unwrap();

    let replaced = texture::run(&file, "tex-0", &image, None, None).unwrap();
    assert_eq!(replaced.id, "tex-0");
    assert_eq!(replaced.encoding, TextureEncoding::Png);
    assert_eq!(replaced.alpha, TextureAlpha::Straight, "the slot's own");
    assert_eq!(replaced.before_bytes, 158);
    assert_eq!(replaced.after_bytes, bytes.len());

    let after = decode(&file);
    assert_eq!(after.textures[0].data, bytes);
    assert_eq!(
        after.textures[1].data, base.textures[1].data,
        "the other texture moved"
    );
    let lines = diff(&base, &after);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("~ texture tex-0 data: 158 bytes"),
        "{lines:?}"
    );
    Model::from_clm_bytes(&read(&file)).unwrap();
}

/// The bytes' own signature outranks the file name, and a TGA 1.0 file — which
/// has no signature at all — falls back to the extension.
#[test]
fn the_encoding_comes_from_the_signature_then_the_extension() {
    let dir = tmp("texture-encoding");
    let base = decode(&common::fixture("welded_seam"));

    for (name, bytes, expected) in [
        (
            "misnamed.tga",
            [PNG_MAGIC.as_slice(), b"png bytes"].concat(),
            TextureEncoding::Png,
        ),
        (
            "misnamed.png",
            [b"tga bytes".as_slice(), TGA_FOOTER].concat(),
            TextureEncoding::Tga,
        ),
        (
            "no-signature.tga",
            b"a tga 1.0 file has no magic".to_vec(),
            TextureEncoding::Tga,
        ),
    ] {
        let file = copy_fixture("welded_seam", &dir);
        let image = dir.join(name);
        std::fs::write(&image, &bytes).unwrap();

        let replaced = texture::run(&file, "tex-0", &image, None, None).unwrap();
        assert_eq!(replaced.encoding, expected, "{name}");

        let after = decode(&file);
        assert_eq!(after.textures[0].encoding, expected, "{name}");
        assert_eq!(after.textures[0].data, bytes, "{name}");
        if expected == base.textures[0].encoding {
            assert_eq!(diff(&base, &after).len(), 1, "{name}");
        } else {
            let lines = diff(&base, &after);
            assert_eq!(lines.len(), 2, "{name}: {lines:?}");
            assert!(
                lines.contains(&"~ texture tex-0 encoding: Png -> Tga".to_string()),
                "{name}: {lines:?}"
            );
        }
    }
}

#[test]
fn an_image_of_no_recognisable_encoding_is_refused() {
    let dir = tmp("texture-unknown-encoding");
    let file = copy_fixture("welded_seam", &dir);
    let before = read(&file);

    let image = dir.join("picture.webp");
    std::fs::write(&image, b"RIFF....WEBP").unwrap();

    let error = texture::run(&file, "tex-0", &image, None, None).unwrap_err();
    assert!(
        matches!(error, Error::UnknownImageEncoding { .. }),
        "expected UnknownImageEncoding, got {error}"
    );
    assert!(error.to_string().contains("picture.webp"), "{error}");
    assert_eq!(read(&file), before, "a refused swap wrote to the file");
}

#[test]
fn an_unknown_texture_id_is_refused() {
    let dir = tmp("texture-unknown-id");
    let file = copy_fixture("welded_seam", &dir);
    let before = read(&file);

    let image = dir.join("new.png");
    std::fs::write(&image, [PNG_MAGIC.as_slice(), b"x"].concat()).unwrap();

    let error = texture::run(&file, "tex-9", &image, None, None).unwrap_err();
    let Error::NoSuchId { kind, id, .. } = &error else {
        panic!("expected NoSuchId, got {error}");
    };
    assert_eq!(*kind, "texture");
    assert_eq!(id, "tex-9");
    assert_eq!(read(&file), before);
}

#[test]
fn writing_the_same_bytes_back_rewrites_the_same_file() {
    let dir = tmp("texture-byte-stable");
    let file = copy_fixture("welded_seam", &dir);
    let before = read(&file);

    let image = dir.join("same.png");
    std::fs::write(&image, &decode(&file).textures[0].data).unwrap();

    texture::run(&file, "tex-0", &image, None, None).unwrap();
    assert_eq!(read(&file), before);
}

/// The bytes cannot say what their alpha means, so the slot's convention
/// stands until someone says otherwise.
#[test]
fn the_alpha_convention_is_kept_unless_asked() {
    let dir = tmp("texture-alpha");
    let base = decode(&common::fixture("composite_masks"));
    assert_eq!(base.textures[0].alpha, TextureAlpha::PremultipliedSrgb);

    let image = dir.join("new.png");
    std::fs::write(&image, [PNG_MAGIC.as_slice(), b"payload"].concat()).unwrap();

    let kept = copy_fixture("composite_masks", &dir);
    texture::run(&kept, "tex-0", &image, None, None).unwrap();
    assert_eq!(
        decode(&kept).textures[0].alpha,
        TextureAlpha::PremultipliedSrgb
    );

    let told = dir.join("told.clm");
    texture::run(
        &kept,
        "tex-0",
        &image,
        Some(TextureAlpha::Straight),
        Some(&told),
    )
    .unwrap();
    assert_eq!(decode(&told).textures[0].alpha, TextureAlpha::Straight);
    assert!(diff(&decode(&kept), &decode(&told))
        .contains(&"~ texture tex-0 alpha: PremultipliedSrgb -> Straight".to_string()));
}

#[test]
fn the_binary_reports_the_swap_and_its_failures() {
    let dir = tmp("texture-cli");
    let file = copy_fixture("welded_seam", &dir);
    let image = dir.join("new.png");
    std::fs::write(&image, [PNG_MAGIC.as_slice(), b"payload"].concat()).unwrap();

    let (code, out, err) = common::run(&[
        "replace-texture",
        file.to_str().unwrap(),
        "tex-1",
        image.to_str().unwrap(),
        "--alpha",
        "straight",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("texture \"tex-1\": 158 bytes -> 15 bytes"),
        "{out}"
    );

    let (code, _, err) = common::run(&[
        "replace-texture",
        file.to_str().unwrap(),
        "tex-1",
        dir.join("missing.png").to_str().unwrap(),
    ]);
    assert_eq!(code, 2);
    assert!(err.contains("missing.png"), "{err}");
}
