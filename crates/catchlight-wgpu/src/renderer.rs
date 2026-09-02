//! Draws a `RenderList` into a caller-supplied command encoder.
//!
//! `renderer.rs` is one ~5.7k-line file on purpose. Every
//! `queue.write_buffer` and every buffer allocation is visible in one place,
//! which is how the aliasing bugs below were caught — twice. It gets split
//! once targeted renderer tests cover the invariants, not before.
//!
//! - **`write_buffer` batches at submit start.** Two writes to the same
//!   buffer offset inside one frame both land before the submit, and the
//!   later one wins for *every* draw that reads that range — silently, and
//!   for the wrong parts. So each buffer offset is written exactly once per
//!   frame. Per-part instance and uniform data is staged in CPU-side buffers
//!   and flushed as a single `write_buffer` at frame end
//!   (`flush_instance_writes`, `flush_part_uniform_writes`, both called from
//!   `render_list_ext`).
//! - **Cursor allocation, never a bare offset 0.** Take instance slots with
//!   `reserve_instances(count)` and uniform slots with `write_part_uniform(..)`;
//!   both hand out offsets from a monotonic per-frame cursor. A helper that
//!   writes offset 0 itself reintroduces the aliasing above.
//! - **One submit per frame.** `render_list` / `render_list_ext` record into
//!   the *caller's* encoder and submit nothing; the caller's submit is the
//!   frame's only one. The only `queue.submit` inside the renderer is
//!   `generate_mips`, at texture-upload time.
//! - **Never grow a GPU buffer mid-frame.** `begin_frame_instances` and
//!   `begin_frame_uniforms` size the frame up front, before any pass is
//!   recorded. A realloc after that strands already-recorded passes on the
//!   freed buffer.
//! - **One camera slot per view.** `reserve_camera` writes each
//!   `render_list`'s view-proj into its own slot of a `CAMERA_RING_SLOTS`-deep
//!   ring and binds it as a dynamic offset, so views sharing a submit can't
//!   alias. `begin_camera_submit` resets the count at the *external*
//!   submission boundary — do not call it between `render_list` calls that
//!   will be submitted together.
//! - **Masking blends need matching color and alpha factors.** A mode whose
//!   color component masks via `DstAlpha` or `Zero` (`ClipToLower`,
//!   `SliceFromLower`, and likewise `Multiply` / `ColorDodge`) must use the
//!   same factors on alpha. Alpha falling through to OVER writes α=1 where
//!   color is 0, producing opaque-black halos — invisible at identity pose,
//!   visible once a deform shrinks the mask. Pinned by
//!   `masking_blend_modes_have_matching_color_and_alpha_factors`.
//! - **Multi-puppet resource sharing.** `StencilTarget`, `CompositePool` and
//!   `FramebufferSnapshotPool` are caller-owned and passed into
//!   `render_list_ext`, so several puppets in one frame share them. Each
//!   puppet still gets its own `WgpuRenderer` (mesh ids from different puppets
//!   would collide in `mesh_buffers`) — see
//!   `crates/catchlight-bevy/src/prepare.rs` (`HashMap<RendererKey,
//!   WgpuRenderer>` beside one `FormatResources`) and
//!   `crates/visual-tests/src/harness.rs`, which serializes every render
//!   through one mutex for the same reason.
//! - **The stencil path has a WebGL fallback.** When `Pipelines::has_stencil`
//!   is false (Chromium swiftshader WebGL2 fails `Depth24PlusStencil8`),
//!   mask/masked draws sample `StencilTarget::mask_alpha_view` instead of
//!   stencil-testing. A masking change has to hold on both paths.
//! - **`base_instance` is native-only.** Part draws select an instance with a
//!   non-zero `first_instance` on Vulkan/DX12/Metal and re-slice vertex buffer
//!   1 per draw on GL/WebGL2 and the adapter-less constructors
//!   (`emit_part_draw`).
//!
//! Most of these are invisible to pixels, so they are pinned directly: the
//! renderer keeps per-frame lifecycle counters (queue writes and buffer
//! reallocs per buffer, slots reserved vs written) and `debug_assert!`s that
//! every staged write starts at or above a monotonic watermark. Targeted
//! per-invariant tests live in `crates/catchlight-wgpu/tests/`;
//! `crates/visual-tests` covers the ones that do reach pixels.

use catchlight_core::{BlendMode, DecodedTexture};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use thiserror::Error;
use wgpu::util::DeviceExt;

mod pipelines;

#[derive(Debug, Error)]
pub enum RendererError {
    #[error("failed to decode texture image: {0}")]
    ImageDecode(#[from] image::ImageError),

    #[error("blit pipeline missing for blend mode {0:?}")]
    MissingBlitPipeline(BlendMode),

    #[error("masked pipeline missing for blend mode {0:?}")]
    MissingMaskedPipeline(BlendMode),

    #[error("mesh {mesh_id}: vertices.len() ({vertices}) != uvs.len() ({uvs})")]
    MeshVertexUvMismatch {
        mesh_id: u32,
        vertices: usize,
        uvs: usize,
    },

    #[error("mesh {mesh_id}: index {index} out of bounds ({vertices} vertices)")]
    MeshIndexOutOfBounds {
        mesh_id: u32,
        index: u32,
        vertices: usize,
    },

    #[error("texture dimensions {width}x{height} are outside the supported range 1..={limit}")]
    TextureDimensionsOutOfRange { width: u32, height: u32, limit: u32 },

    #[error("texture {width}x{height} RGBA8 byte length overflows the host address space")]
    TextureByteLengthOverflow { width: u32, height: u32 },

    #[error(
        "texture {width}x{height} RGBA8 byte length mismatch: expected {expected}, got {actual}"
    )]
    TextureByteLengthMismatch {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },

    #[error("a single GPU submission cannot contain more than {limit} camera views per renderer")]
    TooManyCameraViews { limit: u32 },

    #[error("preparing a model's textures: {0}")]
    TexturePrep(String),
}

pub type RendererResult<T> = Result<T, RendererError>;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RenderStats {
    pub drawn_parts: u32,
    pub drawn_composites: u32,
    pub skipped_missing_mesh: u32,
    pub skipped_missing_texture: u32,
    pub skipped_missing_mask_mesh: u32,
    pub skipped_missing_mask_texture: u32,
    /// Number of texture bind-group rebinds emitted in the main render
    /// pass. Below drawn_parts when consecutive parts share a texture.
    pub texture_binds: u32,
    /// Number of pipeline swaps in the main render pass. Below
    /// drawn_parts when consecutive parts share a blend mode.
    pub pipeline_swaps: u32,
    /// `encoder.begin_render_pass` calls this frame, including
    /// mask-write, blit, and no-draw clear passes. The dominant
    /// driver-CPU cost on tiled / browser backends; the metric the
    /// pass-batching work in `render_list` targets.
    pub render_passes: u32,
    /// `draw` / `draw_indexed` calls this frame across all passes.
    pub total_draw_calls: u32,
    /// Bytes written into `instance_buffer` this frame (the single
    /// batched `write_buffer` flushed at frame end).
    pub instance_bytes_uploaded: u64,
    /// Bytes written into the deform atlas by the preceding
    /// [`WgpuRenderer::upload_deforms`]. Zero on a static frame where every
    /// resident deform generation already matched.
    pub deform_bytes_uploaded: u64,
    /// Offscreen composite slots acquired this frame (peak concurrent
    /// is the pool's reuse cursor — non-nested composites share a slot).
    pub composite_slots_used: u32,
}

/// Resource-lifecycle counters for the current frame, reset at every
/// `render_list` entry and readable afterwards via
/// [`WgpuRenderer::frame_stats`]. Plain integers bumped unconditionally —
/// no allocation, no timing — so tests can pin the buffer invariants that
/// pixels cannot see: one queue write per buffer per frame, no growth
/// after the frame is sized, and one write per reserved slot.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    /// `queue.submit` calls the renderer itself issued since the frame
    /// started. The frame path records into the caller's encoder and
    /// submits nothing, so this reads 0 after `render_list`: the caller's
    /// submit is the frame's only one. Non-zero only when an upload that
    /// submits (texture mip generation) ran inside the frame.
    pub queue_submits: u32,
    /// `queue.write_buffer` calls made this frame, all buffers.
    pub queue_writes: u32,
    /// `queue.write_buffer` calls targeting `instance_buffer`. At most 1:
    /// per-part instance data is staged and flushed once at frame end,
    /// because `write_buffer` batches at submit start and a second write
    /// to a live offset would win for every draw that reads it.
    pub instance_buffer_writes: u32,
    /// `queue.write_buffer` calls targeting `part_uniform_buffer`. At most
    /// 1, for the same reason as `instance_buffer_writes`.
    pub part_uniform_buffer_writes: u32,
    /// `queue.write_buffer` calls targeting `camera_buffer` — one per
    /// `render_list`, into that view's own ring slot.
    pub camera_buffer_writes: u32,
    /// `queue.write_buffer` calls targeting the deform atlas. Deforms
    /// upload from [`WgpuRenderer::upload_deforms`], outside the frame, so
    /// this reads 0.
    pub deform_buffer_writes: u32,
    /// Times `instance_buffer` was recreated this frame. Only
    /// `begin_frame_instances` may do this, so: 1 on the first frame that
    /// outgrows the current capacity, 0 on every later frame of that size.
    pub instance_buffer_reallocs: u32,
    /// Times `part_uniform_buffer` was recreated this frame. Same
    /// discipline as `instance_buffer_reallocs`.
    pub part_uniform_buffer_reallocs: u32,
    /// Times the deform atlas was recreated this frame. Deform sizing
    /// happens at mesh-upload time, so this reads 0 within a frame.
    pub deform_buffer_reallocs: u32,
    /// Buffer recreations that happened after the frame's sizing phase
    /// closed. **Always 0**: growing mid-frame strands already-recorded
    /// passes on the freed buffer.
    pub late_buffer_reallocs: u32,
    /// Instance slots `begin_frame_instances` sized the frame for.
    pub instance_slots_budgeted: u32,
    /// Instance slots handed out by `reserve_instances` this frame; never
    /// above `instance_slots_budgeted`.
    pub instance_slots_reserved: u32,
    /// Instance slots actually written. Equals `instance_slots_reserved`
    /// when every reserved slot is filled exactly once.
    pub instance_slots_written: u32,
    /// Bytes staged into `instance_buffer` this frame.
    pub instance_bytes_written: u64,
    /// Part-uniform slots `begin_frame_uniforms` sized the frame for.
    pub part_uniform_slots_budgeted: u32,
    /// `write_part_uniform` calls this frame — one per slot consumed,
    /// never above `part_uniform_slots_budgeted`.
    pub part_uniform_writes: u32,
}

/// Which renderer-owned GPU buffer a `FrameStats` write / realloc tally
/// belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderBuffer {
    Instance,
    PartUniform,
    Camera,
    Deform,
}

fn blend_mode_to_wgpu(mode: BlendMode) -> wgpu::BlendState {
    match mode {
        // Premultiplied alpha: src already has RGB * A, so use One for src factor
        BlendMode::Normal => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        },
        BlendMode::Multiply => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            // Alpha uses the same factors as color. OVER would write
            // srcA over a transparent composite slot (opaque black halo).
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        },
        BlendMode::Screen => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrc,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        },
        // Alpha must multiply by DstAlpha too. With OVER (src*1 + dst*(1-srcA))
        // the alpha outside the mask becomes 1 while the color term is
        // already 0, producing opaque-black pixels wherever the clipped
        // mesh extends past its mask. Invisible when the mask and content
        // stay aligned (identity pose), visible as black halos as soon as
        // a deform shrinks the mask — e.g. an eye mask at blink=1.
        BlendMode::ClipToLower => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::DstAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::DstAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        },
        BlendMode::SliceFromLower => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        },
        // ColorDodge: additive with destination color influence
        BlendMode::ColorDodge => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            // Alpha uses the same destination-color and additive factors.
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        },
        // LinearDodge (Add): blends color with inverse src color for softer highlight
        // Reference: glBlendFuncSeparate(GL_ONE, GL_ONE_MINUS_SRC_COLOR, GL_ONE, GL_ONE_MINUS_SRC_ALPHA)
        BlendMode::LinearDodge => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrc,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        },
        // Pure additive: src + dst (both color and alpha).
        BlendMode::Add => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        },
        // dst - src (ReverseSubtract): darkens the framebuffer by the
        // src contribution, clipped to [0, 1] by the hardware.
        BlendMode::Subtract => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::ReverseSubtract,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::ReverseSubtract,
            },
        },
        // min(src, dst): per-channel darken.
        BlendMode::Darken => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Min,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Min,
            },
        },
        // max(src, dst): per-channel lighten.
        BlendMode::Lighten => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Max,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Max,
            },
        },
        // Inverse is directly expressible as fixed-function
        // (BlendFactor::OneMinusDst's alpha component is 1-dst.a, matching
        // GL's GL_ONE_MINUS_DST_COLOR alpha behaviour).
        BlendMode::Inverse => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::OneMinusDst,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::OneMinusDst,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        },
        // Overlay / ColorBurn / LinearBurn can't be expressed as a single
        // fixed-function wgpu BlendState — the math reads the destination.
        // The dispatcher in `render_list` routes these through
        // `blit_composite_dst_in_shader`, which renders the src into a
        // composite slot, snapshots the framebuffer, then runs a
        // shader-math blit pipeline that emits the final pixel via
        // `BlendState::REPLACE`. The Normal-OVER returned here is only
        // used by the unmasked composite-child pipeline that renders the
        // src into the offscreen slot, so the src lands premultiplied —
        // matching what every other blit path expects.
        BlendMode::Overlay | BlendMode::ColorBurn | BlendMode::LinearBurn => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        },
    }
}

/// True for blend modes whose math reads the destination color and
/// can't be expressed as a single wgpu fixed-function BlendState.
/// `render_list` routes these through the composite +
/// framebuffer-snapshot path; every other mode renders directly to the
/// main color attachment with hardware blending.
fn is_dst_in_shader(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Overlay | BlendMode::ColorBurn | BlendMode::LinearBurn,
    )
}

/// Pipeline-lookup key for a part draw. The dst-in-shader trio's
/// fixed-function state *is* Normal's OVER (their shader math only
/// runs in the snapshot blit path), so binding Normal's pipeline
/// object avoids spurious pipeline swaps when a run alternates e.g.
/// Normal and ColorBurn parts.
fn canonical_part_blend(mode: BlendMode) -> BlendMode {
    if is_dst_in_shader(mode) {
        BlendMode::Normal
    } else {
        mode
    }
}

/// True when a mode's fixed-function blend is plain premultiplied OVER
/// (`src = One`, `dst = OneMinusSrcAlpha` on both color and alpha). OVER
/// is associative: `(a OVER b) OVER c == a OVER (b OVER c)`. That is the
/// exact condition under which a composite's children can be drawn
/// straight to the parent target instead of into an offscreen slot that
/// is then OVER-blitted — the root-composite flattening fast path. The
/// dst-in-shader trio qualifies here only for its no-snapshot fallback
/// (where it degrades to OVER); when the snapshot path is active the
/// flatten guard rejects such children separately, since their shader
/// math reads the composite's own transparent-start buffer.
fn renders_as_over(mode: BlendMode) -> bool {
    let s = blend_mode_to_wgpu(mode);
    let over = wgpu::BlendComponent::OVER;
    let eq = |c: &wgpu::BlendComponent| {
        c.src_factor == over.src_factor
            && c.dst_factor == over.dst_factor
            && c.operation == over.operation
    };
    eq(&s.color) && eq(&s.alpha)
}

/// True when a fully-transparent src texel `(0,0,0,0)` leaves the
/// destination unchanged under `mode`'s fixed-function blend. This is the
/// exact condition for a scissored composite blit to match the
/// full-screen blit: outside a composite's content the offscreen slot is
/// a transparent clear, so only a mode that is identity-on-transparent
/// may skip shading those pixels.
///
/// Derived from `blend_mode_to_wgpu`'s factors/ops (a unit test replays
/// the table to keep this in sync): the OVER family, the DstAlpha/Zero
/// masking modes, ColorDodge/LinearDodge, Add, Inverse and the OVER
/// fallback for the dst-in-shader trio all reduce to `dst` for a zero
/// src; `Subtract` gives `dst − 0 = dst`; `Lighten` is `max(0,dst) = dst`.
/// `Darken` is the sole exclusion — `BlendOperation::Min` with a zero src
/// yields `min(0,dst) = 0`, so a full-screen Darken blit blackens
/// everything outside the composite and a scissored one would not.
fn blend_transparent_src_is_identity(mode: BlendMode) -> bool {
    !matches!(mode, BlendMode::Darken)
}

/// Axis-aligned bounding box in a mesh's local (origin-shifted) space —
/// the coordinate the vertex shader transforms by the instance model
/// matrix. Projected to a screen scissor rect to bound per-part GPU work.
#[derive(Copy, Clone, Debug)]
struct Aabb2 {
    min: glam::Vec2,
    max: glam::Vec2,
}

/// Framebuffer-pixel rectangle, guaranteed within the viewport. Used as a
/// scissor (and as the copy sub-rect for the dst-in-shader snapshot).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScreenRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// Pixels added on every side of a projected part bound before it becomes
/// a scissor, absorbing floor/ceil rounding and any rasterization slack so
/// the rect never clips the content it bounds.
const SCISSOR_PAD: f32 = 2.0;

/// Project a local-space AABB through `mvp` (= view_proj · model) to a
/// framebuffer-pixel AABB `[min_x, min_y, max_x, max_y]`. All four corners
/// are projected and their screen extents unioned, so the box stays
/// conservative under rotation/scale in `model`. Returns None when a
/// corner is non-finite or lands on/behind the `w ≈ 0` plane — the caller
/// then falls back to the full viewport rather than risk clipping.
fn project_aabb_to_pixels(
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
        if clip.w.abs() < 1e-6 {
            return None;
        }
        // NDC → framebuffer pixels: +X right, +Y up in NDC; framebuffer y
        // grows downward, so the y axis flips.
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

/// Pad a pixel-space AABB, round outward, and clamp to the viewport.
/// Returns None for a degenerate / off-screen rect: the caller then uses
/// the full viewport, which is byte-identical because off-screen geometry
/// contributes nothing under the rasterizer either way.
fn pixels_to_scissor(aabb: [f32; 4], width: u32, height: u32) -> Option<ScreenRect> {
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

/// Every render pipeline in `Pipelines` shares the same primitive /
/// multisample / multiview / cache state and a single color target;
/// they differ only in layout, shader entry points, vertex buffers,
/// blend, color write mask, and the optional depth-stencil state. This
/// collapses that boilerplate into one call site.
#[allow(clippy::too_many_arguments)]
fn make_render_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    vertex_module: &wgpu::ShaderModule,
    vertex_entry: &str,
    vertex_buffers: &[wgpu::VertexBufferLayout],
    fragment_module: &wgpu::ShaderModule,
    fragment_entry: &str,
    format: wgpu::TextureFormat,
    blend: wgpu::BlendState,
    write_mask: wgpu::ColorWrites,
    depth_stencil: Option<wgpu::DepthStencilState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: vertex_module,
            entry_point: Some(vertex_entry),
            buffers: vertex_buffers,
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: fragment_module,
            entry_point: Some(fragment_entry),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil,
        multisample: wgpu::MultisampleState {
            count: 1,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        multiview_mask: None,
        cache: None,
    })
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
}

impl Vertex {
    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct DeformAttr {
    deform: [f32; 2],
}

impl DeformAttr {
    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<DeformAttr>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[wgpu::VertexAttribute {
                offset: 0,
                shader_location: 6,
                format: wgpu::VertexFormat::Float32x2,
            }],
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceRaw {
    // Model-matrix columns x_axis, y_axis, w_axis (48 B). The z_axis
    // column is dropped: vertices enter at z = 0 (the shader builds
    // `vec4(pos.xy, 0, 1)`), so z_axis never contributes, and an SRT
    // matrix has no projective row so w_out is always 1 — the shader
    // rebuilds the affine sum and forces w = 1. Columns stay vec4 (not
    // vec3) so the vertex attributes keep 16-byte alignment.
    col_x: [f32; 4],
    col_y: [f32; 4],
    col_w: [f32; 4],
}

impl InstanceRaw {
    fn from_transform(transform: glam::Mat4) -> Self {
        let cols = transform.to_cols_array_2d();
        Self {
            col_x: cols[0],
            col_y: cols[1],
            col_w: cols[3],
        }
    }

    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 4]>() as wgpu::BufferAddress,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: std::mem::size_of::<[f32; 8]>() as wgpu::BufferAddress,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

/// Per-draw resource references for the unmasked Part pipeline.
#[derive(Copy, Clone, Debug)]
struct PartDraw {
    mesh_id: u32,
    albedo: u32,
    transform: glam::Mat4,
    blend_mode: BlendMode,
}

/// A `PartDraw` paired with the part-uniform slot it reads. Batched
/// runs of unmasked parts each carry their own slot so one render pass
/// can re-bind group 2 per draw without re-binding pipeline / texture.
#[derive(Copy, Clone, Debug)]
struct PreparedPartDraw {
    draw: PartDraw,
    part_uniform_offset: u32,
}

/// A masked `PreparedPartDraw` plus the mask sources whose signature
/// decides whether it can share one mask-write pass with its
/// neighbours. The `mask_sources` slice borrows the RenderList; each
/// source carries its own alpha threshold, which the mask-write pass
/// reads from a per-source part-uniform slot.
#[derive(Copy, Clone)]
struct PreparedMaskedPartDraw<'a> {
    draw: PreparedPartDraw,
    mask_sources: &'a [crate::collect::MaskSourceData],
}

/// Two masked draws share a mask-write pass only when their mask shapes
/// rasterize identically. Compare each source's mesh / texture / mode /
/// threshold — not the target part's own mesh or texture, which the
/// masked draw rebinds per draw.
fn same_mask_signature(
    a_sources: &[crate::collect::MaskSourceData],
    b_sources: &[crate::collect::MaskSourceData],
) -> bool {
    use crate::collect::MaskSourceData;

    a_sources.len() == b_sources.len()
        && a_sources.iter().zip(b_sources).all(|(a, b)| match (a, b) {
            (
                MaskSourceData::Part {
                    mesh_id: a_mesh,
                    texture_id: a_texture,
                    mode: a_mode,
                    mask_threshold: a_threshold,
                    ..
                },
                MaskSourceData::Part {
                    mesh_id: b_mesh,
                    texture_id: b_texture,
                    mode: b_mode,
                    mask_threshold: b_threshold,
                    ..
                },
            ) => {
                a_mesh == b_mesh
                    && a_texture == b_texture
                    && a_mode == b_mode
                    && a_threshold.to_bits() == b_threshold.to_bits()
            }
            (
                MaskSourceData::Composite {
                    node_id: a_node,
                    mode: a_mode,
                },
                MaskSourceData::Composite {
                    node_id: b_node,
                    mode: b_mode,
                },
            ) => a_node == b_node && a_mode == b_mode,
            _ => false,
        })
}

/// One color+stencil render pass kept open across consecutive part
/// batches targeting `view` (stencil path only). `next_ref` allocates
/// a fresh stencil reference per masked batch within the open pass:
/// batch k's mask sources REPLACE k where they rasterize (dodge
/// sources punch 0) and its content tests Equal k — stale stencil
/// values from earlier batches are all < k, so no clear between
/// batches is needed. `next_load` is the color LoadOp for the next
/// (re)open: the frame clear until some pass consumes it, then Load.
struct StencilPassState<'v> {
    view: &'v wgpu::TextureView,
    stencil_view: &'v wgpu::TextureView,
    label: &'static str,
    next_load: wgpu::LoadOp<wgpu::Color>,
    // `forget_lifetime` detaches the pass from the encoder borrow so it
    // can be held across z-walk iterations; it must be dropped (set to
    // None) before the encoder records anything else — wgpu validates
    // this at runtime.
    pass: Option<wgpu::RenderPass<'static>>,
    next_ref: u32,
    // Texture currently bound at group 1, shared across every record
    // helper feeding this pass: bind-group state is pass-scoped in wgpu,
    // so a mask-source or part draw whose texture is already bound
    // (atlas pages make this the common case) can skip the rebind.
    bound_texture: Option<u32>,
    // Scissor applied on every (re)open. Set only for a single-part slot
    // pass (dst-in-shader render), where the whole pass draws one part and
    // its rect is known up front; None everywhere else (the main pass and
    // composite-children pass draw many parts with different bounds). A
    // LoadOp::Clear still clears the whole attachment regardless.
    scissor: Option<ScreenRect>,
}

impl<'v> StencilPassState<'v> {
    fn new(
        view: &'v wgpu::TextureView,
        stencil_view: &'v wgpu::TextureView,
        clear_color: Option<wgpu::Color>,
        label: &'static str,
        scissor: Option<ScreenRect>,
    ) -> Self {
        Self {
            view,
            stencil_view,
            label,
            next_load: match clear_color {
                Some(color) => wgpu::LoadOp::Clear(color),
                None => wgpu::LoadOp::Load,
            },
            pass: None,
            next_ref: 1,
            bound_texture: None,
            scissor,
        }
    }
}

/// Where a run of batched part draws lands. Stencil: one shared pass
/// per color target, masked batches distinguished by per-batch stencil
/// references. Alpha (WebGL fallback): per-batch passes — mask-alpha
/// texture writes plus sampled masked draws.
enum PartSink<'a, 'v> {
    Stencil(&'a mut StencilPassState<'v>),
    Alpha {
        view: &'v wgpu::TextureView,
        stencil: &'v StencilTarget,
        has_rendered: &'a mut bool,
        clear_color: Option<wgpu::Color>,
        mask_write_label: &'static str,
        masked_draw_label: &'static str,
    },
}

impl PartSink<'_, '_> {
    /// The target view received content from a pass outside the sink
    /// (composite blit, dst-in-shader blit) — the frame clear is spent.
    fn mark_rendered(&mut self) {
        match self {
            PartSink::Stencil(s) => s.next_load = wgpu::LoadOp::Load,
            PartSink::Alpha { has_rendered, .. } => **has_rendered = true,
        }
    }
}

pub struct Texture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub bind_group: wgpu::BindGroup,
    // A shared byte allocation is not a complete upload identity: callers can
    // reuse it with different dimensions, which requires a new GPU texture.
    source: Option<TextureSource>,
}

struct TextureSource {
    rgba: std::sync::Arc<[u8]>,
    width: u32,
    height: u32,
}

impl TextureSource {
    fn matches(&self, texture: &DecodedTexture) -> bool {
        self.width == texture.width
            && self.height == texture.height
            && std::sync::Arc::ptr_eq(&self.rgba, &texture.rgba)
    }
}

/// sRGB→linear conversion matching the piecewise curve that
/// `Rgba8UnormSrgb` applies on texture read. Used to pre-convert
/// CPU-side tint uniforms so fragment math stays in linear space.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_to_linear_vec3(c: glam::Vec3) -> glam::Vec3 {
    glam::Vec3::new(
        srgb_to_linear(c.x),
        srgb_to_linear(c.y),
        srgb_to_linear(c.z),
    )
}

/// Every puppet texture uploads as premultiplied linear encoded in sRGB
/// bytes, so the sampler's decode hands shaders premultiplied linear.
/// CPU-side tints go through `srgb_to_linear_vec3` before upload; all
/// fragment math is linear.
pub const PUPPET_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Fill mips 1.. by successively halving the level above on the GPU.
///
/// The CPU alternative walks ~1.33x the base pixel count per texture
/// through an sRGB decode/encode per channel, single-threaded, before the
/// first frame can draw — the largest remaining slice of a model's load
/// time after PNG decode, and unlike PNG decode it has a direct GPU
/// analogue.
pub fn generate_mips(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    mip_level_count: u32,
    pipelines: &Pipelines,
    label: &str,
) {
    if mip_level_count < 2 {
        return;
    }
    let views: Vec<wgpu::TextureView> = (0..mip_level_count)
        .map(|mip| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: mip,
                mip_level_count: Some(1),
                ..Default::default()
            })
        })
        .collect();

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(&format!("{label} mips")),
    });
    for target in 1..mip_level_count as usize {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &pipelines.mip_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&views[target - 1]),
            }],
            label: Some("mip bind group"),
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mip downsample"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &views[target],
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    // The triangle covers every texel, so the cleared
                    // value is never read.
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipelines.mip_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
}

impl Texture {
    pub fn from_decoded_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        pipelines: &Pipelines,
        label: &str,
    ) -> RendererResult<Self> {
        let bind_group_layout = &pipelines.texture_bind_group_layout;
        let sampler = &pipelines.texture_sampler;
        let (w, h) = (width, height);
        let bytes_per_row =
            validate_rgba8_texture(w, h, rgba.len(), device.limits().max_texture_dimension_2d)?;

        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };

        let mip_level_count = 32 - w.max(h).max(1).leading_zeros();

        // Importer produces bytes encoding premultiplied LINEAR color
        // (see ModelTexture::decode). Upload as `Rgba8UnormSrgb` so the
        // sampler decodes sRGB→linear and returns premultiplied linear
        // values — shaders can blend / tint directly in linear space.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PUPPET_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(h),
            },
            size,
        );
        generate_mips(device, queue, &texture, mip_level_count, pipelines, label);

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
            label: Some(label),
        });

        Ok(Self {
            texture,
            view,
            bind_group,
            source: None,
        })
    }
}

fn validate_rgba8_texture(
    width: u32,
    height: u32,
    actual: usize,
    dimension_limit: u32,
) -> RendererResult<u32> {
    if width == 0 || height == 0 || width > dimension_limit || height > dimension_limit {
        return Err(RendererError::TextureDimensionsOutOfRange {
            width,
            height,
            limit: dimension_limit,
        });
    }
    let expected_u64 = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(RendererError::TextureByteLengthOverflow { width, height })?;
    let expected = usize::try_from(expected_u64)
        .map_err(|_| RendererError::TextureByteLengthOverflow { width, height })?;
    if actual != expected {
        return Err(RendererError::TextureByteLengthMismatch {
            width,
            height,
            expected,
            actual,
        });
    }
    width
        .checked_mul(4)
        .ok_or(RendererError::TextureByteLengthOverflow { width, height })
}

pub struct MeshBuffer {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub deform_offset: u64,
    pub deform_size: u64,
    pub vert_count: u32,
    pub num_indices: u32,
    pub index_format: wgpu::IndexFormat,
    /// AABB over the shifted positions this mesh uploads (`pos - origin`,
    /// what the shader transforms). None for an empty mesh — no bound to
    /// project, so callers use the full viewport.
    local_bounds: Option<Aabb2>,
}

#[derive(Clone)]
pub struct CompositeTexture {
    pub view: wgpu::TextureView,
    // A dst-in-shader child blends against this composite's contents, and
    // the snapshot copy needs the texture handle (COPY_SRC), not a view.
    texture: wgpu::Texture,
    // Bind group the blit pipelines use to sample this composite. Built
    // once when the composite texture is created (or resized) and reused
    // across every frame's blit calls.
    blit_bind_group: wgpu::BindGroup,
}

#[derive(Clone)]
struct PreparedCompositeMask {
    texture: CompositeTexture,
    opacity: f32,
    mask_threshold: f32,
}

impl CompositeTexture {
    pub fn new(pipelines: &Pipelines, device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Composite Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipelines.surface_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &pipelines.blit_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.blit_sampler),
                },
            ],
            label: Some("Composite blit bind group"),
        });
        Self {
            view,
            texture,
            blit_bind_group,
        }
    }
}

/// Sample-able copy of the main color attachment used by the dst-in-shader
/// blit path (Overlay/ColorBurn/LinearBurn). Each blit requires
/// snapshotting the framebuffer just before the pass so the fragment
/// shader can read the destination at the same screen pixel; the source
/// must therefore be the user-supplied color texture and must carry
/// `COPY_SRC` usage. The snapshot itself owns COPY_DST + TEXTURE_BINDING.
struct SnapshotTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

impl SnapshotTexture {
    fn new(pipelines: &Pipelines, device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Framebuffer Snapshot Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipelines.surface_format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &pipelines.snapshot_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&pipelines.snapshot_sampler),
                },
            ],
            label: Some("Framebuffer Snapshot bind group"),
        });
        Self {
            texture,
            bind_group,
        }
    }
}

/// Pool of viewport-sized framebuffer snapshots, reused across the four
/// dst-in-shader blit modes within one frame. Same growth/reset
/// discipline as `CompositePool`: cursor reset at `render_list` entry,
/// slots grow on first miss and are then reused for the rest of the
/// program's lifetime. Allocates nothing when no dst-in-shader part
/// shows up — the cursor stays at 0 and `acquire` is never called, so
/// the slot vec stays empty.
pub struct FramebufferSnapshotPool {
    slots: Vec<SnapshotTexture>,
    cursor: usize,
    width: u32,
    height: u32,
}

impl FramebufferSnapshotPool {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            slots: Vec::new(),
            cursor: 0,
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn ensure_size(&mut self, width: u32, height: u32) {
        let w = width.max(1);
        let h = height.max(1);
        if self.width != w || self.height != h {
            self.width = w;
            self.height = h;
            self.slots.clear();
            self.cursor = 0;
        }
    }

    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// See [`CompositePool::mark`].
    pub fn mark(&self) -> usize {
        self.cursor
    }

    /// See [`CompositePool::rewind`].
    pub fn rewind(&mut self, mark: usize) {
        self.cursor = self.cursor.min(mark);
    }

    fn acquire(&mut self, pipelines: &Pipelines, device: &wgpu::Device) -> &SnapshotTexture {
        if self.cursor >= self.slots.len() {
            self.slots.push(SnapshotTexture::new(
                pipelines,
                device,
                self.width,
                self.height,
            ));
        }
        let idx = self.cursor;
        self.cursor += 1;
        &self.slots[idx]
    }
}

/// Where a composite's finished slot blits to: the main view for a root
/// composite, or the enclosing composite's slot for a nested one. `texture`
/// names the same surface as `view` and is only needed by the dst-in-shader
/// blit, which snapshots it; `None` sends that blit down the Normal-OVER
/// fallback instead.
#[derive(Clone, Copy)]
struct CompositeTarget<'a> {
    view: &'a wgpu::TextureView,
    texture: Option<&'a wgpu::Texture>,
}

/// Pool of viewport-sized offscreen textures reused across composites.
/// One pool per render world; `reset()` at the start of every `render_list`
/// call. Slots are allocated lazily (grown on demand).
pub struct CompositePool {
    slots: Vec<CompositeTexture>,
    cursor: usize,
    // Highest cursor reached since `reset`. `cursor` alone can't answer "how
    // many slots did this frame need": rewinds pull it back, so by the end of
    // a frame it has usually returned to 0.
    peak: usize,
    width: u32,
    height: u32,
}

impl CompositePool {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            slots: Vec::new(),
            cursor: 0,
            peak: 0,
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn ensure_size(&mut self, width: u32, height: u32) {
        let w = width.max(1);
        let h = height.max(1);
        if self.width != w || self.height != h {
            self.width = w;
            self.height = h;
            self.slots.clear();
            self.cursor = 0;
            self.peak = 0;
        }
    }

    /// Reset the allocation cursor. Call once per `render_list` so each
    /// frame's composites re-use slots 0, 1, 2…
    pub fn reset(&mut self) {
        self.cursor = 0;
        self.peak = 0;
    }

    /// Current cursor, to hand back to [`Self::rewind`] once every slot
    /// acquired since is dead. This is what makes non-nested composites share
    /// a slot instead of each permanently growing the pool.
    pub fn mark(&self) -> usize {
        self.cursor
    }

    /// Release every slot acquired since `mark`. Never moves the cursor
    /// forward, so a nested acquire that outlives its mark can't be freed
    /// out from under a recorded pass.
    pub fn rewind(&mut self, mark: usize) {
        self.cursor = self.cursor.min(mark);
    }

    /// Acquire the next slot, growing the pool if needed. Returns owned
    /// handles (cheap — wgpu resources are internally ref-counted), so a
    /// caller can hold one slot while the pool hands out further slots,
    /// e.g. a composite's own slot plus the scratch slot its
    /// dst-in-shader children render through.
    pub fn acquire(&mut self, pipelines: &Pipelines, device: &wgpu::Device) -> CompositeTexture {
        if self.cursor >= self.slots.len() {
            self.slots.push(CompositeTexture::new(
                pipelines,
                device,
                self.width,
                self.height,
            ));
        }
        let idx = self.cursor;
        self.cursor += 1;
        self.peak = self.peak.max(self.cursor);
        self.slots[idx].clone()
    }

    /// Snapshot for debugging/overlays: `(peak_this_frame, capacity,
    /// width, height)`. The peak is the most slots live at once since
    /// `reset` — with nested composites that is the nesting depth, and it
    /// survives the rewinds that return `cursor` to 0 before the frame ends.
    pub fn stats(&self) -> (usize, usize, u32, u32) {
        (self.peak, self.slots.len(), self.width, self.height)
    }
}

// Matches `struct PartUniforms` in basic.wgsl — see there for the
// screen_tint.w / mask_threshold packing invariant. `tint` keeps a w lane
// purely for std140 alignment.
#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PartUniform {
    pub opacity: [f32; 4],
    pub tint: [f32; 4],
    pub screen_tint_and_threshold: [f32; 4],
}

impl PartUniform {
    pub fn new(
        opacity: f32,
        tint: glam::Vec3,
        screen_tint: glam::Vec3,
        mask_threshold: f32,
    ) -> Self {
        Self {
            opacity: [opacity, 0.0, 0.0, 0.0],
            tint: [tint.x, tint.y, tint.z, 0.0],
            screen_tint_and_threshold: [
                screen_tint.x,
                screen_tint.y,
                screen_tint.z,
                mask_threshold,
            ],
        }
    }
}

impl Default for PartUniform {
    fn default() -> Self {
        Self::new(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5)
    }
}

/// GPU resources that are safe to share across every puppet rendering in
/// one world: pipelines, bind-group layouts, samplers, and shared texture
/// placeholders. Camera buffers live per `WgpuRenderer`; only their layout
/// and dynamic-offset stride are shared. Built once per (device,
/// surface_format) pair. Wrap in an `Arc` when instantiating multiple
/// `WgpuRenderer`s to avoid recompiling ~18 pipelines per puppet.
pub struct Pipelines {
    pub surface_format: wgpu::TextureFormat,
    pub(crate) pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) mask_write_pipeline: wgpu::RenderPipeline,
    pub(crate) composite_mask_part_pipeline: wgpu::RenderPipeline,
    pub(crate) composite_mask_write_pipeline: wgpu::RenderPipeline,
    pub(crate) composite_mask_alpha_dodge_pipeline: Option<wgpu::RenderPipeline>,
    pub(crate) masked_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) blit_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    // Stencil-path masked blits draw inside the same stencil-attached
    // pass as their mask-source writes, so they carry the Equal-test
    // depth-stencil state the plain blit pipelines (stencil-free
    // passes) lack. Empty when has_stencil == false.
    pub(crate) masked_blit_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    // Fullscreen triangle that REPLACEs the whole viewport's stencil
    // with the pass reference (color writes off, binds nothing). Seeds
    // an all-DodgeMask batch so dodge sources can punch 0 out of it.
    // Only populated when has_stencil == true.
    pub(crate) stencil_fill_pipeline: Option<wgpu::RenderPipeline>,
    pub(crate) blit_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) blit_sampler: wgpu::Sampler,
    // Mip generation for uploaded puppet textures. Fixed to
    // `Rgba8UnormSrgb` (every puppet texture's format), independent of
    // `surface_format`.
    pub(crate) mip_pipeline: wgpu::RenderPipeline,
    pub(crate) mip_bind_group_layout: wgpu::BindGroupLayout,
    // One pipeline per dst-in-shader BlendMode (Overlay/ColorBurn/
    // LinearBurn). Each samples the framebuffer snapshot at the
    // current screen pixel, computes the per-channel blend, and emits
    // the final pixel via `BlendState::REPLACE` — no fixed-function
    // blending is involved on the output side.
    pub(crate) blit_dst_in_shader_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) snapshot_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) snapshot_sampler: wgpu::Sampler,
    // Camera (group 0) is bound with a dynamic offset so a renderer can
    // hold one view-proj slot per view it draws into within a single
    // submit. The buffer + bind group live per-`WgpuRenderer`; only the
    // layout and slot stride are shared here. A single shared camera
    // buffer written once per view would alias under the queue
    // write_buffer batching when multiple views share one submit (the
    // "last camera wins" hazard documented at the top of this file).
    pub(crate) camera_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) camera_stride: u64,
    pub(crate) texture_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) texture_sampler: wgpu::Sampler,
    pub(crate) part_uniform_bind_group_layout: wgpu::BindGroupLayout,
    // part_uniform_buffer is bound with dynamic offset so each draw can
    // point at its own PartUniform slot. `stride` is sizeof(PartUniform)
    // rounded up to the device's min_uniform_buffer_offset_alignment.
    // Blit draws reuse this layout (identical wire format) — see blit.wgsl.
    pub(crate) part_uniform_stride: u64,
    // When true, part draws bind the instance buffer once per pass and
    // select each instance with a non-zero `first_instance` rather than
    // re-slicing the vertex binding per draw. Enabled on native backends
    // (Vulkan/DX12/Metal), which drive `first_instance` in hardware;
    // false on GL/WebGL2 (which only emulate it by rebinding per draw)
    // and on the adapter-less constructors, both of which keep the
    // explicit per-draw slice.
    pub(crate) base_instance: bool,
    // When false, the adapter/backend lacks a usable stencil
    // attachment (Chromium swiftshader WebGL2 fails
    // framebufferTexture2D on Depth24PlusStencil8). Mask/masked paths
    // then sample an offscreen mask alpha texture instead of stencil
    // tests — see the `masked_sampled_*` pipelines and
    // `StencilTarget::mask_alpha_view`.
    pub(crate) has_stencil: bool,
    // Only populated when has_stencil == false.
    pub(crate) mask_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub(crate) mask_sampler: Option<wgpu::Sampler>,
    pub(crate) mask_alpha_pipeline: Option<wgpu::RenderPipeline>,
    // DodgeMask counterpart of mask_alpha_pipeline: writes alpha 0 where
    // the source rasterizes, mirroring the stencil path's REPLACE-with-0
    // (regular sources write 1; masked draws pass where the value is 1).
    pub(crate) mask_alpha_dodge_pipeline: Option<wgpu::RenderPipeline>,
    pub(crate) masked_sampled_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) masked_sampled_blit_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
}

/// Slots in the per-renderer camera ring. Bounds how many distinct
/// view-proj matrices one renderer can have live in a single submit
/// (i.e. the number of `CatchlightCamera` views a puppet draws into per
/// frame). The next reservation is rejected rather than wrapping onto a
/// slot still referenced by an earlier command buffer.
const CAMERA_RING_SLOTS: u32 = 64;

fn camera_slot_offset(slot: u32, stride: u64) -> RendererResult<u64> {
    if slot >= CAMERA_RING_SLOTS {
        return Err(RendererError::TooManyCameraViews {
            limit: CAMERA_RING_SLOTS,
        });
    }
    Ok(u64::from(slot) * stride)
}

/// Dense `Vec<Option<V>>` keyed by a small integer id. MeshId and
/// TextureIdx are puppet-slot / texture-table indices probed several
/// times per drawable per frame; indexing a vector avoids the default
/// SipHash `HashMap` on that hot path. Grows to the largest id inserted
/// — ids come from real puppets (no adversarial sparsity), so the
/// backing storage stays bounded by the puppet's node / texture count.
#[derive(Default)]
struct DenseMap<V> {
    slots: Vec<Option<V>>,
}

impl<V> DenseMap<V> {
    fn new() -> Self {
        Self { slots: Vec::new() }
    }

    fn get(&self, index: usize) -> Option<&V> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    fn contains(&self, index: usize) -> bool {
        matches!(self.slots.get(index), Some(Some(_)))
    }

    fn insert(&mut self, index: usize, value: V) {
        if index >= self.slots.len() {
            self.slots.resize_with(index + 1, || None);
        }
        self.slots[index] = Some(value);
    }

    fn remove(&mut self, index: usize) {
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = None;
        }
    }

    /// Take the value at `index` out, leaving the slot empty. What a rebuild
    /// moving a texture to a slot it did not previously occupy takes it with.
    fn take(&mut self, index: usize) -> Option<V> {
        self.slots.get_mut(index).and_then(Option::take)
    }

    fn clear(&mut self) {
        self.slots.clear();
    }

    /// How many slots currently hold a value.
    fn live(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Indices currently holding a value, ascending. Used to sweep the
    /// deform-upload set for meshes that went inactive this frame.
    fn keys(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|_| i))
    }
}

pub struct WgpuRenderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub shared: std::sync::Arc<Pipelines>,
    part_uniform_buffer: wgpu::Buffer,
    part_uniform_bind_group: wgpu::BindGroup,
    mesh_buffers: DenseMap<MeshBuffer>,
    textures: DenseMap<Texture>,
    instance_buffer: wgpu::Buffer,
    instance_buffer_capacity: usize,
    // Bytes consumed from instance_buffer within the current frame.
    // Reset at the start of each render_list() call. Distinct offsets
    // matter because wgpu::queue.write_buffer batches at submit-start —
    // multiple writes to the same offset inside one frame would alias
    // and every pass would read the last write.
    instance_cursor: u64,
    instance_staging: Vec<u8>,
    instance_staging_len: usize,
    batch_instance_writes: bool,
    part_uniform_capacity: u32,
    part_uniform_cursor: u64,
    part_uniform_staging: Vec<u8>,
    part_uniform_staging_len: usize,
    batch_part_uniform_writes: bool,
    deform_buffer: wgpu::Buffer,
    deform_buffer_capacity: u64,
    deform_buffer_len: u64,
    deform_upload_mirror: Vec<u8>,
    // Bytes written by the most recent `upload_deforms`, folded into the
    // next `render_list`'s RenderStats (the upload runs before render_list,
    // which resets current_stats, so it can't accumulate directly).
    pending_deform_bytes: u64,
    // Per-frame pass / draw tallies. Atomics, not plain counters on
    // current_stats: the blit helpers borrow `&self`, so they can't
    // mutate current_stats — a relaxed fetch_add lets every pass/draw
    // site count uniformly regardless of &self vs &mut self. Reset at
    // render_list entry, folded into current_stats at exit.
    frame_render_passes: AtomicU32,
    frame_draw_calls: AtomicU32,
    // Per-view camera uniform ring. Each `render_list` writes the retained
    // view-proj into the next slot and binds that dynamic offset for all its
    // passes. Distinct slots keep views in one submit from aliasing under
    // queue.write_buffer batching; `begin_camera_submit` resets the count at
    // the external submission boundary.
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    camera_slots_used: u32,
    camera_offset: u32,
    current_stats: RenderStats,
    // Per-frame resource-lifecycle counters, reset alongside
    // `current_stats` at render_list entry. See `FrameStats`.
    frame_stats: FrameStats,
    // Set once `begin_frame_uniforms` has closed the frame's sizing
    // phase. Any buffer recreation after that strands already-recorded
    // passes on the freed buffer, so it counts into
    // `FrameStats::late_buffer_reallocs` and trips a debug assertion.
    frame_sizing_closed: bool,
    // High-water mark of the bytes staged into instance_buffer /
    // part_uniform_buffer this frame. Both offset cursors are monotonic,
    // so a write landing below its watermark is a second write to an
    // offset earlier draws already read — the aliasing hazard that
    // `write_buffer`'s submit-start batching makes silent.
    instance_write_watermark: u64,
    part_uniform_write_watermark: u64,
    /// Set when the device exposes `Features::TIMESTAMP_QUERY`. When
    /// absent (WebGL2 / WebGPU surfaces without the feature) the
    /// profiler is `None` and the render path falls back to CPU-only
    /// `tracing` spans.
    gpu_profiler: Option<wgpu_profiler::GpuProfiler>,
    /// MeshIds whose deform buffer currently holds active data on the
    /// GPU, paired with the `DeformStack` generation that produced it.
    /// `upload_deforms` skips writes when the active generation is already
    /// resident and zero-fills meshes that transition back to inactive.
    deform_uploaded: DenseMap<u64>,
    deform_still_active: DenseMap<()>,
    deform_inactive_scratch: Vec<u32>,
    // Dirty deform byte ranges `[start, end)` collected each sync, then
    // sorted and coalesced so two far-apart meshes don't force one write
    // over the untouched span between them. Pooled across frames.
    deform_write_ranges: Vec<(u64, u64)>,
    /// Per-mesh max absolute deform component currently resident on the
    /// GPU, tracked alongside `deform_uploaded` (same generation memo,
    /// same inactive sweep). The shader adds deform in local space before
    /// the instance transform, so expanding a mesh's local AABB by this
    /// magnitude on both axes bounds the deformed geometry exactly.
    deform_max: DenseMap<f32>,
    /// CPU copy of the last `update_camera` view-proj, kept so per-part
    /// bounds can project to clip space. Multi-view renderers call
    /// `update_camera` per view before `render_list`, so this is the
    /// matrix that frame's passes bind.
    camera_view_proj: glam::Mat4,
    // Per-batch scratch reused across frames (taken out for the duration
    // of a draw helper, put back at its end). Indices into the caller's
    // draw slice rather than references, so the renderer can own them.
    draw_filter_scratch: Vec<usize>,
    instance_scratch: Vec<InstanceRaw>,
    composite_mask_textures: HashMap<u32, PreparedCompositeMask>,
}

/// Viewport-sized texture used for mask compositing. Shared across
/// every puppet in a frame — one allocation per viewport, not per
/// puppet.
///
/// When `Pipelines::has_stencil == true`, `view` is a
/// `Depth24PlusStencil8` texture used as the render pass
/// depth_stencil_attachment for stencil-based masking. When false
/// (WebGL fallback), `view` is instead a color texture (surface
/// format) used as a sampled mask: mask sources draw into it via
/// `fs_mask_alpha`, masked parts sample it via `fs_masked_sampled`
/// with alpha-discard.
pub struct StencilTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    // Bind group that masked draws/blits use to sample the mask
    // texture in the alpha-mask path. None when has_stencil == true.
    pub mask_bind_group: Option<wgpu::BindGroup>,
}

impl StencilTarget {
    /// Build a stencil-backed StencilTarget (Depth24PlusStencil8).
    /// Use `new_for_pipelines` for backend-aware allocation.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Stencil Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth24PlusStencil8,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            mask_bind_group: None,
        }
    }

    // Invariant: Pipelines::new_with_options(has_stencil=false) always builds mask_bind_group_layout + mask_sampler.
    #[allow(clippy::expect_used)]
    fn new_mask_alpha(
        pipelines: &Pipelines,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Mask Alpha Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: pipelines.surface_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // Safe to unwrap: has_stencil == false path always builds the
        // mask bind group layout + sampler in Pipelines::new_with_options.
        // The `expect`s below would only fire on a Pipelines assembled
        // by hand with the flag flipped post-hoc — not a real path.
        let mask_bind_group = {
            let layout = pipelines
                .mask_bind_group_layout
                .as_ref()
                .expect("mask_bind_group_layout must be built when has_stencil == false");
            let sampler = pipelines
                .mask_sampler
                .as_ref()
                .expect("mask_sampler must be built when has_stencil == false");
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
                label: Some("Mask Alpha bind group"),
            })
        };
        Self {
            texture,
            view,
            mask_bind_group: Some(mask_bind_group),
        }
    }

    /// Backend-aware constructor: stencil when `pipelines.has_stencil`,
    /// color mask texture otherwise. Prefer this over `new()` so the
    /// WebGL fallback path gets a sampleable mask target instead of a
    /// Depth24PlusStencil8 texture the backend can't bind.
    pub fn new_for_pipelines(
        pipelines: &Pipelines,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Self {
        if pipelines.has_stencil {
            Self::new(device, width, height)
        } else {
            Self::new_mask_alpha(pipelines, device, width, height)
        }
    }

    /// Recreate when viewport resized. No-op if size already matches.
    /// Rebuilds using `new_for_pipelines` so the mask-alpha target
    /// stays a mask-alpha target across resizes.
    pub fn ensure_size_for_pipelines(
        &mut self,
        pipelines: &Pipelines,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) {
        let w = width.max(1);
        let h = height.max(1);
        if self.texture.width() != w || self.texture.height() != h {
            *self = Self::new_for_pipelines(pipelines, device, w, h);
        }
    }
}

impl WgpuRenderer {
    /// Build a renderer that owns a fresh `Pipelines`. Prefer
    /// `WgpuRenderer::from_pipelines` when you need multiple renderers to
    /// share pipelines (e.g. one per puppet in a bevy scene) — each
    /// `Pipelines::new` compiles ~18 pipelines and is not cheap.
    pub async fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shared = std::sync::Arc::new(Pipelines::new(&device, surface_format));
        Self::from_pipelines(device, queue, shared)
    }

    /// Auto-detect the stencil path based on the adapter's backend.
    /// GL backend (WebGL2) runs the shader-side alpha-discard fallback
    /// because Chromium swiftshader WebGL2 fails framebufferTexture2D
    /// on Depth24PlusStencil8. Every other backend keeps the stencil
    /// path. Respects the `CATCHLIGHT_DISABLE_STENCIL` env var for
    /// manual overrides.
    pub async fn new_autodetect(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let shared =
            std::sync::Arc::new(Pipelines::new_autodetect(adapter, &device, surface_format));
        Self::from_pipelines(device, queue, shared)
    }

    /// Build a renderer sharing the given pre-built `Pipelines`. Allocates
    /// only the per-puppet scratch buffers (instance, part-uniform, stencil).
    pub fn from_pipelines(
        device: wgpu::Device,
        queue: wgpu::Queue,
        shared: std::sync::Arc<Pipelines>,
    ) -> Self {
        let instance_buffer_capacity = 512;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Instance Buffer"),
            size: (instance_buffer_capacity * std::mem::size_of::<InstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let part_uniform_capacity: u32 = 256;
        let part_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Part Uniform Buffer"),
            size: shared.part_uniform_stride * part_uniform_capacity as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let part_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &shared.part_uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &part_uniform_buffer,
                    offset: 0,
                    size: std::num::NonZeroU64::new(std::mem::size_of::<PartUniform>() as u64),
                }),
            }],
            label: Some("part_uniform_bind_group"),
        });

        let deform_buffer_capacity = 8;
        let deform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Deform Buffer Atlas"),
            size: deform_buffer_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Camera Buffer"),
            size: shared.camera_stride * CAMERA_RING_SLOTS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &shared.camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &camera_buffer,
                    offset: 0,
                    size: std::num::NonZeroU64::new(std::mem::size_of::<CameraUniform>() as u64),
                }),
            }],
            label: Some("camera_bind_group"),
        });
        // Slot 0 holds identity so a render_list before any update_camera
        // doesn't read uninitialised memory.
        queue.write_buffer(
            &camera_buffer,
            0,
            bytemuck::cast_slice(&[CameraUniform {
                view_proj: glam::Mat4::IDENTITY.to_cols_array_2d(),
            }]),
        );

        let gpu_profiler = if device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            wgpu_profiler::GpuProfiler::new(&device, wgpu_profiler::GpuProfilerSettings::default())
                .map_err(|e| {
                    tracing::warn!("GpuProfiler::new failed: {e}; GPU timestamps disabled");
                    e
                })
                .ok()
        } else {
            None
        };

        Self {
            device,
            queue,
            shared,
            part_uniform_buffer,
            part_uniform_bind_group,
            mesh_buffers: DenseMap::new(),
            textures: DenseMap::new(),
            instance_buffer,
            instance_buffer_capacity,
            instance_cursor: 0,
            instance_staging: Vec::new(),
            instance_staging_len: 0,
            batch_instance_writes: false,
            part_uniform_capacity,
            part_uniform_cursor: 0,
            part_uniform_staging: Vec::new(),
            part_uniform_staging_len: 0,
            batch_part_uniform_writes: false,
            deform_buffer,
            deform_buffer_capacity,
            deform_buffer_len: 0,
            deform_upload_mirror: Vec::new(),
            pending_deform_bytes: 0,
            frame_render_passes: AtomicU32::new(0),
            frame_draw_calls: AtomicU32::new(0),
            camera_buffer,
            camera_bind_group,
            camera_slots_used: 0,
            camera_offset: 0,
            current_stats: RenderStats::default(),
            frame_stats: FrameStats::default(),
            frame_sizing_closed: false,
            instance_write_watermark: 0,
            part_uniform_write_watermark: 0,
            gpu_profiler,
            deform_uploaded: DenseMap::new(),
            deform_still_active: DenseMap::new(),
            deform_inactive_scratch: Vec::new(),
            deform_write_ranges: Vec::new(),
            deform_max: DenseMap::new(),
            camera_view_proj: glam::Mat4::IDENTITY,
            draw_filter_scratch: Vec::new(),
            instance_scratch: Vec::new(),
            composite_mask_textures: HashMap::new(),
        }
    }

    /// Resource-lifecycle counters for the frame in progress. Reset at
    /// each `render_list` entry, so read it after the call returns.
    pub fn frame_stats(&self) -> FrameStats {
        self.frame_stats
    }

    /// Tally one `queue.write_buffer` against the buffer it targets.
    fn note_queue_write(&mut self, buffer: RenderBuffer) {
        self.frame_stats.queue_writes += 1;
        match buffer {
            RenderBuffer::Instance => self.frame_stats.instance_buffer_writes += 1,
            RenderBuffer::PartUniform => self.frame_stats.part_uniform_buffer_writes += 1,
            RenderBuffer::Camera => self.frame_stats.camera_buffer_writes += 1,
            RenderBuffer::Deform => self.frame_stats.deform_buffer_writes += 1,
        }
    }

    /// Tally one buffer recreation. Growth is only legal while the frame
    /// is being sized: once `begin_frame_uniforms` has closed the sizing
    /// phase, passes are recording against the buffers that exist, and a
    /// new allocation would strand them on the freed one.
    fn note_realloc(&mut self, buffer: RenderBuffer) {
        match buffer {
            RenderBuffer::Instance => self.frame_stats.instance_buffer_reallocs += 1,
            RenderBuffer::PartUniform => self.frame_stats.part_uniform_buffer_reallocs += 1,
            RenderBuffer::Deform => self.frame_stats.deform_buffer_reallocs += 1,
            RenderBuffer::Camera => {}
        }
        if self.frame_sizing_closed {
            self.frame_stats.late_buffer_reallocs += 1;
            debug_assert!(
                false,
                "{buffer:?} buffer grew after the frame was sized — passes already \
                 recorded this frame still point at the freed buffer",
            );
        }
    }

    /// Size instance_buffer for the whole frame and reset the cursor.
    /// Callers must pass the total instance count needed by every draw
    /// in the frame — growing mid-frame would leave already-recorded
    /// passes pointing at the old buffer while later writes land in the
    /// new one.
    fn begin_frame_instances(&mut self, total_instances: usize) {
        self.frame_stats.instance_slots_budgeted = total_instances as u32;
        if total_instances > self.instance_buffer_capacity {
            self.note_realloc(RenderBuffer::Instance);
            self.instance_buffer_capacity = (total_instances * 2).max(512);
            self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Instance Buffer"),
                size: (self.instance_buffer_capacity * std::mem::size_of::<InstanceRaw>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.instance_cursor = 0;
        self.instance_staging_len = 0;
        self.instance_write_watermark = 0;
        self.batch_instance_writes = true;
    }

    fn ensure_deform_buffer_capacity(&mut self, needed: u64) {
        let needed = needed.max(8);
        if needed <= self.deform_buffer_capacity {
            return;
        }
        self.note_realloc(RenderBuffer::Deform);
        self.deform_buffer_capacity = needed.next_power_of_two();
        self.deform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Deform Buffer Atlas"),
            size: self.deform_buffer_capacity,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if !self.deform_upload_mirror.is_empty() {
            self.note_queue_write(RenderBuffer::Deform);
            self.queue
                .write_buffer(&self.deform_buffer, 0, &self.deform_upload_mirror);
        }
        self.deform_uploaded.clear();
        self.deform_still_active.clear();
        self.deform_inactive_scratch.clear();
        self.deform_max.clear();
    }

    fn reset_deform_buffer_layout(&mut self) {
        self.deform_buffer_len = 0;
        self.deform_upload_mirror.clear();
        self.deform_uploaded.clear();
        self.deform_still_active.clear();
        self.deform_inactive_scratch.clear();
        self.deform_max.clear();
    }

    fn reserve_deform_buffer(&mut self, size: u64) -> u64 {
        let offset = self.deform_buffer_len;
        let end = offset + size;
        self.ensure_deform_buffer_capacity(end);
        self.deform_buffer_len = end;
        self.deform_upload_mirror.resize(end as usize, 0);
        offset
    }

    fn reserve_instances(&mut self, count: usize) -> u64 {
        let stride = std::mem::size_of::<InstanceRaw>() as u64;
        let offset = self.instance_cursor;
        self.instance_cursor += count as u64 * stride;
        self.frame_stats.instance_slots_reserved += count as u32;
        debug_assert!(
            self.instance_cursor <= (self.instance_buffer_capacity as u64) * stride,
            "instance_buffer overrun — begin_frame_instances undersized: cursor={} cap={}",
            self.instance_cursor,
            self.instance_buffer_capacity as u64 * stride,
        );
        offset
    }

    /// Bind the whole instance buffer once for a part-draw pass on the
    /// BASE_INSTANCE fast path, where each draw then selects its instance
    /// via a non-zero `first_instance`. A no-op on the portable fallback
    /// — there `emit_part_draw` re-slices vertex buffer 1 per draw.
    fn bind_pass_instances(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        if self.shared.base_instance {
            render_pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
        }
    }

    /// Record one part/mask draw of `num_indices`, selecting its instance
    /// by `first_instance` (fast path; caller bound the buffer via
    /// `bind_pass_instances`) or by re-slicing vertex buffer 1 to the one
    /// instance (fallback). `instance_offset` is the instance's byte
    /// offset; `reserve_instances` hands out stride-aligned offsets from a
    /// zero cursor, so the derived instance index is exact.
    fn emit_part_draw(
        &self,
        render_pass: &mut wgpu::RenderPass<'_>,
        num_indices: u32,
        instance_offset: u64,
    ) {
        let stride = std::mem::size_of::<InstanceRaw>() as u64;
        if self.shared.base_instance {
            let instance = (instance_offset / stride) as u32;
            render_pass.draw_indexed(0..num_indices, 0, instance..instance + 1);
        } else {
            render_pass.set_vertex_buffer(
                1,
                self.instance_buffer
                    .slice(instance_offset..instance_offset + stride),
            );
            render_pass.draw_indexed(0..num_indices, 0, 0..1);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
    }

    fn write_instances(&mut self, offset: u64, instances: &[InstanceRaw]) {
        if instances.is_empty() {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(instances);
        // Offsets come from the monotonic `reserve_instances` cursor, so a
        // write starting below the watermark overlaps one already made
        // this frame. Both land before the frame's single submit, where
        // write_buffer batching makes the later one win for every draw
        // that reads the range — silently, and for the wrong parts.
        debug_assert!(
            offset >= self.instance_write_watermark,
            "instance_buffer offset {offset} rewrites bytes already staged this frame \
             (watermark {}); every draw reading that range would see the later write",
            self.instance_write_watermark,
        );
        self.instance_write_watermark = offset + bytes.len() as u64;
        self.frame_stats.instance_slots_written += instances.len() as u32;
        self.frame_stats.instance_bytes_written += bytes.len() as u64;
        if self.batch_instance_writes {
            let start = offset as usize;
            let end = start + bytes.len();
            if self.instance_staging.len() < end {
                self.instance_staging.resize(end, 0);
            }
            self.instance_staging[start..end].copy_from_slice(bytes);
            self.instance_staging_len = self.instance_staging_len.max(end);
        } else {
            self.note_queue_write(RenderBuffer::Instance);
            self.queue
                .write_buffer(&self.instance_buffer, offset, bytes);
        }
    }

    fn flush_instance_writes(&mut self) {
        if self.batch_instance_writes && self.instance_staging_len > 0 {
            self.note_queue_write(RenderBuffer::Instance);
            self.queue.write_buffer(
                &self.instance_buffer,
                0,
                &self.instance_staging[..self.instance_staging_len],
            );
        }
        self.batch_instance_writes = false;
        self.instance_staging_len = 0;
    }

    /// Grow part_uniform_buffer to fit `count` slots and reset the
    /// cursor. Mid-frame growth would strand already-recorded draws on
    /// the old buffer, so sizing must happen up front.
    fn begin_frame_uniforms(&mut self, count: u32) {
        self.frame_stats.part_uniform_slots_budgeted = count;
        if count > self.part_uniform_capacity {
            self.note_realloc(RenderBuffer::PartUniform);
            self.part_uniform_capacity = count.max(256).next_power_of_two();
            self.part_uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Part Uniform Buffer"),
                size: self.shared.part_uniform_stride * self.part_uniform_capacity as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.part_uniform_bind_group =
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    layout: &self.shared.part_uniform_bind_group_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.part_uniform_buffer,
                            offset: 0,
                            size: std::num::NonZeroU64::new(
                                std::mem::size_of::<PartUniform>() as u64
                            ),
                        }),
                    }],
                    label: Some("part_uniform_bind_group"),
                });
        }
        self.part_uniform_cursor = 0;
        self.part_uniform_staging_len = 0;
        self.part_uniform_write_watermark = 0;
        self.batch_part_uniform_writes = true;
        // Both frame buffers are now sized; every pass recorded from here
        // on binds them, so nothing may reallocate until the next frame.
        self.frame_sizing_closed = true;
    }

    fn flush_part_uniform_writes(&mut self) {
        if self.batch_part_uniform_writes && self.part_uniform_staging_len > 0 {
            self.note_queue_write(RenderBuffer::PartUniform);
            self.queue.write_buffer(
                &self.part_uniform_buffer,
                0,
                &self.part_uniform_staging[..self.part_uniform_staging_len],
            );
        }
        self.batch_part_uniform_writes = false;
        self.part_uniform_staging_len = 0;
    }

    /// Writes a PartUniform into the next slot and returns its offset,
    /// suitable for passing as a dynamic offset in `set_bind_group`.
    pub fn write_part_uniform(
        &mut self,
        opacity: f32,
        tint: glam::Vec3,
        screen_tint: glam::Vec3,
        mask_threshold: f32,
    ) -> u32 {
        // Tints are authored in sRGB space; textures are sampled as
        // Rgba8UnormSrgb (linear on read), so the per-fragment multiply
        // / screen-tint math must also run in linear space. Convert
        // once here on the CPU rather than in the shader.
        let tint_linear = srgb_to_linear_vec3(tint);
        let screen_tint_linear = srgb_to_linear_vec3(screen_tint);
        let uniform = PartUniform::new(opacity, tint_linear, screen_tint_linear, mask_threshold);
        let offset = self.part_uniform_cursor;
        let bytes = bytemuck::bytes_of(&uniform);
        // Same aliasing hazard as `write_instances`: the cursor below is
        // monotonic, so a slot at or under the watermark is one an
        // earlier draw this frame already bound as a dynamic offset.
        debug_assert!(
            offset >= self.part_uniform_write_watermark,
            "part_uniform slot at offset {offset} rewritten this frame (watermark {}); \
             every draw binding that slot would see the later write",
            self.part_uniform_write_watermark,
        );
        self.part_uniform_write_watermark = offset + self.shared.part_uniform_stride;
        self.frame_stats.part_uniform_writes += 1;
        if self.batch_part_uniform_writes {
            let start = offset as usize;
            let end = start + bytes.len();
            if self.part_uniform_staging.len() < end {
                self.part_uniform_staging.resize(end, 0);
            }
            self.part_uniform_staging[start..end].copy_from_slice(bytes);
            self.part_uniform_staging_len = self.part_uniform_staging_len.max(end);
        } else {
            self.note_queue_write(RenderBuffer::PartUniform);
            self.queue
                .write_buffer(&self.part_uniform_buffer, offset, bytes);
        }
        self.part_uniform_cursor += self.shared.part_uniform_stride;
        debug_assert!(
            self.part_uniform_cursor
                <= self.shared.part_uniform_stride * self.part_uniform_capacity as u64,
            "part_uniform_buffer overrun — begin_frame_uniforms undersized",
        );
        offset as u32
    }

    /// Reset camera-slot reservations before recording command buffers for a
    /// new queue submission. Do not call this between `render_list` calls that
    /// will be submitted together.
    pub fn begin_camera_submit(&mut self) {
        self.camera_slots_used = 0;
    }

    /// Set the camera view-projection retained for subsequent `render_list`
    /// calls. The render call reserves and uploads a distinct uniform slot.
    pub fn update_camera(&mut self, view_proj: glam::Mat4) {
        self.camera_view_proj = view_proj;
    }

    fn reserve_camera(&mut self) -> RendererResult<()> {
        let offset = camera_slot_offset(self.camera_slots_used, self.shared.camera_stride)?;
        let camera_uniform = CameraUniform {
            view_proj: self.camera_view_proj.to_cols_array_2d(),
        };
        self.note_queue_write(RenderBuffer::Camera);
        self.queue.write_buffer(
            &self.camera_buffer,
            offset,
            bytemuck::cast_slice(&[camera_uniform]),
        );
        self.camera_offset = offset as u32;
        self.camera_slots_used += 1;
        Ok(())
    }

    /// Framebuffer-pixel AABB of one part's rendered geometry: its
    /// deform-expanded local bounds projected through `model` and the
    /// retained camera. None when the bound is unknown (boundless / not
    /// yet uploaded mesh, or a non-finite projection) — callers then use
    /// the full viewport.
    fn part_pixel_aabb(
        &self,
        mesh_id: u32,
        model: glam::Mat4,
        width: u32,
        height: u32,
    ) -> Option<[f32; 4]> {
        let local = self.mesh_buffers.get(mesh_id as usize)?.local_bounds?;
        let pad = self
            .deform_max
            .get(mesh_id as usize)
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

    /// Scissor rect tightly bounding one part's rendered pixels, or None to
    /// signal "use the full viewport".
    fn part_scissor_rect(
        &self,
        mesh_id: u32,
        model: glam::Mat4,
        width: u32,
        height: u32,
    ) -> Option<ScreenRect> {
        pixels_to_scissor(
            self.part_pixel_aabb(mesh_id, model, width, height)?,
            width,
            height,
        )
    }

    /// Scissor rect bounding the UNION of a composite's children — the
    /// region its blit can touch. None (full viewport) when any child's
    /// bound is unknown or a nested composite child appears.
    fn children_union_scissor(
        &self,
        children: &[crate::collect::DrawableInfo],
        width: u32,
        height: u32,
    ) -> Option<ScreenRect> {
        use crate::collect::DrawableInfo;
        let mut acc: Option<[f32; 4]> = None;
        for c in children {
            let DrawableInfo::Part {
                mesh_id, transform, ..
            } = c
            else {
                return None;
            };
            let aabb = self.part_pixel_aabb(*mesh_id, *transform, width, height)?;
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
        pixels_to_scissor(acc?, width, height)
    }

    /// Shared deform-upload core: write each changed mesh's combined verts
    /// into the mirror, zero meshes that went inactive (otherwise the
    /// vertex shader keeps reading a stale config's offsets), and flush the
    /// single touched byte range. The `generation` skips re-uploading a
    /// mesh whose deform is unchanged from the last sync.
    pub(crate) fn upload_deforms<'a>(
        &mut self,
        active: impl Iterator<Item = (u32, u64, &'a [glam::Vec2])>,
    ) {
        // Coalesce dirty ranges only across gaps this small: below it,
        // one write over the untouched bytes beats a second write_buffer's
        // fixed overhead; above it, the copied span is pure waste.
        const DEFORM_MERGE_GAP: u64 = 4096;

        self.deform_still_active.clear();
        let mut ranges = std::mem::take(&mut self.deform_write_ranges);
        ranges.clear();
        for (mesh_id, generation, combined) in active {
            let Some(buf) = self.mesh_buffers.get(mesh_id as usize) else {
                continue;
            };
            if combined.len() != buf.vert_count as usize {
                tracing::warn!(
                    node = mesh_id,
                    stack_len = combined.len(),
                    buffer_len = buf.vert_count,
                    "upload_deforms: deform length mismatch, skipping",
                );
                continue;
            }
            if self.deform_uploaded.get(mesh_id as usize).copied() == Some(generation) {
                self.deform_still_active.insert(mesh_id as usize, ());
                continue;
            }
            let bytes: &[u8] = bytemuck::cast_slice(combined);
            let start = buf.deform_offset as usize;
            let end = start + bytes.len();
            self.deform_upload_mirror[start..end].copy_from_slice(bytes);
            ranges.push((buf.deform_offset, buf.deform_offset + bytes.len() as u64));
            self.deform_uploaded.insert(mesh_id as usize, generation);
            self.deform_still_active.insert(mesh_id as usize, ());
            // Bounds padding: the largest per-axis local shift this deform
            // applies. Recomputed only on a real upload; the generation
            // memo-hit above keeps the cached value.
            let max_abs = combined
                .iter()
                .fold(0.0f32, |m, v| m.max(v.x.abs()).max(v.y.abs()));
            self.deform_max.insert(mesh_id as usize, max_abs);
        }

        self.deform_inactive_scratch.clear();
        for i in self.deform_uploaded.keys() {
            if !self.deform_still_active.contains(i) {
                self.deform_inactive_scratch.push(i as u32);
            }
        }
        for i in 0..self.deform_inactive_scratch.len() {
            let mesh_id = self.deform_inactive_scratch[i];
            let Some(buf) = self.mesh_buffers.get(mesh_id as usize) else {
                self.deform_uploaded.remove(mesh_id as usize);
                self.deform_max.remove(mesh_id as usize);
                continue;
            };
            let start = buf.deform_offset as usize;
            let end = start + buf.deform_size as usize;
            self.deform_upload_mirror[start..end].fill(0);
            ranges.push((buf.deform_offset, buf.deform_offset + buf.deform_size));
            self.deform_uploaded.remove(mesh_id as usize);
            self.deform_max.remove(mesh_id as usize);
        }

        // Merge sorted ranges across sub-threshold gaps, then flush one
        // write per run. pending_deform_bytes is the bytes actually sent
        // (merged-run lengths), not the min..max span.
        ranges.sort_unstable_by_key(|&(start, _)| start);
        let mut written = 0u64;
        let mut run: Option<(u64, u64)> = None;
        for &(start, end) in ranges.iter() {
            match run {
                Some((run_start, run_end)) if start <= run_end + DEFORM_MERGE_GAP => {
                    run = Some((run_start, run_end.max(end)));
                }
                Some((run_start, run_end)) => {
                    self.note_queue_write(RenderBuffer::Deform);
                    self.queue.write_buffer(
                        &self.deform_buffer,
                        run_start,
                        &self.deform_upload_mirror[run_start as usize..run_end as usize],
                    );
                    written += run_end - run_start;
                    run = Some((start, end));
                }
                None => run = Some((start, end)),
            }
        }
        if let Some((run_start, run_end)) = run {
            self.note_queue_write(RenderBuffer::Deform);
            self.queue.write_buffer(
                &self.deform_buffer,
                run_start,
                &self.deform_upload_mirror[run_start as usize..run_end as usize],
            );
            written += run_end - run_start;
        }
        self.pending_deform_bytes = written;

        ranges.clear();
        self.deform_write_ranges = ranges;
    }

    pub fn clear_meshes(&mut self) {
        self.mesh_buffers.clear();
        self.reset_deform_buffer_layout();
    }

    /// Release the GPU state a [`crate::RenderCache`] rebuild is about to
    /// replace, leaving only what the new build named again.
    ///
    /// Mesh slots and the whole deform atlas go: mesh ids are handed out
    /// densely from zero on every build, so a rebuild renames every one of
    /// them and the atlas ranges reserved for the old ones are dead.
    ///
    /// `keep` is `(old slot, new slot)` for every texture the new build wants
    /// and the last build already uploaded. Those are **moved** into their new
    /// slots rather than freed; every other texture slot is dropped, so a
    /// model that shrank strands nothing under a number the new build does not
    /// address. Moving is the point: a texture removed from the middle of a
    /// model shifts every later one down a slot, and a sweep that only
    /// truncated would make each of those a re-decode and a re-upload of an
    /// image the GPU already holds. A moved texture keeps the debug label its
    /// first upload gave it, which is the one place a slot number here can go
    /// stale.
    ///
    /// A rebuild runs from `prepare` or `refresh`, both outside a frame, so
    /// this never strands a recorded pass on a freed buffer.
    pub(crate) fn release_for_rebuild(&mut self, keep: &[(u32, u32)]) {
        self.clear_meshes();
        let mut moved: Vec<(u32, Texture)> = Vec::with_capacity(keep.len());
        for &(from, to) in keep {
            if let Some(texture) = self.textures.take(from as usize) {
                moved.push((to, texture));
            }
        }
        self.textures.clear();
        for (to, texture) in moved {
            self.textures.insert(to as usize, texture);
        }
    }

    /// Everything [`Self::upload_texture`] would refuse, checked without
    /// touching GPU state — so a rebuild can validate every upload before it
    /// releases the build those uploads replace.
    pub(crate) fn validate_texture_upload(&self, tex: &DecodedTexture) -> RendererResult<()> {
        validate_rgba8_texture(
            tex.width,
            tex.height,
            tex.rgba.len(),
            self.device.limits().max_texture_dimension_2d,
        )
        .map(|_| ())
    }

    /// The device's 2D texture size limit. Test-facing, as
    /// [`Self::live_mesh_slots`]: it is how a test authors a texture the
    /// device refuses.
    #[doc(hidden)]
    pub fn max_texture_dimension(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    /// Mesh slots currently holding a GPU buffer. Test-facing: it is how a
    /// rebuild's release is observable from outside the crate.
    #[doc(hidden)]
    pub fn live_mesh_slots(&self) -> usize {
        self.mesh_buffers.live()
    }

    /// Texture slots currently holding a GPU texture. Test-facing, as
    /// [`Self::live_mesh_slots`].
    #[doc(hidden)]
    pub fn live_texture_slots(&self) -> usize {
        self.textures.live()
    }

    /// Bytes of the deform atlas currently reserved by uploaded meshes.
    /// Test-facing, as [`Self::live_mesh_slots`].
    #[doc(hidden)]
    pub fn reserved_deform_bytes(&self) -> u64 {
        self.deform_buffer_len
    }

    /// Tolerance matches the importer (which matches the reference's
    /// meshdata.d): uvs are optional (zeros substituted) and a trailing
    /// partial triangle is ignored — the runtime only walks whole
    /// triangles. An out-of-range index stays an error: it would poison
    /// vertex fetches for this mesh.
    pub fn upload_mesh(
        &mut self,
        mesh_id: u32,
        mesh: &catchlight_core::Mesh,
    ) -> RendererResult<()> {
        if !mesh.uvs.is_empty() && mesh.vertices.len() != mesh.uvs.len() {
            return Err(RendererError::MeshVertexUvMismatch {
                mesh_id,
                vertices: mesh.vertices.len(),
                uvs: mesh.uvs.len(),
            });
        }
        let vertex_count = mesh.vertices.len();
        if let Some(bad) = mesh
            .indices
            .iter_u32()
            .find(|&i| (i as usize) >= vertex_count)
        {
            return Err(RendererError::MeshIndexOutOfBounds {
                mesh_id,
                index: bad,
                vertices: vertex_count,
            });
        }

        let uv_at = |i: usize| {
            mesh.uvs
                .get(i)
                .copied()
                .unwrap_or(catchlight_core::Vec2::ZERO)
        };
        let vertices: Vec<Vertex> = mesh
            .vertices
            .iter()
            .enumerate()
            .map(|(i, pos)| {
                let uv = uv_at(i);
                Vertex {
                    position: [pos.x - mesh.origin.x, pos.y - mesh.origin.y],
                    uv: [uv.x, uv.y],
                }
            })
            .collect();

        // AABB over the shifted positions the shader transforms. Empty
        // mesh → None (boundless): a bounds consumer then uses the full
        // viewport rather than an ill-defined box.
        let local_bounds = vertices.first().map(|first| {
            let mut min = glam::Vec2::from(first.position);
            let mut max = min;
            for v in &vertices[1..] {
                let p = glam::Vec2::from(v.position);
                min = min.min(p);
                max = max.max(p);
            }
            Aabb2 { min, max }
        });

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("Vertex Buffer {mesh_id}")),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

        let deform_size = (vertex_count * std::mem::size_of::<DeformAttr>()) as u64;
        let deform_offset = self.reserve_deform_buffer(deform_size);
        let deform_start = deform_offset as usize;
        let deform_end = deform_start + deform_size as usize;
        self.deform_upload_mirror[deform_start..deform_end].fill(0);
        self.note_queue_write(RenderBuffer::Deform);
        self.queue.write_buffer(
            &self.deform_buffer,
            deform_offset,
            &self.deform_upload_mirror[deform_start..deform_end],
        );

        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(&format!("Index Buffer {mesh_id}")),
                contents: mesh.indices.as_bytes(),
                usage: wgpu::BufferUsages::INDEX,
            });

        let index_format = match &mesh.indices {
            catchlight_core::MeshIndices::U16(_) => wgpu::IndexFormat::Uint16,
            catchlight_core::MeshIndices::U32(_) => wgpu::IndexFormat::Uint32,
        };

        let mesh_buffer = MeshBuffer {
            vertex_buffer,
            index_buffer,
            deform_offset,
            deform_size,
            vert_count: vertex_count as u32,
            // Truncate a trailing partial triangle.
            num_indices: (mesh.indices.len() - mesh.indices.len() % 3) as u32,
            index_format,
            local_bounds,
        };

        self.mesh_buffers.insert(mesh_id as usize, mesh_buffer);
        Ok(())
    }

    /// Resolve the bind group used for a Part draw: the albedo Texture's
    /// own bind group (albedo + sampler). Returns None only if the albedo
    /// texture has not been uploaded.
    fn part_bind_group(&self, albedo: u32) -> Option<&wgpu::BindGroup> {
        self.textures.get(albedo as usize).map(|t| &t.bind_group)
    }

    /// Uploads a canonical [`DecodedTexture`] straight to the GPU as
    /// `Rgba8UnormSrgb`. The importer has already normalised the bytes
    /// to premultiplied LINEAR color encoded as sRGB bytes (byte =
    /// srgb_encode(linear * alpha); see `ModelTexture::decode` and the
    /// basic.wgsl header), so the upload needs no branching.
    pub fn upload_texture(&mut self, texture_id: u32, tex: &DecodedTexture) -> RendererResult<()> {
        if let Some(existing) = self.textures.get(texture_id as usize) {
            if existing.source.as_ref().is_some_and(|src| src.matches(tex)) {
                return Ok(());
            }
        }

        let mut texture = Texture::from_decoded_rgba8(
            &self.device,
            &self.queue,
            tex.rgba.to_vec(),
            tex.width,
            tex.height,
            &self.shared,
            &format!("Texture {texture_id}"),
        )?;
        // Mip generation for a freshly created texture submits its own
        // command buffer. Harmless where uploads belong (between frames),
        // but tallied so a frame that ends up doing texture work reports a
        // non-zero submit count instead of silently splitting in two.
        self.frame_stats.queue_submits += 1;
        texture.source = Some(TextureSource {
            rgba: tex.rgba.clone(),
            width: tex.width,
            height: tex.height,
        });

        self.textures.insert(texture_id as usize, texture);
        Ok(())
    }

    fn prepare_composite_mask_textures(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
    ) {
        self.composite_mask_textures.clear();
        let mut source_ids: Vec<_> = render_list.composite_mask_sources.keys().copied().collect();
        source_ids.sort_unstable();

        for node_id in source_ids {
            let Some(source) = render_list.composite_mask_sources.get(&node_id) else {
                continue;
            };
            let slot = composites.acquire(&self.shared, &self.device);
            let base_offset = self.reserve_instances(source.parts.len());
            let instances: Vec<_> = source
                .parts
                .iter()
                .map(|part| InstanceRaw::from_transform(part.transform))
                .collect();
            self.write_instances(base_offset, &instances);
            let uniform_offsets: Vec<_> = source
                .parts
                .iter()
                .map(|part| {
                    self.write_part_uniform(
                        1.0,
                        glam::Vec3::ONE,
                        glam::Vec3::ZERO,
                        part.mask_threshold,
                    )
                })
                .collect();

            let mut pass = self.counted_begin_pass(
                encoder,
                &wgpu::RenderPassDescriptor {
                    label: Some("Composite Mask Source Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        depth_slice: None,
                        view: &slot.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                },
            );
            pass.set_pipeline(&self.shared.composite_mask_part_pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
            self.bind_pass_instances(&mut pass);
            let stride = std::mem::size_of::<InstanceRaw>() as u64;
            for (i, part) in source.parts.iter().enumerate() {
                let (Some(mesh), Some(texture)) = (
                    self.mesh_buffers.get(part.mesh_id as usize),
                    self.textures.get(part.texture_id as usize),
                ) else {
                    if !self.mesh_buffers.contains(part.mesh_id as usize) {
                        self.current_stats.skipped_missing_mask_mesh += 1;
                    }
                    if !self.textures.contains(part.texture_id as usize) {
                        self.current_stats.skipped_missing_mask_texture += 1;
                    }
                    continue;
                };
                pass.set_bind_group(1, &texture.bind_group, &[]);
                pass.set_bind_group(2, &self.part_uniform_bind_group, &[uniform_offsets[i]]);
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_vertex_buffer(
                    2,
                    self.deform_buffer
                        .slice(mesh.deform_offset..mesh.deform_offset + mesh.deform_size),
                );
                pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                self.emit_part_draw(&mut pass, mesh.num_indices, base_offset + i as u64 * stride);
            }
            drop(pass);
            self.composite_mask_textures.insert(
                node_id,
                PreparedCompositeMask {
                    texture: slot,
                    opacity: source.opacity,
                    mask_threshold: source.mask_threshold,
                },
            );
        }
    }

    /// Alpha-mask fallback (has_stencil == false): rasterize each mask
    /// source's shape into the shared mask-alpha texture
    /// (`stencil.view`). Each mask source's instance transform must
    /// already be written into `instance_buffer` at `base_offset + i *
    /// size_of::<InstanceRaw>()`. Each source draw writes its own
    /// part-uniform slot carrying the source's alpha threshold
    /// (reference: part/package.d renderMask uses the source's
    /// maskAlphaThreshold).
    #[allow(clippy::expect_used)]
    fn write_mask_sources(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        label: &str,
        stencil: &StencilTarget,
        mask_sources: &[crate::collect::MaskSourceData],
        base_offset: u64,
    ) {
        use crate::collect::MaskSourceData;

        let stride = std::mem::size_of::<InstanceRaw>() as u64;

        for m in mask_sources {
            if let MaskSourceData::Part {
                mesh_id,
                texture_id,
                ..
            } = m
            {
                if !self.mesh_buffers.contains(*mesh_id as usize) {
                    self.current_stats.skipped_missing_mask_mesh += 1;
                }
                if !self.textures.contains(*texture_id as usize) {
                    self.current_stats.skipped_missing_mask_texture += 1;
                }
            }
        }

        let source_uniform_offsets: SmallVec<[u32; 2]> = mask_sources
            .iter()
            .map(|m| match m {
                MaskSourceData::Part { mask_threshold, .. } => {
                    self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, *mask_threshold)
                }
                MaskSourceData::Composite { node_id, .. } => {
                    let (opacity, threshold) = self
                        .composite_mask_textures
                        .get(node_id)
                        .map(|source| (source.opacity, source.mask_threshold))
                        .unwrap_or((0.0, 1.0));
                    self.write_part_uniform(opacity, glam::Vec3::ONE, glam::Vec3::ZERO, threshold)
                }
            })
            .collect();

        // Initialize the mask to 1 when every source is a DodgeMask, otherwise 0;
        // per-source draws then REPLACE with 1 (Mask) or 0 (DodgeMask),
        // and the masked content is gated on mask==1. Mirror that here
        // so DodgeMask sources invert the mask region rather than erasing it.
        let any_regular_mask = mask_sources
            .iter()
            .any(|m| m.mode() == catchlight_core::MaskMode::Mask);
        let c = if any_regular_mask { 0.0 } else { 1.0 };
        let alpha_clear = wgpu::Color {
            r: c,
            g: c,
            b: c,
            a: c,
        };

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: &stencil.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(alpha_clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        self.bind_pass_instances(&mut render_pass);

        let mut current_part_dodge: Option<bool> = None;
        let mut part_index = 0usize;
        for (i, mask_source) in mask_sources.iter().enumerate() {
            match mask_source {
                MaskSourceData::Part {
                    mesh_id,
                    texture_id,
                    mode,
                    ..
                } => {
                    let off = base_offset + part_index as u64 * stride;
                    part_index += 1;
                    let (Some(mask_mesh), Some(mask_texture)) = (
                        self.mesh_buffers.get(*mesh_id as usize),
                        self.textures.get(*texture_id as usize),
                    ) else {
                        continue;
                    };
                    let dodge = *mode == catchlight_core::MaskMode::DodgeMask;
                    if current_part_dodge != Some(dodge) {
                        let pipeline = if dodge {
                            self.shared.mask_alpha_dodge_pipeline.as_ref().expect(
                                "mask_alpha_dodge_pipeline must exist when has_stencil == false",
                            )
                        } else {
                            self.shared
                                .mask_alpha_pipeline
                                .as_ref()
                                .expect("mask_alpha_pipeline must exist when has_stencil == false")
                        };
                        render_pass.set_pipeline(pipeline);
                        render_pass.set_bind_group(
                            0,
                            &self.camera_bind_group,
                            &[self.camera_offset],
                        );
                        current_part_dodge = Some(dodge);
                    }
                    render_pass.set_bind_group(1, &mask_texture.bind_group, &[]);
                    render_pass.set_bind_group(
                        2,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    render_pass.set_vertex_buffer(0, mask_mesh.vertex_buffer.slice(..));
                    render_pass.set_vertex_buffer(
                        2,
                        self.deform_buffer.slice(
                            mask_mesh.deform_offset
                                ..mask_mesh.deform_offset + mask_mesh.deform_size,
                        ),
                    );
                    render_pass
                        .set_index_buffer(mask_mesh.index_buffer.slice(..), mask_mesh.index_format);
                    self.emit_part_draw(&mut render_pass, mask_mesh.num_indices, off);
                }
                MaskSourceData::Composite { node_id, mode } => {
                    let Some(source) = self.composite_mask_textures.get(node_id).cloned() else {
                        continue;
                    };
                    let dodge = *mode == catchlight_core::MaskMode::DodgeMask;
                    let pipeline = if dodge {
                        self.shared
                            .composite_mask_alpha_dodge_pipeline
                            .as_ref()
                            .expect("composite dodge pipeline exists on alpha-mask path")
                    } else {
                        &self.shared.composite_mask_write_pipeline
                    };
                    render_pass.set_pipeline(pipeline);
                    render_pass.set_bind_group(0, &source.texture.blit_bind_group, &[]);
                    render_pass.set_bind_group(
                        1,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
                    render_pass.draw(0..3, 0..1);
                    current_part_dodge = None;
                }
            }
        }
    }

    /// Stencil-path mask-source recording into the caller's open
    /// color+stencil pass: regular sources REPLACE the stencil with
    /// `ref_value` where they rasterize, dodge sources punch 0. An
    /// all-DodgeMask batch first seeds the whole viewport with
    /// `ref_value` via the fullscreen stencil fill so pixels outside every
    /// dodge source remain enabled. Per-source
    /// part-uniform slots carry each source's alpha threshold. The
    /// caller must have bound the camera at group 0 (the mask-write
    /// pipeline shares the part pipeline layout).
    // Invariant: only called on the stencil path, where
    // stencil_fill_pipeline is always built.
    #[allow(clippy::expect_used)]
    fn record_mask_sources_stencil(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        mask_sources: &[crate::collect::MaskSourceData],
        base_offset: u64,
        ref_value: u32,
        bound_texture: &mut Option<u32>,
    ) {
        use crate::collect::MaskSourceData;

        let stride = std::mem::size_of::<InstanceRaw>() as u64;

        for m in mask_sources {
            if let MaskSourceData::Part {
                mesh_id,
                texture_id,
                ..
            } = m
            {
                if !self.mesh_buffers.contains(*mesh_id as usize) {
                    self.current_stats.skipped_missing_mask_mesh += 1;
                }
                if !self.textures.contains(*texture_id as usize) {
                    self.current_stats.skipped_missing_mask_texture += 1;
                }
            }
        }

        let source_uniform_offsets: SmallVec<[u32; 2]> = mask_sources
            .iter()
            .map(|m| match m {
                MaskSourceData::Part { mask_threshold, .. } => {
                    self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, *mask_threshold)
                }
                MaskSourceData::Composite { node_id, .. } => {
                    let (opacity, threshold) = self
                        .composite_mask_textures
                        .get(node_id)
                        .map(|source| (source.opacity, source.mask_threshold))
                        .unwrap_or((0.0, 1.0));
                    self.write_part_uniform(opacity, glam::Vec3::ONE, glam::Vec3::ZERO, threshold)
                }
            })
            .collect();

        let any_regular_mask = mask_sources
            .iter()
            .any(|m| m.mode() == catchlight_core::MaskMode::Mask);
        if !any_regular_mask {
            let fill = self
                .shared
                .stencil_fill_pipeline
                .as_ref()
                .expect("stencil_fill_pipeline must exist when has_stencil == true");
            render_pass.set_pipeline(fill);
            render_pass.set_stencil_reference(ref_value);
            self.current_stats.pipeline_swaps += 1;
            self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
            render_pass.draw(0..3, 0..1);
        }

        self.bind_pass_instances(render_pass);
        let mut part_pipeline_bound = false;
        let mut part_index = 0usize;
        for (i, mask_source) in mask_sources.iter().enumerate() {
            match mask_source {
                MaskSourceData::Part {
                    mesh_id,
                    texture_id,
                    mode,
                    ..
                } => {
                    let off = base_offset + part_index as u64 * stride;
                    part_index += 1;
                    let (Some(mask_mesh), Some(mask_texture)) = (
                        self.mesh_buffers.get(*mesh_id as usize),
                        self.textures.get(*texture_id as usize),
                    ) else {
                        continue;
                    };
                    if !part_pipeline_bound {
                        render_pass.set_pipeline(&self.shared.mask_write_pipeline);
                        render_pass.set_bind_group(
                            0,
                            &self.camera_bind_group,
                            &[self.camera_offset],
                        );
                        self.current_stats.pipeline_swaps += 1;
                        part_pipeline_bound = true;
                    }
                    render_pass.set_stencil_reference(
                        if *mode == catchlight_core::MaskMode::DodgeMask {
                            0
                        } else {
                            ref_value
                        },
                    );
                    if *bound_texture != Some(*texture_id) {
                        render_pass.set_bind_group(1, &mask_texture.bind_group, &[]);
                        *bound_texture = Some(*texture_id);
                        self.current_stats.texture_binds += 1;
                    }
                    render_pass.set_bind_group(
                        2,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    render_pass.set_vertex_buffer(0, mask_mesh.vertex_buffer.slice(..));
                    render_pass.set_vertex_buffer(
                        2,
                        self.deform_buffer.slice(
                            mask_mesh.deform_offset
                                ..mask_mesh.deform_offset + mask_mesh.deform_size,
                        ),
                    );
                    render_pass
                        .set_index_buffer(mask_mesh.index_buffer.slice(..), mask_mesh.index_format);
                    self.emit_part_draw(render_pass, mask_mesh.num_indices, off);
                }
                MaskSourceData::Composite { node_id, mode } => {
                    let Some(source) = self.composite_mask_textures.get(node_id).cloned() else {
                        continue;
                    };
                    render_pass.set_pipeline(&self.shared.composite_mask_write_pipeline);
                    render_pass.set_stencil_reference(
                        if *mode == catchlight_core::MaskMode::DodgeMask {
                            0
                        } else {
                            ref_value
                        },
                    );
                    render_pass.set_bind_group(0, &source.texture.blit_bind_group, &[]);
                    render_pass.set_bind_group(
                        1,
                        &self.part_uniform_bind_group,
                        &[source_uniform_offsets[i]],
                    );
                    self.current_stats.pipeline_swaps += 1;
                    self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
                    render_pass.draw(0..3, 0..1);
                    part_pipeline_bound = false;
                    *bound_texture = None;
                }
            }
        }
        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
    }

    /// Filter a masked batch for resident GPU resources into
    /// renderer-owned index scratch and write the batch's instance
    /// block — `[mask0, .., maskK-1, part0, .., partN-1]` at distinct
    /// offsets in the frame-wide instance_buffer (one reserve carves
    /// the whole block; every pass in the frame shares one submit, so
    /// all writes replay together regardless of pass order). Returns
    /// the surviving indices plus the mask and part instance base
    /// offsets, or None when nothing survives. The caller must hand
    /// the indices back to `draw_filter_scratch`; an early `?` that
    /// skips that loses the scratch (the field reverts to an empty
    /// Vec) — harmless.
    fn prepare_masked_part_draws(
        &mut self,
        draws: &[PreparedMaskedPartDraw<'_>],
    ) -> Option<(Vec<usize>, u64, u64)> {
        // All draws in a batch share one mask signature (caller
        // guarantees), so the sources come from the first draw.
        let mask_sources = draws.first()?.mask_sources;

        let mut valid = std::mem::take(&mut self.draw_filter_scratch);
        valid.clear();
        for (i, d) in draws.iter().enumerate() {
            let has_mesh = self.mesh_buffers.contains(d.draw.draw.mesh_id as usize);
            let has_tex = self.textures.contains(d.draw.draw.albedo as usize);
            if !has_mesh {
                self.current_stats.skipped_missing_mesh += 1;
            }
            if !has_tex {
                self.current_stats.skipped_missing_texture += 1;
            }
            if has_mesh && has_tex {
                valid.push(i);
            }
        }
        if valid.is_empty() {
            self.draw_filter_scratch = valid;
            return None;
        }
        self.current_stats.drawn_parts += valid.len() as u32;

        let stride = std::mem::size_of::<InstanceRaw>() as u64;
        let mask_part_count = mask_sources
            .iter()
            .filter(|source| source.is_part())
            .count();
        let base_offset = self.reserve_instances(mask_part_count + valid.len());
        let parts_base = base_offset + mask_part_count as u64 * stride;

        let mut instance_data = std::mem::take(&mut self.instance_scratch);
        instance_data.clear();
        for m in mask_sources {
            if let crate::collect::MaskSourceData::Part { transform, .. } = m {
                instance_data.push(InstanceRaw::from_transform(*transform));
            }
        }
        for &idx in &valid {
            instance_data.push(InstanceRaw::from_transform(draws[idx].draw.draw.transform));
        }
        self.write_instances(base_offset, &instance_data);
        self.instance_scratch = instance_data;

        Some((valid, base_offset, parts_base))
    }

    // Invariant: `valid` only holds indices whose mesh / texture were
    // verified resident by prepare_masked_part_draws; part_bind_group
    // is populated by render_list.
    //
    // Record a masked batch's content draws into the caller's open
    // pass: pipeline / texture / mesh state switch on change, bind
    // group 2 re-binds per draw. The caller has already set up the
    // mask test (stencil reference on the stencil path, group 3 mask
    // texture on the alpha path). Draw order is preserved.
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn record_masked_part_draws(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        draws: &[PreparedMaskedPartDraw<'_>],
        valid: &[usize],
        parts_base: u64,
        bound_texture: &mut Option<u32>,
    ) -> RendererResult<()> {
        let stride = std::mem::size_of::<InstanceRaw>() as u64;
        let has_stencil = self.shared.has_stencil;

        let mut current_blend_mode: Option<BlendMode> = None;
        let mut current_mesh: Option<u32> = None;
        let mut pipeline_swaps = 0u32;
        let mut texture_binds = 0u32;

        self.bind_pass_instances(render_pass);
        for (i, &idx) in valid.iter().enumerate() {
            let d = &draws[idx];
            let part = &d.draw.draw;
            let blend = canonical_part_blend(part.blend_mode);
            if current_blend_mode != Some(blend) {
                let pipeline = if has_stencil {
                    self.shared
                        .masked_pipelines
                        .get(&blend)
                        .or_else(|| self.shared.masked_pipelines.get(&BlendMode::Normal))
                        .ok_or(RendererError::MissingMaskedPipeline(blend))?
                } else {
                    self.shared
                        .masked_sampled_pipelines
                        .get(&blend)
                        .or_else(|| self.shared.masked_sampled_pipelines.get(&BlendMode::Normal))
                        .ok_or(RendererError::MissingMaskedPipeline(blend))?
                };
                render_pass.set_pipeline(pipeline);
                current_blend_mode = Some(blend);
                pipeline_swaps += 1;
            }

            let mesh = self.mesh_buffers.get(part.mesh_id as usize).unwrap();
            let key = part.albedo;
            if *bound_texture != Some(key) {
                let part_bg = self
                    .part_bind_group(part.albedo)
                    .expect("part bind group must exist — render_list ensures it");
                render_pass.set_bind_group(1, part_bg, &[]);
                *bound_texture = Some(key);
                texture_binds += 1;
            }
            if current_mesh != Some(part.mesh_id) {
                render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(
                    2,
                    self.deform_buffer
                        .slice(mesh.deform_offset..mesh.deform_offset + mesh.deform_size),
                );
                render_pass.set_index_buffer(mesh.index_buffer.slice(..), mesh.index_format);
                current_mesh = Some(part.mesh_id);
            }
            render_pass.set_bind_group(
                2,
                &self.part_uniform_bind_group,
                &[d.draw.part_uniform_offset],
            );
            let off = parts_base + i as u64 * stride;
            self.emit_part_draw(render_pass, mesh.num_indices, off);
        }

        self.current_stats.pipeline_swaps += pipeline_swaps;
        self.current_stats.texture_binds += texture_binds;
        Ok(())
    }

    // Alpha-path batched masked-Part render (has_stencil == false): a
    // run of consecutive masked parts that share one mask signature
    // (see `same_mask_signature`) collapses to one mask-alpha write
    // pass plus one masked-draw pass that samples the mask texture via
    // group 3. Draw order is preserved — callers must not reorder.
    //
    // `clear_target` clears the color target on the masked-draw pass
    // (used to fold a composite-slot clear into the first masked
    // child); the mask-alpha attachment is unaffected. Returns whether
    // a masked-draw pass was emitted.
    #[allow(clippy::too_many_arguments, clippy::expect_used)]
    fn render_masked_prepared_parts_to(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_view: &wgpu::TextureView,
        stencil: &StencilTarget,
        draws: &[PreparedMaskedPartDraw<'_>],
        clear_target: bool,
        clear_color: Option<wgpu::Color>,
        mask_write_label: &str,
        masked_draw_label: &str,
        scissor: Option<ScreenRect>,
    ) -> RendererResult<bool> {
        let Some((valid, base_offset, parts_base)) = self.prepare_masked_part_draws(draws) else {
            return Ok(false);
        };
        let mask_sources = draws[0].mask_sources;

        // Shared mask write (the sources are identical across a
        // same-signature batch); writes its own per-source uniforms.
        self.write_mask_sources(
            encoder,
            mask_write_label,
            stencil,
            mask_sources,
            base_offset,
        );

        let load_op = if clear_target {
            wgpu::LoadOp::Clear(clear_color.unwrap_or(wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }))
        } else {
            wgpu::LoadOp::Load
        };

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some(masked_draw_label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        render_pass.set_bind_group(
            3,
            stencil
                .mask_bind_group
                .as_ref()
                .expect("mask_bind_group must exist when has_stencil == false"),
            &[],
        );
        // Only the content draw is scissored — the mask-alpha texture was
        // written full above, but the masked draw samples it only inside
        // the rect, and the part's geometry lives inside the rect.
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_masked_part_draws(&mut render_pass, draws, &valid, parts_base, &mut None)?;
        drop(render_pass);

        self.draw_filter_scratch = valid;
        Ok(true)
    }

    /// Filter `drawables` for resident GPU resources into
    /// renderer-owned index scratch (no per-frame allocation) and
    /// write their instance block. Returns the surviving indices and
    /// the instance base offset, or None when nothing survives. The
    /// caller must hand the indices back to `draw_filter_scratch`.
    fn prepare_part_draws(&mut self, drawables: &[PreparedPartDraw]) -> Option<(Vec<usize>, u64)> {
        let mut filtered = std::mem::take(&mut self.draw_filter_scratch);
        filtered.clear();
        for (i, p) in drawables.iter().enumerate() {
            let d = &p.draw;
            let has_mesh = self.mesh_buffers.contains(d.mesh_id as usize);
            let has_tex = self.textures.contains(d.albedo as usize);
            if !(has_mesh && has_tex) {
                tracing::trace!(
                    mesh_id = d.mesh_id,
                    texture_id = d.albedo,
                    has_mesh,
                    has_tex,
                    "dropping drawable with missing GPU resources",
                );
                if !has_mesh {
                    self.current_stats.skipped_missing_mesh += 1;
                }
                if !has_tex {
                    self.current_stats.skipped_missing_texture += 1;
                }
                continue;
            }
            filtered.push(i);
        }
        self.current_stats.drawn_parts += filtered.len() as u32;
        if filtered.is_empty() {
            self.draw_filter_scratch = filtered;
            return None;
        }

        let base_offset = self.reserve_instances(filtered.len());
        let mut instance_data = std::mem::take(&mut self.instance_scratch);
        instance_data.clear();
        instance_data.extend(
            filtered
                .iter()
                .map(|&i| InstanceRaw::from_transform(drawables[i].draw.transform)),
        );
        self.write_instances(base_offset, &instance_data);
        self.instance_scratch = instance_data;

        Some((filtered, base_offset))
    }

    // Invariant: `filtered` only holds indices whose mesh / texture
    // were verified resident by prepare_part_draws; the Normal blend
    // mode pipeline is always compiled.
    //
    // Record a run of unmasked part draws into the caller's open pass.
    // Each draw carries its own `part_uniform_offset`, so bind group 2
    // is re-bound per draw while pipeline / texture / mesh state only
    // switch on change. Draw order is preserved exactly.
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn record_unmasked_part_draws(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        drawables: &[PreparedPartDraw],
        filtered: &[usize],
        base_offset: u64,
        bound_texture: &mut Option<u32>,
    ) {
        let stride = std::mem::size_of::<InstanceRaw>() as u64;

        let mut current_blend_mode: Option<BlendMode> = None;
        let mut current_mesh: Option<u32> = None;
        let mut pipeline_swaps = 0u32;
        let mut texture_binds = 0u32;

        self.bind_pass_instances(render_pass);
        for (i, &idx) in filtered.iter().enumerate() {
            let p = &drawables[idx];
            let d = &p.draw;
            let blend = canonical_part_blend(d.blend_mode);
            if current_blend_mode != Some(blend) {
                let pipeline = self
                    .shared
                    .pipelines
                    .get(&blend)
                    .or_else(|| self.shared.pipelines.get(&BlendMode::Normal))
                    .expect("Normal blend mode pipeline must exist");
                render_pass.set_pipeline(pipeline);
                current_blend_mode = Some(blend);
                pipeline_swaps += 1;
            }

            let mesh_buffer = self.mesh_buffers.get(d.mesh_id as usize).unwrap();
            let off = base_offset + i as u64 * stride;
            let key = d.albedo;
            if *bound_texture != Some(key) {
                let bind_group = self
                    .part_bind_group(d.albedo)
                    .expect("part bind group must exist — render_list ensures it");
                render_pass.set_bind_group(1, bind_group, &[]);
                *bound_texture = Some(key);
                texture_binds += 1;
            }
            if current_mesh != Some(d.mesh_id) {
                render_pass.set_vertex_buffer(0, mesh_buffer.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(
                    2,
                    self.deform_buffer.slice(
                        mesh_buffer.deform_offset
                            ..mesh_buffer.deform_offset + mesh_buffer.deform_size,
                    ),
                );
                render_pass
                    .set_index_buffer(mesh_buffer.index_buffer.slice(..), mesh_buffer.index_format);
                current_mesh = Some(d.mesh_id);
            }
            render_pass.set_bind_group(2, &self.part_uniform_bind_group, &[p.part_uniform_offset]);
            self.emit_part_draw(render_pass, mesh_buffer.num_indices, off);
        }

        self.current_stats.pipeline_swaps += pipeline_swaps;
        self.current_stats.texture_binds += texture_binds;
    }

    // Alpha-path batched unmasked-Part render: collapses a run of
    // consecutive unmasked parts into one render pass of its own.
    // Returns whether a draw pass was emitted (false when the batch is
    // empty or every draw filtered out for missing GPU resources) so
    // callers can fold the composite-slot clear into the first batch
    // that actually draws.
    fn render_prepared_parts(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        drawables: &[PreparedPartDraw],
        clear: bool,
        clear_color: Option<wgpu::Color>,
        scissor: Option<ScreenRect>,
    ) -> RendererResult<bool> {
        let Some((filtered, base_offset)) = self.prepare_part_draws(drawables) else {
            return Ok(false);
        };

        let load_op = if clear {
            let color = clear_color.unwrap_or(wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            });
            wgpu::LoadOp::Clear(color)
        } else {
            wgpu::LoadOp::Load
        };

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: load_op,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_unmasked_part_draws(
            &mut render_pass,
            drawables,
            &filtered,
            base_offset,
            &mut None,
        );
        drop(render_pass);

        self.draw_filter_scratch = filtered;
        Ok(true)
    }

    /// Open `s`'s pass if it isn't already open: color = `s.next_load`
    /// into `s.view`, stencil cleared to 0 at open and discarded at the
    /// end (nothing reads stencil across passes), camera bound at
    /// group 0. Resets the per-segment stencil-reference allocator.
    fn open_stencil_pass(&self, encoder: &mut wgpu::CommandEncoder, s: &mut StencilPassState<'_>) {
        if s.pass.is_some() {
            return;
        }
        let mut pass = self
            .counted_begin_pass(
                encoder,
                &wgpu::RenderPassDescriptor {
                    label: Some(s.label),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        depth_slice: None,
                        view: s.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: s.next_load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: s.stencil_view,
                        depth_ops: None,
                        stencil_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0),
                            store: wgpu::StoreOp::Discard,
                        }),
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                },
            )
            .forget_lifetime();
        pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        if let Some(r) = s.scissor {
            pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        s.next_load = wgpu::LoadOp::Load;
        s.next_ref = 1;
        s.bound_texture = None;
        s.pass = Some(pass);
    }

    /// End the sink's open pass (stencil path) before a pass outside
    /// the sink touches the target view (composite slot render + blit,
    /// dst-in-shader snapshot + blit). If the frame clear is still
    /// pending, emit it as an explicit pass — the external consumer
    /// loads the target. Alpha sink: no-op; its up-front explicit
    /// clear covers the first-drawable case.
    fn end_sink_pass(&self, encoder: &mut wgpu::CommandEncoder, sink: &mut PartSink<'_, '_>) {
        let PartSink::Stencil(s) = sink else { return };
        s.pass = None;
        if let wgpu::LoadOp::Clear(color) = s.next_load {
            self.explicit_clear_pass(encoder, s.view, color, "render_list deferred clear");
            s.next_load = wgpu::LoadOp::Load;
        }
    }

    /// Flush a pending run of unmasked parts as one batch. Stencil
    /// sink: record into the shared pass (opened lazily — a batch
    /// whose draws all filtered out leaves the pass and its clear
    /// untouched). Alpha sink: one pass of its own; the first batch
    /// that draws consumes the clear color, later flushes load. No-op
    /// when the batch is empty so barriers can call it unconditionally.
    fn flush_pending_parts(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        pending: &mut Vec<PreparedPartDraw>,
    ) -> RendererResult<()> {
        if pending.is_empty() {
            return Ok(());
        }
        match sink {
            PartSink::Stencil(s) => {
                if let Some((filtered, base_offset)) = self.prepare_part_draws(pending) {
                    self.open_stencil_pass(encoder, s);
                    if let Some(pass) = s.pass.as_mut() {
                        self.record_unmasked_part_draws(
                            pass,
                            pending,
                            &filtered,
                            base_offset,
                            &mut s.bound_texture,
                        );
                    }
                    self.draw_filter_scratch = filtered;
                }
            }
            PartSink::Alpha {
                view,
                has_rendered,
                clear_color,
                ..
            } => {
                let clear = !**has_rendered && clear_color.is_some();
                let drew =
                    self.render_prepared_parts(encoder, view, pending, clear, *clear_color, None)?;
                **has_rendered |= drew;
            }
        }
        pending.clear();
        Ok(())
    }

    /// Flush a pending run of same-signature masked parts. Stencil
    /// sink: allocate the next stencil reference and record mask
    /// sources + content into the shared pass. Alpha sink: one
    /// mask-alpha write pass + one masked draw pass.
    fn flush_pending_masked(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        pending: &mut Vec<PreparedMaskedPartDraw<'_>>,
    ) -> RendererResult<()> {
        if pending.is_empty() {
            return Ok(());
        }
        match sink {
            PartSink::Stencil(s) => {
                let mask_sources = pending[0].mask_sources;
                if let Some((valid, base_offset, parts_base)) =
                    self.prepare_masked_part_draws(pending)
                {
                    self.open_stencil_pass(encoder, s);
                    // Correctness backstop (unreachable for real rigs):
                    // the 8-bit stencil holds refs 1..=255 per cleared
                    // segment; reopening clears and restarts at 1.
                    if s.next_ref > 255 {
                        s.pass = None;
                        self.open_stencil_pass(encoder, s);
                    }
                    let ref_value = s.next_ref;
                    s.next_ref += 1;
                    if let Some(pass) = s.pass.as_mut() {
                        self.record_mask_sources_stencil(
                            pass,
                            mask_sources,
                            base_offset,
                            ref_value,
                            &mut s.bound_texture,
                        );
                        pass.set_stencil_reference(ref_value);
                        self.record_masked_part_draws(
                            pass,
                            pending,
                            &valid,
                            parts_base,
                            &mut s.bound_texture,
                        )?;
                    }
                    self.draw_filter_scratch = valid;
                }
            }
            PartSink::Alpha {
                view,
                stencil,
                has_rendered,
                clear_color,
                mask_write_label,
                masked_draw_label,
            } => {
                let clear = !**has_rendered && clear_color.is_some();
                let drew = self.render_masked_prepared_parts_to(
                    encoder,
                    view,
                    stencil,
                    pending,
                    clear,
                    *clear_color,
                    mask_write_label,
                    masked_draw_label,
                    None,
                )?;
                **has_rendered |= drew;
            }
        }
        pending.clear();
        Ok(())
    }

    // Invariant: Pipelines::new always compiles the Normal blend mode blit pipeline.
    #[allow(clippy::expect_used)]
    fn blit_pipeline_for(&self, blend_mode: BlendMode) -> &wgpu::RenderPipeline {
        self.shared
            .blit_pipelines
            .get(&blend_mode)
            .or_else(|| self.shared.blit_pipelines.get(&BlendMode::Normal))
            .expect("Normal blend mode blit pipeline must exist")
    }

    /// `begin_render_pass` that also tallies the pass. Takes `&self` so
    /// the `&self` blit helpers count too; the returned pass borrows
    /// only `encoder`, so the `&self` borrow ends at the call.
    fn counted_begin_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        desc: &wgpu::RenderPassDescriptor<'_>,
    ) -> wgpu::RenderPass<'e> {
        self.frame_render_passes.fetch_add(1, Ordering::Relaxed);
        encoder.begin_render_pass(desc)
    }

    /// No-draw pass that clears `view` to `color`. Used wherever the
    /// frame's clear can't be folded into a drawing pass (first drawable
    /// isn't an unmasked-Part batch, or nothing drew at all).
    fn explicit_clear_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        color: wgpu::Color,
        label: &str,
    ) {
        let _ = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );
    }

    /// Fold the per-frame atomic tallies and the byte/slot counts into
    /// `current_stats`. Called once at each `render_list` exit.
    fn finalize_frame_stats(&mut self, composites: &CompositePool) {
        self.current_stats.render_passes = self.frame_render_passes.load(Ordering::Relaxed);
        self.current_stats.total_draw_calls = self.frame_draw_calls.load(Ordering::Relaxed);
        self.current_stats.instance_bytes_uploaded = self.instance_staging_len as u64;
        self.current_stats.deform_bytes_uploaded = self.pending_deform_bytes;
        self.current_stats.composite_slots_used = composites.stats().0 as u32;
    }

    /// Sampled masked blit (has_stencil == false): blends `composite`
    /// onto `target_view` where the mask-alpha texture (group 2) holds
    /// 1; the stencil path instead folds mask write and masked blit
    /// into one pass — see `blit_composite_with_masks`.
    #[allow(clippy::expect_used, clippy::too_many_arguments)]
    fn blit_composite_masked(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        composite_texture: &CompositeTexture,
        target_view: &wgpu::TextureView,
        stencil: &StencilTarget,
        blend_mode: BlendMode,
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) {
        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Masked Blit Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        let pipeline = self
            .shared
            .masked_sampled_blit_pipelines
            .get(&blend_mode)
            .or_else(|| {
                self.shared
                    .masked_sampled_blit_pipelines
                    .get(&BlendMode::Normal)
            })
            .expect("Normal blend mode masked_sampled_blit_pipeline must exist");
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &composite_texture.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        render_pass.set_bind_group(
            2,
            stencil
                .mask_bind_group
                .as_ref()
                .expect("mask_bind_group must exist when has_stencil == false"),
            &[],
        );
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    pub(crate) fn blit_composite(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        composite_texture: &CompositeTexture,
        target_view: &wgpu::TextureView,
        blend_mode: BlendMode,
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) {
        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Blit Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );

        render_pass.set_pipeline(self.blit_pipeline_for(blend_mode));
        render_pass.set_bind_group(0, &composite_texture.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    // Invariant: blend_mode is always one of the three dst-in-shader modes
    // (Overlay/ColorBurn/LinearBurn) — render_list filters before
    // calling this helper. Pipeline lookup falls back to the Overlay
    // pipeline if a future variant is added without wiring its pipeline.
    #[allow(clippy::too_many_arguments, clippy::expect_used)]
    fn blit_composite_dst_in_shader(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        composite: &CompositeTexture,
        target_view: &wgpu::TextureView,
        target_color_texture: &wgpu::Texture,
        snapshot: &SnapshotTexture,
        blend_mode: BlendMode,
        part_uniform_offset: u32,
        width: u32,
        height: u32,
        scissor: Option<ScreenRect>,
    ) -> RendererResult<()> {
        // Snapshot the framebuffer before the dst-in-shader pass. The
        // copy executes in encoder order (unlike queue.write_buffer
        // which batches at submit-start), so it sees every prior pass's
        // contributions in this frame. With a scissor, only that rect is
        // copied and only that rect is shaded: the snapshot is sampled
        // 1:1 at screen pixels, so the untouched region is never read, and
        // outside the src's content the shader returns dst unchanged (all
        // dst-in-shader modes early-out on src alpha 0) — matching what a
        // full-screen blit would write there.
        let (copy_origin, copy_extent) = match scissor {
            Some(r) => (
                wgpu::Origin3d {
                    x: r.x,
                    y: r.y,
                    z: 0,
                },
                wgpu::Extent3d {
                    width: r.width,
                    height: r.height,
                    depth_or_array_layers: 1,
                },
            ),
            None => (
                wgpu::Origin3d::ZERO,
                wgpu::Extent3d {
                    width: width.max(1),
                    height: height.max(1),
                    depth_or_array_layers: 1,
                },
            ),
        };
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: target_color_texture,
                mip_level: 0,
                origin: copy_origin,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &snapshot.texture,
                mip_level: 0,
                origin: copy_origin,
                aspect: wgpu::TextureAspect::All,
            },
            copy_extent,
        );

        let pipeline = self
            .shared
            .blit_dst_in_shader_pipelines
            .get(&blend_mode)
            .ok_or(RendererError::MissingBlitPipeline(blend_mode))?;

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Blit (dst-in-shader) Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &composite.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        render_pass.set_bind_group(2, &snapshot.bind_group, &[]);
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
        Ok(())
    }

    /// Render a Part (optionally masked) into a freshly-cleared
    /// composite slot using Normal premultiplied blend. Used by the
    /// dst-in-shader path so each Part has a sample-able src texture
    /// independent of the framebuffer.
    #[allow(clippy::too_many_arguments)]
    fn render_part_to_composite_slot(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        slot: &CompositeTexture,
        mesh_id: u32,
        texture_id: u32,
        transform: glam::Mat4,
        mask_sources: &[crate::collect::MaskSourceData],
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) -> RendererResult<()> {
        let transparent = wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
        let draw = PreparedPartDraw {
            draw: PartDraw {
                mesh_id,
                albedo: texture_id,
                transform,
                blend_mode: BlendMode::Normal,
            },
            part_uniform_offset,
        };

        if self.shared.has_stencil {
            let mut state = StencilPassState::new(
                &slot.view,
                &stencil.view,
                Some(transparent),
                "Composite Slot Pass (dst-in-shader)",
                scissor,
            );
            let mut sink = PartSink::Stencil(&mut state);
            if mask_sources.is_empty() {
                self.flush_pending_parts(encoder, &mut sink, &mut vec![draw])?;
            } else {
                self.flush_pending_masked(
                    encoder,
                    &mut sink,
                    &mut vec![PreparedMaskedPartDraw { draw, mask_sources }],
                )?;
            }
            // If the draw filtered out, this emits the still-pending
            // transparent clear so the blit can't sample stale pool
            // contents.
            self.end_sink_pass(encoder, &mut sink);
            return Ok(());
        }

        let drew = if mask_sources.is_empty() {
            self.render_prepared_parts(
                encoder,
                &slot.view,
                &[draw],
                true,
                Some(transparent),
                scissor,
            )?
        } else {
            self.render_masked_prepared_parts_to(
                encoder,
                &slot.view,
                stencil,
                &[PreparedMaskedPartDraw { draw, mask_sources }],
                true,
                Some(transparent),
                "Composite Slot Mask Write (dst-in-shader)",
                "Composite Slot Masked Part (dst-in-shader)",
                scissor,
            )?
        };

        if !drew {
            self.explicit_clear_pass(
                encoder,
                &slot.view,
                transparent,
                "Composite Slot Fallback Clear (dst-in-shader)",
            );
        }
        Ok(())
    }

    /// Blit a composite with mask compositing. Stencil path: one pass —
    /// rasterize the mask shapes into the stencil (reference 1), then
    /// the stencil-tested blit of `composite` onto `target_view`. Alpha
    /// path: a mask-alpha write pass plus a sampled masked blit pass.
    #[allow(clippy::too_many_arguments, clippy::expect_used)]
    pub(crate) fn blit_composite_with_masks(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        composite: &CompositeTexture,
        target_view: &wgpu::TextureView,
        stencil: &StencilTarget,
        blend_mode: BlendMode,
        mask_sources: &[crate::collect::MaskSourceData],
        part_uniform_offset: u32,
        scissor: Option<ScreenRect>,
    ) {
        if mask_sources.is_empty() {
            self.blit_composite(
                encoder,
                composite,
                target_view,
                blend_mode,
                part_uniform_offset,
                scissor,
            );
            return;
        }

        let base_offset = self.reserve_instances(
            mask_sources
                .iter()
                .filter(|source| source.is_part())
                .count(),
        );
        let mut instance_data = std::mem::take(&mut self.instance_scratch);
        instance_data.clear();
        instance_data.extend(mask_sources.iter().filter_map(|source| match source {
            crate::collect::MaskSourceData::Part { transform, .. } => {
                Some(InstanceRaw::from_transform(*transform))
            }
            crate::collect::MaskSourceData::Composite { .. } => None,
        }));
        self.write_instances(base_offset, &instance_data);
        self.instance_scratch = instance_data;

        if !self.shared.has_stencil {
            self.write_mask_sources(
                encoder,
                "Composite Blit Mask Write Pass",
                stencil,
                mask_sources,
                base_offset,
            );
            self.blit_composite_masked(
                encoder,
                composite,
                target_view,
                stencil,
                blend_mode,
                part_uniform_offset,
                scissor,
            );
            return;
        }

        let mut render_pass = self.counted_begin_pass(
            encoder,
            &wgpu::RenderPassDescriptor {
                label: Some("Masked Blit Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    depth_slice: None,
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &stencil.view,
                    depth_ops: None,
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Discard,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            },
        );
        // Camera at group 0 for the mask-source draws (part pipeline
        // layout); the blit pipeline below rebinds group 0 to the
        // composite texture.
        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        // One scissor bounds both the mask-source stencil writes and the
        // blit to the composite's children union: outside it the composite
        // is a transparent clear (identity under this blend mode) and the
        // stencil there is never tested.
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_mask_sources_stencil(&mut render_pass, mask_sources, base_offset, 1, &mut None);

        let pipeline = self
            .shared
            .masked_blit_pipelines
            .get(&blend_mode)
            .or_else(|| self.shared.masked_blit_pipelines.get(&BlendMode::Normal))
            .expect("Normal blend mode masked blit pipeline must exist");
        render_pass.set_pipeline(pipeline);
        render_pass.set_stencil_reference(1);
        render_pass.set_bind_group(0, &composite.blit_bind_group, &[]);
        render_pass.set_bind_group(1, &self.part_uniform_bind_group, &[part_uniform_offset]);
        self.frame_draw_calls.fetch_add(1, Ordering::Relaxed);
        render_pass.draw(0..3, 0..1);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_list(
        &mut self,
        render_list: &crate::collect::RenderList,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<RenderStats> {
        self.render_list_ext(
            render_list,
            encoder,
            view,
            stencil,
            composites,
            None,
            None,
            width,
            height,
            clear_color,
        )
    }

    /// Like `render_list` but accepts the underlying main-color texture
    /// and a `FramebufferSnapshotPool` so the dst-in-shader blend modes
    /// `Overlay`, `ColorBurn` and `LinearBurn` can snapshot
    /// the current framebuffer per blit. Root drawables snapshot the
    /// main framebuffer; composite children snapshot their composite's
    /// offscreen target. When either is `None` the
    /// dst-in-shader modes fall back to the Normal-OVER approximation
    /// that `blend_mode_to_wgpu` returns for them — output is
    /// approximate but rendering doesn't error.
    ///
    /// `target_color_texture` must be the texture whose view was passed
    /// as `view` and must carry `wgpu::TextureUsages::COPY_SRC`. The
    /// snapshot texture in the pool carries `COPY_DST | TEXTURE_BINDING`.
    #[allow(clippy::too_many_arguments, clippy::expect_used)]
    pub fn render_list_ext(
        &mut self,
        render_list: &crate::collect::RenderList,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<RenderStats> {
        // Frame boundary for the resource-lifecycle counters: the camera
        // slot write below is this frame's first queue write, so the
        // reset has to precede it.
        self.frame_stats = FrameStats::default();
        self.frame_sizing_closed = false;
        self.reserve_camera()?;
        let result = self.render_list_ext_inner(
            render_list,
            encoder,
            view,
            stencil,
            composites,
            target_color_texture,
            snapshots,
            width,
            height,
            clear_color,
        );
        self.flush_instance_writes();
        self.flush_part_uniform_writes();
        // Frame over: uploads between frames may size buffers again.
        self.frame_sizing_closed = false;
        result
    }

    #[allow(clippy::too_many_arguments, clippy::expect_used)]
    fn render_list_ext_inner(
        &mut self,
        render_list: &crate::collect::RenderList,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<RenderStats> {
        use crate::collect::DrawableInfo;

        let span = tracing::trace_span!("render_list", drawn_parts = tracing::field::Empty);
        let _entered = span.enter();

        // Open a single GPU-side timer scope around all GPU work in this
        // render_list when TIMESTAMP_QUERY is supported. The query lives
        // until end_query is called on the same encoder; nested CPU
        // tracing spans below give finer-grained per-callsite breakdown.
        let gpu_query = self
            .gpu_profiler
            .as_ref()
            .map(|p| p.begin_query("render_list", encoder));

        self.current_stats = RenderStats::default();
        self.frame_render_passes.store(0, Ordering::Relaxed);
        self.frame_draw_calls.store(0, Ordering::Relaxed);

        // Per-frame snapshot bookkeeping. `dst_in_shader_active` is
        // true only when both the caller-supplied texture handle and a
        // snapshot pool are present AND there's at least one Part /
        // Composite (root or composite child) using a shader-math mode —
        // keeps the common case (zero such parts) on exactly the same
        // code path as a plain render.
        let dst_in_shader_present = render_list
            .root_drawables
            .iter()
            .chain(render_list.composite_children.values().flatten())
            .any(|d| is_dst_in_shader(d.blend_mode()));
        let dst_in_shader_active =
            dst_in_shader_present && target_color_texture.is_some() && snapshots.is_some();
        if dst_in_shader_present && !dst_in_shader_active {
            // Once per process: legacy `render_list` callers hit this on
            // every frame (the reference model carries ColorBurn parts), and a per-frame
            // warn would drown the log.
            static FALLBACK_WARNED: std::sync::Once = std::sync::Once::new();
            FALLBACK_WARNED.call_once(|| {
                tracing::warn!(
                    "render_list: dst-in-shader blend mode present but caller did \
                     not supply target_color_texture + snapshot pool; falling back \
                     to Normal-OVER approximation",
                );
            });
        }

        // Each dst-in-shader Part — root or composite child — is
        // dispatched through a composite slot, so it gains an extra
        // uniform slot for its blit. Each dst-in-shader Composite uses
        // its existing slot — only the blit pipeline differs. Counting
        // the Parts here keeps the up-front uniform_slot_count exact.
        let dst_in_shader_part_count = if dst_in_shader_active {
            render_list
                .root_drawables
                .iter()
                .chain(render_list.composite_children.values().flatten())
                .filter(|d| {
                    matches!(d, DrawableInfo::Part { .. }) && is_dst_in_shader(d.blend_mode())
                })
                .count()
        } else {
            0
        };
        let dst_in_shader_masked_composite_count = if dst_in_shader_active {
            render_list
                .root_drawables
                .iter()
                .chain(render_list.composite_children.values().flatten())
                .filter(|drawable| {
                    matches!(
                        drawable,
                        DrawableInfo::Composite {
                            blend_mode,
                            mask_sources,
                            ..
                        } if is_dst_in_shader(*blend_mode) && !mask_sources.is_empty()
                    )
                })
                .count()
        } else {
            0
        };

        // Pre-size frame resources so passes recorded below all refer
        // to the same instance_buffer / part_uniform_buffer and no
        // mid-frame growth strands earlier passes on a freed buffer.
        self.begin_frame_instances(render_list.total_instance_count());
        // One uniform slot per root drawable (Part or Composite) plus
        // one per composite child Part, plus one per mask-source draw
        // (each mask write binds its source's threshold in its own
        // slot — total_mask_source_count is the per-frame upper bound).
        // Dst-in-shader Parts get one extra slot each (the
        // composite-blit slot they're routed through).
        let n_child_parts: usize = render_list
            .composite_children
            .values()
            .map(|v| v.len())
            .sum();
        let uniform_slot_count = (render_list.root_drawables.len()
            + n_child_parts
            + dst_in_shader_part_count
            + dst_in_shader_masked_composite_count
            + render_list.total_mask_source_count()
            + render_list
                .composite_mask_sources
                .values()
                .map(|source| source.parts.len())
                .sum::<usize>()) as u32;
        self.begin_frame_uniforms(uniform_slot_count);

        composites.ensure_size(width, height);
        composites.reset();
        let mut snapshots_opt = if dst_in_shader_active {
            snapshots
        } else {
            None
        };
        if let Some(s) = snapshots_opt.as_mut() {
            s.ensure_size(width, height);
            s.reset();
        }
        self.prepare_composite_mask_textures(encoder, render_list, composites);

        // Root-composite flattening: a single root Composite with an
        // identity blit (Normal, opacity 1, white tint, zero screen tint,
        // unmasked) whose children all composite as plain OVER can be
        // drawn straight to the main view, skipping the offscreen slot,
        // the full-screen blit, and the slot's viewport-sized bandwidth.
        // OVER associativity makes "(children OVER clear) blitted OVER bg"
        // identical to "children OVER bg" drawn directly. Guard out nested
        // composites and any non-OVER child (Multiply/Screen/… read the
        // destination, so isolating them in a transparent slot is not
        // equivalent to drawing them onto the populated main view).
        // Dst-in-shader children read the destination too once the
        // snapshot path is active, so they block flattening as well;
        // without snapshots they degrade to OVER and may flatten.
        let flatten_children: Option<&[DrawableInfo]> = (|| {
            if render_list.root_drawables.len() != 1 {
                return None;
            }
            let DrawableInfo::Composite {
                node_id,
                blend_mode,
                opacity,
                tint,
                screen_tint,
                mask_sources,
                ..
            } = &render_list.root_drawables[0]
            else {
                return None;
            };
            if *blend_mode != BlendMode::Normal
                || *opacity != 1.0
                || *tint != glam::Vec3::ONE
                || *screen_tint != glam::Vec3::ZERO
                || !mask_sources.is_empty()
            {
                return None;
            }
            let children = render_list.composite_children.get(node_id)?;
            if children.is_empty() {
                return None;
            }
            for c in children {
                match c {
                    DrawableInfo::Composite { .. } => return None,
                    DrawableInfo::Part { blend_mode, .. } => {
                        if !renders_as_over(*blend_mode)
                            || (dst_in_shader_active && is_dst_in_shader(*blend_mode))
                        {
                            return None;
                        }
                    }
                }
            }
            Some(children.as_slice())
        })();

        let has_stencil = self.shared.has_stencil;

        // Alpha path: if the first drawable doesn't take the
        // unmasked-Part fast path (which is the only place that consumes
        // `clear_color` via a `LoadOp::Clear`), emit an explicit clear
        // pass up front. The dst-in-shader path snapshots the
        // framebuffer per blit and would otherwise sample uninitialized
        // contents on frame 0; composites and masked Parts both use
        // `LoadOp::Load` for the main view too. Skip when `clear_color`
        // is None — that signals "preserve whatever the caller already
        // drew" (bevy). Skip too when flattening: the flattened children
        // fold the clear into their first batch (one fewer pass). The
        // stencil sink instead defers the clear in `next_load` until its
        // first pass opens (or `end_sink_pass` flushes it ahead of an
        // external consumer).
        let needs_explicit_clear = !has_stencil
            && flatten_children.is_none()
            && clear_color.is_some()
            && render_list.root_drawables.first().is_some_and(|d| match d {
                DrawableInfo::Composite { .. } => true,
                DrawableInfo::Part {
                    blend_mode,
                    mask_sources,
                    ..
                } => {
                    !mask_sources.is_empty()
                        || (dst_in_shader_active && is_dst_in_shader(*blend_mode))
                }
            });
        if needs_explicit_clear {
            #[allow(clippy::expect_used)]
            let color = clear_color.expect("needs_explicit_clear implies clear_color is Some");
            self.explicit_clear_pass(encoder, view, color, "render_list initial clear");
        }

        let mut has_rendered_first = needs_explicit_clear;
        let mut main_pass_state =
            StencilPassState::new(view, &stencil.view, clear_color, "Render Pass", None);
        let mut sink = if has_stencil {
            PartSink::Stencil(&mut main_pass_state)
        } else {
            PartSink::Alpha {
                view,
                stencil,
                has_rendered: &mut has_rendered_first,
                clear_color,
                mask_write_label: "Mask Write Pass",
                masked_draw_label: "Masked Part Render Pass",
            }
        };

        // Flattened root composite: draw its children straight to the main
        // view (no slot, no blit). The first drawing pass folds in the
        // clear; if every child filtered out, the fallback below still
        // clears so the frame isn't uninitialized.
        if let Some(children) = flatten_children {
            self.render_children_run(encoder, &mut sink, children)?;
            self.end_sink_pass(encoder, &mut sink);
            if !has_stencil && !has_rendered_first {
                if let Some(color) = clear_color {
                    self.explicit_clear_pass(
                        encoder,
                        view,
                        color,
                        "render_list flatten fallback clear",
                    );
                }
            }
            if let (Some(profiler), Some(query)) = (self.gpu_profiler.as_ref(), gpu_query) {
                profiler.end_query(encoder, query);
            }
            self.finalize_frame_stats(composites);
            span.record("drawn_parts", self.current_stats.drawn_parts);
            return Ok(self.current_stats);
        }

        // Consecutive unmasked root parts accumulate here and flush as
        // one batched pass at the next barrier (masked / dst-in-shader
        // part, composite) or at loop end. Dst-in-shader parts, offscreen
        // composites and framebuffer snapshots can't be folded into a
        // peer's pass, so they force a flush first to preserve z-order.
        let mut pending_parts: Vec<PreparedPartDraw> = Vec::new();
        // Consecutive masked root parts that share one mask signature
        // accumulate here and flush as one mask-write + one masked-draw
        // pass. Mixing with `pending_parts` is mutually exclusive: an
        // unmasked part flushes any pending masked run first and vice
        // versa, so z-order across the two kinds is preserved.
        let mut pending_masked: Vec<PreparedMaskedPartDraw> = Vec::new();

        // Single z-ordered pass. Composites render their children into a
        // pool slot on demand, then blit that slot before moving on —
        // keeps live offscreen-texture count bounded by max concurrent
        // composites per frame (1 for most models; never grows with puppet
        // count).
        for drawable in &render_list.root_drawables {
            match drawable {
                DrawableInfo::Part {
                    mesh_id,
                    texture_id,
                    transform,
                    blend_mode,
                    opacity,
                    tint,
                    screen_tint,
                    ref mask_sources,
                    mask_threshold,
                    ..
                } => {
                    let _part_span =
                        tracing::trace_span!("render_part", masked = !mask_sources.is_empty(),)
                            .entered();
                    let part_uniform_offset =
                        self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);

                    let route_dst_in_shader = dst_in_shader_active && is_dst_in_shader(*blend_mode);

                    if route_dst_in_shader {
                        // Barrier: snapshots the framebuffer, so any
                        // pending parts (masked or not) must land first,
                        // and the sink's open pass must end before the
                        // snapshot copy + slot passes record.
                        self.flush_pending_parts(encoder, &mut sink, &mut pending_parts)?;
                        self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
                        self.end_sink_pass(encoder, &mut sink);
                        // Render the part into a composite slot using
                        // Normal blend (premultiplied) so the snapshot
                        // blit shader receives a normal premultiplied
                        // src. Then snapshot the framebuffer and run
                        // the dst-in-shader blit pipeline. Composite
                        // uniform slot is separate from the part's
                        // child uniform slot above.
                        // One rect bounds both the slot render and the
                        // snapshot copy+blit: the part's contribution lives
                        // inside it, and the dst-in-shader shader returns
                        // dst untouched for the transparent region outside.
                        let part_scissor =
                            self.part_scissor_rect(*mesh_id, *transform, width, height);
                        // Both slots are dead once the blit below is recorded,
                        // so release them for the next root drawable rather
                        // than growing the pools once per dst-in-shader part.
                        let composite_mark = composites.mark();
                        let snapshot_mark = snapshots_opt.as_ref().map(|s| s.mark());
                        let slot = composites.acquire(&self.shared, &self.device);
                        self.render_part_to_composite_slot(
                            encoder,
                            stencil,
                            &slot,
                            *mesh_id,
                            *texture_id,
                            *transform,
                            mask_sources,
                            part_uniform_offset,
                            part_scissor,
                        )?;
                        let blit_uniform_offset =
                            self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5);
                        // Safe to unwrap: dst_in_shader_active implies
                        // both fields are Some; checked above.
                        let snap_pool = snapshots_opt
                            .as_mut()
                            .expect("snapshot pool present when dst_in_shader_active");
                        let snapshot = snap_pool.acquire(&self.shared, &self.device);
                        let main_tex = target_color_texture
                            .expect("target_color_texture present when dst_in_shader_active");
                        self.blit_composite_dst_in_shader(
                            encoder,
                            &slot,
                            view,
                            main_tex,
                            snapshot,
                            *blend_mode,
                            blit_uniform_offset,
                            width,
                            height,
                            part_scissor,
                        )?;
                        composites.rewind(composite_mark);
                        if let (Some(pool), Some(mark)) = (snapshots_opt.as_mut(), snapshot_mark) {
                            pool.rewind(mark);
                        }
                        sink.mark_rendered();
                    } else if !mask_sources.is_empty() {
                        // Masked: flush any pending unmasked run (z-order
                        // barrier), then accumulate into the masked batch.
                        // A signature change from the current masked run
                        // forces that run to flush first.
                        self.flush_pending_parts(encoder, &mut sink, &mut pending_parts)?;
                        if let Some(first) = pending_masked.first() {
                            if !same_mask_signature(first.mask_sources, mask_sources) {
                                self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
                            }
                        }
                        pending_masked.push(PreparedMaskedPartDraw {
                            draw: PreparedPartDraw {
                                draw: PartDraw {
                                    mesh_id: *mesh_id,
                                    albedo: *texture_id,
                                    transform: *transform,
                                    blend_mode: *blend_mode,
                                },
                                part_uniform_offset,
                            },
                            mask_sources,
                        });
                    } else {
                        // Unmasked: flush any pending masked run (z-order
                        // barrier), then accumulate into the pending batch.
                        // The flush (here on the next barrier, or at loop
                        // end) decides the clear — only the first pass of
                        // the frame clears, and only when the caller
                        // supplied a clear color. A None clear color means
                        // "preserve existing content" (e.g. bevy already
                        // cleared the ViewTarget to its configured color);
                        // clearing to transparent black would drop bevy's
                        // contribution.
                        self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
                        pending_parts.push(PreparedPartDraw {
                            draw: PartDraw {
                                mesh_id: *mesh_id,
                                albedo: *texture_id,
                                transform: *transform,
                                blend_mode: *blend_mode,
                            },
                            part_uniform_offset,
                        });
                    }
                }
                DrawableInfo::Composite {
                    node_id,
                    ref mask_sources,
                    ..
                } => {
                    let _comp_span = tracing::trace_span!(
                        "render_composite",
                        masked = !mask_sources.is_empty(),
                    )
                    .entered();
                    // An empty composite draws nothing, and must decide that
                    // before the flush below so it never breaks a pending run.
                    if !render_list
                        .composite_children
                        .get(node_id)
                        .is_some_and(|c| !c.is_empty())
                    {
                        continue;
                    }

                    // Barrier: a composite renders offscreen then blits
                    // in z-order, so pending root parts (masked or not)
                    // flush first and the sink's open pass ends before
                    // the slot + blit passes record.
                    self.flush_pending_parts(encoder, &mut sink, &mut pending_parts)?;
                    self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
                    self.end_sink_pass(encoder, &mut sink);

                    if self.render_composite_drawable(
                        encoder,
                        stencil,
                        CompositeTarget {
                            view,
                            texture: target_color_texture,
                        },
                        drawable,
                        render_list,
                        composites,
                        snapshots_opt.as_deref_mut(),
                        dst_in_shader_active,
                        width,
                        height,
                    )? {
                        sink.mark_rendered();
                    }
                }
            }
        }

        // Flush the trailing run of pending parts (the common case: a
        // model whose last drawables are plain parts). At most one of the
        // two is non-empty.
        self.flush_pending_parts(encoder, &mut sink, &mut pending_parts)?;
        self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
        self.end_sink_pass(encoder, &mut sink);

        // Alpha sink: nothing consumed the clear (empty render list, or
        // every batch filtered out on missing GPU resources) — the
        // target still holds the previous frame, so clear it now. (The
        // stencil sink's end_sink_pass above already flushed a pending
        // clear.)
        if !has_stencil && !has_rendered_first {
            if let Some(color) = clear_color {
                self.explicit_clear_pass(encoder, view, color, "render_list fallback clear");
            }
        }

        if let (Some(profiler), Some(query)) = (self.gpu_profiler.as_ref(), gpu_query) {
            profiler.end_query(encoder, query);
        }

        self.finalize_frame_stats(composites);
        span.record("drawn_parts", self.current_stats.drawn_parts);
        Ok(self.current_stats)
    }

    /// Resolve pending wgpu-profiler timer queries into `encoder`. Records
    /// a query-set resolve + copy into the profiler's read buffer, so it
    /// must run while the encoder is still open and **before** the encoder
    /// is submitted. Pair with `end_gpu_frame` once that submit lands.
    /// No-op when timestamp queries aren't supported.
    pub fn resolve_gpu_queries(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(profiler) = self.gpu_profiler.as_mut() {
            profiler.resolve_queries(encoder);
        }
    }

    /// Close the current wgpu-profiler frame and map its read buffer for
    /// readback. Must run **after** the encoder carrying
    /// `resolve_gpu_queries` has been submitted: `end_frame` maps the read
    /// buffer, and a submit that still writes a mapped buffer fails wgpu
    /// validation ("Buffer is still mapped"). No-op when timestamp queries
    /// aren't supported.
    pub fn end_gpu_frame(&mut self) {
        if let Some(profiler) = self.gpu_profiler.as_mut() {
            if let Err(e) = profiler.end_frame() {
                tracing::warn!("wgpu_profiler::end_frame: {e}");
            }
        }
    }

    /// Drain finished GPU timer results since the last call. Returns
    /// `None` when timestamp queries are unsupported or no completed
    /// frame is available yet.
    pub fn process_gpu_frame(&mut self) -> Option<Vec<wgpu_profiler::GpuTimerQueryResult>> {
        let profiler = self.gpu_profiler.as_mut()?;
        let period = self.queue.get_timestamp_period();
        profiler.process_finished_frame(period)
    }

    // Batch a composite's children (or a flattened root composite's
    // children, drawn straight to the main view) into as few batches as
    // possible against `sink`: consecutive unmasked children share one
    // batch; consecutive masked children with one mask signature share
    // one mask write + one masked-draw batch. At most one pending run is
    // non-empty at a time, so z-order across masked / unmasked children
    // is preserved. The caller owns the sink's end-of-run handling
    // (`end_sink_pass` / fallback clear).
    fn render_children_run(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        children: &[crate::collect::DrawableInfo],
    ) -> RendererResult<()> {
        use crate::collect::DrawableInfo;

        let mut pending_parts: Vec<PreparedPartDraw> = Vec::new();
        let mut pending_masked: Vec<PreparedMaskedPartDraw> = Vec::new();
        for child in children {
            let DrawableInfo::Part {
                mesh_id,
                texture_id,
                transform,
                blend_mode,
                opacity,
                tint,
                screen_tint,
                mask_sources,
                mask_threshold,
                ..
            } = child
            else {
                continue;
            };
            let part_uniform_offset =
                self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);

            if mask_sources.is_empty() {
                self.flush_pending_masked(encoder, sink, &mut pending_masked)?;
                pending_parts.push(PreparedPartDraw {
                    draw: PartDraw {
                        mesh_id: *mesh_id,
                        albedo: *texture_id,
                        transform: *transform,
                        blend_mode: *blend_mode,
                    },
                    part_uniform_offset,
                });
            } else {
                self.flush_pending_parts(encoder, sink, &mut pending_parts)?;
                if let Some(first) = pending_masked.first() {
                    if !same_mask_signature(first.mask_sources, mask_sources) {
                        self.flush_pending_masked(encoder, sink, &mut pending_masked)?;
                    }
                }
                pending_masked.push(PreparedMaskedPartDraw {
                    draw: PreparedPartDraw {
                        draw: PartDraw {
                            mesh_id: *mesh_id,
                            albedo: *texture_id,
                            transform: *transform,
                            blend_mode: *blend_mode,
                        },
                        part_uniform_offset,
                    },
                    mask_sources,
                });
            }
        }

        self.flush_pending_parts(encoder, sink, &mut pending_parts)?;
        self.flush_pending_masked(encoder, sink, &mut pending_masked)?;
        Ok(())
    }

    /// Alpha sink only: a barrier is about to read the target (a snapshot
    /// copy, or a nested composite blitting over it), so a clear that is still
    /// pending has to land first or that read sees the previous frame. The
    /// stencil sink has no equivalent — `end_sink_pass` already flushed its
    /// deferred clear.
    fn spend_pending_alpha_clear(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        label: &str,
    ) {
        if let PartSink::Alpha {
            view,
            has_rendered,
            clear_color,
            ..
        } = sink
        {
            if !**has_rendered {
                if let Some(color) = *clear_color {
                    self.explicit_clear_pass(encoder, view, color, label);
                    **has_rendered = true;
                }
            }
        }
    }

    /// Render one Composite drawable into `target`: children go into a fresh
    /// pool slot, then that slot blits out under the composite's own
    /// blend/opacity/tint/mask. Serves root composites (target = the main
    /// view) and nested ones (target = the enclosing composite's slot)
    /// identically, so the outer blit covers nested content and orders it with
    /// the outer's other children.
    ///
    /// Returns whether anything was drawn. The caller must have flushed its
    /// pending runs and ended the sink's pass: this records its own passes.
    #[allow(clippy::too_many_arguments)]
    fn render_composite_drawable(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        target: CompositeTarget<'_>,
        drawable: &crate::collect::DrawableInfo,
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        mut snapshots: Option<&mut FramebufferSnapshotPool>,
        dst_in_shader_active: bool,
        width: u32,
        height: u32,
    ) -> RendererResult<bool> {
        use crate::collect::DrawableInfo;

        let DrawableInfo::Composite {
            node_id,
            blend_mode,
            opacity,
            tint,
            screen_tint,
            ref mask_sources,
            mask_threshold,
            ..
        } = drawable
        else {
            return Ok(false);
        };
        let children = match render_list.composite_children.get(node_id) {
            Some(c) if !c.is_empty() => c,
            _ => return Ok(false),
        };

        // The composite's blit can only touch the union of its children's
        // screen rects; everything outside is the transparent slot clear.
        // Computed before the children render so the immutable borrow
        // doesn't collide.
        let union = self.children_union_scissor(children, width, height);

        let part_uniform_offset =
            self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);

        // Acquire a pool slot, render children into it, blit out. Marks are
        // taken before the acquire and rewound after the blit, so sibling
        // composites share slots instead of each adding one. A nested
        // composite acquires while its parent's slot is still live, so the
        // pool grows to the nesting depth and no further.
        let composite_mark = composites.mark();
        let snapshot_mark = snapshots.as_ref().map(|s| s.mark());
        let slot = composites.acquire(&self.shared, &self.device);
        self.render_composite_children(
            encoder,
            stencil,
            &slot,
            children,
            render_list,
            composites,
            snapshots.as_deref_mut(),
            dst_in_shader_active,
            width,
            height,
        )?;

        // Needs both a destination texture to snapshot and a pool to snapshot
        // into; without either, fall through to the fixed-function blit (the
        // documented Normal-OVER approximation) rather than panicking.
        let mut blitted = false;
        if dst_in_shader_active && is_dst_in_shader(*blend_mode) {
            if let (Some(main_tex), Some(snap_pool)) = (target.texture, snapshots.as_deref_mut()) {
                let mut source = slot.clone();
                let mut source_uniform_offset = part_uniform_offset;
                if !mask_sources.is_empty() {
                    let masked = composites.acquire(&self.shared, &self.device);
                    self.explicit_clear_pass(
                        encoder,
                        &masked.view,
                        wgpu::Color::TRANSPARENT,
                        "Masked Composite Source Clear",
                    );
                    self.blit_composite_with_masks(
                        encoder,
                        &slot,
                        &masked.view,
                        stencil,
                        BlendMode::Normal,
                        mask_sources,
                        part_uniform_offset,
                        union,
                    );
                    source = masked;
                    source_uniform_offset =
                        self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5);
                }
                let snapshot = snap_pool.acquire(&self.shared, &self.device);
                self.blit_composite_dst_in_shader(
                    encoder,
                    &source,
                    target.view,
                    main_tex,
                    snapshot,
                    *blend_mode,
                    source_uniform_offset,
                    width,
                    height,
                    union,
                )?;
                blitted = true;
            }
        }

        if !blitted {
            // Fixed-function composite blits only skip the outside-union
            // region when a transparent src is a no-op under the mode (all
            // but Darken).
            let blit_scissor = if blend_transparent_src_is_identity(*blend_mode) {
                union
            } else {
                None
            };
            if mask_sources.is_empty() {
                self.blit_composite(
                    encoder,
                    &slot,
                    target.view,
                    *blend_mode,
                    part_uniform_offset,
                    blit_scissor,
                );
            } else {
                self.blit_composite_with_masks(
                    encoder,
                    &slot,
                    target.view,
                    stencil,
                    *blend_mode,
                    mask_sources,
                    part_uniform_offset,
                    blit_scissor,
                );
            }
        }

        composites.rewind(composite_mark);
        if let (Some(pool), Some(mark)) = (snapshots, snapshot_mark) {
            pool.rewind(mark);
        }
        self.current_stats.drawn_composites += 1;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_composite_children(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        target: &CompositeTexture,
        children: &[crate::collect::DrawableInfo],
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        dst_in_shader_active: bool,
        width: u32,
        height: u32,
    ) -> RendererResult<()> {
        let composite_view = &target.view;
        let transparent = wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };

        // The slot needs clearing before the first child composites onto
        // it. Rather than a standalone no-draw clear pass, the first
        // pass that draws folds in a transparent clear. If every child
        // filters out, the fallback clear keeps the blit from sampling
        // stale pool contents.
        if self.shared.has_stencil {
            let mut state = StencilPassState::new(
                composite_view,
                &stencil.view,
                Some(transparent),
                "Composite Children Pass",
                None,
            );
            let mut sink = PartSink::Stencil(&mut state);
            self.render_children_with_dst_blends(
                encoder,
                stencil,
                &mut sink,
                target,
                children,
                render_list,
                composites,
                snapshots,
                dst_in_shader_active,
                width,
                height,
            )?;
            self.end_sink_pass(encoder, &mut sink);
            return Ok(());
        }

        let mut has_rendered_child = false;
        let mut sink = PartSink::Alpha {
            view: composite_view,
            stencil,
            has_rendered: &mut has_rendered_child,
            clear_color: Some(transparent),
            mask_write_label: "Composite Mask Write Pass",
            masked_draw_label: "Composite Masked Part Render Pass",
        };
        self.render_children_with_dst_blends(
            encoder,
            stencil,
            &mut sink,
            target,
            children,
            render_list,
            composites,
            snapshots,
            dst_in_shader_active,
            width,
            height,
        )?;

        if !has_rendered_child {
            self.explicit_clear_pass(
                encoder,
                composite_view,
                transparent,
                "Composite Fallback Clear Pass",
            );
        }

        Ok(())
    }

    /// Children of a composite, split at every child that has to read the
    /// composite's own buffer rather than just blend onto it: a dst-in-shader
    /// Part or a nested Composite (which renders its own slot
    /// and blits it in). Each such child lands between two plain runs: the
    /// preceding run flushes, the sink's pass ends, the child records its own
    /// passes, then the run resumes. With no such child this is one plain run.
    #[allow(clippy::too_many_arguments)]
    fn render_children_with_dst_blends(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stencil: &StencilTarget,
        sink: &mut PartSink<'_, '_>,
        target: &CompositeTexture,
        children: &[crate::collect::DrawableInfo],
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        mut snapshots: Option<&mut FramebufferSnapshotPool>,
        dst_in_shader_active: bool,
        width: u32,
        height: u32,
    ) -> RendererResult<()> {
        use crate::collect::DrawableInfo;

        let snapshots_live = snapshots.is_some();
        let is_barrier = |c: &DrawableInfo| match c {
            DrawableInfo::Composite { .. } => true,
            DrawableInfo::Part { blend_mode, .. } => {
                snapshots_live && is_dst_in_shader(*blend_mode)
            }
        };
        if !children.iter().any(is_barrier) {
            return self.render_children_run(encoder, sink, children);
        }

        let mut run_start = 0;
        for (i, child) in children.iter().enumerate() {
            if !is_barrier(child) {
                continue;
            }

            // Barrier: the preceding run lands first (z-order), and the
            // sink's open pass must end before this child's passes record.
            self.render_children_run(encoder, sink, &children[run_start..i])?;
            run_start = i + 1;
            self.end_sink_pass(encoder, sink);
            self.spend_pending_alpha_clear(encoder, sink, "Composite Children Clear (barrier)");

            match child {
                DrawableInfo::Composite { .. } => {
                    if self.render_composite_drawable(
                        encoder,
                        stencil,
                        CompositeTarget {
                            view: &target.view,
                            texture: Some(&target.texture),
                        },
                        child,
                        render_list,
                        composites,
                        snapshots.as_deref_mut(),
                        dst_in_shader_active,
                        width,
                        height,
                    )? {
                        sink.mark_rendered();
                    }
                }
                DrawableInfo::Part {
                    mesh_id,
                    texture_id,
                    transform,
                    blend_mode,
                    opacity,
                    tint,
                    screen_tint,
                    ref mask_sources,
                    mask_threshold,
                    ..
                } => {
                    let part_uniform_offset =
                        self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);
                    // Same rect for the scratch-slot render and the snapshot
                    // copy+blit against the composite target (see the
                    // root-part path). The scratch slot and snapshot are
                    // re-acquired per child at the same cursor — each child's
                    // slot render, snapshot copy and blit record in encoder
                    // order, so the next child safely overwrites both.
                    let child_scissor = self.part_scissor_rect(*mesh_id, *transform, width, height);
                    let composite_mark = composites.mark();
                    let scratch = composites.acquire(&self.shared, &self.device);
                    self.render_part_to_composite_slot(
                        encoder,
                        stencil,
                        &scratch,
                        *mesh_id,
                        *texture_id,
                        *transform,
                        mask_sources,
                        part_uniform_offset,
                        child_scissor,
                    )?;
                    let blit_uniform_offset =
                        self.write_part_uniform(1.0, glam::Vec3::ONE, glam::Vec3::ZERO, 0.5);
                    #[allow(clippy::expect_used)]
                    let snap_pool = snapshots
                        .as_deref_mut()
                        .expect("snapshot pool present for a dst-in-shader barrier");
                    let snapshot_mark = snap_pool.mark();
                    let snapshot = snap_pool.acquire(&self.shared, &self.device);
                    self.blit_composite_dst_in_shader(
                        encoder,
                        &scratch,
                        &target.view,
                        &target.texture,
                        snapshot,
                        *blend_mode,
                        blit_uniform_offset,
                        width,
                        height,
                        child_scissor,
                    )?;
                    composites.rewind(composite_mark);
                    if let Some(pool) = snapshots.as_deref_mut() {
                        pool.rewind(snapshot_mark);
                    }
                    sink.mark_rendered();
                }
            }
        }

        self.render_children_run(encoder, sink, &children[run_start..])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        blend_mode_to_wgpu, blend_transparent_src_is_identity, camera_slot_offset,
        pixels_to_scissor, project_aabb_to_pixels, renders_as_over, same_mask_signature, Aabb2,
        RendererError, ScreenRect, TextureSource, CAMERA_RING_SLOTS,
    };
    use catchlight_core::{BlendMode, DecodedTexture};
    use std::sync::Arc;

    #[test]
    fn texture_source_identity_includes_dimensions() {
        let rgba: Arc<[u8]> = vec![0; 8].into();
        let source = TextureSource {
            rgba: rgba.clone(),
            width: 1,
            height: 2,
        };

        assert!(source.matches(&DecodedTexture {
            width: 1,
            height: 2,
            rgba: rgba.clone(),
        }));
        assert!(!source.matches(&DecodedTexture {
            width: 2,
            height: 1,
            rgba,
        }));
    }

    #[test]
    fn camera_slot_reservation_rejects_wraparound() {
        let stride = 256;
        assert!(matches!(camera_slot_offset(0, stride), Ok(0)));
        assert!(matches!(
            camera_slot_offset(CAMERA_RING_SLOTS - 1, stride),
            Ok(offset) if offset == u64::from(CAMERA_RING_SLOTS - 1) * stride
        ));
        assert!(matches!(
            camera_slot_offset(CAMERA_RING_SLOTS, stride),
            Err(RendererError::TooManyCameraViews {
                limit: CAMERA_RING_SLOTS
            })
        ));
    }

    /// Root-composite flattening draws children straight to the parent
    /// target, which is only equivalent to slot-render + OVER-blit when
    /// every child composites with associative premultiplied OVER. This
    /// pins exactly which modes qualify: Normal plus the dst-in-shader
    /// trio (whose no-snapshot fallback degrades to OVER; the flatten
    /// guard rejects them separately when the snapshot path is active).
    /// A blend-table edit that made e.g. Multiply look like OVER, or
    /// Normal stop being OVER, would silently make flattening produce
    /// wrong pixels — and trip here first.
    #[test]
    fn renders_as_over_matches_the_flatten_safe_blend_set() {
        for over in [
            BlendMode::Normal,
            BlendMode::Overlay,
            BlendMode::ColorBurn,
            BlendMode::LinearBurn,
        ] {
            assert!(renders_as_over(over), "{over:?} should render as OVER");
        }
        for not_over in [
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::ColorDodge,
            BlendMode::LinearDodge,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Add,
            BlendMode::Inverse,
            BlendMode::Subtract,
            BlendMode::ClipToLower,
            BlendMode::SliceFromLower,
        ] {
            assert!(
                !renders_as_over(not_over),
                "{not_over:?} reads the destination and must not flatten"
            );
        }
    }

    /// A masked-part batch shares one mask-write pass, so two draws may
    /// only join when their mask shapes rasterize identically. Verify
    /// the signature splits on source count and per-source
    /// mesh/texture/mode/threshold — and ignores the target part itself.
    #[test]
    fn same_mask_signature_distinguishes_mask_shapes() {
        use crate::collect::MaskSourceData;
        use catchlight_core::MaskMode;

        let src_t = |mesh: u32, tex: u32, mode, threshold: f32| MaskSourceData::Part {
            mesh_id: mesh,
            texture_id: tex,
            transform: glam::Mat4::IDENTITY,
            mode,
            mask_threshold: threshold,
        };
        let src = |mesh: u32, tex: u32, mode| src_t(mesh, tex, mode, 0.5);
        let a = [src(1, 1, MaskMode::Mask)];

        // Identical signature.
        assert!(same_mask_signature(&a, &[src(1, 1, MaskMode::Mask)]));
        // Differing source threshold.
        assert!(!same_mask_signature(
            &a,
            &[src_t(1, 1, MaskMode::Mask, 0.25)]
        ));
        // Differing source count.
        assert!(!same_mask_signature(
            &a,
            &[src(1, 1, MaskMode::Mask), src(2, 2, MaskMode::Mask)]
        ));
        // Differing mesh / texture / mode.
        assert!(!same_mask_signature(&a, &[src(2, 1, MaskMode::Mask)]));
        assert!(!same_mask_signature(&a, &[src(1, 2, MaskMode::Mask)]));
        assert!(!same_mask_signature(&a, &[src(1, 1, MaskMode::DodgeMask)]));

        let composite = [MaskSourceData::Composite {
            node_id: 7,
            mode: MaskMode::Mask,
        }];
        assert!(same_mask_signature(
            &composite,
            &[MaskSourceData::Composite {
                node_id: 7,
                mode: MaskMode::Mask,
            }]
        ));
        assert!(!same_mask_signature(
            &composite,
            &[MaskSourceData::Composite {
                node_id: 8,
                mode: MaskMode::Mask,
            }]
        ));
        assert!(!same_mask_signature(&composite, &a));
    }

    /// AGENTS.md invariant: blend modes whose color component masks via
    /// `DstAlpha` or `Zero` must use the same factors on the alpha
    /// component. If alpha falls through to the default OVER blend, the
    /// framebuffer gets α=1 where color is 0, producing opaque-black
    /// halos (silent at identity pose, visible once a deform shrinks
    /// the mask — cf. an eye mask at blink=1).
    #[test]
    fn masking_blend_modes_have_matching_color_and_alpha_factors() {
        for mode in [BlendMode::ClipToLower, BlendMode::SliceFromLower] {
            let state = blend_mode_to_wgpu(mode);
            assert_eq!(
                state.color.src_factor, state.alpha.src_factor,
                "{:?}: color.src_factor {:?} != alpha.src_factor {:?}",
                mode, state.color.src_factor, state.alpha.src_factor
            );
            assert_eq!(
                state.color.dst_factor, state.alpha.dst_factor,
                "{:?}: color.dst_factor {:?} != alpha.dst_factor {:?}",
                mode, state.color.dst_factor, state.alpha.dst_factor
            );
            assert_eq!(
                state.color.operation, state.alpha.operation,
                "{:?}: color.operation {:?} != alpha.operation {:?}",
                mode, state.color.operation, state.alpha.operation
            );
        }
    }

    /// Multiply and ColorDodge use the same factors for color and alpha, or a transparent composite
    /// slot stores opaque black.
    #[test]
    fn gl_blend_func_modes_match_color_and_alpha_factors() {
        for mode in [BlendMode::Multiply, BlendMode::ColorDodge] {
            let state = blend_mode_to_wgpu(mode);
            assert_eq!(
                state.color.src_factor, state.alpha.src_factor,
                "{:?}: color.src_factor {:?} != alpha.src_factor {:?}",
                mode, state.color.src_factor, state.alpha.src_factor
            );
            assert_eq!(
                state.color.dst_factor, state.alpha.dst_factor,
                "{:?}: color.dst_factor {:?} != alpha.dst_factor {:?}",
                mode, state.color.dst_factor, state.alpha.dst_factor
            );
            assert_eq!(
                state.color.operation, state.alpha.operation,
                "{:?}: color.operation {:?} != alpha.operation {:?}",
                mode, state.color.operation, state.alpha.operation
            );
        }
    }

    /// Every BlendMode must resolve to a well-formed wgpu::BlendState —
    /// this exhaustively matches the enum so a future variant can't be
    /// added without either listing it here or getting a compile error.
    #[test]
    fn every_blend_mode_produces_a_well_formed_wgpu_blend_state() {
        let all = [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::ClipToLower,
            BlendMode::SliceFromLower,
            BlendMode::ColorDodge,
            BlendMode::LinearDodge,
            BlendMode::Overlay,
            BlendMode::ColorBurn,
            BlendMode::LinearBurn,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Add,
            BlendMode::Inverse,
            BlendMode::Subtract,
        ];
        for mode in all {
            // Exercises every arm of blend_mode_to_wgpu. The values are
            // Copy, so just constructing the state is enough — a panicking
            // arm or unreachable fallthrough would show here.
            let _ = blend_mode_to_wgpu(mode);
        }
        // The exhaustive match below will fail to compile if a variant is
        // added to BlendMode without being covered in `all`.
        fn _exhaustive(mode: BlendMode) {
            match mode {
                BlendMode::Normal
                | BlendMode::Multiply
                | BlendMode::Screen
                | BlendMode::ClipToLower
                | BlendMode::SliceFromLower
                | BlendMode::ColorDodge
                | BlendMode::LinearDodge
                | BlendMode::Overlay
                | BlendMode::ColorBurn
                | BlendMode::LinearBurn
                | BlendMode::Darken
                | BlendMode::Lighten
                | BlendMode::Add
                | BlendMode::Inverse
                | BlendMode::Subtract => {}
            }
        }
    }

    /// Add / Subtract / Darken / Lighten use factors of One for both
    /// color and alpha so the operation applies directly to the raw src
    /// and dst values — without that, Min/Max would compare scaled
    /// values and Subtract would drop terms.
    #[test]
    fn wgpu_native_modes_use_unit_factors() {
        for mode in [
            BlendMode::Add,
            BlendMode::Subtract,
            BlendMode::Darken,
            BlendMode::Lighten,
        ] {
            let state = blend_mode_to_wgpu(mode);
            assert_eq!(state.color.src_factor, wgpu::BlendFactor::One, "{:?}", mode);
            assert_eq!(state.color.dst_factor, wgpu::BlendFactor::One, "{:?}", mode);
            assert_eq!(state.alpha.src_factor, wgpu::BlendFactor::One, "{:?}", mode);
            assert_eq!(state.alpha.dst_factor, wgpu::BlendFactor::One, "{:?}", mode);
        }
    }

    /// Scissoring a composite blit is byte-equivalent to the full-screen
    /// blit only when a fully-transparent src leaves dst unchanged under
    /// the mode's fixed-function blend. Replay every mode's actual
    /// `blend_mode_to_wgpu` state with src = (0,0,0,0) over a nontrivial
    /// dst and check the classifier agrees — so a blend-table edit that
    /// changed which modes qualify trips here before it can silently
    /// corrupt a scissored blit.
    #[test]
    fn blend_transparent_src_identity_matches_the_blend_table() {
        fn factor(f: wgpu::BlendFactor, s: [f32; 4], d: [f32; 4], ch: usize) -> f32 {
            match f {
                wgpu::BlendFactor::Zero => 0.0,
                wgpu::BlendFactor::One => 1.0,
                wgpu::BlendFactor::Src => s[ch],
                wgpu::BlendFactor::OneMinusSrc => 1.0 - s[ch],
                wgpu::BlendFactor::SrcAlpha => s[3],
                wgpu::BlendFactor::OneMinusSrcAlpha => 1.0 - s[3],
                wgpu::BlendFactor::Dst => d[ch],
                wgpu::BlendFactor::OneMinusDst => 1.0 - d[ch],
                wgpu::BlendFactor::DstAlpha => d[3],
                wgpu::BlendFactor::OneMinusDstAlpha => 1.0 - d[3],
                other => panic!("unhandled blend factor {other:?}"),
            }
        }
        fn apply(comp: wgpu::BlendComponent, s: [f32; 4], d: [f32; 4], ch: usize) -> f32 {
            let sf = factor(comp.src_factor, s, d, ch) * s[ch];
            let df = factor(comp.dst_factor, s, d, ch) * d[ch];
            match comp.operation {
                wgpu::BlendOperation::Add => sf + df,
                wgpu::BlendOperation::Subtract => sf - df,
                wgpu::BlendOperation::ReverseSubtract => df - sf,
                // Min/Max ignore blend factors on real hardware; every
                // Min/Max mode here uses One factors, so min/max of the
                // factored values equals min/max of the raw values.
                wgpu::BlendOperation::Min => sf.min(df),
                wgpu::BlendOperation::Max => sf.max(df),
            }
        }
        let all = [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::ClipToLower,
            BlendMode::SliceFromLower,
            BlendMode::ColorDodge,
            BlendMode::LinearDodge,
            BlendMode::Overlay,
            BlendMode::ColorBurn,
            BlendMode::LinearBurn,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Add,
            BlendMode::Inverse,
            BlendMode::Subtract,
        ];
        let src = [0.0, 0.0, 0.0, 0.0];
        let dst = [0.3, 0.6, 0.9, 0.5];
        for mode in all {
            let state = blend_mode_to_wgpu(mode);
            let out = [
                apply(state.color, src, dst, 0),
                apply(state.color, src, dst, 1),
                apply(state.color, src, dst, 2),
                apply(state.alpha, src, dst, 3),
            ];
            let is_identity = (0..4).all(|i| (out[i] - dst[i]).abs() < 1e-6);
            assert_eq!(
                is_identity,
                blend_transparent_src_is_identity(mode),
                "{mode:?}: simulated identity={is_identity} out={out:?} vs dst={dst:?}",
            );
        }
        // Darken is the one mode a transparent src does NOT leave alone.
        assert!(!blend_transparent_src_is_identity(BlendMode::Darken));
    }

    /// The bounds projection maps a local AABB to a framebuffer-pixel
    /// scissor with the correct Y-flip and conservative outward rounding.
    /// A half-unit box under an identity transform in a 100×100 viewport
    /// covers the centre quarter [25,75]²; the 2px pad rounds it outward
    /// to [23,77]².
    #[test]
    fn project_aabb_to_scissor_is_conservative_and_y_flipped() {
        let local = Aabb2 {
            min: glam::Vec2::new(-0.5, -0.5),
            max: glam::Vec2::new(0.5, 0.5),
        };
        let px = project_aabb_to_pixels(local, glam::Mat4::IDENTITY, 100.0, 100.0)
            .expect("finite projection");
        for (got, want) in px.iter().zip([25.0, 25.0, 75.0, 75.0]) {
            assert!((got - want).abs() < 1e-3, "pixel aabb {px:?}");
        }
        assert_eq!(
            pixels_to_scissor(px, 100, 100),
            Some(ScreenRect {
                x: 23,
                y: 23,
                width: 54,
                height: 54,
            }),
        );
        // A box projecting fully off-screen yields no rect (caller then
        // uses the full viewport).
        let off = [200.0, 200.0, 300.0, 300.0];
        assert_eq!(pixels_to_scissor(off, 100, 100), None);
    }

    /// `CompositePool::stats()` reports the width/height it was built
    /// with and (used, capacity) from the cursor and slot vec. ensure_size
    /// on a new dimension clears both, so stats drops to (0, 0).
    #[test]
    fn composite_pool_stats_track_size_and_reset() {
        use super::CompositePool;
        let mut pool = CompositePool::new(800, 600);
        assert_eq!(pool.stats(), (0, 0, 800, 600));
        pool.ensure_size(1024, 768);
        assert_eq!(pool.stats(), (0, 0, 1024, 768));
        pool.ensure_size(1, 1);
        let (_, _, w, h) = pool.stats();
        assert_eq!((w, h), (1, 1));
    }
}
