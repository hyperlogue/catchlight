use super::*;

/// The labels one target's passes carry in a GPU capture.
pub(super) struct PassLabels {
    pub(super) draw: &'static str,
    pub(super) mask_write: &'static str,
    pub(super) masked_draw: &'static str,
    pub(super) fallback_clear: &'static str,
}

pub(super) const ROOT_PASSES: PassLabels = PassLabels {
    draw: "Render Pass",
    mask_write: "Mask Write Pass",
    masked_draw: "Masked Part Render Pass",
    fallback_clear: "render_list fallback clear",
};

pub(super) const COMPOSITE_PASSES: PassLabels = PassLabels {
    draw: "Composite Children Pass",
    mask_write: "Composite Mask Write Pass",
    masked_draw: "Composite Masked Part Render Pass",
    fallback_clear: "Composite Fallback Clear Pass",
};

/// A dst-in-shader part's source, drawn alone into a composite slot.
pub(super) const SLOT_PASSES: PassLabels = PassLabels {
    draw: "Composite Slot Pass (dst-in-shader)",
    mask_write: "Composite Slot Mask Write (dst-in-shader)",
    masked_draw: "Composite Slot Masked Part (dst-in-shader)",
    fallback_clear: "Composite Slot Fallback Clear (dst-in-shader)",
};

/// A view a pass draws into, and the rectangle of it the frame covers.
#[derive(Clone, Copy)]
pub(super) struct Dest<'a> {
    pub(super) view: &'a wgpu::TextureView,
    /// Present when dst-in-shader blends may copy from the view.
    pub(super) texture: Option<&'a wgpu::Texture>,
    /// In the view's pixels. Composite slots are its size, and every bound
    /// and scissor rect is in its pixels.
    pub(super) viewport: ScreenRect,
    /// Whether `viewport` leaves part of the view uncovered.
    pub(super) partial: bool,
    /// Sized to the view.
    pub(super) mask: &'a StencilTarget,
}

impl Dest<'_> {
    pub(super) const fn width(&self) -> u32 {
        self.viewport.width
    }

    pub(super) const fn height(&self) -> u32 {
        self.viewport.height
    }

    /// Confines `pass` to the viewport and, when given, to `scissor`.
    pub(super) fn confine(&self, pass: &mut wgpu::RenderPass<'_>, scissor: Option<ScreenRect>) {
        let v = self.viewport;
        if self.partial {
            pass.set_viewport(
                v.x as f32,
                v.y as f32,
                v.width as f32,
                v.height as f32,
                0.0,
                1.0,
            );
        }
        match scissor {
            Some(r) => pass.set_scissor_rect(v.x + r.x, v.y + r.y, r.width, r.height),
            None if self.partial => pass.set_scissor_rect(v.x, v.y, v.width, v.height),
            None => {}
        }
    }
}

/// One list's recording: the encoder, the frame's scratch targets and the
/// target being drawn into. A composite draws its children through
/// [`Self::with_target`] on its slot.
pub(super) struct PassCtx<'a> {
    pub(super) encoder: &'a mut wgpu::CommandEncoder,
    pub(super) list: &'a crate::collect::RenderList,
    /// The mask target composite slots draw with.
    pub(super) slot_mask: &'a StencilTarget,
    pub(super) composites: &'a mut CompositePool,
    /// `None` when no dst-in-shader blend will read the target.
    pub(super) snapshots: Option<&'a mut FramebufferSnapshotPool>,
    pub(super) target: Dest<'a>,
    pub(super) labels: &'static PassLabels,
}

impl PassCtx<'_> {
    pub(super) fn with_target<'b>(
        &'b mut self,
        target: Dest<'b>,
        labels: &'static PassLabels,
    ) -> PassCtx<'b> {
        PassCtx {
            encoder: &mut *self.encoder,
            list: self.list,
            slot_mask: self.slot_mask,
            composites: &mut *self.composites,
            snapshots: self.snapshots.as_deref_mut(),
            target,
            labels,
        }
    }

    /// `slot` as a target: the viewport's size, at its origin.
    pub(super) const fn slot<'s>(&self, slot: &'s CompositeTexture) -> Dest<'s>
    where
        Self: 's,
    {
        Dest {
            view: &slot.view,
            texture: Some(&slot.texture),
            viewport: ScreenRect {
                x: 0,
                y: 0,
                ..self.target.viewport
            },
            partial: false,
            mask: self.slot_mask,
        }
    }

    /// Dst-in-shader blends read the target only with a snapshot pool and a
    /// copyable target; otherwise they draw OVER.
    pub(super) const fn shader_blends(&self) -> bool {
        self.snapshots.is_some() && self.target.texture.is_some()
    }
}

/// A composite slot blitted onto a target.
#[derive(Clone, Copy)]
pub(super) struct Blit<'s> {
    pub(super) source: &'s CompositeTexture,
    pub(super) part_uniform_offset: u32,
    pub(super) scissor: Option<ScreenRect>,
}
