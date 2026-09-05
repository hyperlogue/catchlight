#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What a browser replica costs per commit.
//!
//! An editor server owns the model and pushes only its `Structure` after
//! every change; the client rebuilds its whole `Model` from those bytes over
//! the texture payloads it already holds. That is a new model *state* on every
//! keystroke-sized edit, so the render cache's whole-cache rebuild runs each
//! time — and it has to cost no texture work at all unless a texture actually
//! changed. The numbers here are the reason per-commit sync is usable on a
//! real rig.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use catchlight_core::{
    Model, ModelNode, ModelNodeKind, ModelTexture, NodeId, Puppet, SeededHex, TexId,
};
use catchlight_wgpu::{
    create_headless_context, PrepareOptions, RenderCache, RenderContext, WgpuRenderer,
};
use common::NO_ADAPTER;

const W: u32 = 64;
const H: u32 = 64;

/// Two parts drawing two different textures, plus the hex source that minted
/// the Ids so the "server" can go on editing.
fn two_texture_model() -> (Model, SeededHex) {
    let mut build = common::Build::new();
    let root = build.root();
    let red = build.texture(common::solid_texture(8, 8, [200, 60, 40, 255]));
    let blue = build.texture(common::solid_texture(8, 8, [40, 60, 200, 255]));
    build.part(&root, "left", 0.0, common::quad(1.0, 1.0), &red, |_| {});
    build.part(&root, "right", 1.0, common::quad(0.5, 0.5), &blue, |_| {});
    (build.model, build.hex)
}

/// Three parts, each drawing a texture nothing else draws, so deleting one
/// part is exactly one texture leaving the model.
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
    let (device, queue) = create_headless_context().await.expect(NO_ADAPTER);
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await;
    RenderContext::with_renderer(renderer, W, H).expect("render context")
}

/// A replica's texture store: payloads by Id, exactly what
/// `Model::replace_structure` sources from.
fn store(model: &Model) -> HashMap<TexId, ModelTexture> {
    model
        .texture_ids()
        .iter()
        .filter_map(|id| Some((id.clone(), model.texture(id)?.clone())))
        .collect()
}

/// A structure push that changed no texture must decode and upload nothing;
/// one that changed a single texture must cost exactly that one.
#[test]
fn a_structure_push_only_pays_for_the_textures_that_changed() {
    for memoize_textures in [false, true] {
        pollster::block_on(replica_sync(PrepareOptions {
            texture_halvings: 0,
            memoize_textures,
        }));
    }
}

async fn replica_sync(options: PrepareOptions) {
    let mut ctx = context().await;

    // The server's model, and the replica built from the file it sent once.
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

    // An edit that touches no texture. Only the structure crosses.
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
    assert_eq!(cache.texture_count(), 2, "and both are still addressable");

    // Now one texture really changes: the server repaints what the second
    // part draws. A payload is immutable under its Id, so that is a new
    // texture and the old one goes with it.
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

    // The client's half of the push: read what the structure names, fetch
    // what the store lacks, then apply.
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
}

/// The in-tab handoff takes the same path: `replace_from` shares the payload
/// `Arc`s, so the cache keeps every GPU texture it holds.
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
    });
}

/// Removing a texture shifts every later slot down one. A memo keyed by slot
/// calls each of those a miss and re-decodes and re-uploads an image the GPU
/// already holds; keyed by Id, nothing that survived the removal is touched
/// and the GPU textures move to their new slots instead.
///
/// The memo is deliberately off here: what this counts is the render cache's
/// own reuse, not the decode cache's.
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

        // Deleting the part takes the one texture it drew with it, so the two
        // survivors are wanted at slots 0 and 1 having been uploaded at 1
        // and 2.
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
