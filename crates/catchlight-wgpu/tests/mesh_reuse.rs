#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use catchlight_core::{Model, Puppet, Vec2};
use catchlight_wgpu::{
    apply_uniform_scratch_deform, DeformSet, PrepareOptions, RenderCache, RenderContext,
    RenderList, RenderStats,
};

fn render_poses(
    ctx: &mut RenderContext,
    cache: &mut RenderCache,
    model: &Model,
    sets: &[DeformSet],
    shift: Option<f32>,
) -> (Vec<u8>, RenderStats) {
    let mut lists = Vec::new();
    for (i, &set) in sets.iter().enumerate() {
        let mut puppet = Puppet::new(model);
        puppet.compute_transforms();
        if let Some(shift) = shift {
            apply_uniform_scratch_deform(
                &mut puppet,
                Vec2::new((i as f32 - 1.0) * 0.6 + shift, 0.0),
            );
        }
        let mut list = RenderList::default();
        cache
            .refresh_puppet(&mut ctx.renderer, model, &puppet, set, &mut list)
            .unwrap();
        lists.push(list);
    }
    let lists: Vec<_> = lists.iter().collect();
    let stats = ctx
        .render_many(&lists, Some(wgpu::Color::TRANSPARENT))
        .unwrap();
    (ctx.read_rgba().unwrap(), stats)
}

#[test]
fn edits_reuse_geometry_and_keep_three_puppets_equal_to_fresh_uploads() {
    let mut ctx = pollster::block_on(RenderContext::new(96, 96)).unwrap();
    let mut reference = pollster::block_on(RenderContext::new(96, 96)).unwrap();
    let sets = [
        DeformSet::FIRST,
        ctx.renderer.acquire_deform_set(),
        ctx.renderer.acquire_deform_set(),
    ];
    let reference_sets = [
        DeformSet::FIRST,
        reference.renderer.acquire_deform_set(),
        reference.renderer.acquire_deform_set(),
    ];
    let mut build = common::Build::new();
    let root = build.root();
    let red = build.texture(common::solid_texture(64, 64, [255, 0, 0, 255]));
    let blue = build.texture(common::solid_texture(64, 64, [0, 0, 255, 255]));
    let first = build.part(&root, "first", 0.0, common::quad(0.35, 0.35), &red, |_| {});
    let second = build.part(&root, "second", 1.0, common::quad(0.2, 0.2), &blue, |_| {});
    let mut model = build.model;
    let mut cache =
        RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default()).unwrap();
    let mut previous_pixels = Vec::new();

    for step in 0..8 {
        let uploads = cache.stats().mesh_uploads;
        let kept = cache.stats().meshes_kept;
        let (new_meshes, reused_meshes) = match step {
            0 => (0, 0),
            1 => {
                model
                    .update_node(&first, |node| {
                        node.name = catchlight_core::Name::truncated("renamed")
                    })
                    .unwrap();
                (0, 2)
            }
            2 => {
                model
                    .update_node(&first, |node| node.transform.translation[1] = 0.1)
                    .unwrap();
                (0, 2)
            }
            3 => {
                let mut mesh = common::quad(0.3, 0.3);
                mesh.verts.extend([0.0, 0.0]);
                mesh.uvs.extend([0.5, 0.5]);
                model.set_node_mesh(&second, mesh).unwrap();
                (1, 1)
            }
            4 => {
                model.reorder(&second, 0).unwrap();
                (0, 2)
            }
            5 => {
                let pixels: Vec<_> = (0..64 * 64)
                    .flat_map(|i| if i % 64 < 8 { [0, 255, 0, 255] } else { [0; 4] })
                    .collect();
                model
                    .add_texture(&first, common::png_texture(64, 64, &pixels), &mut build.hex)
                    .unwrap();
                (1, 1)
            }
            6 => {
                model.delete_node(&second).unwrap();
                (0, 1)
            }
            _ => {
                model
                    .update_node(&first, |node| {
                        node.name = catchlight_core::Name::truncated("plain")
                    })
                    .unwrap();
                (0, 1)
            }
        };
        let shift = (step != 7).then_some(if step >= 2 { 0.1 } else { 0.0 });
        let (actual, stats) = render_poses(&mut ctx, &mut cache, &model, &sets, shift);
        let mut fresh =
            RenderCache::prepare(&mut reference.renderer, &model, PrepareOptions::default())
                .unwrap();
        let (expected, _) =
            render_poses(&mut reference, &mut fresh, &model, &reference_sets, shift);
        assert_eq!(actual, expected, "incremental vs fresh at step {step}");
        assert_eq!(
            cache.stats().mesh_uploads - uploads,
            new_meshes,
            "uploads at step {step}"
        );
        assert_eq!(
            cache.stats().meshes_kept - kept,
            reused_meshes,
            "reuse at step {step}"
        );
        assert_eq!(ctx.renderer.live_mesh_slots(), cache.mesh_count());
        assert_eq!(
            ctx.renderer.reserved_deform_bytes(),
            reference.renderer.reserved_deform_bytes()
        );
        if step == 1 {
            assert_eq!(actual, previous_pixels);
            assert_eq!(
                stats.deform_bytes_uploaded, 0,
                "renaming changes no deforms"
            );
        }
        if step == 2 {
            assert_ne!(
                actual, previous_pixels,
                "rebaked deforms with repeated stack generations must update"
            );
            assert!(stats.deform_bytes_uploaded > 0);
        }
        previous_pixels = actual;
    }
}

#[test]
fn rebuild_reuploads_meshes_the_renderer_dropped() {
    let mut ctx = pollster::block_on(RenderContext::new(96, 96)).unwrap();
    let mut build = common::Build::new();
    let root = build.root();
    let red = build.texture(common::solid_texture(64, 64, [255, 0, 0, 255]));
    let part = build.part(&root, "part", 0.0, common::quad(0.35, 0.35), &red, |_| {});
    let mut model = build.model;
    let mut cache =
        RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default()).unwrap();
    let sets = [DeformSet::FIRST];
    let (expected, _) = render_poses(&mut ctx, &mut cache, &model, &sets, None);

    ctx.renderer.clear_meshes();
    model
        .update_node(&part, |node| {
            node.name = catchlight_core::Name::truncated("renamed")
        })
        .unwrap();
    let uploads = cache.stats().mesh_uploads;
    let (actual, stats) = render_poses(&mut ctx, &mut cache, &model, &sets, None);

    assert_eq!(cache.stats().mesh_uploads - uploads, 1);
    assert_eq!(stats.skipped_missing_mesh, 0);
    assert_eq!(actual, expected);
}
