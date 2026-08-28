//! Import-time texture atlasing: packs the region of each texture that
//! part meshes actually reference (the union UV bounding box) into shared
//! pages, rewriting mesh UVs against the page. Parts that share a page
//! draw with one bound texture, and the unreferenced majority of each
//! source texture (reference rig: 64% of all texels) is never uploaded.
//!
//! Sampling is byte-identical to per-part textures for mip levels 0..=4,
//! including at texture edges and the ClampToBorder transparent skirt.
//! Three invariants make that exact rather than approximate:
//!
//! - Crop origins, sizes, and page placements are all multiples of
//!   [`ALIGN`] = 2^4, so the box-filter footprints of mip levels <= 4
//!   tile inside each placed crop and never straddle its boundary —
//!   every atlas mip texel is computed from exactly the texels the
//!   source texture's own mip used.
//! - Each crop carries a [`MARGIN`]-texel rim of *source content*
//!   (transparent where the source texture ends), because bilinear taps
//!   at mip level k reach ~1.5*2^k texels beyond the UV bbox and read
//!   content there in the source.
//! - Crops are separated by [`ALIGN`] texels of transparency and pages
//!   are zero-initialized, so any tap past a crop's rim returns the
//!   same transparent the source's border (or the page background)
//!   would.
//!
//! Textures whose crop cannot fit a page (or whose UVs are wild enough
//! that the remap would be unsafe) fall back to their own table entry
//! with UVs untouched — pathological inputs degrade to today's
//! behavior instead of failing the import.

use super::error::ImportError;
use crate::components::{NodeId, NodeKind, PuppetTexture, TextureId};
use crate::formats::ModelTexture;
use crate::puppet::Puppet;

pub(crate) const PAGE_SIZE: u32 = 2048;
const ALIGN: i64 = 16;
const MARGIN: i64 = 32;
/// UV bboxes beyond this overshoot are not worth a transparent skirt
/// (the crop would dwarf the content); the texture falls back to its
/// own table entry, where ClampToBorder already handles the overshoot.
const MAX_UV_OVERSHOOT: f32 = 4.0;

#[derive(Debug, Clone, Copy)]
enum TexPlan {
    /// Referenced by no part: dropped from the table entirely.
    Skip,
    /// Decoded as its own table entry, UVs untouched.
    Keep { new_id: u32 },
    /// Cropped into a page; UVs remap as
    /// `(uv * src_dim - src + dst) / PAGE_SIZE`.
    Atlas {
        page: u32,
        dst_x: u32,
        dst_y: u32,
        src_x: i64,
        src_y: i64,
    },
}

/// Decode `textures` (each halved `texture_halvings` times) into atlas
/// pages plus fallback entries, rewriting every part's mesh UVs and
/// albedo id in `puppet`. Returns the new texture table.
pub(crate) fn atlas_textures(
    puppet: &mut Puppet,
    textures: &[ModelTexture],
    texture_halvings: u32,
) -> Result<Vec<PuppetTexture>, ImportError> {
    let dims: Vec<(u32, u32)> = textures
        .iter()
        .map(|t| {
            t.dimensions()
                .map(|(w, h)| halved_dims(w, h, texture_halvings))
                .map_err(|e| ImportError::TextureDecode(e.to_string()))
        })
        .collect::<Result<_, _>>()?;

    let part_ids: Vec<NodeId> = puppet
        .iter()
        .filter(|(_, n)| matches!(n.kind, NodeKind::Part(_)))
        .map(|(id, _)| id)
        .collect();

    // Cumulative rest-pose zsort per node, matching the collector's
    // `parent_z + node.z_order` accumulation. Draw order is ascending
    // cumulative z (back-to-front).
    let mut cumul_z: Vec<f32> = vec![0.0; puppet.len()];
    puppet.tree().traverse_depth_first(|id| {
        let Some(node) = puppet.get(id) else { return };
        let parent_z = puppet
            .tree()
            .get_parent(id)
            .and_then(|p| cumul_z.get(p.0 as usize).copied())
            .unwrap_or(0.0);
        if let Some(slot) = cumul_z.get_mut(id.0 as usize) {
            *slot = parent_z + node.base_z_order;
        }
    });

    let usage = collect_usage(puppet, &part_ids, &cumul_z, textures.len());

    // Pack in draw order — a texture sorts at its first draw (its
    // lowest-z part) — so z-consecutive parts tend to share a page and
    // texture binds collapse.
    let mut order: Vec<usize> = (0..textures.len()).filter(|&i| usage[i].used).collect();
    order.sort_by(|&a, &b| usage[a].min_z.total_cmp(&usage[b].min_z).then(a.cmp(&b)));

    let mut plans = vec![TexPlan::Skip; textures.len()];
    let mut crops: Vec<(usize, i64, i64, u32, u32)> = Vec::new();
    let mut fallback: Vec<usize> = Vec::new();
    for &ti in &order {
        let (tw, th) = dims[ti];
        match crop_rect(&usage[ti], tw, th) {
            Some((x, y, w, h)) => crops.push((ti, x, y, w, h)),
            None => fallback.push(ti),
        }
    }

    let placements = pack(&crops.iter().map(|c| (c.3, c.4)).collect::<Vec<_>>());
    let page_count = placements.iter().map(|p| p.page + 1).max().unwrap_or(0);
    // Trim each page to its occupied extent (plus the ALIGN skirt the
    // footprints already include). Pages need not be square or
    // power-of-two, and shelf tails / a short last page would otherwise
    // upload as dead texels.
    let mut page_dims = vec![(0u32, 0u32); page_count];
    for (&(_, _, _, w, h), pl) in crops.iter().zip(&placements) {
        let d = &mut page_dims[pl.page];
        d.0 = d.0.max(pl.x + w + ALIGN as u32);
        d.1 = d.1.max(pl.y + h + ALIGN as u32);
    }
    for (&(ti, src_x, src_y, _, _), pl) in crops.iter().zip(&placements) {
        plans[ti] = TexPlan::Atlas {
            page: pl.page as u32,
            dst_x: pl.x,
            dst_y: pl.y,
            src_x,
            src_y,
        };
    }
    for (j, &ti) in fallback.iter().enumerate() {
        plans[ti] = TexPlan::Keep {
            new_id: (page_count + j) as u32,
        };
    }

    let mut pages: Vec<Vec<u8>> = page_dims
        .iter()
        .map(|&(w, h)| vec![0u8; (w as usize) * (h as usize) * 4])
        .collect();
    let mut kept: Vec<Option<PuppetTexture>> = vec![None; fallback.len()];
    for (&(ti, src_x, src_y, w, h), pl) in crops.iter().zip(&placements) {
        let tex = decode_halved(&textures[ti], texture_halvings)?;
        let page_w = page_dims[pl.page].0;
        blit(
            &mut pages[pl.page],
            page_w,
            &tex,
            [pl.x, pl.y],
            [src_x, src_y],
            [w, h],
        );
    }
    for (j, &ti) in fallback.iter().enumerate() {
        kept[j] = Some(decode_halved(&textures[ti], texture_halvings)?);
    }

    for id in part_ids {
        let Some(node) = puppet.get_mut(id) else {
            continue;
        };
        let NodeKind::Part(part) = &mut node.kind else {
            continue;
        };
        let ti = part.albedo_texture.0 as usize;
        if ti >= plans.len() {
            continue;
        }
        match plans[ti] {
            TexPlan::Skip => {}
            TexPlan::Keep { new_id } => part.albedo_texture = TextureId(new_id),
            TexPlan::Atlas {
                page,
                dst_x,
                dst_y,
                src_x,
                src_y,
            } => {
                let (tw, th) = dims[ti];
                let (pw, ph) = page_dims[page as usize];
                let off_x = dst_x as f64 - src_x as f64;
                let off_y = dst_y as f64 - src_y as f64;
                for uv in &mut part.mesh.uvs {
                    uv.x = ((uv.x as f64 * tw as f64 + off_x) / pw as f64) as f32;
                    uv.y = ((uv.y as f64 * th as f64 + off_y) / ph as f64) as f32;
                }
                part.albedo_texture = TextureId(page);
            }
        }
    }

    let mut table: Vec<PuppetTexture> = pages
        .into_iter()
        .zip(&page_dims)
        .map(|(rgba, &(w, h))| PuppetTexture {
            width: w,
            height: h,
            rgba: rgba.into(),
        })
        .collect();
    table.extend(kept.into_iter().flatten());
    Ok(table)
}

struct TexUsage {
    used: bool,
    /// Whether every referencing UV was finite and within the overshoot
    /// bound; false forces the fallback path.
    sane: bool,
    lo: glam::Vec2,
    hi: glam::Vec2,
    min_z: f32,
}

fn collect_usage(puppet: &Puppet, part_ids: &[NodeId], cumul_z: &[f32], n: usize) -> Vec<TexUsage> {
    let mut usage: Vec<TexUsage> = (0..n)
        .map(|_| TexUsage {
            used: false,
            sane: true,
            lo: glam::Vec2::splat(f32::MAX),
            hi: glam::Vec2::splat(f32::MIN),
            min_z: f32::MAX,
        })
        .collect();
    for &id in part_ids {
        let Some(node) = puppet.get(id) else { continue };
        let NodeKind::Part(part) = &node.kind else {
            continue;
        };
        let ti = part.albedo_texture.0 as usize;
        let Some(u) = usage.get_mut(ti) else { continue };
        u.used = true;
        let z = cumul_z.get(id.0 as usize).copied().unwrap_or(0.0);
        u.min_z = u.min_z.min(z);
        if part.mesh.uvs.is_empty() {
            // No UVs means nothing to remap — whatever such a part samples
            // must keep sampling the unmoved original.
            u.sane = false;
        }
        for uv in &part.mesh.uvs {
            if !uv.x.is_finite() || !uv.y.is_finite() {
                u.sane = false;
                continue;
            }
            if uv.x < -MAX_UV_OVERSHOOT
                || uv.x > 1.0 + MAX_UV_OVERSHOOT
                || uv.y < -MAX_UV_OVERSHOOT
                || uv.y > 1.0 + MAX_UV_OVERSHOOT
            {
                u.sane = false;
            }
            u.lo = u.lo.min(*uv);
            u.hi = u.hi.max(*uv);
        }
    }
    usage
}

/// Aligned crop rect (origin may be negative; the blit fills
/// out-of-source regions with transparency). `None` when the texture
/// must fall back to its own table entry.
fn crop_rect(u: &TexUsage, tw: u32, th: u32) -> Option<(i64, i64, u32, u32)> {
    if !u.sane {
        return None;
    }
    let x0 = align_down((u.lo.x as f64 * tw as f64).floor() as i64 - MARGIN);
    let y0 = align_down((u.lo.y as f64 * th as f64).floor() as i64 - MARGIN);
    let x1 = align_up((u.hi.x as f64 * tw as f64).ceil() as i64 + MARGIN);
    let y1 = align_up((u.hi.y as f64 * th as f64).ceil() as i64 + MARGIN);
    let (w, h) = (x1 - x0, y1 - y0);
    let max = (PAGE_SIZE as i64) - ALIGN;
    if w <= 0 || h <= 0 || w > max || h > max {
        return None;
    }
    Some((x0, y0, w as u32, h as u32))
}

pub(crate) fn align_down(v: i64) -> i64 {
    v.div_euclid(ALIGN) * ALIGN
}

pub(crate) fn align_up(v: i64) -> i64 {
    (v + ALIGN - 1).div_euclid(ALIGN) * ALIGN
}

#[derive(Debug, Clone, Copy)]
struct Placement {
    page: usize,
    x: u32,
    y: u32,
}

/// Page assignment follows the caller's (draw) order — a z-run of parts
/// stays on one page and binds once — while placement *within* a page is
/// free, so each page's batch is shelf-packed height-sorted
/// (first-fit-decreasing) for density: crops are batched greedily until
/// the batch no longer lays out in one page. Footprints are inflated by
/// [`ALIGN`] on the right/bottom, which provides the inter-crop
/// transparent separation and keeps placements 16-aligned.
fn pack(sizes: &[(u32, u32)]) -> Vec<Placement> {
    let mut out = vec![
        Placement {
            page: 0,
            x: 0,
            y: 0
        };
        sizes.len()
    ];
    let mut page = 0usize;
    let mut batch: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < sizes.len() {
        batch.push(i);
        match shelf_layout(sizes, &batch) {
            Some(layout) => {
                for (j, x, y) in layout {
                    out[j] = Placement { page, x, y };
                }
                i += 1;
            }
            // `crop_rect` caps every footprint at one page, so a batch
            // of one always lays out; the guard keeps an oversized crop
            // from looping if that contract ever drifts.
            None if batch.len() == 1 => {
                out[i] = Placement { page, x: 0, y: 0 };
                page += 1;
                batch.clear();
                i += 1;
            }
            None => {
                page += 1;
                batch.clear();
            }
        }
    }
    out
}

/// Height-sorted shelf layout of `batch` into a single page, or `None`
/// when it doesn't fit.
fn shelf_layout(sizes: &[(u32, u32)], batch: &[usize]) -> Option<Vec<(usize, u32, u32)>> {
    let mut order: Vec<usize> = batch.to_vec();
    order.sort_by_key(|&j| (std::cmp::Reverse(sizes[j].1), j));
    let mut shelves: Vec<(u32, u32, u32)> = Vec::new(); // (y, height, next_x)
    let mut next_y = 0u32;
    let mut out = Vec::with_capacity(order.len());
    for j in order {
        let (fw, fh) = (sizes[j].0 + ALIGN as u32, sizes[j].1 + ALIGN as u32);
        let slot = shelves
            .iter_mut()
            .find(|s| fh <= s.1 && s.2 + fw <= PAGE_SIZE);
        match slot {
            Some(s) => {
                out.push((j, s.2, s.0));
                s.2 += fw;
            }
            None => {
                if next_y + fh > PAGE_SIZE || fw > PAGE_SIZE {
                    return None;
                }
                out.push((j, 0, next_y));
                shelves.push((next_y, fh, fw));
                next_y += fh;
            }
        }
    }
    Some(out)
}

fn halved_dims(mut w: u32, mut h: u32, k: u32) -> (u32, u32) {
    for _ in 0..k {
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    (w, h)
}

pub(crate) fn decode_halved(
    tex: &ModelTexture,
    halvings: u32,
) -> Result<PuppetTexture, ImportError> {
    let mut decoded = tex
        .decode()
        .map_err(|e| ImportError::TextureDecode(e.to_string()))?;
    for _ in 0..halvings {
        decoded = decoded.halved();
    }
    Ok(decoded)
}

/// Copy the crop rect `(src[0], src[1], size[0], size[1])` of `tex` to `dst`
/// in `page` (row stride `page_w`), leaving out-of-source texels at the
/// page's transparent zero-fill (the ClampToBorder equivalent).
pub(crate) fn blit(
    page: &mut [u8],
    page_w: u32,
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
        let dst_off = ((dst_row * page_w as i64 + dst_col) * 4) as usize;
        page[dst_off..dst_off + src_len].copy_from_slice(&tex.rgba[src_off..src_off + src_len]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_places_within_pages_without_overlap() {
        let sizes: Vec<(u32, u32)> = (0..40)
            .map(|i| (96 + (i % 7) * 160, 96 + (i % 5) * 240))
            .collect();
        let placements = pack(&sizes);
        assert_eq!(placements.len(), sizes.len());
        for (p, &(w, h)) in placements.iter().zip(&sizes) {
            assert_eq!(p.x % 16, 0);
            assert_eq!(p.y % 16, 0);
            assert!(p.x + w <= PAGE_SIZE && p.y + h <= PAGE_SIZE);
        }
        for i in 0..sizes.len() {
            for j in (i + 1)..sizes.len() {
                let (a, b) = (placements[i], placements[j]);
                if a.page != b.page {
                    continue;
                }
                // Footprints include the ALIGN separation.
                let (aw, ah) = (sizes[i].0 + ALIGN as u32, sizes[i].1 + ALIGN as u32);
                let (bw, bh) = (sizes[j].0 + ALIGN as u32, sizes[j].1 + ALIGN as u32);
                let disjoint =
                    a.x + aw <= b.x || b.x + bw <= a.x || a.y + ah <= b.y || b.y + bh <= a.y;
                assert!(disjoint, "crops {i} and {j} overlap");
            }
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
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for (i, b) in rgba.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let tex = PuppetTexture {
            width: 4,
            height: 4,
            rgba: rgba.clone().into(),
        };
        let mut page = vec![0u8; (PAGE_SIZE * PAGE_SIZE * 4) as usize];
        blit(&mut page, PAGE_SIZE, &tex, [32, 48], [-16, -16], [48, 48]);
        for y in 0..4i64 {
            for x in 0..4i64 {
                let src = ((y * 4 + x) * 4) as usize;
                let dst = (((48 + 16 + y) * PAGE_SIZE as i64 + 32 + 16 + x) * 4) as usize;
                assert_eq!(&page[dst..dst + 4], &rgba[src..src + 4]);
            }
        }
        // A texel in the transparent rim.
        let rim = ((48 * PAGE_SIZE as i64 + 32) * 4) as usize;
        assert_eq!(&page[rim..rim + 4], &[0, 0, 0, 0]);
    }
}
