#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod errors;
mod offscreen_culling;
mod upload_mesh;
mod upload_texture;

use super::{
    blend_mode_to_wgpu, blend_transparent_src_is_identity, camera_slot_offset, pixels_to_scissor,
    project_aabb_to_pixels, renders_as_over, same_mask_signature, Aabb2, ScreenRect,
    CAMERA_RING_SLOTS,
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
    use crate::{DrawableInfo, RenderList};
    let mut ctx = render_context(100, 100);
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
    list.set_children(1, vec![composite(2), composite(3)]);
    list.set_children(2, vec![part(0)]);
    list.set_children(3, Vec::new());
    ctx.render(&list, Some(wgpu::Color::WHITE));
    assert_eq!(
        ctx.renderer.composite_bounds.get(1),
        Some(&Some([25.0, 25.0, 75.0, 75.0]))
    );

    ctx.renderer
        .update_camera(glam::Mat4::from_translation(glam::vec3(0.25, 0.0, 0.0)));
    ctx.render(&list, Some(wgpu::Color::WHITE));
    assert_eq!(
        ctx.renderer.composite_bounds.get(1),
        Some(&Some([37.5, 25.0, 87.5, 75.0]))
    );

    if let DrawableInfo::Composite { blend_mode, .. } = &mut list.root_drawables[0] {
        *blend_mode = BlendMode::Darken;
    }
    ctx.render(&list, Some(wgpu::Color::WHITE));
    let pixels = ctx.read_rgba().expect("pixels");
    assert_eq!(
        &pixels[..4],
        &[0, 0, 0, 0],
        "Darken affects pixels outside the nested bounds"
    );
    assert_eq!(&pixels[(50 * 100 + 50) * 4..][..4], &[255; 4]);

    list.composite_children
        .get_mut(2)
        .expect("children")
        .push(part(99));
    ctx.render(&list, Some(wgpu::Color::WHITE));
    assert_eq!(ctx.renderer.composite_bounds.get(1), Some(&None));
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

/// One device for the whole unit-test binary, never destroyed: NVIDIA's Vulkan
/// driver can hang in `vkDestroyDevice` while other threads are inside it.
fn shared_context() -> (wgpu::Device, wgpu::Queue) {
    static CONTEXT: std::sync::OnceLock<(wgpu::Device, wgpu::Queue)> = std::sync::OnceLock::new();
    CONTEXT
        .get_or_init(|| {
            pollster::block_on(crate::create_headless_context()).expect("a headless adapter")
        })
        .clone()
}

fn render_context(width: u32, height: u32) -> crate::RenderContext {
    let (device, queue) = shared_context();
    let renderer = crate::WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb);
    crate::RenderContext::with_renderer(renderer, width, height)
}
