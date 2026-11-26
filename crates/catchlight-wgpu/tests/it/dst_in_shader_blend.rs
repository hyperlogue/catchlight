#![expect(clippy::expect_used, reason = "a test aborts on a broken fixture")]

//! End-to-end blend-math tests in linear color space. Overlay / `ColorBurn` /
//! `LinearBurn` are dst-in-shader; Inverse is the fixed-function counter-case.
//! Assertions read the centre pixel: alpha-cropping makes the quad's edge
//! fragments straddle the crop boundary.

use crate::common;

use catchlight_core::{BlendMode, Model, ModelComposite, ModelNodeKind};
use catchlight_wgpu::{PrepareOptions, RenderCache, WgpuRenderer, collect};

const W: u32 = 8;
const H: u32 = 8;

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

fn make_model(blend: BlendMode, src_rgba: [u8; 4]) -> Model {
    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, src_rgba));
    let root = build.root();
    build.part(&root, "quad", 0.0, common::quad(2.0, 2.0), &tex, |part| {
        part.blend_mode = blend;
    });
    build.model
}

async fn render_blend(blend: BlendMode, bg_rgba_u8: [u8; 4], src_rgba_u8: [u8; 4]) -> Vec<u8> {
    render_model(make_model(blend, src_rgba_u8), bg_rgba_u8).await
}

/// What the frame is told about the texture behind its view.
#[derive(Clone, Copy)]
enum Snapshot {
    /// The target itself, so dst-in-shader modes can copy it.
    Target,
    None,
    /// A texture without `COPY_SRC`, which cannot be copied.
    Uncopyable,
}

async fn render_model(model: Model, bg_rgba_u8: [u8; 4]) -> Vec<u8> {
    render_model_with(model, bg_rgba_u8, Snapshot::Target).await
}

async fn render_model_with(model: Model, bg_rgba_u8: [u8; 4], snapshot: Snapshot) -> Vec<u8> {
    let (device, queue) = common::shared_context();
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
    let uncopyable = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dst-in-shader-test uncopyable"),
        size: texture.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });

    let mut renderer = WgpuRenderer::new(device, queue, format);
    let mut scene = common::Scene::new(&mut renderer, model);

    let view_proj = glam::Mat4::orthographic_rh(-1.0, 1.0, 1.0, -1.0, -1000.0, 1000.0);
    renderer.update_camera(view_proj);

    let render_list = scene.frame(&mut renderer, 0.0);
    let mut targets = catchlight_wgpu::FrameTargets::new();

    // Clear colors are linear floats, sRGB-encoded on store.
    let clear = wgpu::Color {
        r: f64::from(srgb_to_linear(f32::from(bg_rgba_u8[0]) / 255.0)),
        g: f64::from(srgb_to_linear(f32::from(bg_rgba_u8[1]) / 255.0)),
        b: f64::from(srgb_to_linear(f32::from(bg_rgba_u8[2]) / 255.0)),
        a: f64::from(bg_rgba_u8[3]) / 255.0,
    };
    renderer
        .frame()
        .render(
            &[&render_list],
            catchlight_wgpu::RenderTarget {
                view: &view,
                texture: match snapshot {
                    Snapshot::Target => Some(&texture),
                    Snapshot::None => None,
                    Snapshot::Uncopyable => Some(&uncopyable),
                },
                width: W,
                height: H,
            },
            &mut targets,
            Some(clear),
        )
        .submit();

    catchlight_wgpu::read_texture_to_rgba(&renderer.device, &renderer.queue, &texture)
        .await
        .expect("readback")
}

fn make_nested_burn_model(pad_rgba: [u8; 4], src_rgba: [u8; 4]) -> Model {
    let mut build = common::Build::new();
    let pad_tex = build.texture(common::solid_texture(4, 4, pad_rgba));
    let burn_tex = build.texture(common::solid_texture(4, 4, src_rgba));
    let root = build.root();
    let comp = build.node(
        &root,
        "composite",
        0.0,
        ModelNodeKind::Composite(ModelComposite::new()),
    );
    build.part(&comp, "pad", 0.0, common::quad(1.0, 2.0), &pad_tex, |_| {});
    build.part(
        &comp,
        "burn",
        1.0,
        common::quad(2.0, 2.0),
        &burn_tex,
        |part| part.blend_mode = BlendMode::ColorBurn,
    );
    build.model
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
    // ±2 LSB for u8 round-trips and shader precision.
    let ok = approx_eq(actual[0], exp[0], 2)
        && approx_eq(actual[1], exp[1], 2)
        && approx_eq(actual[2], exp[2], 2);
    assert!(
        ok,
        "{label}: actual={actual:?}, expected={exp:?} (linear {expected_linear:?})",
    );
}

#[test]
fn overlay_blend_runs_dst_in_shader_math() {
    // Overlay with d ≥ 0.5: out = 1 - 2*(1-s)*(1-d).
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
    // Values chosen so the result doesn't clamp to zero.
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
    // Inverse is fixed-function: src*OneMinusDst + dst*OneMinusSrcAlpha.
    // Opaque over opaque gives color = src*(1-dst) and alpha = 0, which is
    // correct.
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
    // A dst-in-shader Part in a Composite blends against the composite's
    // transparent-cleared buffer, so past the pad KHR degrades to plain src.
    let bg = [100u8, 100, 200, 255];
    let pad = [200u8, 200, 200, 255];
    let src = [230u8, 180, 150, 255];

    let model = make_nested_burn_model(pad, src);
    let pixels = pollster::block_on(render_model(model, bg));

    let s = [
        srgb_to_linear(230.0 / 255.0),
        srgb_to_linear(180.0 / 255.0),
        srgb_to_linear(150.0 / 255.0),
    ];
    let d = srgb_to_linear(200.0 / 255.0);

    let mut blend = [0.0f32; 3];
    for c in 0..3 {
        blend[c] = (1.0 - (1.0 - d) / s[c]).clamp(0.0, 1.0);
    }
    assert_pixel_close(center_pixel(&pixels), blend, "nested ColorBurn over pad");

    assert_pixel_close(
        pixel_at(&pixels, 6, (H / 2) as usize),
        s,
        "nested ColorBurn over transparent composite region",
    );
}

#[test]
fn overlay_with_normal_fallback_differs_from_overlay_with_snapshot() {
    let bg = [128u8, 128, 128, 255];
    let src = [204u8, 76, 76, 255];

    let with_snapshot = pollster::block_on(render_blend(BlendMode::Overlay, bg, src));
    let with_snapshot_center = center_pixel(&with_snapshot);

    // `render_list` without a snapshot pool falls back to OVER for Overlay.
    let model = make_model(BlendMode::Overlay, src);
    let fallback_center = pollster::block_on(async {
        let (device, queue) = common::shared_context();
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
        let mut renderer = WgpuRenderer::new(device, queue, format);
        let mut cache = RenderCache::prepare(&mut renderer, &model, PrepareOptions::default())
            .expect("prepare the render cache");
        renderer.update_camera(glam::Mat4::orthographic_rh(
            -1.0, 1.0, 1.0, -1.0, -1000.0, 1000.0,
        ));
        let mut puppet = catchlight_core::Puppet::new(&model);
        puppet.tick(&model, 0.0);
        cache
            .refresh(&mut renderer, &model, &puppet)
            .expect("refresh the render cache");
        let render_list = collect(&cache, &puppet);
        let mut targets = catchlight_wgpu::FrameTargets::new();
        renderer
            .frame()
            .render(
                &[&render_list],
                catchlight_wgpu::RenderTarget {
                    view: &view,
                    texture: None,
                    width: W,
                    height: H,
                },
                &mut targets,
                Some(wgpu::Color {
                    r: f64::from(bg[0]) / 255.0,
                    g: f64::from(bg[1]) / 255.0,
                    b: f64::from(bg[2]) / 255.0,
                    a: f64::from(bg[3]) / 255.0,
                }),
            )
            .submit();
        let buf =
            catchlight_wgpu::read_texture_to_rgba(&renderer.device, &renderer.queue, &texture)
                .await
                .expect("readback");
        center_pixel(&buf)
    });

    let max_diff = (0..3)
        .map(|c| (i32::from(with_snapshot_center[c]) - i32::from(fallback_center[c])).abs())
        .max()
        .unwrap();
    assert!(
        max_diff > 5,
        "snapshot-path center {with_snapshot_center:?} too close to fallback center {fallback_center:?} (max ch diff {max_diff})",
    );
}

#[test]
fn an_uncopyable_target_texture_takes_the_normal_fallback() {
    let bg = [128u8, 128, 128, 255];
    let src = [204u8, 76, 76, 255];
    let render = |snapshot| {
        pollster::block_on(render_model_with(
            make_model(BlendMode::Overlay, src),
            bg,
            snapshot,
        ))
    };
    let uncopyable = render(Snapshot::Uncopyable);
    assert_eq!(uncopyable, render(Snapshot::None));
    assert_ne!(
        uncopyable,
        render(Snapshot::Target),
        "the fallback is not the shader blend"
    );
}
