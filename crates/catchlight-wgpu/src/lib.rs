//! The wgpu rendering backend: it turns a posed [`catchlight_core::Puppet`]
//! into pixels.
//!
//! A renderer holds exactly one [`RenderCache`]; that cache serves every
//! puppet of its model.
//!
//! ```no_run
//! use catchlight_core::{load_model, ModelFormat, Pose, Puppet};
//! use catchlight_wgpu::{collect, Framing, PrepareOptions, RenderCache, RenderContext};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let bytes = std::fs::read("model.clm")?;
//! let model = load_model(&bytes, ModelFormat::Clm)?;
//! let mut ctx = pollster::block_on(RenderContext::new(960, 1600))?;
//! let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())?;
//! let mut puppet = Puppet::new(&model);
//!
//! puppet.apply_pose(&Pose::new());
//! puppet.tick(&model, 1.0 / 60.0);
//! cache.refresh(&mut ctx.renderer, &model, &puppet)?;
//! let list = collect(&cache, &puppet);
//! let pixels = ctx.render_rgba(&list, Framing::centered(5000.0), 960, 1600, None)?;
//! # let _ = pixels;
//! # Ok(())
//! # }
//! ```

mod collect;
mod gpu;
mod headless;
mod render_cache;
mod renderer;

pub use collect::{
    CompositeMaskPartData, CompositeMaskSourceData, DrawableInfo, MaskSourceData, MaskSources,
    RenderList,
};
pub use gpu::{Acquired, GpuContext, SurfaceContext};
pub use headless::{Framing, RenderContext};
pub use render_cache::{CacheStats, PrepareOptions, RenderCache, collect};
pub use renderer::{
    CAMERA_RING_SLOTS, DeformSet, Frame, FrameStats, FrameTargets, PipelineOptions, Pipelines,
    Recorded, RenderStats, RenderTarget, RendererError, RendererResult, StencilMode, Submission,
    WgpuRenderer,
};

#[must_use]
pub fn create_orthographic_camera(camera_height: f32, aspect: f32) -> glam::Mat4 {
    create_orthographic_camera_at(camera_height, aspect, glam::Vec2::ZERO)
}

/// `camera_height` is in world units. World space is Y-up and the camera
/// holds no axis flip.
#[must_use]
pub fn create_orthographic_camera_at(
    camera_height: f32,
    aspect: f32,
    center: glam::Vec2,
) -> glam::Mat4 {
    let camera_width = camera_height * aspect;
    glam::Mat4::orthographic_rh(
        center.x - camera_width / 2.0,
        center.x + camera_width / 2.0,
        center.y - camera_height / 2.0,
        center.y + camera_height / 2.0,
        -1000.0,
        1000.0,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadbackLayout {
    unpadded_bytes_per_row: usize,
    padded_bytes_per_row: u32,
    pub(crate) buffer_size: u64,
}

/// `None` for an empty texture or one whose rows overflow the copy layout or
/// the address space.
pub(crate) fn readback_layout(width: u32, height: u32) -> Option<ReadbackLayout> {
    if width == 0 || height == 0 {
        return None;
    }
    let unpadded = width.checked_mul(4)?;
    let padded = unpadded.checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)?;
    let buffer_size = u64::from(padded) * u64::from(height);
    usize::try_from(buffer_size).ok()?;
    Some(ReadbackLayout {
        unpadded_bytes_per_row: usize::try_from(unpadded).ok()?,
        padded_bytes_per_row: padded,
        buffer_size,
    })
}

/// How a texture's rows read back as RGBA8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Readback {
    layout: ReadbackLayout,
    swap_red_blue: bool,
}

impl Readback {
    pub(crate) fn of(texture: &wgpu::Texture) -> RendererResult<Self> {
        use wgpu::TextureFormat as F;
        let swap_red_blue = match texture.format() {
            F::Rgba8Unorm | F::Rgba8UnormSrgb => false,
            F::Bgra8Unorm | F::Bgra8UnormSrgb => true,
            other => return Err(RendererError::ReadbackFormat(other)),
        };
        let (width, height) = (texture.width(), texture.height());
        let layout =
            readback_layout(width, height).ok_or(RendererError::ReadbackSize { width, height })?;
        Ok(Self {
            layout,
            swap_red_blue,
        })
    }

    pub(crate) const fn buffer_size(&self) -> u64 {
        self.layout.buffer_size
    }
}

pub(crate) fn create_readback_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    })
}

/// Mip 0 of an RGBA8 or BGRA8 `texture` carrying `COPY_SRC`, as tightly
/// packed RGBA8.
pub async fn read_texture_to_rgba(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> RendererResult<Vec<u8>> {
    let readback = Readback::of(texture)?;
    let buffer = create_readback_buffer(device, readback.buffer_size());
    let mut pixels = Vec::new();
    read_texture_into(device, queue, texture, readback, &buffer, &mut pixels).await?;
    Ok(pixels)
}

/// Replaces `out` with the pixels; `buffer` holds at least
/// `readback.buffer_size()` bytes and is unmapped.
pub(crate) async fn read_texture_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    readback: Readback,
    buffer: &wgpu::Buffer,
    out: &mut Vec<u8>,
) -> RendererResult<()> {
    let layout = readback.layout;
    let (width, height) = (texture.width(), texture.height());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Readback Encoder"),
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(layout.padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(std::iter::once(encoder.finish()));

    let buffer_slice = buffer.slice(..layout.buffer_size);
    let (sender, receiver) = futures_channel::oneshot::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    #[cfg(not(target_arch = "wasm32"))]
    device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: None,
    })?;
    #[cfg(target_arch = "wasm32")]
    let _ = submission;
    receiver
        .await
        .map_err(|_| RendererError::ReadbackAbandoned)??;

    let data = buffer_slice.get_mapped_range();
    out.clear();
    out.reserve(layout.unpadded_bytes_per_row * height as usize);
    for row in data.chunks(layout.padded_bytes_per_row as usize) {
        let row = &row[..layout.unpadded_bytes_per_row];
        if readback.swap_red_blue {
            for [b, g, r, a] in row.as_chunks::<4>().0 {
                out.extend_from_slice(&[*r, *g, *b, *a]);
            }
        } else {
            out.extend_from_slice(row);
        }
    }
    drop(data);
    buffer.unmap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_layout_checks_dimensions_and_alignment() {
        let layout = readback_layout(3, 2).unwrap();
        assert_eq!(layout.unpadded_bytes_per_row, 12);
        assert_eq!(layout.padded_bytes_per_row, 256);
        assert_eq!(layout.buffer_size, 512);

        assert!(readback_layout(0, 1).is_none());
        assert!(readback_layout(1, 0).is_none());
        assert!(readback_layout(u32::MAX, 1).is_none());
        assert!(readback_layout(u32::MAX / 4, 1).is_none());
    }
}
