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

use std::sync::Arc;

use crate::components::{srgb_encode_to_byte, DecodedTexture};
use crate::load_budget::{LoadBudget, LoadLimitError};

/// Why a texture could not be prepared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TextureError {
    #[error("texture decode failed: {0}")]
    Decode(String),
    /// A texture is larger than
    /// [`MAX_TEXTURE_DIMENSION`](crate::load_budget::MAX_TEXTURE_DIMENSION) on
    /// a side, or the textures together decode to more bytes than the
    /// caller's budget allows.
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

/// Undo [`premultiply_linear_into_srgb_inplace`]: bytes encoding
/// premultiplied LINEAR color back to straight-alpha sRGB. For each
/// channel, `srgb_decode(rgb)`, divide by `α`, `srgb_encode` back to byte,
/// clamped at white. α=0 pixels emit `(0, 0, 0, 0)`; α=255 pixels are
/// unchanged.
///
/// **This is the inverse a framebuffer readback needs, not
/// [`unpremultiply_srgb_inplace`].** The two undo different
/// premultiplications and the names are one letter apart, so: a render
/// target is `Rgba8UnormSrgb`, which means the blender works on linear
/// values and encodes on store, so what comes back is
/// `srgb_encode(linear × α)` and the divide has to happen in linear.
/// `unpremultiply_srgb_inplace` undoes the *other* convention, the
/// `byte = srgb_encode(linear) × α` that inx textures are stored in, where
/// the divide happens in byte space. Using that one on a readback saturates
/// every partly transparent pixel toward white.
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
        let inv = 255.0 / a as f32;
        for c in &mut px[..3] {
            *c = srgb_encode_to_byte((decode[*c as usize] * inv).min(1.0));
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
///
/// The skirt runs past the source where opaque content sits within 16 texels
/// of an edge, and is baked rather than left to the sampler on purpose: the
/// web backends have no border sampler, and the shader's stand-in for one is
/// exact only at mip level 0. It gives way on an axis only where it would
/// make the crop larger than the caller can hold (see [`alpha_crop_rect`]).
const MARGIN: i64 = 16;

/// Crop origins and sizes are multiples of 2^4, so the box-filter footprints
/// of mip levels 0..=4 tile inside the crop instead of straddling its edge —
/// each level is computed from exactly the texels the source's own mip used.
const ALIGN: i64 = 16;

#[derive(Clone, Copy)]
struct Plan {
    src_w: u32,
    src_h: u32,
    /// Aligned opaque-bbox crop in source-texel coords; the origin may be
    /// negative and the far edge past the source, where the skirt is.
    /// `None` when there is nothing to remap: the texture has no opaque
    /// texels (it becomes a 1x1 transparent stand-in so the table stays
    /// index-aligned with the source), or the crop is the whole image.
    crop: Option<(i64, i64, u32, u32)>,
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
    /// The image's width and height as its header states them, before any
    /// halving or crop. `width * height * 4` is what [`prepare_textures`]
    /// charged the budget for it, so a caller that keeps this texture across
    /// a rebuild can charge the same amount without reading the header again.
    pub source_size: [u32; 2],
}

/// Decode `textures` (each halved `texture_halvings` times) and crop each to
/// its opaque bounding box, without touching any runtime.
///
/// The returned table is 1:1 with `textures`, so albedo ids index it
/// unchanged; each entry carries the [`UvCrop`] its parts' mesh UVs must be
/// remapped through before they are drawn. Whoever owns those meshes applies
/// it; the render cache rewrites the UVs it uploads.
///
/// Nothing here remembers a decode. A caller that rebuilds after every edit
/// passes only the textures whose payload changed — the render cache keeps
/// the GPU texture of every other one by Id and payload `Arc` — so a memo in
/// front of this would hold a decoded copy of every texture for a hit that
/// never comes.
///
/// **Every header is charged before any pixel is decoded.** A model stores
/// its textures encoded and loading one never looks inside them, so this is
/// the first place a texture's real size is known — and a few kilobytes of
/// PNG can state any size at all. Each image's header dimensions are held to
/// [`MAX_TEXTURE_DIMENSION`](crate::load_budget::MAX_TEXTURE_DIMENSION) and
/// charged to `budget` as RGBA8 at full size
/// ([`LoadBudget::check_texture_dimensions`], the same charge the importer
/// makes), so a file that would decode past the caller's
/// `decoded_texture_bytes` is refused having allocated nothing. Full size,
/// whatever `texture_halvings` says: the halving runs on the decoded image.
///
/// `max_dimension` is the largest side the caller can hold — a device's 2D
/// texture limit. A crop carries a transparent skirt that can make it up to
/// 32 texels larger than the image it was cut from; on an axis where that
/// would pass `max_dimension` the skirt is given up and the crop ends with
/// the source, so a texture that fits the device uncropped is never refused
/// for having been cropped. An image larger than `max_dimension` on its own
/// is prepared as it is and is the caller's to refuse.
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
            .map_err(|e| TextureError::Decode(e.to_string()))?;
        budget.check_texture_dimensions(width, height)?;
        sizes.push([width, height]);
    }

    let prepped = prep_textures(textures, texture_halvings, max_dimension)?;
    // Albedo ids index this table positionally, so a dropped entry would
    // silently repoint every later part at the wrong texture.
    assert_eq!(prepped.len(), textures.len());
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

/// Decode and crop every texture, in order.
///
/// PNG decode dominates the cost of loading a model and is entropy decoding
/// plus un-filtering — pure CPU work with no GPU analogue — but it is
/// independent per texture, so native builds fan it out across cores.
/// wasm stays sequential: the wgpu web backend is not thread-safe and
/// rayon degrades to a serial iterator there in any case.
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
        // Opaque out to every edge with no room for a skirt: the crop is
        // the image, so there is nothing to copy and no remap to apply.
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
///
/// The skirt runs past the source where opaque content is near an edge,
/// unless that makes the crop longer than `max_dimension` on that axis.
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
    // One axis: the skirted, aligned span, or — where that is longer than
    // the caller can hold — the same span stopped at the source.
    //
    // The skirt past the source is not waste. Sampling just outside opaque
    // content has to fade to transparent at every mip level, and only a
    // native sampler has a border to fade to: WebGPU and WebGL2 clamp to
    // the edge texel, and the shader's emulation of the border is exact at
    // level 0 alone. Baked into the texture, the fade is the same
    // everywhere. Stopping every crop at its source moved a few hundred
    // edge pixels per frame of the Lan fixture even with a real border.
    //
    // But the skirt makes a crop up to 32 texels longer than its source, so
    // an image opaque to its edges and already at the device's limit came
    // back over it and was refused. There the skirt gives way: the near
    // edge stops at 0 and the far edge at the source size rounded *up* to
    // [`ALIGN`] — not at the size itself, because an odd-sized crop has no
    // exact box-filtered mip chain — and the sampler's border, or its
    // emulation, stands in for the texels the crop no longer holds.
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

    /// A size limit no crop here reaches: the skirt always fits.
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
        assert_eq!(alpha_crop_rect(&tex(64, 64, &[]), ROOMY), None);
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

    /// The same bytes are a different image under the other alpha convention,
    /// and bytes read under the wrong format are an error rather than a guess.
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

    /// A PNG whose header states `width` x `height` and whose image data is
    /// not a deflate stream at all: [`EncodedTexture::dimensions`] reads it,
    /// a decode fails. Which of the two errors comes back therefore says
    /// whether the header was checked before the pixels were asked for.
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
        // 8-bit RGBA, no interlace.
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

    /// A model keeps its textures encoded, so nothing has looked at a `.clm`
    /// texture's size until it is prepared. One side over the cap is refused
    /// from the header: a few dozen bytes may not ask for a 8193-wide decode.
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
                        resource: "texture dimension",
                        ..
                    })
                ),
                "{width}x{height}: {err:?}"
            );
        }
    }

    /// The decoded-bytes budget is over every texture in the call together,
    /// and all of it is charged before the first decode starts: the textures
    /// here cannot be decoded, so a `Decode` error would mean the budget was
    /// consulted too late to stop the allocation.
    #[test]
    fn the_decoded_budget_is_aggregate_and_charged_before_any_decode() {
        let textures = [png_stating(64, 64), png_stating(64, 64)];
        let both = 2 * 64 * 64 * 4;

        let err = prepare_textures(&textures, 0, ROOMY, &mut budget_of(both - 1)).unwrap_err();
        assert_eq!(
            err,
            TextureError::Limit(LoadLimitError {
                resource: "decoded texture bytes",
                limit: both - 1,
                got: both,
            })
        );
        // Halving does not shrink the charge: the image is decoded at full
        // size first.
        let err = prepare_textures(&textures, 3, ROOMY, &mut budget_of(both - 1)).unwrap_err();
        assert!(matches!(err, TextureError::Limit(_)), "{err:?}");

        // Exactly at the limit the headers pass, and only then is a decode
        // attempted.
        let err = prepare_textures(&textures, 0, ROOMY, &mut budget_of(both)).unwrap_err();
        assert!(matches!(err, TextureError::Decode(_)), "{err:?}");
    }

    /// The budget is the caller's, so what one call charged counts against
    /// the next — which is how a render cache holds a model's kept and fresh
    /// textures to one total.
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
        // One opaque texel at (40, 50) in a 256x256 texture. MARGIN=16,
        // ALIGN=16: x0 = align_down(40-16)=align_down(24)=16,
        // x1 = align_up(41+16)=align_up(57)=64; y0 = align_down(34)=32,
        // y1 = align_up(67)=80.
        let (x, y, w, h) = alpha_crop_rect(&tex(256, 256, &[(40, 50)]), ROOMY).unwrap();
        assert_eq!((x, y, w, h), (16, 32, 48, 48));
        // The opaque texel sits strictly inside the crop, away from the edge.
        assert!(x < 40 && (x + w as i64) > 40);
        assert!(y < 50 && (y + h as i64) > 50);
    }

    /// Opaque content at an edge buys a skirt past the source: that fade to
    /// transparent has to be in the texture, because not every backend has a
    /// border to sample it from.
    #[test]
    fn the_skirt_runs_past_the_source() {
        let corners = tex(96, 64, &[(0, 0), (95, 63)]);
        assert_eq!(alpha_crop_rect(&corners, ROOMY), Some((-16, -16, 128, 96)));

        // One edge only: the skirt is past the source on that side alone.
        let left = tex(256, 256, &[(3, 120)]);
        assert_eq!(alpha_crop_rect(&left, ROOMY), Some((-16, 96, 48, 48)));
    }

    /// The skirt makes a crop up to 32 texels longer than its source, so an
    /// image opaque to its edges and already at the device's limit used to
    /// come back over it and be refused. On an axis where the skirted crop
    /// does not fit, the crop stops at the source instead.
    #[test]
    fn the_skirt_gives_way_where_the_crop_would_not_fit() {
        // Too wide with a skirt, so the width stops at the source; the
        // height has room and keeps its own.
        let wide = tex(96, 1, &[(0, 0), (95, 0)]);
        assert_eq!(alpha_crop_rect(&wide, 96), Some((0, -16, 96, 48)));
        // One texel of room short is still short.
        assert_eq!(alpha_crop_rect(&wide, 127), Some((0, -16, 96, 48)));
        assert_eq!(alpha_crop_rect(&wide, 128), Some((-16, -16, 128, 48)));

        // A source that is not a multiple of 16 long: the far edge rounds
        // out to the alignment, so every mip level still halves exactly, and
        // no further.
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

    /// A texture opaque to every edge and already as large as the caller can
    /// hold is prepared at its own size, with no remap: there is no room for
    /// a skirt and nothing to crop. With room, it gets the skirt.
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
        assert!(prepped[0]
            .texture
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|px| *px == [9, 8, 7, 255]));

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
        let (cx, cy, cw, ch) = alpha_crop_rect(&source, ROOMY).unwrap();
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

    /// The readback inverse round-trips: premultiply a straight colour into
    /// the encoding a render target holds, undo it, and get the colour back.
    ///
    /// Not at every alpha. Premultiplying scales a channel down before it is
    /// quantised to 8 bits, so at alpha 1 only about 1/255 of the range
    /// survives and dividing it back multiplies the quantisation error by
    /// 255 — a pixel that faint carries no colour to recover. The bound here
    /// is what the encoding can honestly promise.
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

    /// The two unpremultiplies are not interchangeable, which is the whole
    /// reason both exist. A quarter-opacity pixel off a render target comes
    /// back as its own colour under the linear inverse and saturates under
    /// the byte-space one.
    #[test]
    fn the_two_unpremultiplies_disagree_and_only_one_fits_a_readback() {
        let mut target = vec![210, 90, 70, 255];
        // What the blender leaves for a part drawn at opacity 0.25.
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
