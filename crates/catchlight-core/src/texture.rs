//! A model's textures, from the bytes it stores to the images a renderer
//! uploads.
//!
//! A [`Model`](crate::Model) keeps every texture exactly as its author
//! supplied it — encoded, never re-encoded — so something has to decode them,
//! and that is [`prepare_textures`]. It is the whole texture strategy in one
//! entry point: decode, convert into the canonical premultiplied-linear
//! encoding, and crop each image to the bounding box of its *opaque* texels
//! (plus a transparent mip skirt), handing back a [`UvCrop`] per texture for
//! whoever owns the meshes to apply. The render cache is what calls it; the
//! table it returns stays 1:1 with the model's texture order, so albedo slots
//! never move and only part UVs are rewritten.
//!
//! Why the *opaque* bbox and not the mesh-referenced *UV* bbox: on imported
//! models the UV bbox is dominated by transparent texels the mesh's vertices
//! straddle but never show (on the reference model it comes out ~12% opaque).
//! The opaque bbox is ~4x tighter — reference model ~158 MB -> ~56 MB.
//!
//! Sampling stays correct because premultiplied storage plus a transparent
//! ClampToBorder make taps past the opaque region read transparent, and the
//! 16-texel skirt reproduces the source texture's mip neighborhood so
//! box-filtered levels 0..=4 match. Each crop is its own texture with its own
//! mip chain, so no mip footprint can straddle into another crop's texels.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::components::{srgb_encode_to_byte, DecodedTexture};

/// Why a texture could not be prepared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextureError {
    #[error("texture decode failed: {0}")]
    Decode(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Png,
    Tga,
}

impl TextureFormat {
    pub fn to_image_format(self) -> image::ImageFormat {
        match self {
            TextureFormat::Png => image::ImageFormat::Png,
            TextureFormat::Tga => image::ImageFormat::Tga,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodedTexture {
    pub format: TextureFormat,
    pub data: Arc<[u8]>,
    /// `true` for premultiplied-in-sRGB on-disk bytes (every `.inx`
    /// texture); `false` for editor-authored straight-alpha. Decides whether
    /// `decode` un-premultiplies before re-premultiplying into linear.
    pub premultiplied: bool,
}

impl EncodedTexture {
    /// Decode into the canonical [`DecodedTexture`] form: bytes encoding
    /// premultiplied LINEAR color, ready to upload as `Rgba8UnormSrgb`.
    /// The sampler then decodes sRGB→linear and returns premultiplied
    /// linear values; the shader treats the sample as premultiplied and
    /// runs all blend / tint math in linear space.
    ///
    /// The inx/inp on-disk convention pre-multiplies RGB by alpha in
    /// sRGB byte space (`byte = srgb_encode(linear) * α`). To convert
    /// to "premultiplied linear, encoded as sRGB byte" we
    /// (1) unpremultiply in sRGB byte space → straight sRGB bytes,
    /// (2) decode each channel sRGB→linear, multiply by α, encode
    ///     linear→sRGB byte back into the buffer.
    ///
    /// Premultiplied storage handles alpha-edge bilinear correctly
    /// without an explicit alpha-bleed pass: at an edge, the filter
    /// mixes `(rgb·α, α)` with `(0, 0)` and the resulting gradient is
    /// already premultiplied.
    /// Width/height from the image header alone — no pixel decode. Lets
    /// the importer plan every texture crop before paying
    /// for any full decode.
    pub fn dimensions(&self) -> Result<(u32, u32), image::ImageError> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&self.data[..]));
        reader.set_format(self.format.to_image_format());
        reader.into_dimensions()
    }

    pub fn decode(&self) -> Result<DecodedTexture, image::ImageError> {
        let img = image::load_from_memory_with_format(&self.data, self.format.to_image_format())?;
        let (width, height) = (img.width(), img.height());
        let mut rgba = img.into_rgba8().into_raw();
        // Both conventions converge on premultiplied-linear; only a
        // premultiplied-sRGB source needs unwinding to straight first.
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

/// Every `(channel byte, alpha byte)` pair's final value under the inx
/// conversion. Indexed `alpha * 256 + channel`.
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

/// Convert premultiplied-in-sRGB bytes straight to the
/// premultiplied-linear encoding, in one pass.
///
/// Composing [`unpremultiply_srgb_inplace`] with
/// [`premultiply_linear_into_srgb_inplace`] gives the same bytes, but each
/// output depends only on its own channel and alpha, so the composition is
/// a pure function of two bytes and tabulates completely. That turns the
/// per-pixel divide, `powf` pair, and second sweep over the buffer into one
/// indexed read — and the second sweep is most of the cost, because the
/// overwhelming majority of an atlas is fully transparent or fully opaque
/// and does no arithmetic either way.
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

/// Undo premultiplication in sRGB byte space on an RGBA8 buffer.
/// For each pixel: `straight = round((rgb * 255) / α)`, computed in
/// f32 so partial-alpha pixels don't lose precision to integer
/// division. Pixels with α=0 are left as `(0, 0, 0, 0)`.
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
        let inv = 255.0 / a as f32;
        for c in &mut px[..3] {
            *c = (*c as f32 * inv).round().min(255.0) as u8;
        }
    }
}

/// Convert straight-alpha sRGB-encoded RGBA to bytes encoding
/// premultiplied LINEAR color: for each channel, `srgb_decode(rgb)`,
/// multiply by `α`, `srgb_encode` back to byte. Alpha is unchanged.
/// α=0 pixels emit `(0, 0, 0, 0)`; α=255 pixels are unchanged
/// (premultiply by 1 is a no-op).
///
/// Result is suitable for upload as `Rgba8UnormSrgb`: the sampler
/// decodes sRGB→linear and returns premultiplied linear values, which
/// the shader can blend / tint directly without re-multiplying by α.
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
        let af = a as f32 / 255.0;
        for c in &mut px[..3] {
            let premul = decode[*c as usize] * af;
            *c = srgb_encode_to_byte(premul);
        }
    }
}

/// Edge-bleed (a.k.a. alpha bleed / edge padding): for every α=0 pixel,
/// copy the RGB of the nearest α>0 pixel, leaving α=0 untouched. Without
/// this, bilinear texture filtering at a Part's alpha boundary mixes
/// `(rgb, 1)` with `(0, 0)` and produces a darkening colour shift that
/// shows up as faint mesh-edge halos on top of underlying parts. The imported
/// format gets the same effect for free because it stores premultiplied bytes:
/// at the boundary the filter mixes `rgb·α` with `0` and the gradient is
/// already premultiplied. Catchlight stores straight-alpha so we have to
/// stage the boundary RGB manually.
///
/// BFS from every α>0 pixel; each α=0 neighbour reached for the first
/// time copies the source's RGB. O(W·H), runs once per texture at load.
pub fn alpha_bleed_inplace(rgba: &mut [u8], width: u32, height: u32) {
    let w = width as usize;
    let h = height as usize;
    debug_assert_eq!(rgba.len(), w * h * 4);
    if w == 0 || h == 0 {
        return;
    }

    // `seen[i]` is true when pixel i has α>0, OR has already been bled
    // from a neighbour. Doubles as the "in queue" marker.
    let mut seen = vec![false; w * h];
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    for i in 0..(w * h) {
        if rgba[i * 4 + 3] > 0 {
            seen[i] = true;
            queue.push_back(i as u32);
        }
    }

    while let Some(idx) = queue.pop_front() {
        let idx = idx as usize;
        let x = idx % w;
        let y = idx / w;
        let r = rgba[idx * 4];
        let g = rgba[idx * 4 + 1];
        let b = rgba[idx * 4 + 2];
        let visit = |nx: usize,
                     ny: usize,
                     queue: &mut std::collections::VecDeque<u32>,
                     seen: &mut [bool],
                     rgba: &mut [u8]| {
            let ni = ny * w + nx;
            if seen[ni] {
                return;
            }
            seen[ni] = true;
            rgba[ni * 4] = r;
            rgba[ni * 4 + 1] = g;
            rgba[ni * 4 + 2] = b;
            // alpha stays 0 — the pixel is still invisible
            queue.push_back(ni as u32);
        };
        if x > 0 {
            visit(x - 1, y, &mut queue, &mut seen, rgba);
        }
        if x + 1 < w {
            visit(x + 1, y, &mut queue, &mut seen, rgba);
        }
        if y > 0 {
            visit(x, y - 1, &mut queue, &mut seen, rgba);
        }
        if y + 1 < h {
            visit(x, y + 1, &mut queue, &mut seen, rgba);
        }
    }
}

/// Transparent skirt around the opaque bbox, in texels. The renderer
/// box-filters each texture's own mip chain, so the skirt only has to feed the
/// boundary mip box: 16 = 2^4 covers mip levels 0..=4 ([`ALIGN`]), and
/// anything the box reaches past it is the transparent `ClampToBorder` — the
/// same transparent the source had beyond its opaque content.
const MARGIN: i64 = 16;

/// Crop origins and sizes are multiples of 2^4, so the box-filter footprints
/// of mip levels 0..=4 tile inside the crop instead of straddling its edge —
/// each level is computed from exactly the texels the source's own mip used.
const ALIGN: i64 = 16;

#[derive(Clone, Copy)]
struct Plan {
    src_w: u32,
    src_h: u32,
    /// Aligned opaque-bbox crop in source-texel coords, or `None` when the
    /// texture has no opaque texels (it becomes a 1x1 transparent stand-in so
    /// the table stays index-aligned with the source).
    crop: Option<(i64, i64, u32, u32)>,
}

#[derive(Clone, PartialEq, Eq)]
struct TexturePrepKey {
    format: TextureFormat,
    premultiplied: bool,
    texture_halvings: u32,
    content_hash: u64,
    data: Arc<[u8]>,
}

impl Hash for TexturePrepKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.format.hash(state);
        self.premultiplied.hash(state);
        self.texture_halvings.hash(state);
        self.content_hash.hash(state);
    }
}

/// Memo for the pure per-texture half of [`prepare_textures`]. An editor
/// rebuilding the puppet after every edit skips decode and crop work for
/// unchanged textures. Keys retain the encoded bytes so hash collisions are
/// verified by equality. Each rebuild discards entries that are not used by
/// the current document, so revisions cannot accumulate stale texture data.
#[derive(Default)]
pub struct TexturePrepCache {
    entries: std::collections::HashMap<TexturePrepKey, (DecodedTexture, Plan)>,
}

fn prep_key(tex: &EncodedTexture, texture_halvings: u32) -> TexturePrepKey {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    tex.data.hash(&mut h);
    TexturePrepKey {
        format: tex.format,
        premultiplied: tex.premultiplied,
        texture_halvings,
        content_hash: h.finish(),
        data: Arc::clone(&tex.data),
    }
}

/// Where one texture's opaque crop sits inside the source image, as the UV
/// remap a mesh authored against the uncropped texture needs.
///
/// `None` on a [`PreppedTexture`] means "no remap": either the texture was
/// fully transparent (it became a 1x1 stand-in and nothing sampling it is
/// visible) or it had no crop to apply.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UvCrop {
    src_w: u32,
    src_h: u32,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
}

impl UvCrop {
    /// Map a UV authored against the *source* texture onto the crop.
    ///
    /// In f64 because the source dimensions run to thousands of texels and
    /// the crop is a small window inside them: doing this in f32 loses a
    /// texel's worth of precision at the far edge of a 4k texture.
    pub fn map(&self, uv: crate::Vec2) -> crate::Vec2 {
        let (tw, th) = (self.src_w as f64, self.src_h as f64);
        crate::Vec2::new(
            ((uv.x as f64 * tw - self.x as f64) / self.w as f64) as f32,
            ((uv.y as f64 * th - self.y as f64) / self.h as f64) as f32,
        )
    }
}

/// One decoded, cropped texture and the UV remap that goes with it.
#[derive(Debug, Clone)]
pub struct PreppedTexture {
    pub texture: DecodedTexture,
    pub uv_crop: Option<UvCrop>,
}

/// Decode `textures` (each halved `texture_halvings` times) and crop each to
/// its opaque bounding box, without touching any runtime.
///
/// The returned table is 1:1 with `textures`, so albedo ids index it
/// unchanged; each entry carries the [`UvCrop`] its parts' mesh UVs must be
/// remapped through before they are drawn. Whoever owns those meshes applies
/// it; the render cache rewrites the UVs it uploads.
pub fn prepare_textures(
    textures: &[EncodedTexture],
    texture_halvings: u32,
    cache: Option<&mut TexturePrepCache>,
) -> Result<Vec<PreppedTexture>, TextureError> {
    let (table, plans) = prep_all(textures, texture_halvings, cache)?;
    Ok(table
        .into_iter()
        .zip(plans)
        .map(|(texture, plan)| PreppedTexture {
            texture,
            uv_crop: plan.crop.map(|(x, y, w, h)| UvCrop {
                src_w: plan.src_w,
                src_h: plan.src_h,
                x,
                y,
                w,
                h,
            }),
        })
        .collect())
}

/// Probe the memo, decode and crop the misses, and return the table beside
/// the crop plans.
fn prep_all(
    textures: &[EncodedTexture],
    texture_halvings: u32,
    mut cache: Option<&mut TexturePrepCache>,
) -> Result<(Vec<DecodedTexture>, Vec<Plan>), TextureError> {
    let keys: Vec<Option<TexturePrepKey>> = textures
        .iter()
        .map(|tex| cache.as_ref().map(|_| prep_key(tex, texture_halvings)))
        .collect();
    if let Some(cache) = cache.as_deref_mut() {
        let active: std::collections::HashSet<_> = keys.iter().flatten().cloned().collect();
        cache.entries.retain(|key, _| active.contains(key));
    }

    // Probe the cache first so the expensive half runs only for misses.
    let mut prepped: Vec<Option<(DecodedTexture, Plan)>> = keys
        .iter()
        .map(|key| {
            let (Some(cache), Some(key)) = (cache.as_deref(), key.as_ref()) else {
                return None;
            };
            cache.entries.get(key).cloned()
        })
        .collect();

    let misses: Vec<usize> = prepped
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_none())
        .map(|(i, _)| i)
        .collect();
    let done = prep_textures(&misses, textures, texture_halvings)?;
    for (i, entry) in misses.iter().zip(done) {
        if let (Some(cache), Some(key)) = (cache.as_deref_mut(), keys[*i].clone()) {
            cache.entries.insert(key, entry.clone());
        }
        prepped[*i] = Some(entry);
    }

    let (table, plans): (Vec<DecodedTexture>, Vec<Plan>) = prepped.into_iter().flatten().unzip();
    // Albedo ids index this table positionally, so a dropped entry would
    // silently repoint every later part at the wrong texture. Filling by
    // index makes that impossible; this pins it, since the 1:1 property
    // stopped being local once the decode was split out to fan out.
    assert_eq!(table.len(), textures.len());
    Ok((table, plans))
}

/// Decode and crop the textures at `indices`.
///
/// PNG decode dominates the cost of loading a model and is entropy decoding
/// plus un-filtering — pure CPU work with no GPU analogue — but it is
/// independent per texture, so native builds fan it out across cores.
/// wasm stays sequential: the wgpu web backend is not thread-safe and
/// rayon degrades to a serial iterator there in any case.
fn prep_textures(
    indices: &[usize],
    textures: &[EncodedTexture],
    texture_halvings: u32,
) -> Result<Vec<(DecodedTexture, Plan)>, TextureError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rayon::prelude::*;
        indices
            .par_iter()
            .map(|&i| prep_one(&textures[i], texture_halvings))
            .collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        indices
            .iter()
            .map(|&i| prep_one(&textures[i], texture_halvings))
            .collect()
    }
}

fn prep_one(
    tex: &EncodedTexture,
    texture_halvings: u32,
) -> Result<(DecodedTexture, Plan), TextureError> {
    let decoded = decode_halved(tex, texture_halvings)?;
    let (src_w, src_h) = (decoded.width, decoded.height);
    Ok(match alpha_crop_rect(&decoded) {
        Some((x, y, w, h)) => (
            crop(&decoded, x, y, w, h),
            Plan {
                src_w,
                src_h,
                crop: Some((x, y, w, h)),
            },
        ),
        // A fully transparent texture becomes a 1x1 stand-in so the table
        // stays index-aligned with the source.
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

/// Bounding box of the opaque (alpha>0) texels, expanded by [`MARGIN`] and
/// snapped to [`ALIGN`] so the box-filter footprints of mip levels 0..=4
/// match the source's. `None` when the texture is fully transparent.
fn alpha_crop_rect(tex: &DecodedTexture) -> Option<(i64, i64, u32, u32)> {
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
    let x0 = align_down(minx as i64 - MARGIN);
    let y0 = align_down(miny as i64 - MARGIN);
    let x1 = align_up(maxx as i64 + 1 + MARGIN);
    let y1 = align_up(maxy as i64 + 1 + MARGIN);
    Some((x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// Copy crop `(x, y, w, h)` (source-texel coords, origin may be negative) of
/// `tex` into a fresh `w x h` texture, out-of-source texels left at the
/// transparent zero-fill — the ClampToBorder equivalent.
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
        .map_err(|e| TextureError::Decode(e.to_string()))?;
    for _ in 0..halvings {
        decoded = decoded.halved();
    }
    Ok(decoded)
}

/// Copy the crop rect `(src[0], src[1], size[0], size[1])` of `tex` to `dst`
/// in `out` (row stride `out_w`), leaving out-of-source texels at the
/// destination's transparent zero-fill (the ClampToBorder equivalent).
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
    let (tw, th) = (tex.width as i64, tex.height as i64);
    for row in 0..h as i64 {
        let sy = src_y + row;
        if sy < 0 || sy >= th {
            continue;
        }
        let cx0 = src_x.max(0);
        let cx1 = (src_x + w as i64).min(tw);
        if cx0 >= cx1 {
            continue;
        }
        let src_off = ((sy * tw + cx0) * 4) as usize;
        let src_len = ((cx1 - cx0) * 4) as usize;
        let dst_row = dst_y as i64 + row;
        let dst_col = dst_x as i64 + (cx0 - src_x);
        let dst_off = ((dst_row * out_w as i64 + dst_col) * 4) as usize;
        out[dst_off..dst_off + src_len].copy_from_slice(&tex.rgba[src_off..src_off + src_len]);
    }
}

#[cfg(test)]
#[allow(clippy::identity_op)]
mod tests {
    use super::*;

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
        // 4x4 source with row-major distinct bytes; crop extends 16 texels
        // beyond every edge (negative origin) — out-of-source texels must
        // stay zero, in-source bytes must land shifted by the offset.
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
                let dst = (((48 + 16 + y) * OUT as i64 + 32 + 16 + x) * 4) as usize;
                assert_eq!(&out[dst..dst + 4], &rgba[src..src + 4]);
            }
        }
        // A texel in the transparent rim.
        let rim = ((48 * OUT as i64 + 32) * 4) as usize;
        assert_eq!(&out[rim..rim + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn fully_transparent_has_no_crop() {
        assert_eq!(alpha_crop_rect(&tex(64, 64, &[])), None);
    }

    /// The decode path takes a model's payload by reference count, not by
    /// copy. A render cache converts every texture it is about to decode on
    /// every rebuild, so a copy here would be one copy of every changed
    /// texture per edit — which is what the two `Arc` shapes used to cost.
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
    fn cache_key_distinguishes_alpha_and_format() {
        let straight = encoded_png([64, 0, 0, 128], false);
        let mut cache = TexturePrepCache::default();
        let straight_result =
            prepare_textures(std::slice::from_ref(&straight), 0, Some(&mut cache)).unwrap();

        let mut premultiplied = straight.clone();
        premultiplied.premultiplied = true;
        let premultiplied_result = prepare_textures(&[premultiplied], 0, Some(&mut cache)).unwrap();
        assert_ne!(
            straight_result[0].texture.rgba,
            premultiplied_result[0].texture.rgba
        );

        let mut wrong_format = straight;
        wrong_format.format = TextureFormat::Tga;
        assert!(matches!(
            prepare_textures(&[wrong_format], 0, Some(&mut cache)),
            Err(TextureError::Decode(_))
        ));
    }

    #[test]
    fn cache_discards_entries_outside_the_current_document() {
        let first = encoded_png([255, 0, 0, 255], false);
        let second = encoded_png([0, 255, 0, 255], false);
        let first_key = prep_key(&first, 0);
        let second_key = prep_key(&second, 0);
        let mut cache = TexturePrepCache::default();

        prepare_textures(&[first], 0, Some(&mut cache)).unwrap();
        assert!(cache.entries.contains_key(&first_key));

        prepare_textures(&[second], 0, Some(&mut cache)).unwrap();
        assert!(!cache.entries.contains_key(&first_key));
        assert!(cache.entries.contains_key(&second_key));
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn crop_rect_brackets_opaque_with_aligned_margin() {
        // One opaque texel at (40, 50) in a 256x256 texture. MARGIN=16,
        // ALIGN=16: x0 = align_down(40-16)=align_down(24)=16,
        // x1 = align_up(41+16)=align_up(57)=64; y0 = align_down(34)=32,
        // y1 = align_up(67)=80.
        let (x, y, w, h) = alpha_crop_rect(&tex(256, 256, &[(40, 50)])).unwrap();
        assert_eq!((x, y, w, h), (16, 32, 48, 48));
        // The opaque texel sits strictly inside the crop, away from the edge.
        assert!(x < 40 && (x + w as i64) > 40);
        assert!(y < 50 && (y + h as i64) > 50);
    }

    #[test]
    fn crop_copies_opaque_texel_and_zeroes_the_rest() {
        let source = tex(256, 256, &[(40, 50)]);
        let (x, y, w, h) = alpha_crop_rect(&source).unwrap();
        let cropped = crop(&source, x, y, w, h);
        assert_eq!((cropped.width, cropped.height), (w, h));
        let (lx, ly) = ((40 - x) as u32, (50 - y) as u32);
        let i = ((ly * w + lx) * 4) as usize;
        assert_eq!(&cropped.rgba[i..i + 4], &[200, 100, 50, 255]);
        // A corner texel of the crop is in the transparent skirt.
        assert_eq!(&cropped.rgba[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn remap_keeps_opaque_uvs_inside_unit_square() {
        // A UV pointing at the opaque texel maps to the crop interior; a UV
        // at the source corner (transparent, and >MARGIN from the content)
        // maps outside [0,1] -> sampled transparent by ClampToBorder, exactly
        // as in the source.
        let source = tex(256, 256, &[(140, 150)]);
        let (cx, cy, cw, ch) = alpha_crop_rect(&source).unwrap();
        let remap = |u: f64, v: f64| {
            (
                ((u * 256.0 - cx as f64) / cw as f64) as f32,
                ((v * 256.0 - cy as f64) / ch as f64) as f32,
            )
        };
        let (ox, oy) = remap(140.5 / 256.0, 150.5 / 256.0);
        assert!((0.0..=1.0).contains(&ox) && (0.0..=1.0).contains(&oy));
        let (gx, gy) = remap(0.0, 0.0);
        assert!(gx < 0.0 && gy < 0.0);
    }

    /// [`UvCrop::map`] is the whole contract between the crop and whoever owns
    /// the meshes: a UV authored against the source texture has to land on the
    /// same texel of the crop. Checked against the window arithmetic done by
    /// hand, in f64, so a change to `map` cannot quietly shift every part's
    /// UVs by a texel.
    #[test]
    fn the_uv_remap_lands_on_the_same_texel_of_the_crop() {
        use crate::Vec2;

        // A 64x64 PNG with one opaque texel, so the crop is a real window
        // rather than the whole image.
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

        let prepped = prepare_textures(std::slice::from_ref(&source), 0, None).unwrap();
        assert_eq!(prepped.len(), 1);
        let crop = prepped[0].uv_crop.expect("an opaque texel means a crop");
        let (cx, cy, cw, ch) = alpha_crop_rect(&decode_halved(&source, 0).unwrap())
            .expect("the same crop the prep found");
        assert_eq!(
            (prepped[0].texture.width, prepped[0].texture.height),
            (cw, ch)
        );

        let by_hand = |uv: Vec2| {
            Vec2::new(
                ((uv.x as f64 * 64.0 - cx as f64) / cw as f64) as f32,
                ((uv.y as f64 * 64.0 - cy as f64) / ch as f64) as f32,
            )
        };
        for uv in [
            Vec2::new(0.0, 0.0),
            Vec2::new(40.5 / 64.0, 33.5 / 64.0),
            Vec2::new(1.0, 1.0),
        ] {
            assert_eq!(crop.map(uv), by_hand(uv), "uv {uv:?}");
        }

        // The opaque texel's UV lands inside the crop; the source corner does
        // not, and reads transparent through ClampToBorder exactly as it did
        // in the source.
        let inside = crop.map(Vec2::new(40.5 / 64.0, 33.5 / 64.0));
        assert!((0.0..=1.0).contains(&inside.x) && (0.0..=1.0).contains(&inside.y));
        let corner = crop.map(Vec2::ZERO);
        assert!(corner.x < 0.0 && corner.y < 0.0);
    }

    #[test]
    fn fused_premultiply_matches_the_two_pass_composition() {
        // The fused table replaces unpremultiply-then-premultiply, so it
        // has to agree on every input it can ever see — and it can see all
        // of them, since each output channel depends only on its own byte
        // and the pixel's alpha. Exhaustive, not sampled.
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

    #[test]
    fn unpremultiply_srgb_recovers_straight_alpha() {
        // Pixel layout: opaque (α=255), fully transparent (α=0),
        // half-alpha grey, tiny-alpha saturated color.
        let mut px = [
            200, 100, 50, 255, // fully opaque: untouched
            123, 234, 89, 0, // α=0: rgb zeroed
            64, 64, 64, 128, // α≈0.5: premul 0.5 byte → straight ≈ 0.5 sRGB = 128
            10, 5, 2, 20, // α≈8%: premul 10/20 → straight ≈ 0.5 sRGB = 128
        ];
        unpremultiply_srgb_inplace(&mut px);
        assert_eq!(&px[0..4], &[200, 100, 50, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
        // (64 * 255 / 128).round() = 127.5 → banker's/half-away-up 128.
        assert!((px[4 + 4] as i32 - 127).abs() <= 1);
        assert!((px[5 + 4] as i32 - 127).abs() <= 1);
        assert!((px[6 + 4] as i32 - 127).abs() <= 1);
        assert_eq!(px[7 + 4], 128);
        // α=20: inv = 12.75. 10*12.75 = 127.5; 5*12.75 = 63.75; 2*12.75 = 25.5.
        assert!((px[12] as i32 - 128).abs() <= 1);
        assert!((px[13] as i32 - 64).abs() <= 1);
        assert!((px[14] as i32 - 26).abs() <= 1);
        assert_eq!(px[15], 20);
    }

    #[test]
    fn unpremultiply_srgb_clamps_noise() {
        // Slightly-invalid asset where rgb > alpha (impossible in a
        // correct premultiply). The clamp to 255 must hold.
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
        // sRGB byte 188 ≈ linear 0.5026. Premul by α=128/255≈0.502 →
        // linear 0.2523. srgb_encode → byte ≈ 138 (linear premul,
        // not the byte-space premul which would give 188*0.5=94).
        let mut px = [188u8, 188, 188, 128];
        premultiply_linear_into_srgb_inplace(&mut px);
        for (c, &v) in px[..3].iter().enumerate() {
            assert!(
                (v as i32 - 138).abs() <= 1,
                "channel {} = {}, expected ~138",
                c,
                v,
            );
        }
        assert_eq!(px[3], 128, "alpha unchanged");
    }

    #[test]
    fn alpha_bleed_extends_visible_rgb_into_transparent_neighbors() {
        // 3x3 with a single visible (red, opaque) centre pixel and an
        // 8-pixel transparent ring around it. After bleed, every ring
        // pixel must hold the centre's RGB (alpha still 0).
        let mut rgba = vec![0u8; 3 * 3 * 4];
        // centre at (1, 1)
        rgba[(1 * 3 + 1) * 4] = 220;
        rgba[(1 * 3 + 1) * 4 + 1] = 30;
        rgba[(1 * 3 + 1) * 4 + 2] = 60;
        rgba[(1 * 3 + 1) * 4 + 3] = 255;

        alpha_bleed_inplace(&mut rgba, 3, 3);

        for i in 0..9 {
            let r = rgba[i * 4];
            let g = rgba[i * 4 + 1];
            let b = rgba[i * 4 + 2];
            let a = rgba[i * 4 + 3];
            assert_eq!((r, g, b), (220, 30, 60), "pixel {} RGB", i);
            if i == 4 {
                assert_eq!(a, 255, "centre alpha");
            } else {
                assert_eq!(a, 0, "ring pixel {} alpha", i);
            }
        }
    }

    #[test]
    fn alpha_bleed_no_op_when_fully_opaque_or_fully_transparent() {
        // Fully opaque buffer: no α=0 pixels, nothing to bleed into.
        let mut opaque = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        let snapshot = opaque.clone();
        alpha_bleed_inplace(&mut opaque, 2, 1);
        assert_eq!(opaque, snapshot);

        // Fully transparent buffer: no α>0 source — every pixel stays
        // at its initial RGB (the BFS queue is empty so the function
        // exits immediately).
        let mut blank = vec![0u8; 2 * 1 * 4];
        alpha_bleed_inplace(&mut blank, 2, 1);
        assert_eq!(blank, vec![0u8; 8]);
    }
}
