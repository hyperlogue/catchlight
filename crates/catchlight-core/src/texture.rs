//! A model's textures, from stored bytes to renderer-ready images.
//!
//! Crops use the *opaque* bbox rather than the UV bbox: on imported models the UV
//! bbox is mostly transparent texels.

use std::sync::Arc;

use crate::components::{DecodedTexture, srgb_encode_to_byte};
use crate::load_budget::{LoadBudget, LoadLimitError};

#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum TextureError {
    #[error("texture decode failed: {0}")]
    Decode(#[source] Arc<image::ImageError>),
    #[error(transparent)]
    Limit(#[from] LoadLimitError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Png,
    Tga,
}

impl TextureFormat {
    pub fn to_image_format(self) -> image::ImageFormat {
        match self {
            Self::Png => image::ImageFormat::Png,
            Self::Tga => image::ImageFormat::Tga,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodedTexture {
    pub format: TextureFormat,
    pub data: Arc<[u8]>,
    /// `true` for premultiplied-in-sRGB on-disk bytes (every `.inx` texture);
    /// `false` for editor-authored straight alpha.
    pub premultiplied: bool,
}

impl EncodedTexture {
    /// From the image header alone, without a pixel decode.
    pub fn dimensions(&self) -> Result<(u32, u32), image::ImageError> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&self.data[..]));
        reader.set_format(self.format.to_image_format());
        reader.into_dimensions()
    }

    /// Output encodes premultiplied *linear* color, for upload as `Rgba8UnormSrgb`.
    /// Premultiplied storage makes alpha-edge bilinear filtering correct without an
    /// alpha-bleed pass.
    pub fn decode(&self) -> Result<DecodedTexture, image::ImageError> {
        let img = image::load_from_memory_with_format(&self.data, self.format.to_image_format())?;
        let (width, height) = (img.width(), img.height());
        let mut rgba = img.into_rgba8().into_raw();
        if self.premultiplied {
            premultiplied_srgb_to_premultiplied_linear_inplace(&mut rgba);
        } else {
            premultiply_linear_into_srgb_inplace(&mut rgba);
        }
        Ok(DecodedTexture {
            width,
            height,
            rgba: rgba.into(),
        })
    }
}

/// Indexed `alpha * 256 + channel`.
static PREMULTIPLY_SRGB_LUT: std::sync::OnceLock<Box<[u8; 65536]>> = std::sync::OnceLock::new();

fn premultiply_srgb_lut() -> &'static [u8; 65536] {
    PREMULTIPLY_SRGB_LUT.get_or_init(|| {
        let decode = crate::components::srgb_decode_table();
        let mut table = Box::new([0u8; 65536]);
        for a in 1..=255usize {
            let inv = 255.0 / a as f32;
            let af = a as f32 / 255.0;
            for c in 0..256usize {
                table[a * 256 + c] = if a == 255 {
                    c as u8
                } else {
                    let straight = (c as f32 * inv).round().min(255.0) as u8;
                    srgb_encode_to_byte(decode[straight as usize] * af)
                };
            }
        }
        table
    })
}

/// Same bytes as [`unpremultiply_srgb_inplace`] then
/// [`premultiply_linear_into_srgb_inplace`], tabulated: each output depends only
/// on its channel and alpha byte.
pub fn premultiplied_srgb_to_premultiplied_linear_inplace(rgba: &mut [u8]) {
    debug_assert_eq!(rgba.len() % 4, 0, "rgba buffer must be a multiple of 4");
    let lut = premultiply_srgb_lut();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3] as usize;
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        if a == 255 {
            continue;
        }
        let row = &lut[a * 256..a * 256 + 256];
        for c in &mut px[..3] {
            *c = row[*c as usize];
        }
    }
}

/// `straight = round((rgb * 255) / α)`, in f32 so partial alpha keeps precision.
/// α=0 pixels stay `(0, 0, 0, 0)`.
pub fn unpremultiply_srgb_inplace(rgba: &mut [u8]) {
    debug_assert_eq!(rgba.len() % 4, 0, "rgba buffer must be a multiple of 4");
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        if a == 255 {
            continue;
        }
        let inv = 255.0 / f32::from(a);
        for c in &mut px[..3] {
            *c = (f32::from(*c) * inv).round().min(255.0) as u8;
        }
    }
}

/// Straight-alpha sRGB to bytes encoding premultiplied linear color. Alpha is unchanged.
pub fn premultiply_linear_into_srgb_inplace(rgba: &mut [u8]) {
    debug_assert_eq!(rgba.len() % 4, 0, "rgba buffer must be a multiple of 4");
    let decode = crate::components::srgb_decode_table();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        if a == 255 {
            continue;
        }
        let af = f32::from(a) / 255.0;
        for c in &mut px[..3] {
            let premul = decode[*c as usize] * af;
            *c = srgb_encode_to_byte(premul);
        }
    }
}

/// Inverse of [`premultiply_linear_into_srgb_inplace`], clamped at white.
///
/// This is the inverse a framebuffer readback needs, not
/// [`unpremultiply_srgb_inplace`]: an `Rgba8UnormSrgb` target stores
/// `srgb_encode(linear × α)`, so the divide must happen in linear. The byte-space
/// one saturates partly transparent readback pixels toward white.
pub fn unpremultiply_linear_from_srgb_inplace(rgba: &mut [u8]) {
    debug_assert_eq!(rgba.len() % 4, 0, "rgba buffer must be a multiple of 4");
    let decode = crate::components::srgb_decode_table();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        if a == 255 {
            continue;
        }
        let inv = 255.0 / f32::from(a);
        for c in &mut px[..3] {
            *c = srgb_encode_to_byte((decode[*c as usize] * inv).min(1.0));
        }
    }
}

/// Transparent skirt around the opaque bbox, in texels: 16 = 2^4 feeds mip
/// levels 0..=4. Baked rather than left to the sampler because the web backends
/// have no border sampler and the shader's stand-in is exact only at mip 0.
const MARGIN: i64 = 16;

/// Multiples of 2^4, so mip 0..=4 box-filter footprints tile inside the crop
/// instead of straddling its edge.
const ALIGN: i64 = 16;

#[derive(Clone, Copy)]
struct Plan {
    src_w: u32,
    src_h: u32,
    /// Source-texel coords; the origin may be negative and the far edge past the
    /// source. `None` when fully transparent (1x1 stand-in) or uncropped.
    crop: Option<(i64, i64, u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UvCrop {
    src_w: u32,
    src_h: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
}

impl UvCrop {
    /// In f64: f32 loses a texel of precision at the far edge of a 4k texture.
    pub fn map(&self, uv: crate::Vec2) -> crate::Vec2 {
        let (tw, th) = (f64::from(self.src_w), f64::from(self.src_h));
        crate::Vec2::new(
            ((f64::from(uv.x) * tw - self.x as f64) / f64::from(self.w)) as f32,
            ((f64::from(uv.y) * th - self.y as f64) / f64::from(self.h)) as f32,
        )
    }
}

#[derive(Debug, Clone)]
pub struct PreppedTexture {
    pub texture: DecodedTexture,
    pub uv_crop: Option<UvCrop>,
    /// Header size before halving or crop; what the budget was charged for it.
    pub source_size: [u32; 2],
}

/// The returned table is 1:1 with `textures`, so albedo ids index it unchanged.
///
/// Every header is charged to `budget` as full-size RGBA8 before any pixel is
/// decoded, since a few kilobytes of PNG can state any size.
///
/// `max_dimension` is the device's 2D texture limit. On an axis where the skirt
/// would exceed it, the crop ends at the source instead; an image larger than
/// `max_dimension` on its own is the caller's to refuse.
pub fn prepare_textures(
    textures: &[EncodedTexture],
    texture_halvings: u32,
    max_dimension: u32,
    budget: &mut LoadBudget,
) -> Result<Vec<PreppedTexture>, TextureError> {
    let mut sizes = Vec::with_capacity(textures.len());
    for tex in textures {
        let (width, height) = tex
            .dimensions()
            .map_err(|e| TextureError::Decode(Arc::new(e)))?;
        budget.check_texture_dimensions(width, height)?;
        sizes.push([width, height]);
    }

    let prepped = prep_textures(textures, texture_halvings, max_dimension)?;
    Ok(prepped
        .into_iter()
        .zip(sizes)
        .map(|((texture, plan), source_size)| PreppedTexture {
            texture,
            uv_crop: plan.crop.map(|(x, y, w, h)| UvCrop {
                src_w: plan.src_w,
                src_h: plan.src_h,
                x,
                y,
                w,
                h,
            }),
            source_size,
        })
        .collect())
}

/// Native builds decode in parallel. wasm stays sequential: the wgpu web
/// backend is not thread-safe.
fn prep_textures(
    textures: &[EncodedTexture],
    texture_halvings: u32,
    max_dimension: u32,
) -> Result<Vec<(DecodedTexture, Plan)>, TextureError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        textures
            .par_iter()
            .map(|tex| prep_one(tex, texture_halvings, max_dimension))
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        textures
            .iter()
            .map(|tex| prep_one(tex, texture_halvings, max_dimension))
            .collect()
    }
}

fn prep_one(
    tex: &EncodedTexture,
    texture_halvings: u32,
    max_dimension: u32,
) -> Result<(DecodedTexture, Plan), TextureError> {
    let decoded = decode_halved(tex, texture_halvings)?;
    let (src_w, src_h) = (decoded.width, decoded.height);
    Ok(match alpha_crop_rect(&decoded, max_dimension) {
        Some((0, 0, w, h)) if (w, h) == (src_w, src_h) => (
            decoded,
            Plan {
                src_w,
                src_h,
                crop: None,
            },
        ),
        Some((x, y, w, h)) => (
            crop(&decoded, x, y, w, h),
            Plan {
                src_w,
                src_h,
                crop: Some((x, y, w, h)),
            },
        ),
        None => (
            DecodedTexture {
                width: 1,
                height: 1,
                rgba: vec![0u8; 4].into(),
            },
            Plan {
                src_w,
                src_h,
                crop: None,
            },
        ),
    })
}

fn alpha_crop_rect(tex: &DecodedTexture, max_dimension: u32) -> Option<(i64, i64, u32, u32)> {
    let (w, h) = (tex.width as usize, tex.height as usize);
    let (mut minx, mut miny, mut maxx, mut maxy) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut any = false;
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            if tex.rgba[(row + x) * 4 + 3] > 0 {
                any = true;
                minx = minx.min(x);
                miny = miny.min(y);
                maxx = maxx.max(x);
                maxy = maxy.max(y);
            }
        }
    }
    if !any {
        return None;
    }
    // Where the skirted span exceeds the caller's limit, stop at the source:
    // the near edge at 0 and the far edge rounded *up* to [`ALIGN`], since an
    // odd-sized crop has no exact box-filtered mip chain.
    let span = |min: usize, max: usize, size: usize| {
        let lo = align_down(min as i64 - MARGIN);
        let hi = align_up(max as i64 + 1 + MARGIN);
        if hi - lo <= i64::from(max_dimension) {
            (lo, hi)
        } else {
            (lo.max(0), hi.min(align_up(size as i64)))
        }
    };
    let (x0, x1) = span(minx, maxx, w);
    let (y0, y1) = span(miny, maxy, h);
    Some((x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// Out-of-source texels stay at the transparent zero-fill (`ClampToBorder`).
fn crop(tex: &DecodedTexture, x: i64, y: i64, w: u32, h: u32) -> DecodedTexture {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    blit(&mut rgba, w, tex, [0, 0], [x, y], [w, h]);
    DecodedTexture {
        width: w,
        height: h,
        rgba: rgba.into(),
    }
}

fn align_down(v: i64) -> i64 {
    v.div_euclid(ALIGN) * ALIGN
}

fn align_up(v: i64) -> i64 {
    (v + ALIGN - 1).div_euclid(ALIGN) * ALIGN
}

fn decode_halved(tex: &EncodedTexture, halvings: u32) -> Result<DecodedTexture, TextureError> {
    let mut decoded = tex
        .decode()
        .map_err(|e| TextureError::Decode(Arc::new(e)))?;
    for _ in 0..halvings {
        decoded = decoded.halved();
    }
    Ok(decoded)
}

fn blit(
    out: &mut [u8],
    out_w: u32,
    tex: &DecodedTexture,
    dst: [u32; 2],
    src: [i64; 2],
    size: [u32; 2],
) {
    let [dst_x, dst_y] = dst;
    let [src_x, src_y] = src;
    let [w, h] = size;
    let (tw, th) = (i64::from(tex.width), i64::from(tex.height));
    for row in 0..i64::from(h) {
        let sy = src_y + row;
        if sy < 0 || sy >= th {
            continue;
        }
        let cx0 = src_x.max(0);
        let cx1 = (src_x + i64::from(w)).min(tw);
        if cx0 >= cx1 {
            continue;
        }
        let src_off = ((sy * tw + cx0) * 4) as usize;
        let src_len = ((cx1 - cx0) * 4) as usize;
        let dst_row = i64::from(dst_y) + row;
        let dst_col = i64::from(dst_x) + (cx0 - src_x);
        let dst_off = ((dst_row * i64::from(out_w) + dst_col) * 4) as usize;
        out[dst_off..dst_off + src_len].copy_from_slice(&tex.rgba[src_off..src_off + src_len]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOMY: u32 = u32::MAX;

    fn encoded_png(rgba: [u8; 4], premultiplied: bool) -> EncodedTexture {
        let image = image::RgbaImage::from_raw(1, 1, rgba.to_vec()).unwrap();
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        EncodedTexture {
            format: TextureFormat::Png,
            data: data.into_inner().into(),
            premultiplied,
        }
    }

    fn tex(w: u32, h: u32, opaque: &[(u32, u32)]) -> DecodedTexture {
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        for &(x, y) in opaque {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[200, 100, 50, 255]);
        }
        DecodedTexture {
            width: w,
            height: h,
            rgba: rgba.into(),
        }
    }

    #[test]
    fn align_helpers_round_toward_multiples_of_16() {
        assert_eq!(align_down(-1), -16);
        assert_eq!(align_down(0), 0);
        assert_eq!(align_down(31), 16);
        assert_eq!(align_up(1), 16);
        assert_eq!(align_up(-1), 0);
        assert_eq!(align_up(16), 16);
    }

    #[test]
    fn blit_clips_to_source_and_preserves_bytes() {
        const OUT: u32 = 128;
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for (i, b) in rgba.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let source = DecodedTexture {
            width: 4,
            height: 4,
            rgba: rgba.clone().into(),
        };
        let mut out = vec![0u8; (OUT * OUT * 4) as usize];
        blit(&mut out, OUT, &source, [32, 48], [-16, -16], [48, 48]);
        for y in 0..4i64 {
            for x in 0..4i64 {
                let src = ((y * 4 + x) * 4) as usize;
                let dst = (((48 + 16 + y) * i64::from(OUT) + 32 + 16 + x) * 4) as usize;
                assert_eq!(&out[dst..dst + 4], &rgba[src..src + 4]);
            }
        }
        let rim = ((48 * i64::from(OUT) + 32) * 4) as usize;
        assert_eq!(&out[rim..rim + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn fully_transparent_has_no_crop() {
        assert_eq!(alpha_crop_rect(&tex(64, 64, &[]), ROOMY), None);
    }

    #[test]
    fn the_decoders_view_of_a_model_texture_shares_its_bytes() {
        let model_texture = crate::ModelTexture {
            encoding: crate::formats::clm::TextureEncoding::Png,
            alpha: crate::formats::clm::TextureAlpha::Straight,
            data: vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4].into(),
        };
        let encoded = EncodedTexture::from(&model_texture);
        assert!(
            Arc::ptr_eq(&model_texture.data, &encoded.data),
            "the decoder's view copied the payload instead of sharing it",
        );
    }

    #[test]
    fn alpha_convention_and_format_decide_the_decode() {
        let budget = &mut LoadBudget::default();
        let straight = encoded_png([64, 0, 0, 128], false);
        let straight_result =
            prepare_textures(std::slice::from_ref(&straight), 0, ROOMY, budget).unwrap();

        let mut premultiplied = straight.clone();
        premultiplied.premultiplied = true;
        let premultiplied_result = prepare_textures(&[premultiplied], 0, ROOMY, budget).unwrap();
        assert_ne!(
            straight_result[0].texture.rgba,
            premultiplied_result[0].texture.rgba
        );

        let mut wrong_format = straight;
        wrong_format.format = TextureFormat::Tga;
        assert!(matches!(
            prepare_textures(&[wrong_format], 0, ROOMY, budget),
            Err(TextureError::Decode(_))
        ));
    }

    /// Header states `width` x `height`; the image data is not a deflate stream,
    /// so a decode fails while `dimensions` succeeds.
    fn png_stating(width: u32, height: u32) -> EncodedTexture {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            let mut crc = u32::MAX;
            for &byte in &out[start..] {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
                }
            }
            out.extend_from_slice(&(!crc).to_be_bytes());
        }
        let mut data = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut data, b"IHDR", &header);
        chunk(&mut data, b"IDAT", &[0xFF; 16]);
        chunk(&mut data, b"IEND", &[]);
        EncodedTexture {
            format: TextureFormat::Png,
            data: data.into(),
            premultiplied: false,
        }
    }

    fn budget_of(decoded_texture_bytes: u64) -> LoadBudget {
        LoadBudget::new(crate::load_budget::LoadLimits {
            decoded_texture_bytes,
            ..Default::default()
        })
    }

    #[test]
    fn a_texture_over_the_dimension_cap_is_refused_from_its_header() {
        use crate::load_budget::MAX_TEXTURE_DIMENSION;

        for (width, height) in [
            (MAX_TEXTURE_DIMENSION + 1, 1),
            (1, MAX_TEXTURE_DIMENSION + 1),
        ] {
            let err = prepare_textures(
                &[png_stating(width, height)],
                0,
                ROOMY,
                &mut budget_of(u64::MAX),
            )
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    TextureError::Limit(LoadLimitError {
                        resource: crate::LoadResource::TextureDimension,
                        ..
                    })
                ),
                "{width}x{height}: {err:?}"
            );
        }
    }

    #[test]
    fn the_decoded_budget_is_aggregate_and_charged_before_any_decode() {
        let textures = [png_stating(64, 64), png_stating(64, 64)];
        let both = 2 * 64 * 64 * 4;

        let err = prepare_textures(&textures, 0, ROOMY, &mut budget_of(both - 1)).unwrap_err();
        assert!(
            matches!(
                err,
                TextureError::Limit(LoadLimitError {
                    resource: crate::LoadResource::DecodedTextureBytes,
                    limit,
                    got,
                }) if limit == both - 1 && got == both
            ),
            "{err:?}"
        );
        let err = prepare_textures(&textures, 3, ROOMY, &mut budget_of(both - 1)).unwrap_err();
        assert!(matches!(err, TextureError::Limit(_)), "{err:?}");

        let err = prepare_textures(&textures, 0, ROOMY, &mut budget_of(both)).unwrap_err();
        assert!(matches!(err, TextureError::Decode(_)), "{err:?}");
    }

    #[test]
    fn a_budget_carries_across_calls() {
        let one = encoded_png([255, 0, 0, 255], false);
        let mut budget = budget_of(4);
        let prepped = prepare_textures(std::slice::from_ref(&one), 0, ROOMY, &mut budget).unwrap();
        assert_eq!(prepped[0].source_size, [1, 1]);
        assert!(matches!(
            prepare_textures(&[one], 0, ROOMY, &mut budget),
            Err(TextureError::Limit(_))
        ));
    }

    #[test]
    fn crop_rect_brackets_opaque_with_aligned_margin() {
        let (x, y, w, h) = alpha_crop_rect(&tex(256, 256, &[(40, 50)]), ROOMY).unwrap();
        assert_eq!((x, y, w, h), (16, 32, 48, 48));
        assert!(x < 40 && (x + i64::from(w)) > 40);
        assert!(y < 50 && (y + i64::from(h)) > 50);
    }

    #[test]
    fn the_skirt_runs_past_the_source() {
        let corners = tex(96, 64, &[(0, 0), (95, 63)]);
        assert_eq!(alpha_crop_rect(&corners, ROOMY), Some((-16, -16, 128, 96)));

        let left = tex(256, 256, &[(3, 120)]);
        assert_eq!(alpha_crop_rect(&left, ROOMY), Some((-16, 96, 48, 48)));
    }

    #[test]
    fn the_skirt_gives_way_where_the_crop_would_not_fit() {
        let wide = tex(96, 1, &[(0, 0), (95, 0)]);
        assert_eq!(alpha_crop_rect(&wide, 96), Some((0, -16, 96, 48)));
        assert_eq!(alpha_crop_rect(&wide, 127), Some((0, -16, 96, 48)));
        assert_eq!(alpha_crop_rect(&wide, 128), Some((-16, -16, 128, 48)));

        let odd = tex(250, 100, &[(0, 0), (249, 99)]);
        let (x, y, w, h) = alpha_crop_rect(&odd, 256).unwrap();
        assert_eq!((x, y, w, h), (0, -16, 256, 144));
        let cropped = crop(&odd, x, y, w, h);
        let at = |cx: u32, cy: u32| {
            let i = ((cy * w + cx) * 4) as usize;
            [
                cropped.rgba[i],
                cropped.rgba[i + 1],
                cropped.rgba[i + 2],
                cropped.rgba[i + 3],
            ]
        };
        assert_eq!(at(249, 115), [200, 100, 50, 255], "the opaque texel");
        assert_eq!(at(250, 115), [0; 4], "past the source's right edge");
    }

    fn opaque_png(width: u32, height: u32) -> EncodedTexture {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([9, 8, 7, 255]));
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        EncodedTexture {
            format: TextureFormat::Png,
            data: data.into_inner().into(),
            premultiplied: false,
        }
    }

    #[test]
    fn an_opaque_texture_at_the_limit_is_prepared_at_its_own_size() {
        let source = opaque_png(48, 32);

        let budget = &mut LoadBudget::default();
        let prepped = prepare_textures(std::slice::from_ref(&source), 0, 48, budget).unwrap();
        assert_eq!(
            (prepped[0].texture.width, prepped[0].texture.height),
            (48, 32)
        );
        assert_eq!(prepped[0].uv_crop, None);
        assert!(
            prepped[0]
                .texture
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|px| *px == [9, 8, 7, 255])
        );

        let prepped = prepare_textures(&[source], 0, ROOMY, budget).unwrap();
        assert_eq!(
            (prepped[0].texture.width, prepped[0].texture.height),
            (80, 64)
        );
        assert!(prepped[0].uv_crop.is_some());
        assert_eq!(&prepped[0].texture.rgba[0..4], &[0; 4], "the skirt");
    }

    #[test]
    fn crop_copies_opaque_texel_and_zeroes_the_rest() {
        let source = tex(256, 256, &[(40, 50)]);
        let (x, y, w, h) = alpha_crop_rect(&source, ROOMY).unwrap();
        let cropped = crop(&source, x, y, w, h);
        assert_eq!((cropped.width, cropped.height), (w, h));
        let (lx, ly) = ((40 - x) as u32, (50 - y) as u32);
        let i = ((ly * w + lx) * 4) as usize;
        assert_eq!(&cropped.rgba[i..i + 4], &[200, 100, 50, 255]);
        assert_eq!(&cropped.rgba[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn remap_keeps_opaque_uvs_inside_unit_square() {
        let source = tex(256, 256, &[(140, 150)]);
        let (cx, cy, cw, ch) = alpha_crop_rect(&source, ROOMY).unwrap();
        let remap = |u: f64, v: f64| {
            (
                ((u * 256.0 - cx as f64) / f64::from(cw)) as f32,
                ((v * 256.0 - cy as f64) / f64::from(ch)) as f32,
            )
        };
        let (ox, oy) = remap(140.5 / 256.0, 150.5 / 256.0);
        assert!((0.0..=1.0).contains(&ox) && (0.0..=1.0).contains(&oy));
        let (gx, gy) = remap(0.0, 0.0);
        assert!(gx < 0.0 && gy < 0.0);
    }

    #[test]
    fn the_uv_remap_lands_on_the_same_texel_of_the_crop() {
        use crate::Vec2;

        let mut image = image::RgbaImage::new(64, 64);
        image.put_pixel(40, 33, image::Rgba([10, 20, 30, 255]));
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        let source = EncodedTexture {
            format: TextureFormat::Png,
            data: data.into_inner().into(),
            premultiplied: false,
        };

        let prepped = prepare_textures(
            std::slice::from_ref(&source),
            0,
            ROOMY,
            &mut LoadBudget::default(),
        )
        .unwrap();
        assert_eq!(prepped.len(), 1);
        assert_eq!(prepped[0].source_size, [64, 64]);
        let crop = prepped[0].uv_crop.expect("an opaque texel means a crop");
        let (cx, cy, cw, ch) = alpha_crop_rect(&decode_halved(&source, 0).unwrap(), ROOMY)
            .expect("the same crop the prep found");
        assert_eq!(
            (prepped[0].texture.width, prepped[0].texture.height),
            (cw, ch)
        );

        let by_hand = |uv: Vec2| {
            Vec2::new(
                ((f64::from(uv.x) * 64.0 - cx as f64) / f64::from(cw)) as f32,
                ((f64::from(uv.y) * 64.0 - cy as f64) / f64::from(ch)) as f32,
            )
        };
        for uv in [
            Vec2::new(0.0, 0.0),
            Vec2::new(40.5 / 64.0, 33.5 / 64.0),
            Vec2::new(1.0, 1.0),
        ] {
            assert_eq!(crop.map(uv), by_hand(uv), "uv {uv:?}");
        }

        let inside = crop.map(Vec2::new(40.5 / 64.0, 33.5 / 64.0));
        assert!((0.0..=1.0).contains(&inside.x) && (0.0..=1.0).contains(&inside.y));
        let corner = crop.map(Vec2::ZERO);
        assert!(corner.x < 0.0 && corner.y < 0.0);
    }

    #[test]
    fn fused_premultiply_matches_the_two_pass_composition() {
        // Exhaustive: each output depends only on its own byte and the pixel's alpha.
        let mut fused: Vec<u8> = Vec::with_capacity(256 * 256 * 4);
        for a in 0..256usize {
            for c in 0..256usize {
                fused.extend_from_slice(&[c as u8, c as u8, c as u8, a as u8]);
            }
        }
        let mut two_pass = fused.clone();
        unpremultiply_srgb_inplace(&mut two_pass);
        premultiply_linear_into_srgb_inplace(&mut two_pass);
        premultiplied_srgb_to_premultiplied_linear_inplace(&mut fused);

        for (i, (f, t)) in fused
            .as_chunks::<4>()
            .0
            .iter()
            .zip(two_pass.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(f, t, "channel {} alpha {}", i % 256, i / 256);
        }
    }

    /// Not at every alpha: at alpha 1 dividing back multiplies quantisation
    /// error by 255, so the bound is what the encoding can promise.
    #[test]
    fn unpremultiply_linear_recovers_what_premultiply_linear_encoded() {
        for alpha in [40u8, 64, 128, 200, 254, 255] {
            let mut px = vec![210, 90, 70, alpha, 0, 255, 128, alpha];
            let straight = px.clone();
            premultiply_linear_into_srgb_inplace(&mut px);
            unpremultiply_linear_from_srgb_inplace(&mut px);
            for (i, (got, want)) in px.iter().zip(&straight).enumerate() {
                assert!(
                    got.abs_diff(*want) <= 2,
                    "alpha {alpha}, byte {i}: {got} is not {want}"
                );
            }
        }
    }

    #[test]
    fn the_two_unpremultiplies_disagree_and_only_one_fits_a_readback() {
        let mut target = vec![210, 90, 70, 255];
        target[3] = 64;
        premultiply_linear_into_srgb_inplace(&mut target);

        let mut linear = target.clone();
        unpremultiply_linear_from_srgb_inplace(&mut linear);
        assert!(
            linear[0].abs_diff(210) <= 1 && linear[1].abs_diff(90) <= 1,
            "the linear inverse recovers the texture colour: {linear:?}"
        );

        let mut bytes = target.clone();
        unpremultiply_srgb_inplace(&mut bytes);
        assert_eq!(bytes[0], 255, "the byte-space inverse saturates instead");
    }

    #[test]
    fn unpremultiply_srgb_recovers_straight_alpha() {
        let mut px = [
            200, 100, 50, 255, // fully opaque: untouched
            123, 234, 89, 0, // α=0: rgb zeroed
            64, 64, 64, 128, // α≈0.5: premul 0.5 byte → straight ≈ 0.5 sRGB = 128
            10, 5, 2, 20, // α≈8%: premul 10/20 → straight ≈ 0.5 sRGB = 128
        ];
        unpremultiply_srgb_inplace(&mut px);
        assert_eq!(&px[0..4], &[200, 100, 50, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
        assert!((i32::from(px[4 + 4]) - 127).abs() <= 1);
        assert!((i32::from(px[5 + 4]) - 127).abs() <= 1);
        assert!((i32::from(px[6 + 4]) - 127).abs() <= 1);
        assert_eq!(px[7 + 4], 128);
        assert!((i32::from(px[12]) - 128).abs() <= 1);
        assert!((i32::from(px[13]) - 64).abs() <= 1);
        assert!((i32::from(px[14]) - 26).abs() <= 1);
        assert_eq!(px[15], 20);
    }

    #[test]
    fn unpremultiply_srgb_clamps_noise() {
        let mut px = [200u8, 100, 50, 10];
        unpremultiply_srgb_inplace(&mut px);
        assert_eq!(px, [255, 255, 255, 10]);
    }

    #[test]
    fn premultiply_linear_zeros_transparent_keeps_opaque_unchanged() {
        let mut px = [
            200, 100, 50, 255, // opaque: untouched (premultiply by 1)
            200, 100, 50, 0, // α=0: rgb zeroed
        ];
        premultiply_linear_into_srgb_inplace(&mut px);
        assert_eq!(&px[0..4], &[200, 100, 50, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn premultiply_linear_half_alpha_matches_linear_premul_then_encode() {
        // Linear premul gives ≈138; byte-space premul would give 94.
        let mut px = [188u8, 188, 188, 128];
        premultiply_linear_into_srgb_inplace(&mut px);
        for (c, &v) in px[..3].iter().enumerate() {
            assert!(
                (i32::from(v) - 138).abs() <= 1,
                "channel {c} = {v}, expected ~138",
            );
        }
        assert_eq!(px[3], 128, "alpha unchanged");
    }
}
