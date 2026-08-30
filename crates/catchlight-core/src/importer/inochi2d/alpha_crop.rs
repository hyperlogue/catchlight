//! Per-texture alpha cropping: catchlight's texture strategy, applied
//! wherever a model's encoded bytes are turned into uploadable images.
//! Crops each texture to the bounding box of its opaque texels (plus a
//! transparent mip skirt) and keeps it as its own table entry. Texture ids
//! stay 1:1 with the source table, so only part UVs are rewritten, never
//! albedo slots.
//!
//! Two entry points over one implementation. [`prepare_textures`] is the
//! puppet-free one: it decodes and crops and hands back a [`UvCrop`] per
//! texture for whoever owns the meshes to apply — that is what the render
//! cache calls, because a `Model` keeps its textures encoded and its meshes
//! authored against them. [`crop_textures`] is the legacy load path: it
//! applies the remap to the `LegacyPuppet`'s own mesh copies in place.
//!
//! Why the *opaque* bbox and not the mesh-referenced *UV* bbox: on inochi2d
//! models the UV bbox is dominated by transparent texels the mesh's vertices
//! straddle but never show (on the reference model it comes out ~12% opaque).
//! The opaque bbox is ~4x tighter — reference model ~158 MB -> ~56 MB.
//!
//! Sampling stays correct because premultiplied storage plus a transparent
//! ClampToBorder make taps past the opaque region read transparent, and the
//! [`MARGIN`] skirt reproduces the source texture's mip neighborhood so
//! box-filtered levels 0..=4 match. Each crop is its own texture with its own
//! mip chain, so no mip footprint can straddle into another crop's texels.

use super::error::ImportError;
use crate::components::{NodeIdx, NodeKind, PuppetTexture};
use crate::formats::ModelTexture;
use crate::legacy_puppet::LegacyPuppet;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

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
    format: crate::formats::TextureFormat,
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

/// Memo for the pure per-texture half of [`crop_textures`]. An editor
/// rebuilding the puppet after every edit skips decode and crop work for
/// unchanged textures. Keys retain the encoded bytes so hash collisions are
/// verified by equality. Each rebuild discards entries that are not used by
/// the current document, so revisions cannot accumulate stale texture data.
#[derive(Default)]
pub struct TexturePrepCache {
    entries: std::collections::HashMap<TexturePrepKey, (PuppetTexture, Plan)>,
}

fn prep_key(tex: &ModelTexture, texture_halvings: u32) -> TexturePrepKey {
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
    pub texture: PuppetTexture,
    pub uv_crop: Option<UvCrop>,
}

/// Decode `textures` (each halved `texture_halvings` times) and crop each to
/// its opaque bounding box, without touching any runtime.
///
/// The returned table is 1:1 with `textures`, so albedo ids index it
/// unchanged; each entry carries the [`UvCrop`] its parts' mesh UVs must be
/// remapped through before they are drawn. Whoever owns those meshes applies
/// it — the legacy load path rewrites the puppet's copy in place
/// ([`crop_textures_cached`]), the render cache rewrites the UVs it uploads.
pub fn prepare_textures(
    textures: &[ModelTexture],
    texture_halvings: u32,
    cache: Option<&mut TexturePrepCache>,
) -> Result<Vec<PreppedTexture>, ImportError> {
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

/// Decode `textures`, crop each to its opaque bounding box, and rewrite every
/// part's mesh UVs against its crop. The returned table is 1:1 with the source
/// table, so albedo ids are left untouched.
pub(crate) fn crop_textures(
    puppet: &mut LegacyPuppet,
    textures: &[ModelTexture],
    texture_halvings: u32,
) -> Result<Vec<PuppetTexture>, ImportError> {
    crop_textures_cached(puppet, textures, texture_halvings, None)
}

pub(crate) fn crop_textures_cached(
    puppet: &mut LegacyPuppet,
    textures: &[ModelTexture],
    texture_halvings: u32,
    cache: Option<&mut TexturePrepCache>,
) -> Result<Vec<PuppetTexture>, ImportError> {
    let (table, plans) = prep_all(textures, texture_halvings, cache)?;

    let part_ids: Vec<NodeIdx> = puppet
        .iter()
        .filter(|(_, n)| matches!(n.kind, NodeKind::Part(_)))
        .map(|(id, _)| id)
        .collect();
    for id in part_ids {
        let Some(node) = puppet.get_mut(id) else {
            continue;
        };
        let NodeKind::Part(part) = &mut node.kind else {
            continue;
        };
        let ti = part.albedo_texture.0 as usize;
        let Some(plan) = plans.get(ti) else { continue };
        let Some((cx, cy, cw, ch)) = plan.crop else {
            continue;
        };
        let (tw, th) = (plan.src_w as f64, plan.src_h as f64);
        for uv in &mut part.mesh.uvs {
            uv.x = ((uv.x as f64 * tw - cx as f64) / cw as f64) as f32;
            uv.y = ((uv.y as f64 * th - cy as f64) / ch as f64) as f32;
        }
    }
    Ok(table)
}

/// The puppet-free half: probe the memo, decode and crop the misses, and
/// return the table beside the crop plans.
fn prep_all(
    textures: &[ModelTexture],
    texture_halvings: u32,
    mut cache: Option<&mut TexturePrepCache>,
) -> Result<(Vec<PuppetTexture>, Vec<Plan>), ImportError> {
    let keys: Vec<Option<TexturePrepKey>> = textures
        .iter()
        .map(|tex| cache.as_ref().map(|_| prep_key(tex, texture_halvings)))
        .collect();
    if let Some(cache) = cache.as_deref_mut() {
        let active: std::collections::HashSet<_> = keys.iter().flatten().cloned().collect();
        cache.entries.retain(|key, _| active.contains(key));
    }

    // Probe the cache first so the expensive half runs only for misses.
    let mut prepped: Vec<Option<(PuppetTexture, Plan)>> = keys
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

    let (table, plans): (Vec<PuppetTexture>, Vec<Plan>) = prepped.into_iter().flatten().unzip();
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
    textures: &[ModelTexture],
    texture_halvings: u32,
) -> Result<Vec<(PuppetTexture, Plan)>, ImportError> {
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
    tex: &ModelTexture,
    texture_halvings: u32,
) -> Result<(PuppetTexture, Plan), ImportError> {
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
            PuppetTexture {
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
fn alpha_crop_rect(tex: &PuppetTexture) -> Option<(i64, i64, u32, u32)> {
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
fn crop(tex: &PuppetTexture, x: i64, y: i64, w: u32, h: u32) -> PuppetTexture {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    blit(&mut rgba, w, tex, [0, 0], [x, y], [w, h]);
    PuppetTexture {
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

fn decode_halved(tex: &ModelTexture, halvings: u32) -> Result<PuppetTexture, ImportError> {
    let mut decoded = tex
        .decode()
        .map_err(|e| ImportError::TextureDecode(e.to_string()))?;
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
    tex: &PuppetTexture,
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
mod tests {
    use super::*;

    fn encoded_png(rgba: [u8; 4], premultiplied: bool) -> ModelTexture {
        let image = image::RgbaImage::from_raw(1, 1, rgba.to_vec()).unwrap();
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        ModelTexture {
            format: crate::formats::TextureFormat::Png,
            data: data.into_inner().into(),
            premultiplied,
        }
    }

    fn tex(w: u32, h: u32, opaque: &[(u32, u32)]) -> PuppetTexture {
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        for &(x, y) in opaque {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[200, 100, 50, 255]);
        }
        PuppetTexture {
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
        let source = PuppetTexture {
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

    #[test]
    fn cache_key_distinguishes_alpha_and_format() {
        let straight = encoded_png([64, 0, 0, 128], false);
        let mut cache = TexturePrepCache::default();
        let straight_result = crop_textures_cached(
            &mut LegacyPuppet::new(),
            std::slice::from_ref(&straight),
            0,
            Some(&mut cache),
        )
        .unwrap();

        let mut premultiplied = straight.clone();
        premultiplied.premultiplied = true;
        let premultiplied_result = crop_textures_cached(
            &mut LegacyPuppet::new(),
            &[premultiplied],
            0,
            Some(&mut cache),
        )
        .unwrap();
        assert_ne!(straight_result[0].rgba, premultiplied_result[0].rgba);

        let mut wrong_format = straight;
        wrong_format.format = crate::formats::TextureFormat::Tga;
        assert!(matches!(
            crop_textures_cached(
                &mut LegacyPuppet::new(),
                &[wrong_format],
                0,
                Some(&mut cache)
            ),
            Err(ImportError::TextureDecode(_))
        ));
    }

    #[test]
    fn cache_discards_entries_outside_the_current_document() {
        let first = encoded_png([255, 0, 0, 255], false);
        let second = encoded_png([0, 255, 0, 255], false);
        let first_key = prep_key(&first, 0);
        let second_key = prep_key(&second, 0);
        let mut cache = TexturePrepCache::default();

        crop_textures_cached(&mut LegacyPuppet::new(), &[first], 0, Some(&mut cache)).unwrap();
        assert!(cache.entries.contains_key(&first_key));

        crop_textures_cached(&mut LegacyPuppet::new(), &[second], 0, Some(&mut cache)).unwrap();
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

    /// The two entry points must agree texel for texel: whatever
    /// `crop_textures` writes into a part's mesh, `prepare_textures` +
    /// [`UvCrop::map`] must produce for the same UV. This is what lets the
    /// render cache and the legacy load path sample the same texels.
    #[test]
    fn the_puppet_free_prep_remaps_uvs_exactly_as_the_in_place_one() {
        use crate::components::{Mesh, MeshIndices, Node, PartData};
        use crate::Vec2;

        // A 64x64 PNG with one opaque texel, so the crop is a real window
        // rather than the whole image.
        let mut image = image::RgbaImage::new(64, 64);
        image.put_pixel(40, 33, image::Rgba([10, 20, 30, 255]));
        let mut data = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut data, image::ImageFormat::Png)
            .unwrap();
        let source = ModelTexture {
            format: crate::formats::TextureFormat::Png,
            data: data.into_inner().into(),
            premultiplied: false,
        };

        let uvs = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(40.5 / 64.0, 33.5 / 64.0),
            Vec2::new(1.0, 1.0),
        ];
        let mut puppet = LegacyPuppet::new();
        puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Part(Box::new(PartData {
                    mesh: Mesh::new(
                        vec![Vec2::ZERO; 3],
                        uvs.clone(),
                        MeshIndices::U16(vec![0, 1, 2]),
                        Vec2::ZERO,
                    ),
                    ..Default::default()
                })),
                ..Default::default()
            },
            Some(1),
        );

        let in_place = crop_textures(&mut puppet, std::slice::from_ref(&source), 0).unwrap();
        let rewritten = puppet
            .iter()
            .find_map(|(_, n)| match &n.kind {
                NodeKind::Part(p) => Some(p.mesh.uvs.clone()),
                _ => None,
            })
            .unwrap();

        let prepped = prepare_textures(std::slice::from_ref(&source), 0, None).unwrap();
        assert_eq!(prepped.len(), 1);
        assert_eq!(prepped[0].texture.rgba, in_place[0].rgba);
        let crop = prepped[0].uv_crop.expect("an opaque texel means a crop");
        let mapped: Vec<Vec2> = uvs.iter().map(|&uv| crop.map(uv)).collect();
        assert_eq!(mapped, rewritten);
    }
}
