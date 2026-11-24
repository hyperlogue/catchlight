#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

#[test]
fn each_puppets_deforms_can_bring_offscreen_geometry_into_view() {
    use catchlight_core::{ModelComposite, ModelNodeKind, Puppet, Vec2};
    use catchlight_wgpu::{DeformSet, PrepareOptions, RenderCache, RenderList};
    use common::apply_uniform_scratch_deform;

    let mut ctx = common::render_context(64, 64);
    let mut build = common::Build::new();
    let mut group = ModelComposite::new();
    group.opacity = 0.75;
    let group = build.node(&build.root(), "group", 0.0, ModelNodeKind::Composite(group));
    let tex = build.texture(common::solid_texture(4, 4, [255; 4]));
    let part = build.part(&group, "part", 0.0, common::quad(1.0, 1.0), &tex, |_| {});
    build
        .model
        .update_node(&part, |node| node.transform.translation[0] = 3.0)
        .unwrap();
    let model = build.model;
    let mut cache =
        RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default()).unwrap();
    let sets = [
        DeformSet::FIRST,
        ctx.renderer.acquire_deform_set(),
        ctx.renderer.acquire_deform_set(),
    ];
    let mut lists = Vec::new();
    for (i, set) in sets.into_iter().enumerate() {
        let mut puppet = Puppet::new(&model);
        puppet.compute_transforms();
        if i == 1 {
            apply_uniform_scratch_deform(&mut puppet, Vec2::new(-3.0, 0.0));
        }
        let mut list = RenderList::default();
        cache
            .refresh_puppet(&mut ctx.renderer, &model, &puppet, set, &mut list)
            .unwrap();
        lists.push(list);
    }
    let stats = ctx.render_many(
        &lists.iter().collect::<Vec<_>>(),
        Some(wgpu::Color::TRANSPARENT),
    );
    assert_eq!(stats.drawn_parts, 1);
    assert_eq!(stats.drawn_composites, 1);
    assert!(ctx.read_rgba().unwrap()[(32 * 64 + 32) * 4 + 3] > 0);
}
