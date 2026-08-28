#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end blend-math tests. Overlay / ColorBurn / LinearBurn are
//! dst-in-shader modes (the shader reads the framebuffer snapshot and
//! computes the blend per-channel). Inverse is *not* — it's a plain
//! fixed-function blend (`OneMinusDst` / `OneMinusSrcAlpha`), included here
//! only as the fixed-function counter-case.
//! Each test builds a synthetic single-Part puppet on a known
//! background, runs it through `render_list_ext`, and checks a center
//! pixel against the math computed in linear color space (matching the
//! shader, which samples sRGB textures as linear).

use catchlight_core::{
    BlendMode, GlobalTransforms, Mesh, Node, NodeKind, PartData, Puppet, PuppetTexture, TextureId,
    Vec3,
};
use catchlight_wgpu::{
    collect_drawables, create_headless_context, FramebufferSnapshotPool, WgpuRenderer,
};
use std::sync::Arc;

const W: u32 = 8;
const H: u32 = 8;

const NO_ADAPTER: &str =
    "no Vulkan adapter for the headless context; see AGENTS.md, \"Native headless rendering\"";

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb_u8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

fn solid_texture(r: u8, g: u8, b: u8, a: u8) -> PuppetTexture {
    let rgba: Vec<u8> = (0..16).flat_map(|_| [r, g, b, a]).collect();
    PuppetTexture {
        width: 4,
        height: 4,
        rgba: Arc::from(rgba),
    }
}

fn make_puppet(blend: BlendMode, src_rgba: [u8; 4]) -> Puppet {
    // One Part covering the full viewport. The mesh is a quad in world
    // units that matches the orthographic camera below; uvs span [0,1]
    // so the Part samples its solid-color texture at every fragment.
    let mut puppet = Puppet::new();
    puppet.set_textures(vec![solid_texture(
        src_rgba[0],
        src_rgba[1],
        src_rgba[2],
        src_rgba[3],
    )]);
    let mesh = Mesh::quad(2.0, 2.0); // matches orthographic [-1,1] world bounds
    let mut part = PartData {
        mesh,
        blend_mode: blend,
        ..Default::default()
    };
    part.opacity = 1.0;
    part.tint = Vec3::ONE;
    part.screen_tint = Vec3::ZERO;
    let part_node = Node {
        kind: NodeKind::Part(Box::new(part)),
        ..Default::default()
    };
    let root = puppet.root();
    puppet.insert_child(root, part_node, None);
    puppet
}

async fn render_blend(blend: BlendMode, bg_rgba_u8: [u8; 4], src_rgba_u8: [u8; 4]) -> Vec<u8> {
    render_puppet(&make_puppet(blend, src_rgba_u8), bg_rgba_u8).await
}

/// Render any puppet through `render_list_ext` (snapshot pool wired)
/// onto a solid background and read back the pixels.
async fn render_puppet(puppet: &Puppet, bg_rgba_u8: [u8; 4]) -> Vec<u8> {
    let (device, queue) = create_headless_context().await.expect(NO_ADAPTER);
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dst-in-shader-test target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut renderer = WgpuRenderer::new(device, queue, format).await;
    renderer.upload_puppet(puppet).expect("upload");

    // Orthographic camera matching the quad's [-1,1] world bounds, so
    // the part covers the entire framebuffer.
    let view_proj = glam::Mat4::orthographic_rh(-1.0, 1.0, 1.0, -1.0, -1000.0, 1000.0);
    renderer.update_camera(view_proj);
    renderer.sync_deforms(puppet);

    let mut transforms = GlobalTransforms::new();
    puppet.compute_transforms(&mut transforms);
    let render_list = collect_drawables(puppet, &transforms);
    let stencil =
        catchlight_wgpu::StencilTarget::new_for_pipelines(&renderer.shared, &renderer.device, W, H);
    let mut composites = catchlight_wgpu::CompositePool::new(W, H);
    let mut snapshots = FramebufferSnapshotPool::new(W, H);

    let mut encoder = renderer
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    renderer.begin_camera_submit();

    // wgpu/WebGPU clear color values are interpreted as **linear-space**
    // floats: when the attachment is sRGB the value is sRGB-encoded on
    // store. So the bg_rgba_u8 byte triple — which is the desired sRGB
    // pixel — must first be sRGB→linear converted before being passed
    // as the clear color (otherwise wgpu re-encodes a value already in
    // sRGB space and the final byte is brighter than intended).
    let clear = wgpu::Color {
        r: srgb_to_linear(bg_rgba_u8[0] as f32 / 255.0) as f64,
        g: srgb_to_linear(bg_rgba_u8[1] as f32 / 255.0) as f64,
        b: srgb_to_linear(bg_rgba_u8[2] as f32 / 255.0) as f64,
        a: bg_rgba_u8[3] as f64 / 255.0,
    };
    renderer
        .render_list_ext(
            &render_list,
            &mut encoder,
            &view,
            &stencil,
            &mut composites,
            Some(&texture),
            Some(&mut snapshots),
            W,
            H,
            Some(clear),
        )
        .expect("render");
    renderer.queue.submit(std::iter::once(encoder.finish()));

    catchlight_wgpu::read_texture_to_rgba(&renderer.device, &renderer.queue, &texture, W, H)
        .await
        .expect("readback")
}

/// Pad (gray, x in [-0.5, 0.5]) behind a full-viewport ColorBurn quad,
/// both children of one Normal Composite. Higher z_order draws last
/// (in front), so the burn part blends against the pad where they overlap
/// and against the composite's transparent clear elsewhere.
fn make_nested_burn_puppet(pad_rgba: [u8; 4], src_rgba: [u8; 4]) -> Puppet {
    let mut puppet = Puppet::new();
    puppet.set_textures(vec![
        solid_texture(pad_rgba[0], pad_rgba[1], pad_rgba[2], pad_rgba[3]),
        solid_texture(src_rgba[0], src_rgba[1], src_rgba[2], src_rgba[3]),
    ]);
    let root = puppet.root();
    let comp_id = puppet.insert_child(
        root,
        Node {
            kind: NodeKind::Composite(Box::default()),
            ..Default::default()
        },
        None,
    );
    let mut pad = PartData {
        mesh: Mesh::quad(1.0, 2.0),
        ..Default::default()
    };
    pad.opacity = 1.0;
    pad.tint = Vec3::ONE;
    pad.screen_tint = Vec3::ZERO;
    puppet.insert_child(
        comp_id,
        Node {
            kind: NodeKind::Part(Box::new(pad)),
            ..Default::default()
        },
        None,
    );
    let mut burn = PartData {
        mesh: Mesh::quad(2.0, 2.0),
        blend_mode: BlendMode::ColorBurn,
        ..Default::default()
    };
    burn.albedo_texture = TextureId(1);
    burn.opacity = 1.0;
    burn.tint = Vec3::ONE;
    burn.screen_tint = Vec3::ZERO;
    puppet.insert_child(
        comp_id,
        Node {
            kind: NodeKind::Part(Box::new(burn)),
            z_order: 1.0,
            base_z_order: 1.0,
            ..Default::default()
        },
        None,
    );
    puppet
}

fn pixel_at(buf: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * W as usize + x) * 4;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn center_pixel(buf: &[u8]) -> [u8; 4] {
    pixel_at(buf, (W / 2) as usize, (H / 2) as usize)
}

fn approx_eq(a: u8, b: u8, tol: u8) -> bool {
    a.abs_diff(b) <= tol
}

fn assert_pixel_close(actual: [u8; 4], expected_linear: [f32; 3], label: &str) {
    let exp = [
        linear_to_srgb_u8(expected_linear[0]),
        linear_to_srgb_u8(expected_linear[1]),
        linear_to_srgb_u8(expected_linear[2]),
    ];
    // Allow ±2 LSB for precision drift through u8 round-trips and the
    // shader's per-channel math.
    let ok = approx_eq(actual[0], exp[0], 2)
        && approx_eq(actual[1], exp[1], 2)
        && approx_eq(actual[2], exp[2], 2);
    assert!(
        ok,
        "{label}: actual={:?}, expected={:?} (linear {:?})",
        actual, exp, expected_linear,
    );
}

#[test]
fn overlay_blend_runs_dst_in_shader_math() {
    // Background gray (sRGB 128/255 ≈ 0.502). Src red (sRGB 204/255 ≈
    // 0.8). Overlay with d ≥ 0.5 takes the high branch:
    //   out = 1 - 2*(1-s)*(1-d), per channel.
    let bg = [128u8, 128, 128, 255];
    let src = [204u8, 76, 76, 255]; // sRGB ~ (0.8, 0.3, 0.3)

    let pixels = pollster::block_on(render_blend(BlendMode::Overlay, bg, src));
    let center = center_pixel(&pixels);

    let s = [
        srgb_to_linear(204.0 / 255.0),
        srgb_to_linear(76.0 / 255.0),
        srgb_to_linear(76.0 / 255.0),
    ];
    let d = [
        srgb_to_linear(128.0 / 255.0),
        srgb_to_linear(128.0 / 255.0),
        srgb_to_linear(128.0 / 255.0),
    ];
    let mut blend = [0.0f32; 3];
    for c in 0..3 {
        blend[c] = if d[c] < 0.5 {
            2.0 * s[c] * d[c]
        } else {
            1.0 - 2.0 * (1.0 - s[c]) * (1.0 - d[c])
        }
        .clamp(0.0, 1.0);
    }

    assert_pixel_close(center, blend, "Overlay center pixel");
}

#[test]
fn linear_burn_blend_runs_dst_in_shader_math() {
    // LinearBurn: src + dst - 1, clamped. Pick values where the result
    // doesn't underflow to zero.
    let bg = [200u8, 200, 200, 255];
    let src = [200u8, 100, 200, 255];

    let pixels = pollster::block_on(render_blend(BlendMode::LinearBurn, bg, src));
    let center = center_pixel(&pixels);

    let s = [
        srgb_to_linear(200.0 / 255.0),
        srgb_to_linear(100.0 / 255.0),
        srgb_to_linear(200.0 / 255.0),
    ];
    let d = [
        srgb_to_linear(200.0 / 255.0),
        srgb_to_linear(200.0 / 255.0),
        srgb_to_linear(200.0 / 255.0),
    ];
    let mut blend = [0.0f32; 3];
    for c in 0..3 {
        blend[c] = (s[c] + d[c] - 1.0).clamp(0.0, 1.0);
    }

    assert_pixel_close(center, blend, "LinearBurn center pixel");
}

#[test]
fn inverse_blend_runs_fixed_function_math() {
    // Inverse is fixed-function, NOT dst-in-shader: color and alpha both
    // blend src*OneMinusDst + dst*OneMinusSrcAlpha.
    // For an opaque part (src.a=1) over an opaque bg (dst.a=1):
    //   color = src*(1-dst) + dst*(1-1) = src*(1-dst)
    //   alpha = 1*(1-1) + 1*(1-1)       = 0
    // The alpha=0 is the reference-faithful result, not a bug — it's the
    // same OneMinusSrcAlpha family as the masking blends (AGENTS.md), here
    // with color and alpha agreeing on factors.
    let bg = [100u8, 100, 100, 255];
    let src = [50u8, 200, 100, 255];

    let pixels = pollster::block_on(render_blend(BlendMode::Inverse, bg, src));
    let center = center_pixel(&pixels);

    let s = [
        srgb_to_linear(50.0 / 255.0),
        srgb_to_linear(200.0 / 255.0),
        srgb_to_linear(100.0 / 255.0),
    ];
    let d = [
        srgb_to_linear(100.0 / 255.0),
        srgb_to_linear(100.0 / 255.0),
        srgb_to_linear(100.0 / 255.0),
    ];
    let mut blend = [0.0f32; 3];
    for c in 0..3 {
        blend[c] = (s[c] * (1.0 - d[c])).clamp(0.0, 1.0);
    }

    assert_pixel_close(center, blend, "Inverse center pixel");
    assert_eq!(
        center[3], 0,
        "Inverse alpha: OneMinusSrcAlpha gives 0 over opaque bg"
    );
}

#[test]
fn color_burn_inside_composite_blends_against_composite_content() {
    // A dst-in-shader Part nested in a Composite must blend against the
    // composite's own transparent-cleared buffer, never against the
    // framebuffer behind the composite.
    // Over the pad the ColorBurn math applies; past the pad the
    // composite is transparent, so KHR degrades to plain src — not
    // colorburn(src, background).
    let bg = [100u8, 100, 200, 255];
    let pad = [200u8, 200, 200, 255];
    let src = [230u8, 180, 150, 255];

    let puppet = make_nested_burn_puppet(pad, src);
    let pixels = pollster::block_on(render_puppet(&puppet, bg));

    let s = [
        srgb_to_linear(230.0 / 255.0),
        srgb_to_linear(180.0 / 255.0),
        srgb_to_linear(150.0 / 255.0),
    ];
    let d = srgb_to_linear(200.0 / 255.0);

    // Center pixel sits over the pad: 1 - (1 - d) / s, per channel
    // (r and g land mid-range; b clamps to 0).
    let mut blend = [0.0f32; 3];
    for c in 0..3 {
        blend[c] = (1.0 - (1.0 - d) / s[c]).clamp(0.0, 1.0);
    }
    assert_pixel_close(center_pixel(&pixels), blend, "nested ColorBurn over pad");

    // x=6 (world x = 0.625) is past the pad: the composite holds plain
    // src there, and the Normal composite blit lays it over the
    // background unchanged. Matching the raw src color pins that the
    // child blended against the composite buffer, not the main view.
    assert_pixel_close(
        pixel_at(&pixels, 6, (H / 2) as usize),
        s,
        "nested ColorBurn over transparent composite region",
    );
}

#[test]
fn overlay_with_normal_fallback_differs_from_overlay_with_snapshot() {
    // Sanity check that the dst-in-shader path actually does something
    // different from the OVER-blend approximation. With OVER, an
    // opaque red Part draws solid red over the gray bg; with Overlay,
    // the result is very different. Bypass the snapshot-pool path by
    // using `render_list` (no main_color_texture) and compare.
    let bg = [128u8, 128, 128, 255];
    let src = [204u8, 76, 76, 255];

    let with_snapshot = pollster::block_on(render_blend(BlendMode::Overlay, bg, src));
    let with_snapshot_center = center_pixel(&with_snapshot);

    // Re-render via the legacy `render_list` (no snapshot pool) — that
    // path falls back to Normal OVER blend for Overlay so the part
    // simply paints its solid color.
    let puppet = make_puppet(BlendMode::Overlay, src);
    let fallback_center = pollster::block_on(async {
        let (device, queue) = create_headless_context().await.expect(NO_ADAPTER);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut renderer = WgpuRenderer::new(device, queue, format).await;
        renderer.upload_puppet(&puppet).unwrap();
        renderer.update_camera(glam::Mat4::orthographic_rh(
            -1.0, 1.0, 1.0, -1.0, -1000.0, 1000.0,
        ));
        renderer.sync_deforms(&puppet);
        let mut transforms = GlobalTransforms::new();
        puppet.compute_transforms(&mut transforms);
        let render_list = collect_drawables(&puppet, &transforms);
        let stencil = catchlight_wgpu::StencilTarget::new_for_pipelines(
            &renderer.shared,
            &renderer.device,
            W,
            H,
        );
        let mut composites = catchlight_wgpu::CompositePool::new(W, H);
        let mut encoder = renderer
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        renderer.begin_camera_submit();
        renderer
            .render_list(
                &render_list,
                &mut encoder,
                &view,
                &stencil,
                &mut composites,
                W,
                H,
                Some(wgpu::Color {
                    r: bg[0] as f64 / 255.0,
                    g: bg[1] as f64 / 255.0,
                    b: bg[2] as f64 / 255.0,
                    a: bg[3] as f64 / 255.0,
                }),
            )
            .unwrap();
        renderer.queue.submit(std::iter::once(encoder.finish()));
        let buf = catchlight_wgpu::read_texture_to_rgba(
            &renderer.device,
            &renderer.queue,
            &texture,
            W,
            H,
        )
        .await
        .expect("readback");
        center_pixel(&buf)
    });

    // The two paths must produce visibly different center pixels: the
    // dst-in-shader Overlay differs from the fallback OVER blend by at
    // least one channel beyond rounding noise.
    let max_diff = (0..3)
        .map(|c| (with_snapshot_center[c] as i32 - fallback_center[c] as i32).abs())
        .max()
        .unwrap();
    assert!(
        max_diff > 5,
        "snapshot-path center {:?} too close to fallback center {:?} (max ch diff {})",
        with_snapshot_center,
        fallback_center,
        max_diff,
    );
}
