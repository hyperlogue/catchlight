use crate::{
    CompositeMaskPartData, CompositeMaskSourceData, DrawableInfo, MaskSourceData, RenderContext,
    RenderList,
};
use catchlight_core::{BlendMode, MaskMode, Mesh};

fn context() -> RenderContext {
    let mut ctx = super::render_context(64, 64);
    ctx.renderer.upload_mesh(0, &Mesh::quad(1.0, 1.0)).unwrap();
    ctx.renderer
        .upload_textures([(
            0,
            &catchlight_core::DecodedTexture {
                width: 4,
                height: 4,
                rgba: vec![255; 64].into(),
            },
        )])
        .unwrap();
    ctx
}

fn part(x: f32, blend_mode: BlendMode) -> DrawableInfo {
    DrawableInfo::Part {
        mesh_id: 0,
        texture_id: 0,
        transform: glam::Mat4::from_translation(glam::vec3(x, 0.0, 0.0)),
        z_order: 0.0,
        blend_mode,
        opacity: 1.0,
        tint: glam::Vec3::ONE,
        screen_tint: glam::Vec3::ZERO,
        mask_sources: Default::default(),
        mask_threshold: 0.5,
    }
}

fn composite(node_id: u32, blend_mode: BlendMode) -> DrawableInfo {
    DrawableInfo::Composite {
        node_id,
        z_order: 0.0,
        blend_mode,
        opacity: 0.75,
        tint: glam::Vec3::ONE,
        screen_tint: glam::Vec3::ZERO,
        mask_sources: Default::default(),
        mask_threshold: 0.5,
    }
}

#[test]
fn offscreen_parts_and_nested_composites_only_clear_the_frame() {
    let mut ctx = context();
    let mut list = RenderList {
        root_drawables: vec![
            part(3.0, BlendMode::Normal),
            part(-3.0, BlendMode::Overlay),
            composite(1, BlendMode::Normal),
            composite(2, BlendMode::ColorBurn),
        ],
        ..Default::default()
    };
    list.set_children(1, vec![composite(3, BlendMode::Normal)]);
    list.set_children(2, vec![part(3.0, BlendMode::Normal)]);
    list.set_children(3, vec![part(3.0, BlendMode::Normal)]);
    let stats = ctx.render(&list, Some(wgpu::Color::WHITE));
    assert_eq!(stats.total_draw_calls, 0);
    assert_eq!(stats.render_passes, 1);
    assert_eq!(stats.composite_slots_used, 0);
    assert_eq!(ctx.renderer.frame_stats().instance_slots_written, 0);
    assert_eq!(ctx.renderer.frame_stats().part_uniform_writes, 0);
    assert!(ctx.read_rgba().unwrap().iter().all(|&v| v == 255));
}

#[test]
fn culled_barriers_preserve_pixels_and_pending_batches() {
    let mut ctx = context();
    for opacity in [None, Some(0.75), Some(1.0)] {
        let nested = opacity.is_some();
        let mut list = RenderList::default();
        let visible = vec![part(-0.3, BlendMode::Normal), part(0.3, BlendMode::Normal)];
        if nested {
            let mut group = composite(1, BlendMode::Normal);
            if let DrawableInfo::Composite { opacity: value, .. } = &mut group {
                *value = opacity.unwrap();
            }
            list.root_drawables.push(group);
            list.set_children(1, visible);
        } else {
            list.root_drawables = visible;
        }
        let expected = ctx.render(&list, Some(wgpu::Color::TRANSPARENT));
        let pixels = ctx.read_rgba().unwrap();
        let children = if nested {
            &mut list.composite_children[1]
        } else {
            &mut list.root_drawables
        };
        children.insert(1, part(3.0, BlendMode::Overlay));
        children.insert(1, composite(2, BlendMode::Normal));
        children.insert(0, part(3.0, BlendMode::ColorBurn));
        list.set_children(2, vec![part(3.0, BlendMode::Normal)]);
        let actual = ctx.render(&list, Some(wgpu::Color::TRANSPARENT));
        assert_eq!(actual.render_passes, expected.render_passes);
        assert_eq!(actual.total_draw_calls, expected.total_draw_calls);
        assert_eq!(ctx.read_rgba().unwrap(), pixels);
    }
}

#[test]
fn culling_keeps_darken_effects_and_clears_before_the_first_snapshot() {
    let mut ctx = context();
    let mut list = RenderList {
        root_drawables: vec![part(3.0, BlendMode::Normal), part(0.0, BlendMode::Overlay)],
        ..Default::default()
    };
    ctx.render(&list, Some(wgpu::Color::BLACK));
    let expected = ctx.render(&list, Some(wgpu::Color::WHITE));
    assert_eq!(expected.drawn_parts, 1);
    assert!(ctx.read_rgba().unwrap().iter().all(|&v| v == 255));

    list.root_drawables = vec![composite(1, BlendMode::Darken)];
    list.set_children(1, vec![part(3.0, BlendMode::Normal)]);
    let stats = ctx.render(&list, Some(wgpu::Color::WHITE));
    assert_eq!(stats.drawn_parts, 0);
    assert_eq!(stats.drawn_composites, 1);
    assert!(ctx.read_rgba().unwrap().iter().all(|&v| v == 0));
}

#[test]
fn composite_masks_are_prepared_only_for_visible_receivers() {
    let mut ctx = context();
    let mut receiver = part(3.0, BlendMode::Normal);
    if let DrawableInfo::Part { mask_sources, .. } = &mut receiver {
        mask_sources.push(MaskSourceData::Composite {
            node_id: 7,
            mode: MaskMode::Mask,
        });
    }
    let mut list = RenderList::default();
    list.root_drawables.push(receiver);
    list.set_mask_source(
        7,
        CompositeMaskSourceData {
            opacity: 1.0,
            mask_threshold: 0.5,
            parts: vec![CompositeMaskPartData {
                mesh_id: 0,
                texture_id: 0,
                transform: glam::Mat4::IDENTITY,
                mask_threshold: 0.5,
            }],
        },
    );
    let stats = ctx.render(&list, Some(wgpu::Color::TRANSPARENT));
    assert_eq!(stats.total_draw_calls, 0);
    assert_eq!(stats.composite_slots_used, 0);
    if let DrawableInfo::Part { transform, .. } = &mut list.root_drawables[0] {
        *transform = glam::Mat4::IDENTITY;
    }
    let stats = ctx.render(&list, Some(wgpu::Color::TRANSPARENT));
    assert_eq!(stats.drawn_parts, 1);
    let pixels = ctx.read_rgba().unwrap();
    assert_eq!(&pixels[(32 * 64 + 32) * 4..][..4], &[255; 4]);
    assert_eq!(&pixels[..4], &[0; 4]);
}
