#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use catchlight_core::{Mesh, MeshIndices, PuppetTexture, Vec2};
use catchlight_wgpu::{create_headless_context, RendererError, WgpuRenderer};

async fn make_renderer() -> WgpuRenderer {
    let (device, queue) = create_headless_context().await.expect("headless");
    WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm).await
}

#[test]
fn upload_mesh_rejects_vertex_uv_length_mismatch() {
    let mut r = pollster::block_on(make_renderer());
    let mesh = Mesh::new(
        vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
        vec![Vec2::ZERO, Vec2::ZERO], // one short
        MeshIndices::U16(vec![0, 1, 2]),
        Vec2::ZERO,
    );
    match r.upload_mesh(42, &mesh) {
        Err(RendererError::MeshVertexUvMismatch {
            mesh_id: 42,
            vertices: 3,
            uvs: 2,
        }) => {}
        other => panic!("expected MeshVertexUvMismatch, got {:?}", other),
    }
}

// Importer tolerance (matching the reference's meshdata.d): a trailing
// partial triangle is truncated and missing uvs get zeros substituted —
// neither may fail the upload, or one quirky mesh blanks the puppet.
#[test]
fn upload_mesh_tolerates_non_triangle_index_count() {
    let mut r = pollster::block_on(make_renderer());
    let mesh = Mesh::new(
        vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
        vec![Vec2::ZERO; 3],
        MeshIndices::U16(vec![0, 1, 2, 0]), // not %3: trailing index dropped
        Vec2::ZERO,
    );
    r.upload_mesh(7, &mesh)
        .expect("partial triangle must be truncated, not rejected");
}

#[test]
fn upload_mesh_tolerates_empty_uvs() {
    let mut r = pollster::block_on(make_renderer());
    let mesh = Mesh::new(
        vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
        vec![], // uvs are optional in the format
        MeshIndices::U16(vec![0, 1, 2]),
        Vec2::ZERO,
    );
    r.upload_mesh(8, &mesh)
        .expect("empty uvs must be zero-substituted, not rejected");
}

#[test]
fn upload_mesh_rejects_out_of_bounds_index() {
    let mut r = pollster::block_on(make_renderer());
    let mesh = Mesh::new(
        vec![Vec2::ZERO, Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
        vec![Vec2::ZERO; 3],
        MeshIndices::U16(vec![0, 1, 9]), // 9 >= 3
        Vec2::ZERO,
    );
    match r.upload_mesh(99, &mesh) {
        Err(RendererError::MeshIndexOutOfBounds {
            mesh_id: 99,
            index: 9,
            vertices: 3,
        }) => {}
        other => panic!("expected MeshIndexOutOfBounds, got {:?}", other),
    }
}

#[test]
fn upload_texture_rejects_zero_dimensions() {
    let mut r = pollster::block_on(make_renderer());
    let texture = PuppetTexture {
        width: 0,
        height: 1,
        rgba: Vec::new().into(),
    };

    match r.upload_texture(0, &texture) {
        Err(RendererError::TextureDimensionsOutOfRange {
            width: 0,
            height: 1,
            ..
        }) => {}
        other => panic!("expected TextureDimensionsOutOfRange, got {other:?}"),
    }
}

#[test]
fn upload_texture_rejects_device_limit_overflow() {
    let mut r = pollster::block_on(make_renderer());
    let limit = r.device.limits().max_texture_dimension_2d;
    let oversized = limit.checked_add(1).expect("finite GPU dimension limit");
    let texture = PuppetTexture {
        width: oversized,
        height: 1,
        rgba: Vec::new().into(),
    };

    match r.upload_texture(0, &texture) {
        Err(RendererError::TextureDimensionsOutOfRange {
            width,
            height: 1,
            limit: reported_limit,
        }) if width == oversized && reported_limit == limit => {}
        other => panic!("expected TextureDimensionsOutOfRange, got {other:?}"),
    }
}

#[test]
fn upload_texture_rejects_rgba_length_mismatch() {
    let mut r = pollster::block_on(make_renderer());
    let texture = PuppetTexture {
        width: 2,
        height: 2,
        rgba: vec![0; 15].into(),
    };

    match r.upload_texture(0, &texture) {
        Err(RendererError::TextureByteLengthMismatch {
            width: 2,
            height: 2,
            expected: 16,
            actual: 15,
        }) => {}
        other => panic!("expected TextureByteLengthMismatch, got {other:?}"),
    }
}
