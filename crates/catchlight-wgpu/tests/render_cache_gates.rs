#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The render cache's model-generation and deform-generation gates.

mod common;

use catchlight_core::{Model, ModelNodeKind, ModelPart, Puppet, SeededHex, Vec2};
use catchlight_wgpu::{PrepareOptions, RenderCache, RenderContext, WgpuRenderer};

const W: u32 = 64;
const H: u32 = 64;

const CLEAR: Option<wgpu::Color> = Some(wgpu::Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
});

fn one_quad_model() -> (Model, SeededHex) {
    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, [200, 60, 40, 255]));
    let root = build.root();
    build.part(&root, "quad", 0.0, common::quad(1.0, 1.0), &tex, |_| {});
    (build.model, build.hex)
}

async fn context() -> RenderContext {
    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb);
    RenderContext::with_renderer(renderer, W, H)
}

fn camera() -> glam::Mat4 {
    glam::Mat4::orthographic_rh(-1.0, 1.0, -1.0, 1.0, -1000.0, 1000.0)
}

/// A model edit between frames reaches the GPU with no manual invalidation.
#[test]
fn a_model_edit_between_frames_rebuilds_the_cache() {
    let options = PrepareOptions::default();
    pollster::block_on(async {
        let mut ctx = context().await;
        let (model, mut hex) = one_quad_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, options).expect("prepare");
        let mut puppet = Puppet::new(&model);
        ctx.renderer.update_camera(camera());

        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let before = catchlight_wgpu::collect(&cache, &puppet);
        assert_eq!(before.root_drawables().len(), 1);
        let stats = ctx.render(&before, CLEAR);
        assert_eq!(stats.drawn_parts, 1);

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
        assert_eq!(after.root_drawables().len(), 2);
        let stats = ctx.render(&after, CLEAR);
        assert_eq!(stats.drawn_parts, 2);
    });
}

/// A frame whose deform stacks have not moved uploads nothing.
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
        let moved = ctx.render(&list, CLEAR);
        assert!(
            moved.deform_bytes_uploaded > 0,
            "a deform that moved must reach the atlas",
        );

        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let still = ctx.render(&list, CLEAR);
        assert_eq!(
            still.deform_bytes_uploaded, 0,
            "an unchanged deform must not be re-uploaded",
        );
    });
}

#[test]
fn mesh_groups_keep_their_deforms_off_the_gpu() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let mut build = common::Build::new();
        let group = build.node(
            &build.root(),
            "mesh group",
            0.0,
            ModelNodeKind::MeshGroup(catchlight_core::ModelMeshGroup::new(common::quad(1.0, 1.0))),
        );
        let mut scene = common::Scene::new(&mut ctx.renderer, build.model);
        let group = scene.puppet.node_idx(&group).unwrap();
        assert_eq!(scene.cache.mesh_count(), 0);
        assert!(scene.cache.mesh_slot_of_node(group.0).is_none());
        assert!(
            scene
                .puppet
                .set_scratch_deform(group, &[Vec2::new(0.2, 0.0); 4])
        );
        let list = scene.frame(&mut ctx.renderer, 0.0);
        let stats = ctx.render(&list, CLEAR);
        assert_eq!(stats.deform_bytes_uploaded, 0);
        assert_eq!(stats.total_draw_calls, 0);
    });
}

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

/// Unedited models share generation 0, so only identity tells them apart.
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
