#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What a [`RenderCache`] rebuild hands back. A rebuild renames every slot it
//! is about to fill, so the GPU state under the slots the new build does not
//! reach has to go — otherwise a model that shrank leaves mesh buffers,
//! textures and deform-atlas ranges resident under numbers nothing addresses.
//!
//! The counterweight is in the same tests: releasing must not cost the
//! re-upload memo, which is the whole reason
//! [`PrepareOptions::memoize_textures`] exists.

mod common;

use catchlight_core::formats::clm::ClmMesh;
use catchlight_core::{Model, NodeId, Puppet, TexId, Vec2};
use catchlight_wgpu::{
    create_headless_context, PrepareOptions, RenderCache, RenderContext, WgpuRenderer,
};
use common::NO_ADAPTER;

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

/// The memo is on: a shrink must keep it useful, and a test that leaves it off
/// could not tell a preserved slot from a re-uploaded one.
const MEMOIZED: PrepareOptions = PrepareOptions {
    texture_halvings: 0,
    memoize_textures: true,
};

async fn context() -> RenderContext {
    let (device, queue) = create_headless_context().await.expect(NO_ADAPTER);
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await;
    RenderContext::with_renderer(renderer, W, H).expect("render context")
}

fn camera() -> glam::Mat4 {
    glam::Mat4::orthographic_rh(-1.0, 1.0, -1.0, 1.0, -1000.0, 1000.0)
}

/// A `half`-sized quad centred on (`cx`, `cy`) rather than on the origin, so
/// three parts can occupy three disjoint columns of the frame.
fn quad_at(cx: f32, cy: f32, half: f32) -> ClmMesh {
    let mut mesh = common::quad(half * 2.0, half * 2.0);
    for (i, v) in mesh.verts.iter_mut().enumerate() {
        *v += if i % 2 == 0 { cx } else { cy };
    }
    mesh
}

/// Three parts side by side, each with its own solid texture: left red,
/// middle green, right blue. Returns the model with the node and texture Ids
/// in that order, so a test can delete one by position.
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

/// The pixel at the centre of the column each part occupies.
fn column_centre(x: f32) -> (u32, u32) {
    (((x + 1.0) / 2.0 * W as f32) as u32, H / 2)
}

/// Which of the palette colours a pixel is closest to, so an assertion says
/// "the right part drew here" instead of chasing exact texels.
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

/// Deleting a part and its texture must hand the GPU state under their slots
/// back, not leave it resident: mesh buffers, textures and the deform-atlas
/// ranges the old meshes reserved.
///
/// The deleted one is the *middle* on purpose. `delete_texture` closes the gap
/// in the texture order, so blue moves from slot 2 to slot 1 — a rebuild that
/// released the slot but kept the old contents would draw green where blue
/// belongs, which the pixel checks catch.
#[test]
fn a_rebuild_releases_the_slots_the_smaller_model_no_longer_names() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, nodes, textures) = three_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, MEMOIZED).expect("prepare");
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

        // The shrink. Same model, so the cache's identity gate still passes
        // and its generation gate is what notices.
        model
            .delete_node(&nodes[1])
            .expect("delete the middle part");
        model
            .delete_texture(&textures[1])
            .expect("delete its texture");

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        assert_eq!(cache.mesh_count(), 2);
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

        // The atlas is laid out afresh too, so the meshes that survived have
        // new offsets: a deform pushed after the shrink still has to land.
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

/// Releasing is a sweep of what the new build will not name, not a wipe: a
/// texture slot the rebuild fills with the same image again must keep its GPU
/// texture. `upload_texture`'s memo is what makes
/// [`PrepareOptions::memoize_textures`] pay for itself in an editor, which
/// rebuilds on every edit.
#[test]
fn a_rebuild_keeps_the_textures_it_names_again() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, nodes, textures) = three_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, MEMOIZED).expect("prepare");
        let mut puppet = Puppet::new(&model);
        ctx.renderer.update_camera(camera());

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let list = catchlight_wgpu::collect(&cache, &puppet);
        ctx.render(&list, CLEAR).expect("render");

        // `frame_stats` is reset at `render_list` entry and a texture upload
        // is the only thing that submits outside a frame, so what it counts
        // from here is exactly the rebuild's re-uploads.
        assert_eq!(ctx.renderer.frame_stats().queue_submits, 0);

        // Drop the *last* part and texture, which leaves the surviving two at
        // the slots they already hold.
        model.delete_node(&nodes[2]).expect("delete the right part");
        model
            .delete_texture(&textures[2])
            .expect("delete its texture");
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
    });
}
