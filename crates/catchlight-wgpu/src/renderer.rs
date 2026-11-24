//! Draws a `RenderList` into a frame the renderer owns, or into a
//! caller-supplied command encoder.

use catchlight_core::{BlendMode, DecodedTexture};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use thiserror::Error;
use wgpu::util::DeviceExt;

mod arena;
mod pipelines;

use arena::FrameArena;

#[derive(Debug, Error)]
pub enum RendererError {
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

    #[error(
        "this submission has already issued its {limit} camera views; \
         one submission token belongs to one submit"
    )]
    CameraViewsExhausted { limit: u32 },

    #[error("this submission token was made by a different renderer")]
    ForeignSubmission,

    #[error("preparing a model's textures: {0}")]
    TexturePrep(String),
}

pub type RendererResult<T> = Result<T, RendererError>;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RenderStats {
    pub drawn_parts: u32,
    pub drawn_composites: u32,
    pub skipped_missing_mesh: u32,
    /// Counts visible parts only; offscreen parts are culled first.
    pub skipped_missing_texture: u32,
    pub skipped_missing_mask_mesh: u32,
    pub skipped_missing_mask_texture: u32,
    pub texture_binds: u32,
    pub pipeline_swaps: u32,
    /// Includes mask-write, blit, and no-draw clear passes.
    pub render_passes: u32,
    pub total_draw_calls: u32,
    pub instance_bytes_uploaded: u64,
    /// Zero when every resident deform generation already matched.
    pub deform_bytes_uploaded: u64,
    pub composite_slots_used: u32,
}

/// Per-frame resource-lifecycle counters, reset at every frame entry.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    /// Excludes [`Recorded::submit`]; non-zero only when an upload (mip
    /// generation) submitted inside the frame.
    pub queue_submits: u32,
    pub queue_writes: u32,
    /// At most 1: `write_buffer` batches at submit start, so a second write to
    /// a live offset would win for every draw that reads it.
    pub instance_buffer_writes: u32,
    /// At most 1, as `instance_buffer_writes`.
    pub part_uniform_buffer_writes: u32,
    pub camera_buffer_writes: u32,
    /// Reads 0: deforms upload outside the frame.
    pub deform_buffer_writes: u32,
    pub instance_buffer_reallocs: u32,
    pub part_uniform_buffer_reallocs: u32,
    pub deform_buffer_reallocs: u32,
    /// Always 0: growing mid-frame strands already-recorded passes on the
    /// freed buffer.
    pub late_buffer_reallocs: u32,
    pub instance_slots_budgeted: u32,
    /// Never above `instance_slots_budgeted`.
    pub instance_slots_reserved: u32,
    pub instance_slots_written: u32,
    pub instance_bytes_written: u64,
    pub part_uniform_slots_budgeted: u32,
    /// Never above `part_uniform_slots_budgeted`.
    pub part_uniform_writes: u32,
}

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
        // Alpha must multiply by DstAlpha too: OVER would write alpha 1 where
        // color is 0, an opaque-black halo once a deform shrinks the mask.
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
        BlendMode::ColorDodge => wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Dst,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        },
        // Matches glBlendFuncSeparate(GL_ONE, GL_ONE_MINUS_SRC_COLOR, GL_ONE, GL_ONE_MINUS_SRC_ALPHA).
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
        // OneMinusDst's alpha is 1-dst.a, matching GL_ONE_MINUS_DST_COLOR.
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
        // Dst-reading modes draw via `blit_composite_dst_in_shader`; this OVER
        // state only renders their src into the offscreen slot, premultiplied.
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

/// Blend modes whose math reads the destination color, which no
/// fixed-function BlendState can express.
fn is_dst_in_shader(mode: BlendMode) -> bool {
    matches!(
        mode,
        BlendMode::Overlay | BlendMode::ColorBurn | BlendMode::LinearBurn,
    )
}

/// The dst-in-shader trio shares Normal's OVER pipeline to avoid spurious
/// pipeline swaps.
fn canonical_part_blend(mode: BlendMode) -> BlendMode {
    if is_dst_in_shader(mode) {
        BlendMode::Normal
    } else {
        mode
    }
}

/// Premultiplied OVER is associative, so a composite whose children all
/// blend OVER can draw them straight to the parent target. The
/// dst-in-shader trio qualifies only for its no-snapshot fallback.
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

/// A transparent src leaves dst unchanged, so a scissored composite blit
/// matches the full-screen one. `Darken` (`min(0, dst) = 0`) is the sole
/// exclusion; a unit test replays `blend_mode_to_wgpu` to keep this in sync.
fn blend_transparent_src_is_identity(mode: BlendMode) -> bool {
    !matches!(mode, BlendMode::Darken)
}

/// AABB in a mesh's local (origin-shifted) space, before the instance
/// model matrix.
#[derive(Copy, Clone, Debug)]
struct Aabb2 {
    min: glam::Vec2,
    max: glam::Vec2,
}

/// Framebuffer pixels, within the viewport.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScreenRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ScreenBounds {
    Empty,
    Unbounded,
    Rect(ScreenRect),
}

impl ScreenBounds {
    fn from_pixels(bounds: Option<[f32; 4]>, width: u32, height: u32) -> Self {
        match bounds {
            None => Self::Unbounded,
            Some(bounds) => {
                pixels_to_scissor(bounds, width, height).map_or(Self::Empty, Self::Rect)
            }
        }
    }

    fn scissor(self) -> Option<ScreenRect> {
        match self {
            Self::Rect(rect) => Some(rect),
            Self::Empty | Self::Unbounded => None,
        }
    }
}

/// Absorbs floor/ceil rounding and rasterization slack.
const SCISSOR_PAD: f32 = 2.0;

/// Returns `[min_x, min_y, max_x, max_y]` in framebuffer pixels, or None when
/// a corner is non-finite or on/behind `w ≈ 0` (callers use the full viewport).
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
    // z_axis is dropped: vertices enter at z = 0 and an SRT matrix keeps
    // w = 1, which the shader forces. Columns stay vec4 for 16-byte alignment.
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

const INSTANCE_STRIDE: u64 = std::mem::size_of::<InstanceRaw>() as u64;

fn part_uniform_bind_group(
    device: &wgpu::Device,
    pipelines: &Pipelines,
    buffer: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        layout: &pipelines.part_uniform_bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: std::num::NonZeroU64::new(std::mem::size_of::<PartUniform>() as u64),
            }),
        }],
        label: Some("part_uniform_bind_group"),
    })
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

#[derive(Copy, Clone, Debug)]
struct PartDraw {
    mesh_id: u32,
    albedo: u32,
    transform: glam::Mat4,
    blend_mode: BlendMode,
}

#[derive(Copy, Clone, Debug)]
struct PreparedPartDraw {
    draw: PartDraw,
    part_uniform_offset: u32,
}

#[derive(Copy, Clone)]
struct PreparedMaskedPartDraw<'a> {
    draw: PreparedPartDraw,
    mask_sources: &'a [crate::collect::MaskSourceData],
}

/// Compares mask sources only; the masked draw rebinds its own mesh and
/// texture per draw.
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

/// Stencil path only. Masked batch k's sources REPLACE k (dodge sources
/// punch 0) and its content tests Equal k; earlier batches' values are all
/// < k, so no clear between batches is needed.
struct StencilPassState<'v> {
    view: &'v wgpu::TextureView,
    stencil_view: &'v wgpu::TextureView,
    label: &'static str,
    next_load: wgpu::LoadOp<wgpu::Color>,
    // Must be dropped before the encoder records anything else (wgpu
    // validates this at runtime).
    pass: Option<wgpu::RenderPass<'static>>,
    next_ref: u32,
    // Bind-group state is pass-scoped, so a draw whose texture is already
    // bound can skip the rebind.
    bound_texture: Option<u32>,
    // Only for a single-part slot pass. A LoadOp::Clear still clears the whole
    // attachment.
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

/// Alpha is the WebGL fallback: per-batch passes with mask-alpha textures.
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
    /// A pass outside the sink wrote the target, spending the frame clear.
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
}

/// Matches the piecewise curve `Rgba8UnormSrgb` applies on texture read.
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

/// Bytes are premultiplied linear encoded as sRGB; the sampler's decode hands
/// shaders premultiplied linear, so CPU-side tints are converted to linear.
pub const PUPPET_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Fill mips 1.. by successively halving the level above on the GPU.
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
        rgba: &[u8],
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

        // Bytes encode premultiplied linear color; the sRGB format makes the
        // sampler return linear values.
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
            rgba,
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
    /// AABB over `pos - origin`. None for an empty mesh.
    local_bounds: Option<Aabb2>,
}

#[derive(Clone)]
pub struct CompositeTexture {
    pub view: wgpu::TextureView,
    // A dst-in-shader child blends against this composite's contents, and
    // the snapshot copy needs the texture handle (COPY_SRC), not a view.
    texture: wgpu::Texture,
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

/// Snapshot of the main color attachment for the dst-in-shader blit. The
/// source must carry `COPY_SRC`.
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

/// Allocates nothing until a dst-in-shader part shows up.
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

    pub fn mark(&self) -> usize {
        self.cursor
    }

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

/// `texture` is only needed by the dst-in-shader blit's snapshot; `None`
/// sends that blit down the Normal-OVER fallback.
#[derive(Clone, Copy)]
struct CompositeTarget<'a> {
    view: &'a wgpu::TextureView,
    texture: Option<&'a wgpu::Texture>,
}

/// Viewport-sized offscreen textures reused across composites; `reset()` at
/// the start of every frame.
pub struct CompositePool {
    slots: Vec<CompositeTexture>,
    cursor: usize,
    // Rewinds pull `cursor` back, so it cannot say how many slots a frame needed.
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

    pub fn reset(&mut self) {
        self.cursor = 0;
        self.peak = 0;
    }

    /// Lets non-nested composites share a slot instead of each growing the pool.
    pub fn mark(&self) -> usize {
        self.cursor
    }

    /// Never moves the cursor forward, so a nested acquire that outlives its
    /// mark is not freed under a recorded pass.
    pub fn rewind(&mut self, mark: usize) {
        self.cursor = self.cursor.min(mark);
    }

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

    /// `(peak_this_frame, capacity, width, height)`.
    pub fn stats(&self) -> (usize, usize, u32, u32) {
        (self.peak, self.slots.len(), self.width, self.height)
    }
}

// Matches `struct PartUniforms` in basic.wgsl. `tint.w` is std140 padding.
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

/// Shared across every puppet in one world; built once per (device,
/// surface_format).
pub struct Pipelines {
    pub surface_format: wgpu::TextureFormat,
    pub(crate) pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) mask_write_pipeline: wgpu::RenderPipeline,
    pub(crate) composite_mask_part_pipeline: wgpu::RenderPipeline,
    pub(crate) composite_mask_write_pipeline: wgpu::RenderPipeline,
    pub(crate) composite_mask_alpha_dodge_pipeline: Option<wgpu::RenderPipeline>,
    pub(crate) masked_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) blit_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    // Equal-test stencil variants for blits drawn inside the mask pass. Empty
    // when has_stencil == false.
    pub(crate) masked_blit_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    // Seeds an all-DodgeMask batch so dodge sources can punch 0 out of it.
    // None when has_stencil == false.
    pub(crate) stencil_fill_pipeline: Option<wgpu::RenderPipeline>,
    pub(crate) blit_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) blit_sampler: wgpu::Sampler,
    // Fixed to `Rgba8UnormSrgb`, independent of `surface_format`.
    pub(crate) mip_pipeline: wgpu::RenderPipeline,
    pub(crate) mip_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) blit_dst_in_shader_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) snapshot_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) snapshot_sampler: wgpu::Sampler,
    // Dynamic offset: one camera slot per view in a submit, so views do not
    // alias under `write_buffer` batching.
    pub(crate) camera_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) camera_stride: u64,
    pub(crate) texture_bind_group_layout: wgpu::BindGroupLayout,
    pub(crate) texture_sampler: wgpu::Sampler,
    pub(crate) part_uniform_bind_group_layout: wgpu::BindGroupLayout,
    // Rounded up to `min_uniform_buffer_offset_alignment`. Blits share the
    // wire format.
    pub(crate) part_uniform_stride: u64,
    // Native backends drive `first_instance` in hardware; GL/WebGL2 and the
    // adapter-less constructors re-slice the instance buffer per draw.
    pub(crate) base_instance: bool,
    // False where the backend lacks a usable stencil attachment (Chromium
    // swiftshader WebGL2 fails Depth24PlusStencil8); masks then sample an
    // offscreen alpha texture.
    pub(crate) has_stencil: bool,
    // Only populated when has_stencil == false.
    pub(crate) mask_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub(crate) mask_sampler: Option<wgpu::Sampler>,
    pub(crate) mask_alpha_pipeline: Option<wgpu::RenderPipeline>,
    // Writes alpha 0 where the source rasterizes, mirroring the stencil
    // path's REPLACE-with-0.
    pub(crate) mask_alpha_dodge_pipeline: Option<wgpu::RenderPipeline>,
    pub(crate) masked_sampled_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    pub(crate) masked_sampled_blit_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
}

/// Views past the last are refused rather than wrapping onto a slot an
/// already-recorded command buffer still reads.
#[doc(hidden)]
pub const CAMERA_RING_SLOTS: u32 = 64;

/// `slot` must be below [`CAMERA_RING_SLOTS`]; the ring does not wrap.
fn camera_slot_offset(slot: u32, stride: u64) -> u64 {
    debug_assert!(
        slot < CAMERA_RING_SLOTS,
        "camera slot {slot} is past the ring"
    );
    u64::from(slot) * stride
}

/// One host-owned submit's worth of views for [`WgpuRenderer::render_into`]:
/// each view takes its own camera slot and continues the frame buffers after
/// the previous one. Make a fresh one per submit: one kept across submits
/// runs the ring out, and two over one submit alias (hence not `Clone`).
#[derive(Debug)]
pub struct Submission {
    /// A token spent on another renderer is refused.
    renderer: u64,
    views: u32,
}

impl Submission {
    #[doc(hidden)]
    pub fn views_issued(&self) -> u32 {
        self.views
    }
}

/// Dense `Vec<Option<V>>` keyed by small ids, avoiding SipHash on the
/// per-drawable hot path.
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

    fn take(&mut self, index: usize) -> Option<V> {
        self.slots.get_mut(index).and_then(Option::take)
    }

    fn clear(&mut self) {
        self.slots.clear();
    }

    fn live(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    fn keys(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|_| i))
    }
}

/// One puppet's copy of the cache's mesh layout in the deform atlas, so
/// puppets sharing a [`crate::RenderCache`] do not overwrite each other's
/// deforms within one submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeformSet(u32);

impl DeformSet {
    pub const FIRST: Self = Self(0);

    #[doc(hidden)]
    pub fn index(self) -> u32 {
        self.0
    }
}

impl Default for DeformSet {
    fn default() -> Self {
        Self::FIRST
    }
}

#[derive(Default)]
struct DeformSetState {
    /// Mesh slots with active deform data, keyed to the `DeformStack`
    /// generation that produced them. None rechecks bytes after a model rebake.
    uploaded: DenseMap<Option<u64>>,
    still_active: DenseMap<()>,
    /// Per-mesh max absolute deform component. Deform is added in local
    /// space, so expanding the local AABB by this bounds the deformed geometry.
    max: DenseMap<f32>,
    /// The next upload zeroes the whole slice first, so a previous tenant's
    /// bytes never draw.
    needs_clear: bool,
    live: bool,
}

impl DeformSetState {
    fn live() -> Self {
        Self {
            live: true,
            ..Self::default()
        }
    }

    fn invalidate(&mut self) {
        self.uploaded.clear();
        self.still_active.clear();
        self.max.clear();
        self.needs_clear = true;
    }
}

static NEXT_RENDERER_ID: AtomicU64 = AtomicU64::new(0);

pub struct WgpuRenderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub shared: std::sync::Arc<Pipelines>,
    id: u64,
    part_uniforms: FrameArena,
    part_uniform_bind_group: wgpu::BindGroup,
    mesh_buffers: DenseMap<MeshBuffer>,
    textures: DenseMap<Texture>,
    instances: FrameArena,
    deform_buffer: wgpu::Buffer,
    deform_buffer_capacity: u64,
    deform_buffer_len: u64,
    deform_upload_mirror: Vec<u8>,
    // Folded into the next frame's stats; the upload runs before the frame
    // resets them.
    pending_deform_bytes: u64,
    // Atomic so `&self` blit helpers can count too.
    frame_render_passes: AtomicU32,
    frame_draw_calls: AtomicU32,
    // Owned frames use slot 0; `render_into` spends the ring from the
    // caller's `Submission`.
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    camera_offset: u32,
    current_stats: RenderStats,
    frame_stats: FrameStats,
    // Any buffer recreation after this is set trips a debug assertion.
    frame_sizing_closed: bool,
    /// None without `Features::TIMESTAMP_QUERY`; CPU `tracing` spans only.
    gpu_profiler: Option<wgpu_profiler::GpuProfiler>,
    /// Indexed by set; slot 0 is [`DeformSet::FIRST`].
    deform_sets: Vec<DeformSetState>,
    deform_free: Vec<u32>,
    deform_inactive_scratch: Vec<u32>,
    // Sorted and coalesced so far-apart meshes don't force one write over
    // the untouched span between them.
    deform_write_ranges: Vec<(u64, u64)>,
    /// Frame-scoped: `record_frame` points these at each list before recording it.
    deform_recording_set: DeformSet,
    deform_recording_base: u64,
    /// CPU copy kept so per-part bounds can project to clip space.
    camera_view_proj: glam::Mat4,
    draw_filter_scratch: Vec<usize>,
    instance_scratch: Vec<InstanceRaw>,
    composite_mask_textures: HashMap<u32, PreparedCompositeMask>,
    composite_bounds: HashMap<u32, Option<[f32; 4]>>,
}

/// Viewport-sized mask target shared across every puppet in a frame:
/// `Depth24PlusStencil8` on the stencil path, otherwise a sampled color
/// mask texture (WebGL fallback).
pub struct StencilTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    // None when has_stencil == true.
    pub mask_bind_group: Option<wgpu::BindGroup>,
}

impl StencilTarget {
    /// Stencil-backed only; `new_for_pipelines` picks by backend.
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
    /// Prefer `from_pipelines` to share pipelines across renderers; each
    /// `Pipelines::new` compiles ~18 pipelines.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        // `Arc`, not `Rc`: catchlight-bevy needs `Pipelines` Send + Sync, which it
        // lacks only on wasm32 with wgpu's `webgl` feature.
        #[allow(clippy::arc_with_non_send_sync)]
        let shared = std::sync::Arc::new(Pipelines::new(&device, surface_format));
        Self::from_pipelines(device, queue, shared)
    }

    /// GL (WebGL2) takes the alpha-discard fallback because Chromium
    /// swiftshader fails Depth24PlusStencil8. `CATCHLIGHT_DISABLE_STENCIL`
    /// forces it elsewhere.
    pub fn new_autodetect(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        // `Arc` for the reason given in `new` above.
        #[allow(clippy::arc_with_non_send_sync)]
        let shared =
            std::sync::Arc::new(Pipelines::new_autodetect(adapter, &device, surface_format));
        Self::from_pipelines(device, queue, shared)
    }

    pub fn from_pipelines(
        device: wgpu::Device,
        queue: wgpu::Queue,
        shared: std::sync::Arc<Pipelines>,
    ) -> Self {
        let instances = FrameArena::new(
            &device,
            "Instance Buffer",
            wgpu::BufferUsages::VERTEX,
            512 * INSTANCE_STRIDE,
        );
        let part_uniforms = FrameArena::new(
            &device,
            "Part Uniform Buffer",
            wgpu::BufferUsages::UNIFORM,
            256 * shared.part_uniform_stride,
        );
        let part_uniform_bind_group =
            part_uniform_bind_group(&device, &shared, part_uniforms.buffer());

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
        // Slot 0 holds identity so a frame before any update_camera
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
            id: NEXT_RENDERER_ID.fetch_add(1, Ordering::Relaxed),
            part_uniforms,
            part_uniform_bind_group,
            mesh_buffers: DenseMap::new(),
            textures: DenseMap::new(),
            instances,
            deform_buffer,
            deform_buffer_capacity,
            deform_buffer_len: 0,
            deform_upload_mirror: Vec::new(),
            pending_deform_bytes: 0,
            frame_render_passes: AtomicU32::new(0),
            frame_draw_calls: AtomicU32::new(0),
            camera_buffer,
            camera_bind_group,
            camera_offset: 0,
            current_stats: RenderStats::default(),
            frame_stats: FrameStats::default(),
            frame_sizing_closed: false,
            gpu_profiler,
            deform_sets: vec![DeformSetState::live()],
            deform_free: Vec::new(),
            deform_inactive_scratch: Vec::new(),
            deform_write_ranges: Vec::new(),
            deform_recording_set: DeformSet::FIRST,
            deform_recording_base: 0,
            camera_view_proj: glam::Mat4::IDENTITY,
            draw_filter_scratch: Vec::new(),
            instance_scratch: Vec::new(),
            composite_mask_textures: HashMap::new(),
            composite_bounds: HashMap::new(),
        }
    }

    /// Reset at each frame entry; read it after the render call returns.
    pub fn frame_stats(&self) -> FrameStats {
        self.frame_stats
    }

    fn note_queue_write(&mut self, buffer: RenderBuffer) {
        self.frame_stats.queue_writes += 1;
        match buffer {
            RenderBuffer::Instance => self.frame_stats.instance_buffer_writes += 1,
            RenderBuffer::PartUniform => self.frame_stats.part_uniform_buffer_writes += 1,
            RenderBuffer::Camera => self.frame_stats.camera_buffer_writes += 1,
            RenderBuffer::Deform => self.frame_stats.deform_buffer_writes += 1,
        }
    }

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

    /// `total_instances` must cover every draw in the frame; growing mid-frame
    /// strands recorded passes.
    fn begin_frame_instances(&mut self, total_instances: usize, continues: bool) {
        self.frame_stats.instance_slots_budgeted = total_instances as u32;
        let needed = total_instances as u64 * INSTANCE_STRIDE;
        if self.instances.begin_frame(&self.device, needed, continues) {
            self.note_realloc(RenderBuffer::Instance);
        }
    }

    /// The mirror is a byte-exact copy of the atlas, so re-uploading it keeps
    /// every set's residency memo true. Only legal between frames.
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
    }

    fn reset_deform_buffer_layout(&mut self) {
        self.deform_buffer_len = 0;
        self.deform_upload_mirror.clear();
        self.deform_inactive_scratch.clear();
        // Every set's bytes moved: the mesh slots the offsets were derived
        // from are all being handed out again.
        for set in &mut self.deform_sets {
            set.invalidate();
        }
    }

    fn reserve_deform_buffer(&mut self, size: u64) -> u64 {
        let offset = self.deform_buffer_len;
        let end = offset + size;
        self.ensure_deform_buffer_capacity(end);
        self.deform_buffer_len = end;
        if self.deform_upload_mirror.len() < end as usize {
            self.deform_upload_mirror.resize(end as usize, 0);
        }
        offset
    }

    /// Padded so every set's base stays 16-byte aligned (`set_vertex_buffer`
    /// needs 4).
    fn deform_stride(&self) -> u64 {
        self.deform_buffer_len.next_multiple_of(16)
    }

    /// Where `set`'s copy of the mesh layout starts. Derived rather than
    /// stored, so a rebuild that changes the stride moves every set at once.
    fn deform_base(&self, set: DeformSet) -> u64 {
        set.0 as u64 * self.deform_stride()
    }

    /// Take a deform set for one more puppet of this renderer's model. Release
    /// it with [`Self::release_deform_set`] so the index is reused.
    pub fn acquire_deform_set(&mut self) -> DeformSet {
        let index = match self.deform_free.pop() {
            Some(index) => index,
            None => {
                self.deform_sets.push(DeformSetState::default());
                (self.deform_sets.len() - 1) as u32
            }
        };
        self.deform_sets[index as usize] = DeformSetState::live();
        self.deform_sets[index as usize].needs_clear = true;
        DeformSet(index)
    }

    /// [`DeformSet::FIRST`] is never released.
    pub fn release_deform_set(&mut self, set: DeformSet) {
        if set == DeformSet::FIRST {
            return;
        }
        let Some(state) = self.deform_sets.get_mut(set.0 as usize) else {
            return;
        };
        if !state.live {
            return;
        }
        *state = DeformSetState::default();
        self.deform_free.push(set.0);
    }

    #[doc(hidden)]
    pub fn live_deform_sets(&self) -> usize {
        self.deform_sets.iter().filter(|s| s.live).count()
    }

    /// Runs at upload time because a set's base depends on the mesh layout,
    /// which a rebuild can still change.
    fn ensure_deform_sets_sized(&mut self) {
        let needed = self.deform_stride() * self.deform_sets.len() as u64;
        if self.deform_upload_mirror.len() < needed as usize {
            self.deform_upload_mirror.resize(needed as usize, 0);
        }
        self.ensure_deform_buffer_capacity(needed);
    }

    fn deform_slice(&self, mesh: &MeshBuffer) -> wgpu::BufferSlice<'_> {
        let start = self.deform_recording_base + mesh.deform_offset;
        self.deform_buffer.slice(start..start + mesh.deform_size)
    }

    fn reserve_instances(&mut self, count: usize) -> u64 {
        self.frame_stats.instance_slots_reserved += count as u32;
        self.instances.reserve(count as u64 * INSTANCE_STRIDE)
    }

    fn bind_pass_instances(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        if self.shared.base_instance {
            render_pass.set_vertex_buffer(1, self.instances.buffer().slice(..));
        }
    }

    /// `instance_offset` is a whole number of strides into the buffer, so the
    /// derived instance index is exact.
    fn emit_part_draw(
        &self,
        render_pass: &mut wgpu::RenderPass<'_>,
        num_indices: u32,
        instance_offset: u64,
    ) {
        if self.shared.base_instance {
            let instance = (instance_offset / INSTANCE_STRIDE) as u32;
            render_pass.draw_indexed(0..num_indices, 0, instance..instance + 1);
        } else {
            render_pass.set_vertex_buffer(
                1,
                self.instances
                    .buffer()
                    .slice(instance_offset..instance_offset + INSTANCE_STRIDE),
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
        self.frame_stats.instance_slots_written += instances.len() as u32;
        self.frame_stats.instance_bytes_written += bytes.len() as u64;
        self.instances.write(offset, bytes);
    }

    fn flush_instance_writes(&mut self) {
        let uploaded = self.instances.flush(&self.queue);
        if uploaded > 0 {
            self.note_queue_write(RenderBuffer::Instance);
        }
        self.current_stats.instance_bytes_uploaded = uploaded;
    }

    fn begin_frame_uniforms(&mut self, count: u32, continues: bool) {
        self.frame_stats.part_uniform_slots_budgeted = count;
        let needed = u64::from(count) * self.shared.part_uniform_stride;
        if self
            .part_uniforms
            .begin_frame(&self.device, needed, continues)
        {
            self.note_realloc(RenderBuffer::PartUniform);
            self.part_uniform_bind_group =
                part_uniform_bind_group(&self.device, &self.shared, self.part_uniforms.buffer());
        }
        // Both frame buffers are now sized; every pass recorded from here
        // on binds them, so nothing may reallocate until the next frame.
        self.frame_sizing_closed = true;
    }

    fn flush_part_uniform_writes(&mut self) {
        if self.part_uniforms.flush(&self.queue) > 0 {
            self.note_queue_write(RenderBuffer::PartUniform);
        }
    }

    /// Only inside a frame: the slot is staged and uploaded with the frame.
    fn write_part_uniform(
        &mut self,
        opacity: f32,
        tint: glam::Vec3,
        screen_tint: glam::Vec3,
        mask_threshold: f32,
    ) -> u32 {
        // Tints are authored in sRGB; fragment math runs in linear.
        let tint_linear = srgb_to_linear_vec3(tint);
        let screen_tint_linear = srgb_to_linear_vec3(screen_tint);
        let uniform = PartUniform::new(opacity, tint_linear, screen_tint_linear, mask_threshold);
        let offset = self.part_uniforms.reserve(self.shared.part_uniform_stride);
        self.part_uniforms
            .write(offset, bytemuck::bytes_of(&uniform));
        self.frame_stats.part_uniform_writes += 1;
        offset as u32
    }

    /// Uploaded into a camera slot of its own by the next render call.
    pub fn update_camera(&mut self, view_proj: glam::Mat4) {
        self.camera_view_proj = view_proj;
    }

    #[doc(hidden)]
    pub fn camera_dynamic_offset(&self) -> u32 {
        self.camera_offset
    }

    fn write_camera(&mut self, slot: u32) {
        let offset = camera_slot_offset(slot, self.shared.camera_stride);
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
    }

    /// None when the bound is unknown or the projection is non-finite; callers
    /// then use the full viewport.
    fn part_pixel_aabb(
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
            .get(self.deform_recording_set.0 as usize)
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

    fn part_screen_bounds(
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

    fn drawable_screen_bounds(
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
                if !render_list
                    .composite_children
                    .get(node_id)
                    .is_some_and(|c| !c.is_empty())
                {
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
    fn composite_pixel_aabb(
        &mut self,
        node_id: u32,
        render_list: &crate::collect::RenderList,
        width: u32,
        height: u32,
    ) -> Option<[f32; 4]> {
        use crate::collect::DrawableInfo;
        if let Some(bounds) = self.composite_bounds.get(&node_id) {
            return *bounds;
        }
        let bounds = (|| {
            let mut acc: Option<[f32; 4]> = None;
            for child in render_list.composite_children.get(&node_id)? {
                let aabb = match child {
                    DrawableInfo::Part {
                        mesh_id, transform, ..
                    } => self.part_pixel_aabb(*mesh_id, *transform, width, height)?,
                    DrawableInfo::Composite { node_id, .. } => {
                        if !render_list
                            .composite_children
                            .get(node_id)
                            .is_some_and(|c| !c.is_empty())
                        {
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
        self.composite_bounds.insert(node_id, bounds);
        bounds
    }

    /// Meshes that went inactive are zeroed, otherwise the vertex shader keeps
    /// reading a stale config's offsets.
    pub(crate) fn upload_deforms<'a>(
        &mut self,
        set: DeformSet,
        active: impl Iterator<Item = (u32, u64, &'a [glam::Vec2])>,
    ) {
        // Coalesce dirty ranges only across gaps this small: below it,
        // one write over the untouched bytes beats a second write_buffer's
        // fixed overhead; above it, the copied span is pure waste.
        const DEFORM_MERGE_GAP: u64 = 4096;

        self.ensure_deform_sets_sized();
        let base = self.deform_base(set);
        let stride = self.deform_stride();
        let Some(mut state) = self
            .deform_sets
            .get_mut(set.0 as usize)
            .filter(|state| state.live)
            .map(std::mem::take)
        else {
            tracing::warn!(set = set.0, "upload_deforms: no such deform set");
            return;
        };
        state.still_active.clear();
        let mut ranges = std::mem::take(&mut self.deform_write_ranges);
        ranges.clear();
        if state.needs_clear {
            state.needs_clear = false;
            let end = (base + stride).min(self.deform_upload_mirror.len() as u64);
            if end > base {
                self.deform_upload_mirror[base as usize..end as usize].fill(0);
                ranges.push((base, end));
            }
        }
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
            if state.uploaded.get(mesh_id as usize).copied() == Some(Some(generation)) {
                state.still_active.insert(mesh_id as usize, ());
                continue;
            }
            let bytes: &[u8] = bytemuck::cast_slice(combined);
            let start = (base + buf.deform_offset) as usize;
            let end = start + bytes.len();
            if self.deform_upload_mirror[start..end] != *bytes {
                self.deform_upload_mirror[start..end].copy_from_slice(bytes);
                ranges.push((start as u64, end as u64));
            }
            state.uploaded.insert(mesh_id as usize, Some(generation));
            state.still_active.insert(mesh_id as usize, ());
            let max_abs = combined
                .iter()
                .fold(0.0f32, |m, v| m.max(v.x.abs()).max(v.y.abs()));
            state.max.insert(mesh_id as usize, max_abs);
        }

        self.deform_inactive_scratch.clear();
        for i in state.uploaded.keys() {
            if !state.still_active.contains(i) {
                self.deform_inactive_scratch.push(i as u32);
            }
        }
        for i in 0..self.deform_inactive_scratch.len() {
            let mesh_id = self.deform_inactive_scratch[i];
            state.uploaded.remove(mesh_id as usize);
            state.max.remove(mesh_id as usize);
            let Some(buf) = self.mesh_buffers.get(mesh_id as usize) else {
                continue;
            };
            let start = (base + buf.deform_offset) as usize;
            let end = start + buf.deform_size as usize;
            self.deform_upload_mirror[start..end].fill(0);
            ranges.push((start as u64, end as u64));
        }

        // pending_deform_bytes counts merged-run lengths, not the min..max span.
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
        // Accumulated: N puppets each upload before the frame that draws them all.
        self.pending_deform_bytes += written;

        ranges.clear();
        self.deform_write_ranges = ranges;
        if let Some(slot) = self.deform_sets.get_mut(set.0 as usize) {
            *slot = state;
        }
    }

    pub fn clear_meshes(&mut self) {
        self.mesh_buffers.clear();
        self.reset_deform_buffer_layout();
    }

    /// `keep` maps old slots to new slots; a moved resource keeps its debug label.
    pub(crate) fn retain_textures(&mut self, keep: &[(u32, u32)]) {
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

    pub(crate) fn has_mesh(&self, slot: u32) -> bool {
        self.mesh_buffers.contains(slot as usize)
    }

    pub(crate) fn retain_meshes(&mut self, keep: &[(u32, u32)], total: usize) {
        if keep.len() == total
            && total == self.mesh_buffers.live()
            && keep.iter().all(|&(from, to)| from == to)
        {
            // Rebaking restarts stack generations even when geometry is unchanged.
            // Keep residency and active slots, but compare their bytes on refresh.
            for set in &mut self.deform_sets {
                for generation in set.uploaded.slots.iter_mut().flatten() {
                    *generation = None;
                }
            }
            return;
        }
        let moved: Vec<_> = keep
            .iter()
            .filter_map(|&(from, to)| self.mesh_buffers.take(from as usize).map(|mesh| (to, mesh)))
            .collect();
        self.clear_meshes();
        for (to, mut mesh) in moved {
            mesh.deform_offset = self.reserve_deform_buffer(mesh.deform_size);
            self.mesh_buffers.insert(to as usize, mesh);
        }
    }

    /// Checked without touching GPU state, so a rebuild can validate before
    /// releasing the build it replaces.
    pub(crate) fn validate_texture_upload(&self, tex: &DecodedTexture) -> RendererResult<()> {
        validate_rgba8_texture(
            tex.width,
            tex.height,
            tex.rgba.len(),
            self.device.limits().max_texture_dimension_2d,
        )
        .map(|_| ())
    }

    #[doc(hidden)]
    pub fn max_texture_dimension(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    #[doc(hidden)]
    pub fn live_mesh_slots(&self) -> usize {
        self.mesh_buffers.live()
    }

    #[doc(hidden)]
    pub fn live_texture_slots(&self) -> usize {
        self.textures.live()
    }

    #[doc(hidden)]
    pub fn reserved_deform_bytes(&self) -> u64 {
        self.deform_buffer_len
    }

    /// Missing uvs become zeros and a trailing partial triangle is ignored,
    /// matching the importer. An out-of-range index is an error.
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
            num_indices: (mesh.indices.len() - mesh.indices.len() % 3) as u32,
            index_format,
            local_bounds,
        };

        self.mesh_buffers.insert(mesh_id as usize, mesh_buffer);
        Ok(())
    }

    fn part_bind_group(&self, albedo: u32) -> Option<&wgpu::BindGroup> {
        self.textures.get(albedo as usize).map(|t| &t.bind_group)
    }

    pub fn upload_texture(&mut self, texture_id: u32, tex: &DecodedTexture) -> RendererResult<()> {
        let texture = Texture::from_decoded_rgba8(
            &self.device,
            &self.queue,
            &tex.rgba,
            tex.width,
            tex.height,
            &self.shared,
            &format!("Texture {texture_id}"),
        )?;
        // Mip generation submits its own command buffer.
        self.frame_stats.queue_submits += u32::from(texture.texture.mip_level_count() > 1);

        self.textures.insert(texture_id as usize, texture);
        Ok(())
    }

    fn prepare_composite_mask_textures(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        render_list: &crate::collect::RenderList,
        composites: &mut CompositePool,
        width: u32,
        height: u32,
    ) {
        use crate::collect::{DrawableInfo, MaskSourceData};
        self.composite_mask_textures.clear();
        if render_list.composite_mask_sources.is_empty() {
            return;
        }
        let mut source_ids = Vec::new();
        let mut pending: Vec<_> = render_list.root_drawables.iter().collect();
        while let Some(drawable) = pending.pop() {
            if self.drawable_screen_bounds(drawable, render_list, width, height)
                == ScreenBounds::Empty
            {
                continue;
            }
            let masks = match drawable {
                DrawableInfo::Part { mask_sources, .. } => mask_sources,
                DrawableInfo::Composite {
                    node_id,
                    mask_sources,
                    ..
                } => {
                    if let Some(children) = render_list.composite_children.get(node_id) {
                        pending.extend(children);
                    }
                    mask_sources
                }
            };
            source_ids.extend(masks.iter().filter_map(|mask| match mask {
                MaskSourceData::Composite { node_id, .. } => Some(*node_id),
                MaskSourceData::Part { .. } => None,
            }));
        }
        source_ids.sort_unstable();
        source_ids.dedup();

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
            let stride = INSTANCE_STRIDE;
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
                pass.set_vertex_buffer(2, self.deform_slice(mesh));
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

    /// Alpha-mask fallback. Each source's instance must already be at
    /// `base_offset + i * size_of::<InstanceRaw>()`.
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

        let stride = INSTANCE_STRIDE;

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

        // Start at 1 for an all-DodgeMask batch, else 0; sources REPLACE with 1
        // (Mask) or 0 (DodgeMask) and content passes where the mask is 1.
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
                    render_pass.set_vertex_buffer(2, self.deform_slice(mask_mesh));
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

    /// Stencil path: regular sources REPLACE `ref_value`, dodge sources punch
    /// 0, and an all-DodgeMask batch first fills the viewport with `ref_value`.
    /// The caller must have bound the camera at group 0.
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

        let stride = INSTANCE_STRIDE;

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
                    render_pass.set_vertex_buffer(2, self.deform_slice(mask_mesh));
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

    /// Writes the batch's instance block `[mask0..maskK-1, part0..partN-1]` and
    /// returns the surviving indices plus mask and part bases. The caller must
    /// return the indices to `draw_filter_scratch`.
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

        let stride = INSTANCE_STRIDE;
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
    // is populated by record_list.
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn record_masked_part_draws(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        draws: &[PreparedMaskedPartDraw<'_>],
        valid: &[usize],
        parts_base: u64,
        bound_texture: &mut Option<u32>,
    ) -> RendererResult<()> {
        let stride = INSTANCE_STRIDE;
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
                    .expect("part bind group must exist — record_list ensures it");
                render_pass.set_bind_group(1, part_bg, &[]);
                *bound_texture = Some(key);
                texture_binds += 1;
            }
            if current_mesh != Some(part.mesh_id) {
                render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(2, self.deform_slice(mesh));
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

    // Alpha path: a same-signature masked run is one mask-alpha write pass
    // plus one sampled masked-draw pass. `clear_target` folds a composite-slot
    // clear into the masked-draw pass. Returns whether that pass was emitted.
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
        // Only the content draw is scissored; it samples the mask only inside the
        // rect.
        if let Some(r) = scissor {
            render_pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        }
        self.record_masked_part_draws(&mut render_pass, draws, &valid, parts_base, &mut None)?;
        drop(render_pass);

        self.draw_filter_scratch = valid;
        Ok(true)
    }

    /// The caller must hand the indices back to `draw_filter_scratch`.
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
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn record_unmasked_part_draws(
        &mut self,
        render_pass: &mut wgpu::RenderPass<'_>,
        drawables: &[PreparedPartDraw],
        filtered: &[usize],
        base_offset: u64,
        bound_texture: &mut Option<u32>,
    ) {
        let stride = INSTANCE_STRIDE;

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
                    .expect("part bind group must exist — record_list ensures it");
                render_pass.set_bind_group(1, bind_group, &[]);
                *bound_texture = Some(key);
                texture_binds += 1;
            }
            if current_mesh != Some(d.mesh_id) {
                render_pass.set_vertex_buffer(0, mesh_buffer.vertex_buffer.slice(..));
                render_pass.set_vertex_buffer(2, self.deform_slice(mesh_buffer));
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

    // Alpha path. Returns whether a draw pass was emitted, so callers can fold
    // the composite-slot clear into the first batch that draws.
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

    /// Stencil is cleared at open and discarded at the end: nothing reads it
    /// across passes.
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

    /// Ends the stencil sink's pass before an outside pass touches the target,
    /// emitting a still-pending frame clear first. No-op for the alpha sink.
    fn end_sink_pass(&self, encoder: &mut wgpu::CommandEncoder, sink: &mut PartSink<'_, '_>) {
        let PartSink::Stencil(s) = sink else { return };
        s.pass = None;
        if let wgpu::LoadOp::Clear(color) = s.next_load {
            self.explicit_clear_pass(encoder, s.view, color, "render_list deferred clear");
            s.next_load = wgpu::LoadOp::Load;
        }
    }

    /// No-op when the batch is empty, so barriers can call it unconditionally.
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

    /// Takes `&self` so the `&self` blit helpers count too.
    fn counted_begin_pass<'e>(
        &self,
        encoder: &'e mut wgpu::CommandEncoder,
        desc: &wgpu::RenderPassDescriptor<'_>,
    ) -> wgpu::RenderPass<'e> {
        self.frame_render_passes.fetch_add(1, Ordering::Relaxed);
        encoder.begin_render_pass(desc)
    }

    /// For when the frame clear can't be folded into a drawing pass.
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

    fn finalize_frame_stats(&mut self) {
        self.current_stats.render_passes = self.frame_render_passes.load(Ordering::Relaxed);
        self.current_stats.total_draw_calls = self.frame_draw_calls.load(Ordering::Relaxed);
        self.current_stats.deform_bytes_uploaded = self.pending_deform_bytes;
    }

    /// Alpha path only; the stencil path folds the mask write and blit into
    /// one pass (`blit_composite_with_masks`).
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

    // Invariant: blend_mode is a dst-in-shader mode; lookup falls back to the
    // Overlay pipeline for an unwired future variant.
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
        // The copy runs in encoder order (unlike write_buffer), so it sees every
        // prior pass. A scissored copy suffices: the snapshot is sampled 1:1 and
        // dst-in-shader modes return dst where src alpha is 0.
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

    /// Gives the dst-in-shader blit a src texture independent of the framebuffer.
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
        // Mask-source draws use the part layout; the blit pipeline rebinds group 0.
        render_pass.set_bind_group(0, &self.camera_bind_group, &[self.camera_offset]);
        // Outside the children union the composite is a transparent clear and the
        // stencil is never tested.
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

    /// One frame is one submit, so it keeps camera slot 0 and monotonic
    /// cursors. Hosts that own the submit use [`Self::render_into`].
    pub fn frame(&mut self) -> Frame<'_> {
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("catchlight frame"),
            });
        Frame {
            renderer: self,
            encoder,
        }
    }

    /// Make a fresh one per submit; see [`Submission`].
    pub fn submission(&self) -> Submission {
        Submission {
            renderer: self.id,
            views: 0,
        }
    }

    /// Record into a caller-owned encoder (catchlight-bevy's render graph).
    /// Errors without recording when `submission` came from another renderer or
    /// has already issued [`CAMERA_RING_SLOTS`] views.
    #[allow(clippy::too_many_arguments)]
    pub fn render_into(
        &mut self,
        submission: &mut Submission,
        lists: &[&crate::collect::RenderList],
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
        if submission.renderer != self.id {
            return Err(RendererError::ForeignSubmission);
        }
        if submission.views >= CAMERA_RING_SLOTS {
            return Err(RendererError::CameraViewsExhausted {
                limit: CAMERA_RING_SLOTS,
            });
        }
        let slot = submission.views;
        submission.views += 1;
        self.record_frame(
            slot,
            lists,
            encoder,
            view,
            stencil,
            composites,
            target_color_texture,
            snapshots,
            width,
            height,
            clear_color,
        )
    }

    /// Lists record back to back under one camera slot. A later view of the
    /// same submit takes the next `camera_slot` and continues the frame
    /// buffers. Only the first list may consume `clear_color`. Without `target_color_texture` (which
    /// must carry `COPY_SRC`) and `snapshots`, dst-in-shader modes fall back to
    /// Normal-OVER.
    #[allow(clippy::too_many_arguments, clippy::expect_used)]
    fn record_frame(
        &mut self,
        camera_slot: u32,
        lists: &[&crate::collect::RenderList],
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        mut snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<RenderStats> {
        use crate::collect::DrawableInfo;

        let span = tracing::trace_span!(
            "render_lists",
            puppets = lists.len(),
            drawn_parts = tracing::field::Empty
        );
        let _entered = span.enter();

        // Must precede the camera write, this frame's first queue write.
        self.frame_stats = FrameStats::default();
        self.frame_sizing_closed = false;
        self.write_camera(camera_slot);
        self.current_stats = RenderStats::default();
        self.frame_render_passes.store(0, Ordering::Relaxed);
        self.frame_draw_calls.store(0, Ordering::Relaxed);

        let gpu_query = self
            .gpu_profiler
            .as_ref()
            .map(|p| p.begin_query("render_list", encoder));

        // Keeps frames without shader-math modes on the plain render path.
        let drawables = || {
            lists.iter().flat_map(|list| {
                list.root_drawables
                    .iter()
                    .chain(list.composite_children.values().flatten())
            })
        };
        let dst_in_shader_present = drawables().any(|d| is_dst_in_shader(d.blend_mode()));
        let dst_in_shader_active =
            dst_in_shader_present && target_color_texture.is_some() && snapshots.is_some();
        if dst_in_shader_present && !dst_in_shader_active {
            // Once per process: the plain render path hits this every frame.
            static FALLBACK_WARNED: std::sync::Once = std::sync::Once::new();
            FALLBACK_WARNED.call_once(|| {
                tracing::warn!(
                    "dst-in-shader blend mode present but the caller supplied no \
                     target_color_texture + snapshot pool; falling back to the \
                     Normal-OVER approximation",
                );
            });
        }

        // Sized over every list; mid-frame growth would strand earlier passes.
        let total_instances: usize = lists.iter().map(|l| l.total_instance_count()).sum();
        // Later views of one submit keep the bytes of the earlier ones.
        let continues = camera_slot > 0;
        self.begin_frame_instances(total_instances, continues);
        // One slot per drawable, composite child and mask source, plus one per
        // dst-in-shader part and masked dst-in-shader composite.
        let dst_in_shader_extra = if dst_in_shader_active {
            drawables()
                .filter(|d| match d {
                    DrawableInfo::Part { .. } => is_dst_in_shader(d.blend_mode()),
                    DrawableInfo::Composite { mask_sources, .. } => {
                        is_dst_in_shader(d.blend_mode()) && !mask_sources.is_empty()
                    }
                })
                .count()
        } else {
            0
        };
        let uniform_slot_count = lists
            .iter()
            .map(|list| {
                let child_parts: usize = list.composite_children.values().map(|v| v.len()).sum();
                list.root_drawables.len()
                    + child_parts
                    + list.total_mask_source_count()
                    + list
                        .composite_mask_sources
                        .values()
                        .map(|source| source.parts.len())
                        .sum::<usize>()
            })
            .sum::<usize>()
            + dst_in_shader_extra;
        self.begin_frame_uniforms(uniform_slot_count as u32, continues);

        let mut result = Ok(());
        for (i, list) in lists.iter().enumerate() {
            self.deform_recording_set = list.deform_set;
            self.deform_recording_base = self.deform_base(list.deform_set);
            result = self.record_list(
                list,
                encoder,
                view,
                stencil,
                composites,
                target_color_texture,
                snapshots.as_deref_mut(),
                width,
                height,
                // The clear belongs to the frame, not to a puppet: a later
                // list clearing would wipe the ones already recorded.
                if i == 0 { clear_color } else { None },
                dst_in_shader_active,
            );
            // Each list resets the pool, so the frame's peak is the deepest list's.
            self.current_stats.composite_slots_used = self
                .current_stats
                .composite_slots_used
                .max(composites.stats().0 as u32);
            if result.is_err() {
                break;
            }
        }
        self.deform_recording_set = DeformSet::FIRST;
        self.deform_recording_base = 0;

        if let (Some(profiler), Some(query)) = (self.gpu_profiler.as_ref(), gpu_query) {
            profiler.end_query(encoder, query);
        }
        self.flush_instance_writes();
        self.flush_part_uniform_writes();
        self.finalize_frame_stats();
        self.pending_deform_bytes = 0;
        span.record("drawn_parts", self.current_stats.drawn_parts);
        self.frame_sizing_closed = false;
        result.map(|()| self.current_stats)
    }

    /// Frame-level state belongs to [`Self::record_frame`], so this may run
    /// several times per frame.
    #[allow(clippy::too_many_arguments, clippy::expect_used)]
    fn record_list(
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
        dst_in_shader_active: bool,
    ) -> RendererResult<()> {
        use crate::collect::DrawableInfo;

        let span = tracing::trace_span!("render_list", drawn_parts = tracing::field::Empty);
        let _entered = span.enter();

        composites.ensure_size(width, height);
        composites.reset();
        self.composite_bounds.clear();
        let mut snapshots_opt = if dst_in_shader_active {
            snapshots
        } else {
            None
        };
        if let Some(s) = snapshots_opt.as_mut() {
            s.ensure_size(width, height);
            s.reset();
        }
        self.prepare_composite_mask_textures(encoder, render_list, composites, width, height);

        // Root-composite flattening: OVER is associative, so an identity-blit
        // root composite whose children all blend OVER draws them straight to the
        // main view. Nested composites, non-OVER children, and dst-in-shader
        // children on the snapshot path read the destination and block it.
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
                let blocks_flattening = match c {
                    DrawableInfo::Composite { .. } => true,
                    DrawableInfo::Part { blend_mode, .. } => {
                        !renders_as_over(*blend_mode)
                            || (dst_in_shader_active && is_dst_in_shader(*blend_mode))
                    }
                };
                if blocks_flattening
                    && self.drawable_screen_bounds(c, render_list, width, height)
                        != ScreenBounds::Empty
                {
                    return None;
                }
            }
            Some(children.as_slice())
        })();

        let has_stencil = self.shared.has_stencil;

        // Alpha path: clear up front unless the first drawable is an unmasked
        // part batch (the only LoadOp::Clear consumer) or flattening applies;
        // snapshots would otherwise read uninitialized memory on frame 0. A None
        // clear color preserves the caller's content (bevy).
        let needs_explicit_clear = !has_stencil
            && flatten_children.is_none()
            && clear_color.is_some()
            && render_list
                .root_drawables
                .iter()
                .find(|d| {
                    self.drawable_screen_bounds(d, render_list, width, height)
                        != ScreenBounds::Empty
                })
                .is_some_and(|d| match d {
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

        // If every child filtered out, the fallback below still clears.
        if let Some(children) = flatten_children {
            self.render_children_run(encoder, &mut sink, children, width, height)?;
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
            span.record("drawn_parts", self.current_stats.drawn_parts);
            return Ok(());
        }

        // Dst-in-shader parts, composites, and snapshots can't share a peer's
        // pass, so they flush pending runs first to preserve z-order.
        let mut pending_parts: Vec<PreparedPartDraw> = Vec::new();
        // Mutually exclusive with `pending_parts`, preserving z-order.
        let mut pending_masked: Vec<PreparedMaskedPartDraw> = Vec::new();

        for drawable in &render_list.root_drawables {
            let bounds = self.drawable_screen_bounds(drawable, render_list, width, height);
            if bounds == ScreenBounds::Empty {
                continue;
            }
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
                        // Barrier: the snapshot must see pending parts.
                        self.flush_pending_parts(encoder, &mut sink, &mut pending_parts)?;
                        self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
                        self.end_sink_pass(encoder, &mut sink);
                        self.spend_pending_alpha_clear(
                            encoder,
                            &mut sink,
                            "Root Parts Clear (barrier)",
                        );
                        // One rect bounds the slot render and the snapshot copy+blit; the shader
                        // returns dst outside the part's content.
                        let part_scissor = bounds.scissor();
                        // Release both slots after the blit so the pools don't grow per part.
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
                        // A None clear color preserves existing content (bevy already cleared
                        // the ViewTarget).
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
                    ref mask_sources, ..
                } => {
                    let _comp_span = tracing::trace_span!(
                        "render_composite",
                        masked = !mask_sources.is_empty(),
                    )
                    .entered();
                    self.flush_pending_parts(encoder, &mut sink, &mut pending_parts)?;
                    self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
                    self.end_sink_pass(encoder, &mut sink);
                    self.spend_pending_alpha_clear(
                        encoder,
                        &mut sink,
                        "Root Composite Clear (barrier)",
                    );

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

        self.flush_pending_parts(encoder, &mut sink, &mut pending_parts)?;
        self.flush_pending_masked(encoder, &mut sink, &mut pending_masked)?;
        self.end_sink_pass(encoder, &mut sink);

        // Alpha sink: nothing consumed the clear, so the target still holds the
        // previous frame.
        if !has_stencil && !has_rendered_first {
            if let Some(color) = clear_color {
                self.explicit_clear_pass(encoder, view, color, "render_list fallback clear");
            }
        }

        span.record("drawn_parts", self.current_stats.drawn_parts);
        Ok(())
    }

    /// Must run while the encoder is open and before it is submitted; pair
    /// with `end_gpu_frame` after. [`Recorded::submit`] does both itself.
    pub fn resolve_gpu_queries(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(profiler) = self.gpu_profiler.as_mut() {
            profiler.resolve_queries(encoder);
        }
    }

    /// Must run after the submit carrying `resolve_gpu_queries`: it maps the
    /// read buffer, and a submit still writing a mapped buffer fails validation.
    pub fn end_gpu_frame(&mut self) {
        if let Some(profiler) = self.gpu_profiler.as_mut() {
            if let Err(e) = profiler.end_frame() {
                tracing::warn!("wgpu_profiler::end_frame: {e}");
            }
        }
    }

    /// `None` when timestamp queries are unsupported or no frame completed yet.
    pub fn process_gpu_frame(&mut self) -> Option<Vec<wgpu_profiler::GpuTimerQueryResult>> {
        let profiler = self.gpu_profiler.as_mut()?;
        let period = self.queue.get_timestamp_period();
        profiler.process_finished_frame(period)
    }

    // At most one pending run is non-empty at a time, preserving z-order.
    // The caller owns the sink's end-of-run handling.
    fn render_children_run(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        sink: &mut PartSink<'_, '_>,
        children: &[crate::collect::DrawableInfo],
        width: u32,
        height: u32,
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
            if self.part_screen_bounds(*mesh_id, *transform, width, height) == ScreenBounds::Empty {
                continue;
            }
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

    /// Alpha sink only: a pending clear must land before a barrier reads the
    /// target.
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

    /// Serves root and nested composites identically. The caller must have
    /// flushed its pending runs and ended the sink's pass.
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

        // Computed before the children render so the borrow doesn't collide.
        let bounds = self.drawable_screen_bounds(drawable, render_list, width, height);
        if bounds == ScreenBounds::Empty {
            return Ok(false);
        }
        let union = bounds.scissor();

        let part_uniform_offset =
            self.write_part_uniform(*opacity, *tint, *screen_tint, *mask_threshold);

        // Rewinding after the blit lets siblings share slots; the pool grows to
        // the nesting depth and no further.
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

        // Without a texture or pool, fall through to the Normal-OVER blit.
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

        // The first drawing pass folds in the slot clear; the fallback clear
        // keeps the blit from sampling stale pool contents.
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

    /// Splits at children that read the composite's own buffer (dst-in-shader
    /// parts, nested composites); each records its own passes between runs.
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
            return self.render_children_run(encoder, sink, children, width, height);
        }

        let mut run_start = 0;
        for (i, child) in children.iter().enumerate() {
            if !is_barrier(child) {
                continue;
            }
            let bounds = self.drawable_screen_bounds(child, render_list, width, height);
            if bounds == ScreenBounds::Empty {
                continue;
            }

            self.render_children_run(encoder, sink, &children[run_start..i], width, height)?;
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
                    // Scratch slot and snapshot are re-acquired per child at one cursor;
                    // encoder order makes overwriting them safe.
                    let child_scissor = bounds.scissor();
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

        self.render_children_run(encoder, sink, &children[run_start..], width, height)
    }
}

/// A frame the renderer owns end to end: it made the encoder, and it will
/// make the submit.
///
/// Recording is the one thing a `Frame` does, and it does it once:
/// [`Self::render`] / [`Self::render_ext`] take `self` by value and hand back
/// a [`Recorded`]. A frame that errs is consumed with nothing submitted.
#[must_use = "a frame draws nothing until render() records it"]
pub struct Frame<'r> {
    renderer: &'r mut WgpuRenderer,
    encoder: wgpu::CommandEncoder,
}

impl<'r> Frame<'r> {
    /// See [`Self::render_ext`] for the dst-in-shader blend modes.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        self,
        lists: &[&crate::collect::RenderList],
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<Recorded<'r>> {
        self.render_ext(
            lists,
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

    /// Without the target texture and snapshot pool, `Overlay`, `ColorBurn`
    /// and `LinearBurn` fall back to Normal-OVER.
    #[allow(clippy::too_many_arguments)]
    pub fn render_ext(
        mut self,
        lists: &[&crate::collect::RenderList],
        view: &wgpu::TextureView,
        stencil: &StencilTarget,
        composites: &mut CompositePool,
        target_color_texture: Option<&wgpu::Texture>,
        snapshots: Option<&mut FramebufferSnapshotPool>,
        width: u32,
        height: u32,
        clear_color: Option<wgpu::Color>,
    ) -> RendererResult<Recorded<'r>> {
        let stats = self.renderer.record_frame(
            0,
            lists,
            &mut self.encoder,
            view,
            stencil,
            composites,
            target_color_texture,
            snapshots,
            width,
            height,
            clear_color,
        )?;
        Ok(Recorded {
            renderer: self.renderer,
            encoder: Some(self.encoder),
            stats,
        })
    }
}

/// A recorded frame, waiting for its submit. Add caller work through
/// [`Self::encoder`]; a second render call would reset the spent cursors.
#[must_use = "a recorded frame reaches the queue only through submit()"]
pub struct Recorded<'r> {
    renderer: &'r mut WgpuRenderer,
    encoder: Option<wgpu::CommandEncoder>,
    stats: RenderStats,
}

impl Recorded<'_> {
    pub fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        // A live `Recorded` always has one; the fallback only satisfies the type.
        self.encoder.get_or_insert_with(|| {
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("catchlight frame"),
                })
        })
    }

    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    /// Resolves timer queries into this encoder before submitting, then closes
    /// the profiler frame.
    pub fn submit(mut self) -> wgpu::SubmissionIndex {
        let mut encoder = self.encoder.take();
        if let Some(encoder) = encoder.as_mut() {
            self.renderer.resolve_gpu_queries(encoder);
        }
        let index = self
            .renderer
            .queue
            .submit(encoder.map(wgpu::CommandEncoder::finish));
        self.renderer.end_gpu_frame();
        index
    }
}

impl Drop for Recorded<'_> {
    fn drop(&mut self) {
        if self.encoder.is_some() {
            // Once per process: a caller that drops one frame drops every frame.
            static DROPPED_WARNED: std::sync::Once = std::sync::Once::new();
            DROPPED_WARNED.call_once(|| {
                tracing::warn!(
                    "a recorded catchlight frame was dropped without submit(); \
                     nothing it drew reached the queue",
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        blend_mode_to_wgpu, blend_transparent_src_is_identity, camera_slot_offset,
        pixels_to_scissor, project_aabb_to_pixels, renders_as_over, same_mask_signature, Aabb2,
        ScreenRect, CAMERA_RING_SLOTS,
    };
    use catchlight_core::BlendMode;

    #[test]
    fn camera_slot_offsets_stay_inside_the_ring() {
        let stride = 256;
        assert_eq!(camera_slot_offset(0, stride), 0);
        let offsets: Vec<u64> = (0..CAMERA_RING_SLOTS)
            .map(|slot| camera_slot_offset(slot, stride))
            .collect();
        for pair in offsets.windows(2) {
            assert_eq!(pair[1] - pair[0], stride, "slots must be one stride apart");
        }
        assert_eq!(
            offsets[CAMERA_RING_SLOTS as usize - 1] + stride,
            stride * u64::from(CAMERA_RING_SLOTS),
            "the last slot must end at the end of the camera buffer",
        );
    }

    /// A blend-table edit that made a mode look like OVER would make
    /// flattening silently wrong.
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

        assert!(same_mask_signature(&a, &[src(1, 1, MaskMode::Mask)]));
        assert!(!same_mask_signature(
            &a,
            &[src_t(1, 1, MaskMode::Mask, 0.25)]
        ));
        assert!(!same_mask_signature(
            &a,
            &[src(1, 1, MaskMode::Mask), src(2, 2, MaskMode::Mask)]
        ));
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

    /// Alpha falling through to OVER writes α=1 where color is 0: black halos.
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
            let _ = blend_mode_to_wgpu(mode);
        }
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

    /// Without One factors, Min/Max would compare scaled values and Subtract
    /// would drop terms.
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
        assert!(!blend_transparent_src_is_identity(BlendMode::Darken));
    }

    /// Half-unit box, identity, 100×100: [25,75]² padded outward to [23,77]².
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
        let off = [200.0, 200.0, 300.0, 300.0];
        assert_eq!(pixels_to_scissor(off, 100, 100), None);

        // Projecting only corners is not conservative across the eye plane.
        let mut crossing = glam::Mat4::IDENTITY;
        crossing.x_axis.w = 4.0;
        assert_eq!(project_aabb_to_pixels(local, crossing, 100.0, 100.0), None);
    }

    #[test]
    fn nested_composites_keep_tight_bounds_without_clipping_darken() {
        use crate::{DrawableInfo, RenderContext, RenderList};
        let mut ctx = pollster::block_on(RenderContext::new(100, 100)).expect("headless context");
        ctx.renderer
            .upload_mesh(0, &catchlight_core::Mesh::quad(1.0, 1.0))
            .expect("mesh");
        ctx.renderer
            .upload_texture(
                0,
                &catchlight_core::DecodedTexture {
                    width: 4,
                    height: 4,
                    rgba: vec![255; 64].into(),
                },
            )
            .expect("texture");
        let composite = |node_id| DrawableInfo::Composite {
            node_id,
            z_order: 0.0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            tint: glam::Vec3::ONE,
            screen_tint: glam::Vec3::ZERO,
            mask_sources: Default::default(),
            mask_threshold: 0.5,
        };
        let part = |mesh_id| DrawableInfo::Part {
            mesh_id,
            texture_id: 0,
            transform: glam::Mat4::IDENTITY,
            z_order: 0.0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            tint: glam::Vec3::ONE,
            screen_tint: glam::Vec3::ZERO,
            mask_sources: Default::default(),
            mask_threshold: 0.5,
        };
        let mut list = RenderList::default();
        list.root_drawables.push(composite(1));
        list.composite_children
            .insert(1, vec![composite(2), composite(3)]);
        list.composite_children.insert(2, vec![part(0)]);
        list.composite_children.insert(3, Vec::new());
        ctx.render(&list, Some(wgpu::Color::WHITE)).expect("render");
        assert_eq!(
            ctx.renderer.composite_bounds.get(&1),
            Some(&Some([25.0, 25.0, 75.0, 75.0]))
        );

        ctx.renderer
            .update_camera(glam::Mat4::from_translation(glam::vec3(0.25, 0.0, 0.0)));
        ctx.render(&list, Some(wgpu::Color::WHITE))
            .expect("moved camera");
        assert_eq!(
            ctx.renderer.composite_bounds.get(&1),
            Some(&Some([37.5, 25.0, 87.5, 75.0]))
        );

        if let DrawableInfo::Composite { blend_mode, .. } = &mut list.root_drawables[0] {
            *blend_mode = BlendMode::Darken;
        }
        ctx.render(&list, Some(wgpu::Color::WHITE)).expect("darken");
        let pixels = ctx.read_rgba().expect("pixels");
        assert_eq!(
            &pixels[..4],
            &[0, 0, 0, 0],
            "Darken affects pixels outside the nested bounds"
        );
        assert_eq!(&pixels[(50 * 100 + 50) * 4..][..4], &[255; 4]);

        list.composite_children
            .get_mut(&2)
            .expect("children")
            .push(part(99));
        ctx.render(&list, Some(wgpu::Color::WHITE))
            .expect("unknown bounds");
        assert_eq!(ctx.renderer.composite_bounds.get(&1), Some(&None));
    }

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
