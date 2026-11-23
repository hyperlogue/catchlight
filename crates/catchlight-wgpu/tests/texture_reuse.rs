#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A browser replica rebuilds its `Model` on every commit; the rebuild must
//! cost no texture work unless a texture changed.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use catchlight_core::{
    Model, ModelNode, ModelNodeKind, ModelTexture, NodeId, Puppet, SeededHex, TexId,
};
use catchlight_wgpu::{PrepareOptions, RenderCache, RenderContext, WgpuRenderer};

const W: u32 = 64;
const H: u32 = 64;

fn two_texture_model() -> (Model, SeededHex) {
    let mut build = common::Build::new();
    let root = build.root();
    let red = build.texture(common::solid_texture(8, 8, [200, 60, 40, 255]));
    let blue = build.texture(common::solid_texture(8, 8, [40, 60, 200, 255]));
    build.part(&root, "left", 0.0, common::quad(1.0, 1.0), &red, |_| {});
    build.part(&root, "right", 1.0, common::quad(0.5, 0.5), &blue, |_| {});
    (build.model, build.hex)
}

fn three_texture_model() -> (Model, Vec<NodeId>) {
    let mut build = common::Build::new();
    let root = build.root();
    let colours = [[200, 60, 40, 255], [40, 200, 60, 255], [40, 60, 200, 255]];
    let parts = colours
        .into_iter()
        .enumerate()
        .map(|(i, rgba)| {
            let tex = build.texture(common::solid_texture(8, 8, rgba));
            build.part(
                &root,
                &format!("part{i}"),
                i as f32,
                common::quad(1.0, 1.0),
                &tex,
                |_| {},
            )
        })
        .collect();
    (build.model, parts)
}

async fn context() -> RenderContext {
    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await;
    RenderContext::with_renderer(renderer, W, H).expect("render context")
}

#[test]
fn uploads_release_decoded_pixels_and_replace_texture_dimensions() {
    pollster::block_on(async {
        let mut ctx = context().await;
        ctx.renderer
            .upload_mesh(0, &catchlight_core::Mesh::quad(1.0, 1.0))
            .unwrap();
        let mut texture = catchlight_core::DecodedTexture {
            width: 1,
            height: 2,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255].into(),
        };
        let pixels = Arc::downgrade(&texture.rgba);
        let mut list = catchlight_wgpu::RenderList::default();
        list.root_drawables
            .push(catchlight_wgpu::DrawableInfo::Part {
                mesh_id: 0,
                texture_id: 0,
                transform: glam::Mat4::IDENTITY,
                z_order: 0.0,
                blend_mode: catchlight_core::BlendMode::Normal,
                opacity: 1.0,
                tint: glam::Vec3::ONE,
                screen_tint: glam::Vec3::ZERO,
                mask_sources: Default::default(),
                mask_threshold: 0.5,
            });
        ctx.renderer.upload_texture(0, &texture).unwrap();
        assert_eq!(Arc::strong_count(&texture.rgba), 1);
        ctx.render(&list, Some(wgpu::Color::TRANSPARENT)).unwrap();
        let vertical = ctx.read_rgba().unwrap();

        texture.width = 2;
        texture.height = 1;
        ctx.renderer.upload_texture(0, &texture).unwrap();
        drop(texture);
        assert!(
            pixels.upgrade().is_none(),
            "the GPU upload must not retain CPU pixels"
        );
        ctx.render(&list, Some(wgpu::Color::TRANSPARENT)).unwrap();
        let horizontal = ctx.read_rgba().unwrap();
        assert_ne!(
            vertical, horizontal,
            "the replacement uses its new dimensions"
        );
        assert!(horizontal.as_chunks::<4>().0.iter().any(|p| p[3] != 0));
    });
}

fn store(model: &Model) -> HashMap<TexId, ModelTexture> {
    model
        .texture_ids()
        .iter()
        .filter_map(|id| Some((id.clone(), model.texture(id)?.clone())))
        .collect()
}

#[test]
fn a_structure_push_only_pays_for_the_textures_that_changed() {
    pollster::block_on(replica_sync(PrepareOptions::default()));
}

async fn replica_sync(options: PrepareOptions) {
    let mut ctx = context().await;

    let (mut server, mut hex) = two_texture_model();
    let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).expect("load replica");
    let mut held = store(&replica);

    let mut cache = RenderCache::prepare(&mut ctx.renderer, &replica, options).expect("prepare");
    let mut puppet = Puppet::new(&replica);
    assert_eq!(
        cache.stats().texture_uploads,
        2,
        "the first build uploads both"
    );
    assert_eq!(cache.stats().textures_kept, 0);

    let root = server.root().expect("a complete model").clone();
    server
        .add_node(
            &root,
            ModelNode::new("added", ModelNodeKind::Group),
            &mut hex,
        )
        .expect("add node");
    replica
        .replace_structure(&server.to_structure_bytes().unwrap(), |id| {
            held.get(id).cloned()
        })
        .expect("apply the structure");

    let before = cache.stats();
    puppet.tick(&replica, 0.0);
    cache.refresh(&mut ctx.renderer, &replica, &puppet).unwrap();
    let after = cache.stats();
    assert_eq!(after.rebuilds, before.rebuilds + 1, "the model moved");
    assert_eq!(
        after.texture_uploads - before.texture_uploads,
        0,
        "a push that changed no payload must decode and upload nothing",
    );
    assert_eq!(after.textures_kept - before.textures_kept, 2);
    assert_eq!(after.mesh_uploads, before.mesh_uploads);
    assert_eq!(after.meshes_kept - before.meshes_kept, 2);
    assert_eq!(cache.texture_count(), 2, "and both are still addressable");

    // Payloads are immutable under their Id, so a repaint is a new texture.
    let right = server
        .nodes_in_order()
        .into_iter()
        .find(|id| server.node(id).is_some_and(|n| n.name.as_str() == "right"))
        .expect("the part named right");
    let green = common::solid_texture(8, 8, [40, 200, 60, 255]);
    server
        .add_texture(&right, green.clone(), &mut hex)
        .expect("repaint");
    let structure = server.to_structure_bytes().unwrap();

    let listed = catchlight_core::formats::clm::structure_texture_ids(&structure).unwrap();
    let missing: Vec<TexId> = listed
        .iter()
        .map(|t| t.id.clone())
        .filter(|id| !held.contains_key(id))
        .collect();
    assert_eq!(missing.len(), 1, "one payload to fetch: {missing:?}");
    held.insert(missing[0].clone(), green);
    replica
        .replace_structure(&structure, |id| held.get(id).cloned())
        .expect("apply the structure");
    assert_eq!(
        replica.texture_ids().len(),
        2,
        "the old payload went with it"
    );

    let before = after;
    puppet.tick(&replica, 0.0);
    cache.refresh(&mut ctx.renderer, &replica, &puppet).unwrap();
    let after = cache.stats();
    assert_eq!(
        after.texture_uploads - before.texture_uploads,
        1,
        "only the payload that moved is decoded and uploaded",
    );
    assert_eq!(after.textures_kept - before.textures_kept, 1);
    assert_eq!(after.mesh_uploads, before.mesh_uploads);
    assert_eq!(after.meshes_kept - before.meshes_kept, 2);
}

/// Kept textures count against the decoded-texture limit too.
#[test]
fn the_decoded_texture_limit_counts_the_textures_a_rebuild_keeps() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, mut hex) = two_texture_model();
        let both = 2 * 8 * 8 * 4;

        let tight = PrepareOptions {
            max_decoded_texture_bytes: both - 1,
            ..PrepareOptions::default()
        };
        let Err(error) = RenderCache::prepare(&mut ctx.renderer, &model, tight) else {
            panic!("a model over the limit was prepared");
        };
        assert!(
            error.to_string().contains("decoded texture bytes"),
            "{error}"
        );

        let exact = PrepareOptions {
            max_decoded_texture_bytes: both,
            ..PrepareOptions::default()
        };
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, exact).expect("prepare");
        let mut puppet = Puppet::new(&model);

        let root = model.root().expect("a complete model").clone();
        let part = model
            .add_node(
                &root,
                ModelNode::new(
                    "third",
                    ModelNodeKind::Part(catchlight_core::ModelPart::new(common::quad(0.25, 0.25))),
                ),
                &mut hex,
            )
            .expect("add part");
        model
            .add_texture(
                &part,
                common::solid_texture(8, 8, [40, 200, 60, 255]),
                &mut hex,
            )
            .expect("add texture");

        puppet.tick(&model, 0.0);
        let error = cache
            .refresh(&mut ctx.renderer, &model, &puppet)
            .expect_err("the kept textures were not charged");
        assert!(
            error.to_string().contains("decoded texture bytes"),
            "{error}"
        );
        assert_eq!(
            cache.texture_count(),
            2,
            "the previous build stays resident"
        );
    });
}

#[test]
fn an_in_tab_handoff_keeps_every_gpu_texture() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut server, mut hex) = two_texture_model();
        let mut replica = Model::new();
        replica.replace_from(&server);

        let mut cache =
            RenderCache::prepare(&mut ctx.renderer, &replica, PrepareOptions::default())
                .expect("prepare");
        let mut puppet = Puppet::new(&replica);

        let root = server.root().expect("a complete model").clone();
        server
            .add_node(
                &root,
                ModelNode::new("added", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add node");
        replica.replace_from(&server);
        for id in replica.texture_ids() {
            assert!(
                Arc::ptr_eq(
                    &replica.texture(id).unwrap().data,
                    &server.texture(id).unwrap().data,
                ),
                "a handoff shares payloads rather than copying them",
            );
        }

        let before = cache.stats();
        puppet.tick(&replica, 0.0);
        cache.refresh(&mut ctx.renderer, &replica, &puppet).unwrap();
        let after = cache.stats();
        assert_eq!(after.rebuilds, before.rebuilds + 1);
        assert_eq!(after.texture_uploads, before.texture_uploads);
        assert_eq!(after.textures_kept - before.textures_kept, 2);
        assert_eq!(after.mesh_uploads, before.mesh_uploads);
        assert_eq!(after.meshes_kept - before.meshes_kept, 2);
    });
}

/// Removing a texture shifts later slots; keyed by Id, survivors are moved,
/// not re-decoded.
#[test]
fn removing_the_first_texture_re_uploads_no_survivor() {
    pollster::block_on(async {
        let mut ctx = context().await;
        let (mut model, parts) = three_texture_model();
        let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
            .expect("prepare");
        let mut puppet = Puppet::new(&model);
        assert_eq!(
            cache.stats().texture_uploads,
            3,
            "the first build uploads all three"
        );

        model.delete_node(&parts[0]).expect("delete the first part");
        assert_eq!(model.texture_ids().len(), 2, "one texture left the model");

        let before = cache.stats();
        puppet.tick(&model, 0.0);
        cache.refresh(&mut ctx.renderer, &model, &puppet).unwrap();
        let after = cache.stats();
        assert_eq!(after.rebuilds, before.rebuilds + 1, "the model moved");
        assert_eq!(
            after.texture_uploads - before.texture_uploads,
            0,
            "a texture that only changed slots was decoded and uploaded again",
        );
        assert_eq!(
            after.textures_kept - before.textures_kept,
            2,
            "every survivor is kept, whatever slot it moved to",
        );
        assert_eq!(cache.texture_count(), 2, "and both are still addressable");
        assert_eq!(
            ctx.renderer.live_texture_slots(),
            2,
            "the deleted texture outlived the rebuild",
        );
    });
}
