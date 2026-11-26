//! The collector and `Puppet` report the same z and opacity, so
//! `catchlight-cli poses` describes the frame the renderer draws.

use crate::common;

use catchlight_core::{Mesh, Model, ModelNodeKind, Puppet};
use catchlight_wgpu::{DrawableInfo, WgpuRenderer, collect};

/// Distinct z at every level so a dropped ancestor shows as a wrong sum.
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
    let (device, queue) = common::shared_context();
    let mut renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);

    let (model, names) = nested_model();
    let mut scene = common::Scene::new(&mut renderer, model);
    scene.puppet.tick(&scene.model, 0.0);
    scene
        .cache
        .refresh(&mut renderer, &scene.model, &scene.puppet)
        .expect("refresh");
    let list = collect(&scene.cache, &scene.puppet);

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

    // Literal sums, so a bug changing both rule and test still fails.
    assert_eq!(
        expected,
        vec![(-4.5, 0.75), (6.0, 0.25)],
        "the puppet's own numbers"
    );

    let drawn: Vec<(f32, f32)> = list
        .root_drawables()
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

/// A composite's opacity is not multiplied into its parts; its blit applies
/// it.
#[test]
fn a_composite_does_not_fold_its_opacity_into_its_children() {
    let (device, queue) = common::shared_context();
    let mut renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);

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
    // Below 1 so the composite is not flattened.
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
    let [DrawableInfo::Part { opacity, .. }] = list.composite_children(composite_idx.0) else {
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
