#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The two gates a render cache lives by: the model's generation, which says
//! when the cache itself is stale, and the deform stacks' generations, which
//! say whether a frame has anything to upload at all.

mod common;

use catchlight_core::{Model, ModelNodeKind, ModelPart, Puppet, SeededHex, Vec2};
use catchlight_wgpu::{
    create_headless_context, PrepareOptions, RenderCache, RenderContext, WgpuRenderer,
};
use common::NO_ADAPTER;

const W: u32 = 64;
const H: u32 = 64;

const CLEAR: Option<wgpu::Color> = Some(wgpu::Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
});

/// One opaque quad, plus the hex source that minted its Ids so a test can go
/// on editing the model afterwards.
fn one_quad_model() -> (Model, SeededHex) {
    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, [200, 60, 40, 255]));
    let root = build.root();
    build.part(&root, "quad", 0.0, common::quad(1.0, 1.0), &tex, |_| {});
    (build.model, build.hex)
}

async fn context() -> RenderContext {
    let (device, queue) = create_headless_context().await.expect(NO_ADAPTER);
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await;
    RenderContext::with_renderer(renderer, W, H).expect("render context")
}

fn camera() -> glam::Mat4 {
    glam::Mat4::orthographic_rh(-1.0, 1.0, -1.0, 1.0, -1000.0, 1000.0)
}

/// Adding a node to the model between frames must reach the GPU: the puppet
/// rebakes on its next tick and the cache rebuilds on its next refresh, with
/// nothing for the caller to invalidate by hand.
///
/// Run with and without the decode memo, because a rebuild is exactly where
/// the memo has to hand back the textures the edit did not touch.
#[test]
fn a_model_edit_between_frames_rebuilds_the_cache() {
    for memoize_textures in [false, true] {
        rebuild_after_an_edit(PrepareOptions {
            texture_halvings: 0,
            memoize_textures,
        });
    }
}

fn rebuild_after_an_edit(options: PrepareOptions) {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (model, mut hex) = one_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, options).expect("prepare");
        let mut puppet = Puppet::new(&model);
        ctx.renderer.update_camera(camera());

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let before = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(before.root_drawables.len(), 1);
        let stats = ctx.render(&before, CLEAR).expect("render");
        assert_eq!(stats.drawn_parts, 1);

        // The edit. Nothing tells the puppet or the cache about it.
        let mut model = model;
        let root = model.root().expect("a complete model").clone();
        let tex = model.texture_ids()[0].clone();
        let mesh = common::quad(0.5, 0.5);
        let node =
            catchlight_core::ModelNode::new("second", ModelNodeKind::Part(ModelPart::new(mesh)));
        let added = model.add_node(&root, node, &mut hex).expect("add node");
        model.set_part_albedo(&added, Some(tex)).expect("albedo");

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        assert_eq!(
            cache.generation(),
            model.generation(),
            "refresh must rebuild the cache the model outran",
        );
        assert_eq!(cache.mesh_count(), 2, "the added part's mesh must upload");
        let after = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(after.root_drawables.len(), 2);
        let stats = ctx.render(&after, CLEAR).expect("render");
        assert_eq!(stats.drawn_parts, 2);
    });
}

/// A frame whose deform stacks have not moved must upload nothing. This is
/// the whole point of carrying the stack generation into the cache: a still
/// puppet costs one collect and zero bytes.
#[test]
fn a_frame_whose_deforms_did_not_move_uploads_nothing() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (model, _) = one_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
            .expect("prepare");
        let mut puppet = Puppet::new(&model);
        ctx.renderer.update_camera(camera());
        puppet.compute_transforms();

        // A scratch deform stands in for any source that moves a vertex; it
        // is the one a caller can drive without authoring a binding.
        let part = puppet
            .iter()
            .find_map(|(idx, node)| {
                matches!(node.kind, catchlight_core::NodeKind::Part(_)).then_some(idx)
            })
            .expect("the model has a part");
        assert!(puppet.set_scratch_deform(part, &[Vec2::new(0.2, 0.0); 4]));
        puppet.combine_deforms();

        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let list = catchlight_wgpu::collect(&cache, &puppet);
        let moved = ctx.render(&list, CLEAR).expect("render");
        assert!(
            moved.deform_bytes_uploaded > 0,
            "a deform that moved must reach the atlas",
        );

        // Same pose, same stacks: the generation memo must skip the upload.
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let still = ctx.render(&list, CLEAR).expect("render");
        assert_eq!(
            still.deform_bytes_uploaded, 0,
            "an unchanged deform must not be re-uploaded",
        );
    });
}

/// Refreshing with a puppet baked against a different state of the model is a
/// programmer error, not something to paper over: the two disagree about what
/// every node index names.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "baked against a different model state")]
fn refreshing_with_a_puppet_from_another_generation_is_a_programmer_error() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, mut hex) = one_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
            .expect("prepare");
        let puppet = Puppet::new(&model);

        // Edit the model but never sync the puppet, so its bake is stale.
        let root = model.root().expect("a complete model").clone();
        model
            .add_node(
                &root,
                catchlight_core::ModelNode::new("stale", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add node");

        let _ = cache.refresh(&mut ctx.renderer, &model, &puppet);
    });
}

/// Two models that have not been edited since they were built sit at the same
/// generation — every loader hands one back at 0 — so the generation gate
/// alone would let a cache prepared from one be refreshed against the other
/// and never notice. The model identity is what catches it.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "prepared from a different model")]
fn refreshing_a_cache_against_another_model_is_a_programmer_error() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let prepared_from = Model::new();
        let other = Model::new();
        assert_eq!(
            prepared_from.generation(),
            other.generation(),
            "two unedited models are indistinguishable by generation",
        );
        let mut cache =
            RenderCache::prepare(&mut ctx.renderer, &prepared_from, PrepareOptions::default())
                .expect("prepare");
        let puppet = Puppet::new(&other);

        let _ = cache.refresh(&mut ctx.renderer, &other, &puppet);
    });
}
