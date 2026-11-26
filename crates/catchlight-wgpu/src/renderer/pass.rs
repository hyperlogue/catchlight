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

/// One list's recording: the encoder, the frame's scratch targets and the
/// target being drawn into. A composite draws its children through
/// [`Self::with_target`] on its slot.
pub(super) struct PassCtx<'a> {
    pub(super) encoder: &'a mut wgpu::CommandEncoder,
    pub(super) list: &'a crate::collect::RenderList,
    pub(super) stencil: &'a StencilTarget,
    pub(super) composites: &'a mut CompositePool,
    /// `None` when no dst-in-shader blend will read the target.
    pub(super) snapshots: Option<&'a mut FramebufferSnapshotPool>,
    pub(super) target: RenderTarget<'a>,
    pub(super) labels: &'static PassLabels,
}

impl PassCtx<'_> {
    pub(super) fn with_target<'b>(
        &'b mut self,
        target: RenderTarget<'b>,
        labels: &'static PassLabels,
    ) -> PassCtx<'b> {
        PassCtx {
            encoder: &mut *self.encoder,
            list: self.list,
            stencil: self.stencil,
            composites: &mut *self.composites,
            snapshots: self.snapshots.as_deref_mut(),
            target,
            labels,
        }
    }

    /// Dst-in-shader blends read the target only with a snapshot pool and a
    /// copyable target; otherwise they draw OVER.
    pub(super) fn shader_blends(&self) -> bool {
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
