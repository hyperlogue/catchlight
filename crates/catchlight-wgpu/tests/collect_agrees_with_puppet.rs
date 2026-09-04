#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The collector and `Puppet` report the same z and the same opacity.
//!
//! `catchlight-cli poses` dumps both for a rig evaluator, and the whole point
//! of that dump is that it describes the frame the renderer would draw. The z
//! rule lives on `Puppet` and reaches the collector through `DrawSource`, so
//! that half cannot drift; opacity is read straight off the node in both
//! places, which is exactly the kind of agreement that rots silently. This
//! pins both against what `collect` actually put in the list.

mod common;

use catchlight_core::{Mesh, Model, ModelNodeKind, Puppet};
use catchlight_wgpu::{collect, create_headless_context, DrawableInfo, WgpuRenderer};
use common::NO_ADAPTER;

/// Two parts, each two groups deep, with distinct z at every level so a
/// dropped ancestor shows up as a wrong sum rather than a coincidence, and
/// distinct opacities so the two drawables cannot be confused.
fn nested_model() -> (Model, Vec<&'static str>) {
    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, [200, 120, 60, 255]));
    let root = build.root();
    let upper = build.node(&root, "upper", 2.0, ModelNodeKind::Group);
    let lower = build.node(&root, "lower", -5.0, ModelNodeKind::Group);
    build.part(
        &upper,
        "front",
        4.0,
        common::mesh_to_clm(&Mesh::quad(20.0, 20.0)),
        &tex,
        |part| part.opacity = 0.25,
    );
    build.part(
        &lower,
        "back",
        0.5,
        common::mesh_to_clm(&Mesh::quad(20.0, 20.0)),
        &tex,
        |part| part.opacity = 0.75,
    );
    (build.model, vec!["front", "back"])
}

#[test]
fn a_drawables_z_and_opacity_are_the_puppets() {
    let (device, queue) = pollster::block_on(create_headless_context()).expect(NO_ADAPTER);
    let mut renderer = pollster::block_on(WgpuRenderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
    ));

    let (model, names) = nested_model();
    let mut scene = common::Scene::new(&mut renderer, model);
    scene.puppet.tick(&scene.model, 0.0);
    scene
        .cache
        .refresh(&mut renderer, &scene.model, &scene.puppet)
        .expect("refresh");
    let list = collect(&scene.cache, &scene.puppet);

    // What the puppet says, in the order the collector sorts by.
    let mut expected: Vec<(f32, f32)> = names
        .iter()
        .map(|name| {
            let (idx, _) = scene
                .puppet
                .iter()
                .find(|(_, node)| node.name == *name)
                .unwrap_or_else(|| panic!("the model carries a part named {name}"));
            (
                scene.puppet.accumulated_z(idx),
                scene.puppet.opacity(idx).expect("a part carries opacity"),
            )
        })
        .collect();
    expected.sort_by(|a, b| a.0.total_cmp(&b.0));

    // The sums the tree actually spells out, so a bug that changed both the
    // rule and this test still fails: root 0 + lower -5 + back 0.5, and
    // root 0 + upper 2 + front 4.
    assert_eq!(
        expected,
        vec![(-4.5, 0.75), (6.0, 0.25)],
        "the puppet's own numbers"
    );

    let drawn: Vec<(f32, f32)> = list
        .root_drawables
        .iter()
        .map(|drawable| match drawable {
            DrawableInfo::Part {
                z_order, opacity, ..
            } => (*z_order, *opacity),
            DrawableInfo::Composite { .. } => panic!("no composites in this model"),
        })
        .collect();
    assert_eq!(drawn, expected, "collect and Puppet disagree");
}

/// A composite's opacity is reported the same way, and is *not* multiplied
/// into the parts under it: the composite renders into its own target and
/// that target is blitted at its opacity, so folding it in here would apply
/// it twice.
#[test]
fn a_composite_does_not_fold_its_opacity_into_its_children() {
    let (device, queue) = pollster::block_on(create_headless_context()).expect(NO_ADAPTER);
    let mut renderer = pollster::block_on(WgpuRenderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
    ));

    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, [200, 120, 60, 255]));
    let root = build.root();
    let composite = build.node(
        &root,
        "group",
        1.0,
        ModelNodeKind::Composite(catchlight_core::ModelComposite::default()),
    );
    build.part(
        &composite,
        "inner",
        3.0,
        common::mesh_to_clm(&Mesh::quad(20.0, 20.0)),
        &tex,
        |part| part.opacity = 0.5,
    );
    let model = build.model;
    let mut puppet = Puppet::new(&model);
    // An opacity below 1 keeps the composite from being flattened away.
    let composite_idx = puppet
        .iter()
        .find(|(_, node)| node.name == "group")
        .map(|(idx, _)| idx)
        .expect("the composite");
    let part_idx = puppet
        .iter()
        .find(|(_, node)| node.name == "inner")
        .map(|(idx, _)| idx)
        .expect("the part");

    let mut cache = catchlight_wgpu::RenderCache::prepare(
        &mut renderer,
        &model,
        catchlight_wgpu::PrepareOptions::default(),
    )
    .expect("prepare");
    puppet.tick(&model, 0.0);
    cache
        .refresh(&mut renderer, &model, &puppet)
        .expect("refresh");
    let list = collect(&cache, &puppet);

    assert_eq!(
        puppet.opacity(part_idx),
        Some(0.5),
        "the part's own opacity"
    );
    assert_eq!(
        puppet.opacity(composite_idx),
        Some(1.0),
        "the composite's own opacity"
    );
    let children = list
        .composite_children
        .get(&composite_idx.0)
        .expect("the composite keeps its children");
    let [DrawableInfo::Part { opacity, .. }] = &children[..] else {
        panic!("one part under the composite");
    };
    assert_eq!(
        *opacity, 0.5,
        "the child carries its own opacity, not a product"
    );
    assert_eq!(
        puppet.accumulated_z(part_idx),
        4.0,
        "the part's z is its own plus the composite's"
    );
}
