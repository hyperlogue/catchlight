use super::*;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Vertex {
    pub(super) position: [f32; 2],
    pub(super) uv: [f32; 2],
}

impl Vertex {
    pub(super) fn desc() -> wgpu::VertexBufferLayout<'static> {
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
pub(super) struct DeformAttr {
    pub(super) deform: [f32; 2],
}

impl DeformAttr {
    pub(super) fn desc() -> wgpu::VertexBufferLayout<'static> {
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
pub(super) struct InstanceRaw {
    // z_axis is dropped: vertices enter at z = 0 and an SRT matrix keeps
    // w = 1, which the shader forces. Columns stay vec4 for 16-byte alignment.
    pub(super) col_x: [f32; 4],
    pub(super) col_y: [f32; 4],
    pub(super) col_w: [f32; 4],
}

impl InstanceRaw {
    pub(super) fn from_transform(transform: glam::Mat4) -> Self {
        let cols = transform.to_cols_array_2d();
        Self {
            col_x: cols[0],
            col_y: cols[1],
            col_w: cols[3],
        }
    }

    pub(super) fn desc() -> wgpu::VertexBufferLayout<'static> {
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

pub(super) const INSTANCE_STRIDE: u64 = std::mem::size_of::<InstanceRaw>() as u64;

pub(super) fn part_uniform_bind_group(
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
pub(super) struct CameraUniform {
    pub(super) view_proj: [[f32; 4]; 4],
}

/// Matches the piecewise curve `Rgba8UnormSrgb` applies on texture read.
pub(super) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn srgb_to_linear_vec3(c: glam::Vec3) -> glam::Vec3 {
    glam::Vec3::new(
        srgb_to_linear(c.x),
        srgb_to_linear(c.y),
        srgb_to_linear(c.z),
    )
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
