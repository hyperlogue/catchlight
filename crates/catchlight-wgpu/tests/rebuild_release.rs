#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A [`RenderCache`] rebuild releases GPU state the new build does not name,
//! and keeps textures it names again.

mod common;

use catchlight_core::formats::clm::{ClmMesh, TextureAlpha, TextureEncoding};
use catchlight_core::{
    Model, ModelNode, ModelNodeKind, ModelPart, ModelTexture, NodeId, Puppet, SeededHex, TexId,
    Vec2,
};
use catchlight_wgpu::{PrepareOptions, RenderCache, RenderContext, WgpuRenderer};

const W: u32 = 64;
const H: u32 = 64;

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [200, 60, 40, 255];
const GREEN: [u8; 4] = [40, 200, 60, 255];
const BLUE: [u8; 4] = [40, 60, 200, 255];

const CLEAR: Option<wgpu::Color> = Some(wgpu::Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
});

async fn context() -> RenderContext {
    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb);
    RenderContext::with_renderer(renderer, W, H).expect("render context")
}

fn camera() -> glam::Mat4 {
    glam::Mat4::orthographic_rh(-1.0, 1.0, -1.0, 1.0, -1000.0, 1000.0)
}

fn quad_at(cx: f32, cy: f32, half: f32) -> ClmMesh {
    let mut mesh = common::quad(half * 2.0, half * 2.0);
    for (i, v) in mesh.verts.iter_mut().enumerate() {
        *v += if i % 2 == 0 { cx } else { cy };
    }
    mesh
}

/// Left red, middle green, right blue.
fn three_quad_model() -> (Model, Vec<NodeId>, Vec<TexId>) {
    let mut build = common::Build::new();
    let root = build.root();
    let mut nodes = Vec::new();
    let mut textures = Vec::new();
    for (i, (name, x, rgba)) in [
        ("left", -0.6, RED),
        ("middle", 0.0, GREEN),
        ("right", 0.6, BLUE),
    ]
    .into_iter()
    .enumerate()
    {
        let tex = build.texture(common::solid_texture(4, 4, rgba));
        let node = build.part(&root, name, i as f32, quad_at(x, 0.0, 0.25), &tex, |_| {});
        nodes.push(node);
        textures.push(tex);
    }
    (build.model, nodes, textures)
}

fn column_centre(x: f32) -> (u32, u32) {
    (((x + 1.0) / 2.0 * W as f32) as u32, H / 2)
}

fn nearest(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    let px = &pixels[i..i + 4];
    [WHITE, RED, GREEN, BLUE]
        .into_iter()
        .min_by_key(|c| {
            c.iter()
                .zip(px)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).pow(2))
                .sum::<i32>()
        })
        .expect("the palette is not empty")
}

/// Deletes the middle part so blue shifts from texture slot 2 to 1; stale
/// contents would draw green there.
#[test]
fn a_rebuild_releases_the_slots_the_smaller_model_no_longer_names() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, nodes, textures) = three_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
            .expect("prepare");
        let mut puppet = Puppet::new(&model);
        ctx.renderer.update_camera(camera());

        assert_eq!(ctx.renderer.live_mesh_slots(), 3);
        assert_eq!(ctx.renderer.live_texture_slots(), 3);
        let deform_bytes = ctx.renderer.reserved_deform_bytes();
        assert!(deform_bytes > 0, "three uploaded meshes reserved no atlas");

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let list = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(ctx.render(&list, CLEAR).expect("render").drawn_parts, 3);
        let pixels = ctx.read_rgba().expect("read back");
        for (x, expected) in [(-0.6, RED), (0.0, GREEN), (0.6, BLUE)] {
            let (px, py) = column_centre(x);
            assert_eq!(nearest(&pixels, px, py), expected, "column at x={x}");
        }

        model
            .delete_node(&nodes[1])
            .expect("delete the middle part");
        assert!(model.texture(&textures[1]).is_none());

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        assert_eq!(cache.mesh_count(), 2);
        assert_eq!(cache.stats().mesh_uploads, 3);
        assert_eq!(cache.stats().meshes_kept, 2);
        assert_eq!(cache.texture_count(), 2);
        assert_eq!(
            ctx.renderer.live_mesh_slots(),
            2,
            "the deleted part's mesh buffer outlived the rebuild",
        );
        assert_eq!(
            ctx.renderer.live_texture_slots(),
            2,
            "the deleted texture outlived the rebuild",
        );
        assert!(
            ctx.renderer.reserved_deform_bytes() < deform_bytes,
            "the deleted mesh's deform-atlas range outlived the rebuild",
        );

        let list = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(ctx.render(&list, CLEAR).expect("render").drawn_parts, 2);
        let pixels = ctx.read_rgba().expect("read back");
        for (x, expected) in [(-0.6, RED), (0.0, WHITE), (0.6, BLUE)] {
            let (px, py) = column_centre(x);
            assert_eq!(nearest(&pixels, px, py), expected, "column at x={x}");
        }

        // Surviving meshes have new atlas offsets; deforms must still land.
        catchlight_wgpu::apply_uniform_scratch_deform(&mut puppet, Vec2::new(0.1, 0.0));
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let list = catchlight_wgpu::collect(&cache, &puppet);
        let stats = ctx.render(&list, CLEAR).expect("render");
        assert!(
            stats.deform_bytes_uploaded > 0,
            "a deform pushed after a rebuild must reach the atlas",
        );
        assert_eq!(stats.drawn_parts, 2);
    });
}

#[test]
fn a_rebuild_keeps_the_textures_it_names_again() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, nodes, textures) = three_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
            .expect("prepare");
        let mut puppet = Puppet::new(&model);
        ctx.renderer.update_camera(camera());

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let list = catchlight_wgpu::collect(&cache, &puppet);
        ctx.render(&list, CLEAR).expect("render");

        // A texture upload is the only submit outside a frame.
        assert_eq!(ctx.renderer.frame_stats().queue_submits, 0);

        model.delete_node(&nodes[2]).expect("delete the right part");
        assert!(model.texture(&textures[2]).is_none());
        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();

        assert_eq!(ctx.renderer.live_texture_slots(), 2);
        assert_eq!(
            ctx.renderer.frame_stats().queue_submits,
            0,
            "a rebuild re-uploaded a texture it had already uploaded",
        );

        let list = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(ctx.render(&list, CLEAR).expect("render").drawn_parts, 2);
        let pixels = ctx.read_rgba().expect("read back");
        for (x, expected) in [(-0.6, RED), (0.0, GREEN), (0.6, WHITE)] {
            let (px, py) = column_centre(x);
            assert_eq!(nearest(&pixels, px, py), expected, "column at x={x}");
        }

        // Removing the first part moves the survivor from slot 1 to 0; keyed
        // by Id it is relocated, not re-uploaded.
        model.delete_node(&nodes[0]).expect("delete the left part");
        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();

        assert_eq!(ctx.renderer.live_texture_slots(), 1);
        assert_eq!(
            ctx.renderer.frame_stats().queue_submits,
            0,
            "a rebuild re-uploaded a texture that had only changed slots",
        );

        let list = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(ctx.render(&list, CLEAR).expect("render").drawn_parts, 1);
        let pixels = ctx.read_rgba().expect("read back");
        for (x, expected) in [(-0.6, WHITE), (0.0, GREEN), (0.6, WHITE)] {
            let (px, py) = column_centre(x);
            assert_eq!(nearest(&pixels, px, py), expected, "column at x={x}");
        }
    });
}

/// A failed rebuild must leave the previous build resident.
#[test]
fn a_failed_rebuild_leaves_the_previous_build_resident() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, _nodes, _textures) = three_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
            .expect("prepare");
        let mut puppet = Puppet::new(&model);
        ctx.renderer.update_camera(camera());

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let list = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(ctx.render(&list, CLEAR).expect("render").drawn_parts, 3);

        let mut hex = SeededHex::new(7);
        let root = model.root().expect("a complete model").clone();
        let cases: [(&str, ModelTexture); 2] = [
            (
                "bytes that do not decode",
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: b"not a png"[..].into(),
                },
            ),
            ("a texture wider than the device allows", {
                let side = ctx.renderer.max_texture_dimension() + 1;
                common::solid_texture(side, 1, WHITE)
            }),
        ];
        for (why, texture) in cases {
            let part = model
                .add_node(
                    &root,
                    ModelNode::new(
                        "bad",
                        ModelNodeKind::Part(ModelPart::new(quad_at(0.9, 0.0, 0.05))),
                    ),
                    &mut hex,
                )
                .expect("add part");
            model
                .add_texture(&part, texture, &mut hex)
                .expect("add texture");

            puppet.tick(&model, 0.0);
            cache
                .refresh(&mut ctx.renderer, &model, &puppet)
                .expect_err(why);

            assert_eq!(ctx.renderer.live_mesh_slots(), 3, "{why}");
            assert_eq!(ctx.renderer.live_texture_slots(), 3, "{why}");
            assert_eq!(cache.texture_count(), 3, "{why}");

            model.delete_node(&part).expect("delete the bad part");
            puppet.tick(&model, 0.0);
            cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
            let list = catchlight_wgpu::collect(&cache, &puppet);
            assert_eq!(
                ctx.render(&list, CLEAR).expect("render").drawn_parts,
                3,
                "{why}"
            );
            let pixels = ctx.read_rgba().expect("read back");
            for (x, expected) in [(-0.6, RED), (0.0, GREEN), (0.6, BLUE)] {
                let (px, py) = column_centre(x);
                assert_eq!(nearest(&pixels, px, py), expected, "{why}: column at x={x}");
            }
        }
    });
}
