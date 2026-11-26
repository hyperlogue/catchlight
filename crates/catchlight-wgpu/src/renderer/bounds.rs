use super::*;

/// AABB in a mesh's local (origin-shifted) space, before the instance
/// model matrix.
#[derive(Copy, Clone, Debug)]
pub(super) struct Aabb2 {
    pub(super) min: glam::Vec2,
    pub(super) max: glam::Vec2,
}

/// Framebuffer pixels, within the viewport.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScreenRect {
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) width: u32,
    pub(super) height: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum ScreenBounds {
    Empty,
    Unbounded,
    Rect(ScreenRect),
}

impl ScreenBounds {
    pub(super) fn from_pixels(bounds: Option<[f32; 4]>, width: u32, height: u32) -> Self {
        match bounds {
            None => Self::Unbounded,
            Some(bounds) => {
                pixels_to_scissor(bounds, width, height).map_or(Self::Empty, Self::Rect)
            }
        }
    }

    pub(super) fn scissor(self) -> Option<ScreenRect> {
        match self {
            Self::Rect(rect) => Some(rect),
            Self::Empty | Self::Unbounded => None,
        }
    }
}

/// Absorbs floor/ceil rounding and rasterization slack.
pub(super) const SCISSOR_PAD: f32 = 2.0;

/// Returns `[min_x, min_y, max_x, max_y]` in framebuffer pixels, or None when
/// a corner is non-finite or on/behind `w ≈ 0` (callers use the full viewport).
pub(super) fn project_aabb_to_pixels(
    local: Aabb2,
    mvp: glam::Mat4,
    width: f32,
    height: f32,
) -> Option<[f32; 4]> {
    let corners = [
        glam::Vec4::new(local.min.x, local.min.y, 0.0, 1.0),
        glam::Vec4::new(local.max.x, local.min.y, 0.0, 1.0),
        glam::Vec4::new(local.min.x, local.max.y, 0.0, 1.0),
        glam::Vec4::new(local.max.x, local.max.y, 0.0, 1.0),
    ];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for corner in corners {
        let clip = mvp * corner;
        if !clip.w.is_finite() || clip.w <= 1e-6 {
            return None;
        }
        // NDC +Y is up; framebuffer y grows downward.
        let px = (clip.x / clip.w * 0.5 + 0.5) * width;
        let py = (0.5 - clip.y / clip.w * 0.5) * height;
        if !px.is_finite() || !py.is_finite() {
            return None;
        }
        min_x = min_x.min(px);
        min_y = min_y.min(py);
        max_x = max_x.max(px);
        max_y = max_y.max(py);
    }
    Some([min_x, min_y, max_x, max_y])
}

/// None when the padded bounds do not intersect the viewport.
pub(super) fn pixels_to_scissor(aabb: [f32; 4], width: u32, height: u32) -> Option<ScreenRect> {
    let w = width as f32;
    let h = height as f32;
    let min_x = (aabb[0] - SCISSOR_PAD).floor().clamp(0.0, w);
    let min_y = (aabb[1] - SCISSOR_PAD).floor().clamp(0.0, h);
    let max_x = (aabb[2] + SCISSOR_PAD).ceil().clamp(0.0, w);
    let max_y = (aabb[3] + SCISSOR_PAD).ceil().clamp(0.0, h);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    Some(ScreenRect {
        x: min_x as u32,
        y: min_y as u32,
        width: (max_x - min_x) as u32,
        height: (max_y - min_y) as u32,
    })
}

impl WgpuRenderer {
    /// None when the bound is unknown or the projection is non-finite; callers
    /// then use the full viewport.
    pub(super) fn part_pixel_aabb(
        &self,
        mesh_id: u32,
        model: glam::Mat4,
        width: u32,
        height: u32,
    ) -> Option<[f32; 4]> {
        let local = self.mesh_buffers.get(mesh_id as usize)?.local_bounds?;
        // The deforms of the set being recorded, not of some other puppet's:
        // two puppets of one model expand the same mesh by different amounts.
        let pad = self
            .deform_sets
            .get(self.deform_recording_set as usize)
            .and_then(|set| set.max.get(mesh_id as usize))
            .copied()
            .unwrap_or(0.0);
        let expanded = Aabb2 {
            min: local.min - glam::Vec2::splat(pad),
            max: local.max + glam::Vec2::splat(pad),
        };
        project_aabb_to_pixels(
            expanded,
            self.camera_view_proj * model,
            width as f32,
            height as f32,
        )
    }

    pub(super) fn part_screen_bounds(
        &self,
        mesh_id: u32,
        model: glam::Mat4,
        width: u32,
        height: u32,
    ) -> ScreenBounds {
        ScreenBounds::from_pixels(
            self.part_pixel_aabb(mesh_id, model, width, height),
            width,
            height,
        )
    }

    pub(super) fn drawable_screen_bounds(
        &mut self,
        drawable: &crate::collect::DrawableInfo,
        render_list: &crate::collect::RenderList,
        width: u32,
        height: u32,
    ) -> ScreenBounds {
        use crate::collect::DrawableInfo;
        match drawable {
            DrawableInfo::Part {
                mesh_id, transform, ..
            } => self.part_screen_bounds(*mesh_id, *transform, width, height),
            DrawableInfo::Composite {
                node_id,
                blend_mode,
                ..
            } => {
                if render_list.composite_children(*node_id).is_empty() {
                    return ScreenBounds::Empty;
                }
                let bounds = self.composite_pixel_aabb(*node_id, render_list, width, height);
                if blend_transparent_src_is_identity(*blend_mode) {
                    ScreenBounds::from_pixels(bounds, width, height)
                } else {
                    ScreenBounds::Unbounded
                }
            }
        }
    }

    /// Bounds the nontransparent contents, not the area a blend can affect.
    /// A Darken blit must still cover the whole target.
    pub(super) fn composite_pixel_aabb(
        &mut self,
        node_id: u32,
        render_list: &crate::collect::RenderList,
        width: u32,
        height: u32,
    ) -> Option<[f32; 4]> {
        use crate::collect::DrawableInfo;
        if let Some(bounds) = self.composite_bounds.get(node_id as usize) {
            return *bounds;
        }
        let bounds = (|| {
            let mut acc: Option<[f32; 4]> = None;
            for child in render_list.composite_children(node_id) {
                let aabb = match child {
                    DrawableInfo::Part {
                        mesh_id, transform, ..
                    } => self.part_pixel_aabb(*mesh_id, *transform, width, height)?,
                    DrawableInfo::Composite { node_id, .. } => {
                        if render_list.composite_children(*node_id).is_empty() {
                            continue;
                        }
                        self.composite_pixel_aabb(*node_id, render_list, width, height)?
                    }
                };
                acc = Some(match acc {
                    None => aabb,
                    Some(a) => [
                        a[0].min(aabb[0]),
                        a[1].min(aabb[1]),
                        a[2].max(aabb[2]),
                        a[3].max(aabb[3]),
                    ],
                });
            }
            acc
        })();
        self.composite_bounds.insert(node_id as usize, bounds);
        bounds
    }
}
