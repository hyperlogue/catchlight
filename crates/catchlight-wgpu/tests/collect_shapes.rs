#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What the collector makes of a model's shape: which drawables it emits,
//! where it routes them, in what order, and which it drops.
//!
//! These ran as unit tests over the legacy draw source until it was deleted.
//! They need a `RenderCache` now, so they are integration tests on the
//! headless context — which is the better place for them anyway: they exercise
//! the draw source the product actually collects through, and the slot
//! numbering that goes with it.
//!
//! Two numberings appear in a [`RenderList`] and they are not the same:
//! a **part** is named by its dense mesh slot, a **composite** by its node
//! slot. Every assertion here goes through `mesh_slot` / `node_slot` rather
//! than a literal, because a literal would encode that confusion.

mod common;

use catchlight_core::components::BlendMode;
use catchlight_core::{
    MaskMode, Model, ModelComposite, ModelNodeKind, ModelPart, NodeId, Puppet, TexId,
};
use catchlight_wgpu::{
    collect, create_headless_context, DrawableInfo, MaskSourceData, PrepareOptions, RenderCache,
    RenderList, WgpuRenderer,
};
use common::{Build, Scene, NO_ADAPTER};
use std::path::PathBuf;

async fn renderer() -> WgpuRenderer {
    let (device, queue) = create_headless_context().await.expect(NO_ADAPTER);
    WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await
}

/// A model, its puppet and its cache, with one frame already collected.
fn framed(model: Model) -> (Scene, RenderList) {
    pollster::block_on(async {
        let mut renderer = renderer().await;
        let mut scene = Scene::new(&mut renderer, model);
        let list = scene.frame(&mut renderer, 0.0);
        (scene, list)
    })
}

fn node_slot(scene: &Scene, id: &NodeId) -> u32 {
    scene.puppet.node_idx(id).expect("node is baked").0
}

fn mesh_slot(scene: &Scene, id: &NodeId) -> u32 {
    scene
        .cache
        .mesh_slot_of_node(node_slot(scene, id))
        .expect("the node's mesh uploaded")
}

/// The parts of a drawable list, by mesh slot, in list order.
fn parts(drawables: &[DrawableInfo]) -> Vec<u32> {
    drawables
        .iter()
        .filter_map(|d| match d {
            DrawableInfo::Part { mesh_id, .. } => Some(*mesh_id),
            _ => None,
        })
        .collect()
}

/// The composites of a drawable list, by node slot, in list order.
fn composites(drawables: &[DrawableInfo]) -> Vec<u32> {
    drawables
        .iter()
        .filter_map(|d| match d {
            DrawableInfo::Composite { node_id, .. } => Some(*node_id),
            _ => None,
        })
        .collect()
}

/// Every composite drawable in the list, wherever it is routed.
fn all_composites(list: &RenderList) -> Vec<u32> {
    composites(&list.root_drawables)
        .into_iter()
        .chain(list.composite_children.values().flat_map(|c| composites(c)))
        .collect()
}

/// A composite under `parent`, with the defaults a new one has.
fn composite(build: &mut Build, parent: &NodeId, name: &str, z_order: f32) -> NodeId {
    build.node(
        parent,
        name,
        z_order,
        ModelNodeKind::Composite(ModelComposite::new()),
    )
}

/// A textured unit quad under `parent`.
fn part(
    build: &mut Build,
    parent: &NodeId,
    name: &str,
    z_order: f32,
    tex: &TexId,
    configure: impl FnOnce(&mut ModelPart),
) -> NodeId {
    build.part(
        parent,
        name,
        z_order,
        common::quad(1.0, 1.0),
        tex,
        configure,
    )
}

fn one_texture(build: &mut Build) -> TexId {
    build.texture(common::solid_texture(2, 2, [255, 255, 255, 255]))
}

#[test]
fn a_part_mask_source_is_collected_with_its_mode() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    let source = part(&mut build, &root, "source", 0.0, &tex, |_| {});
    let comp = composite(&mut build, &root, "comp", 0.0);
    part(&mut build, &comp, "content", 0.0, &tex, |_| {});
    build
        .model
        .mask_add(&comp, &source, MaskMode::DodgeMask)
        .expect("mask the composite with a part");
    let (scene, list) = framed(build.model);

    let comp_slot = node_slot(&scene, &comp);
    let mask_sources = list
        .root_drawables
        .iter()
        .find_map(|d| match d {
            DrawableInfo::Composite {
                node_id,
                mask_sources,
                ..
            } if *node_id == comp_slot => Some(mask_sources),
            _ => None,
        })
        .expect("the masked composite is a root drawable");

    assert_eq!(mask_sources.len(), 1);
    let expected = mesh_slot(&scene, &source);
    assert!(
        matches!(
            mask_sources[0],
            MaskSourceData::Part {
                mesh_id,
                mode: MaskMode::DodgeMask,
                ..
            } if mesh_id == expected
        ),
        "got {:?}",
        mask_sources[0],
    );
    // The source part, the composite's one mask source, and the content part.
    assert_eq!(list.total_instance_count(), 3);
}

/// A composite used as a mask source is flattened into the parts that draw its
/// shape, kept under its own slot with the composite's own opacity and
/// threshold. A Model refuses to *author* a composite mask source
/// (`mask_add` takes a part), so the case comes from a file — which is where
/// it comes from in production too.
#[test]
fn a_composite_mask_source_keeps_its_descendant_parts() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/models/composite_masks.clm");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let model = Model::from_clm_bytes(&bytes).expect("load composite_masks");

    let named = |name: &str| -> NodeId {
        model
            .nodes_in_order()
            .into_iter()
            .find(|id| model.node(id).is_some_and(|n| n.name.as_str() == name))
            .unwrap_or_else(|| panic!("the fixture has a node named {name}"))
    };
    let source = named("CompositeMaskSource");
    let (left, center) = (named("LeftMask"), named("CenterMask"));
    let (opacity, threshold) = match &model.node(&source).expect("source node").kind {
        ModelNodeKind::Composite(c) => (c.opacity, c.mask_threshold),
        _ => panic!("CompositeMaskSource is a composite"),
    };

    let (scene, list) = framed(model);
    let data = list
        .composite_mask_sources
        .get(&node_slot(&scene, &source))
        .expect("the composite mask source is collected under its node slot");

    assert_eq!(data.opacity, opacity, "the composite's own opacity");
    assert_eq!(data.mask_threshold, threshold);
    // Compared as a set: mask sources all stencil into one buffer, so the
    // collector never sorts them and their order is the descent's, not a
    // promise.
    let mut shapes: Vec<u32> = data.parts.iter().map(|p| p.mesh_id).collect();
    shapes.sort_unstable();
    let mut expected = vec![mesh_slot(&scene, &left), mesh_slot(&scene, &center)];
    expected.sort_unstable();
    assert_eq!(
        shapes, expected,
        "its descendant parts are what draw its shape",
    );
}

/// A pass-through Composite (Normal/opaque/identity/no-mask, all-Normal
/// children, no nested Composite) inside another Composite is flattened: its
/// Parts join the enclosing composite's children, interleaved by cumulative z,
/// and it emits no Composite drawable of its own. This lets a Part it holds
/// sort *behind* a more-positive-z Part of the enclosing composite — the
/// eyelash-behind-hair-bangs case. Without flattening the inner composite
/// hoists to root and paints on top of the bangs.
#[test]
fn passthrough_nested_composite_flattens_into_enclosing() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    // Outer isolates (it is a root composite). A direct Part at z=10 draws in
    // front; the nested pass-through composite holds a Part at z=0 that must
    // sort behind it.
    let outer = composite(&mut build, &root, "outer", 0.0);
    let front = part(&mut build, &outer, "front", 10.0, &tex, |_| {});
    let inner = composite(&mut build, &outer, "inner", 0.0);
    let behind = part(&mut build, &inner, "behind", 0.0, &tex, |_| {});
    let (scene, list) = framed(build.model);

    assert_eq!(
        composites(&list.root_drawables),
        vec![node_slot(&scene, &outer)],
        "only the outer composite emits a drawable; inner is flattened away",
    );
    let kids = list
        .composite_children
        .get(&node_slot(&scene, &outer))
        .expect("outer composite children");
    assert_eq!(
        parts(kids),
        vec![mesh_slot(&scene, &behind), mesh_slot(&scene, &front)],
        "both parts are the outer's children, ascending by z",
    );
}

#[test]
fn drawable_order_is_low_to_high_and_stable_for_equal_z() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    let first_front = part(&mut build, &root, "first_front", 2.0, &tex, |_| {});
    let behind = part(&mut build, &root, "behind", -1.0, &tex, |_| {});
    let last_front = part(&mut build, &root, "last_front", 2.0, &tex, |_| {});
    let (scene, list) = framed(build.model);

    assert_eq!(
        parts(&list.root_drawables),
        vec![
            mesh_slot(&scene, &behind),
            mesh_slot(&scene, &first_front),
            mesh_slot(&scene, &last_front),
        ],
    );
}

/// An inner Composite that isolates something — here a Multiply child, which
/// reads the destination — is NOT flattened: it keeps its own drawable so the
/// child blends against the isolated buffer, not the enclosing composite's
/// accumulated content. That drawable belongs to the *enclosing* composite,
/// not to the root: the renderer blits the inner's slot into the outer's, so
/// the outer's opacity/tint/blend/mask cover it and it z-sorts among the
/// outer's children. Escaping to `root_drawables` would render it straight to
/// the framebuffer at root z.
#[test]
fn isolating_nested_composite_is_not_flattened() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    let outer = composite(&mut build, &root, "outer", 0.0);
    let inner = composite(&mut build, &outer, "inner", 0.0);
    part(&mut build, &inner, "multiply", 0.0, &tex, |p| {
        p.blend_mode = BlendMode::Multiply;
    });
    let (scene, list) = framed(build.model);

    let (outer_slot, inner_slot) = (node_slot(&scene, &outer), node_slot(&scene, &inner));
    assert_eq!(
        composites(&list.root_drawables),
        vec![outer_slot],
        "only the outer composite is a root drawable",
    );
    let outer_kids = list
        .composite_children
        .get(&outer_slot)
        .expect("outer composite children");
    assert_eq!(
        composites(outer_kids),
        vec![inner_slot],
        "the isolating nested composite is a child drawable of the outer",
    );
    assert!(
        list.composite_children
            .get(&inner_slot)
            .is_some_and(|kids| kids.len() == 1),
        "the inner composite keeps its own child list",
    );
}

/// Opacity 0 is a no-op for every blend mode except Darken
/// (BlendOperation::Min ignores blend factors, so a zero-alpha src still
/// darkens), so the collector culls all but Darken.
#[test]
fn opacity_zero_drawables_are_culled_except_darken() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    part(&mut build, &root, "invisible", 0.0, &tex, |p| {
        p.opacity = 0.0;
    });
    let darken = part(&mut build, &root, "darken", 0.0, &tex, |p| {
        p.opacity = 0.0;
        p.blend_mode = BlendMode::Darken;
    });
    let visible = part(&mut build, &root, "visible", 0.0, &tex, |p| {
        p.opacity = 0.5;
    });
    let (scene, list) = framed(build.model);

    let drawn = parts(&list.root_drawables);
    assert_eq!(drawn.len(), 2);
    assert!(drawn.contains(&mesh_slot(&scene, &darken)));
    assert!(drawn.contains(&mesh_slot(&scene, &visible)));
}

/// The cached structural pass-through verdict must be re-evaluated when the
/// model grows. A collector reused across frames caches "inner is a
/// pass-through group" on the first frame; adding a Multiply Part under
/// `inner` makes it genuinely isolating, and the second frame must stop
/// flattening it.
///
/// The cache's own collector is what carries the memo across frames, so this
/// goes through `collect_into` rather than the allocating `collect`.
#[test]
fn growing_a_composite_re_evaluates_the_passthrough_verdict() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    let outer = composite(&mut build, &root, "outer", 0.0);
    let inner = composite(&mut build, &outer, "inner", 0.0);
    part(&mut build, &inner, "content", 0.0, &tex, |_| {});
    let mut hex = build.hex;

    pollster::block_on(async {
        let mut renderer = renderer().await;
        let mut scene = Scene::new(&mut renderer, build.model);
        let mut list = RenderList::default();

        scene.puppet.tick(&scene.model, 0.0);
        scene
            .cache
            .refresh(&mut renderer, &scene.model, &scene.puppet)
            .expect("refresh");
        scene.cache.collect_into(&scene.puppet, &mut list);
        let outer_slot = node_slot(&scene, &outer);
        assert_eq!(
            all_composites(&list),
            vec![outer_slot],
            "inner starts as a pass-through group and flattens into outer",
        );

        // Grow the tree: a Multiply Part under inner makes it isolating.
        let mut multiply = ModelPart::new(common::quad(1.0, 1.0));
        multiply.blend_mode = BlendMode::Multiply;
        let added = scene
            .model
            .add_node(
                &inner,
                catchlight_core::ModelNode::new("multiply", ModelNodeKind::Part(multiply)),
                &mut hex,
            )
            .expect("add the multiply part");
        scene
            .model
            .set_part_albedo(&added, Some(tex.clone()))
            .expect("albedo");

        scene.puppet.tick(&scene.model, 0.0);
        scene
            .cache
            .refresh(&mut renderer, &scene.model, &scene.puppet)
            .expect("refresh");
        scene.cache.collect_into(&scene.puppet, &mut list);
        let inner_slot = node_slot(&scene, &inner);
        assert!(
            all_composites(&list).contains(&inner_slot),
            "growth must re-evaluate the verdict so inner isolates again, got {:?}",
            all_composites(&list),
        );
    });
}

/// The same verdict has to follow a composite's blend mode and masks, which an
/// author changes without adding or removing a node.
#[test]
fn editing_a_composites_blend_or_masks_re_routes_it() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    let source = part(&mut build, &root, "source", 0.0, &tex, |_| {});
    let outer = composite(&mut build, &root, "outer", 0.0);
    let inner = composite(&mut build, &outer, "inner", 0.0);
    part(&mut build, &inner, "content", 0.0, &tex, |_| {});

    pollster::block_on(async {
        let mut renderer = renderer().await;
        let mut scene = Scene::new(&mut renderer, build.model);
        let mut list = RenderList::default();
        let inner_slot = node_slot(&scene, &inner);

        let mut emits_inner = |scene: &mut Scene, renderer: &mut WgpuRenderer| -> bool {
            scene.puppet.tick(&scene.model, 0.0);
            scene
                .cache
                .refresh(renderer, &scene.model, &scene.puppet)
                .expect("refresh");
            scene.cache.collect_into(&scene.puppet, &mut list);
            all_composites(&list).contains(&inner_slot)
        };
        let set_blend = |scene: &mut Scene, mode: BlendMode| {
            scene
                .model
                .update_node(&inner, |n| {
                    if let ModelNodeKind::Composite(c) = &mut n.kind {
                        c.blend_mode = mode;
                    }
                })
                .expect("set the inner composite's blend mode");
        };

        assert!(!emits_inner(&mut scene, &mut renderer), "starts flattened");

        set_blend(&mut scene, BlendMode::Multiply);
        assert!(emits_inner(&mut scene, &mut renderer), "multiply isolates");

        set_blend(&mut scene, BlendMode::Normal);
        assert!(!emits_inner(&mut scene, &mut renderer), "back to flattened");

        scene
            .model
            .mask_add(&inner, &source, MaskMode::Mask)
            .expect("mask the inner composite");
        assert!(emits_inner(&mut scene, &mut renderer), "a mask isolates");

        scene.model.mask_delete(&inner, 0).expect("drop the mask");
        assert!(!emits_inner(&mut scene, &mut renderer), "flattened again");
    });
}

/// The allocating `collect` and the cache's `collect_into` must agree; the
/// difference between them is only who owns the buffers.
#[test]
fn collect_and_collect_into_produce_the_same_list() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    let outer = composite(&mut build, &root, "outer", 0.0);
    part(&mut build, &outer, "content", 0.0, &tex, |_| {});
    part(&mut build, &root, "loose", 1.0, &tex, |_| {});

    pollster::block_on(async {
        let mut renderer = renderer().await;
        let mut scene = Scene::new(&mut renderer, build.model);
        scene.puppet.tick(&scene.model, 0.0);
        scene
            .cache
            .refresh(&mut renderer, &scene.model, &scene.puppet)
            .expect("refresh");

        let allocated = collect(&scene.cache, &scene.puppet);
        let mut reused = RenderList::default();
        scene.cache.collect_into(&scene.puppet, &mut reused);

        assert_eq!(
            composites(&allocated.root_drawables),
            composites(&reused.root_drawables),
        );
        assert_eq!(
            parts(&allocated.root_drawables),
            parts(&reused.root_drawables)
        );
        assert_eq!(
            allocated.total_instance_count(),
            reused.total_instance_count(),
        );
    });
}

/// A prepared cache is not a posed one: nothing here should need a renderer to
/// answer "which mesh slot is this node's".
#[test]
fn a_node_without_a_mesh_has_no_mesh_slot() {
    let mut build = Build::new();
    let tex = one_texture(&mut build);
    let root = build.root();
    let group = build.node(&root, "group", 0.0, ModelNodeKind::Group);
    let drawn = part(&mut build, &root, "drawn", 0.0, &tex, |_| {});

    pollster::block_on(async {
        let mut renderer = renderer().await;
        let model = build.model;
        let cache = RenderCache::prepare(&mut renderer, &model, PrepareOptions::default())
            .expect("prepare");
        let puppet = Puppet::new(&model);
        let slot_of = |id: &NodeId| puppet.node_idx(id).expect("baked").0;

        assert!(cache.mesh_slot_of_node(slot_of(&group)).is_none());
        assert!(cache.mesh_slot_of_node(slot_of(&drawn)).is_some());
        assert!(
            cache.mesh_slot_of_node(u32::MAX).is_none(),
            "a slot past the arena names nothing",
        );
    });
}
