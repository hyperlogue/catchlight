//! One [`RenderCache`] serving several puppets: `write_buffer` batches at
//! submit start, so one puppet's deforms must not overwrite another's.
//! Asserts on pixels, since the failure is invisible in counters.

use crate::common;
use crate::common::apply_uniform_scratch_deform;

use catchlight_core::{Mesh, Model, Puppet, Vec2};
use catchlight_wgpu::{
    DeformSet, PrepareOptions, RenderCache, RenderContext, RenderList, WgpuRenderer,
    create_orthographic_camera,
};

const W: u32 = 128;
const H: u32 = 128;
const CAMERA_HEIGHT: f32 = 100.0;
const SHIFT: f32 = 25.0;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

fn one_quad_model() -> Model {
    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, [230, 120, 40, 255]));
    let root = build.root();
    build.part(
        &root,
        "quad",
        0.0,
        common::mesh_to_clm(&Mesh::quad(20.0, 20.0)),
        &tex,
        |_| {},
    );
    build.model
}

fn ctx() -> RenderContext {
    let (device, queue) = common::shared_context();
    let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8Unorm);
    let mut ctx = RenderContext::with_renderer(renderer, W, H);
    ctx.renderer
        .update_camera(create_orthographic_camera(CAMERA_HEIGHT, 1.0));
    ctx
}

fn posed(model: &Model, shift: f32) -> Puppet {
    let mut puppet = Puppet::new(model);
    puppet.compute_transforms();
    apply_uniform_scratch_deform(&mut puppet, Vec2::new(shift, 0.0));
    puppet
}

fn drawn_span(pixels: &[u8]) -> Option<(u32, u32)> {
    let mut span: Option<(u32, u32)> = None;
    for (i, px) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if px[0] < 40 && px[1] < 40 && px[2] < 40 {
            continue;
        }
        let x = (i as u32) % W;
        span = Some(match span {
            None => (x, x),
            Some((lo, hi)) => (lo.min(x), hi.max(x)),
        });
    }
    span
}

fn drawn_pixels(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|px| px[0] >= 40 || px[1] >= 40 || px[2] >= 40)
        .count()
}

#[test]
fn two_puppets_of_one_model_share_a_cache_and_keep_their_own_deforms() {
    let model = one_quad_model();
    let mut ctx = ctx();
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .expect("prepare one cache for the model");

    let left_set = DeformSet::FIRST;
    let right_set = ctx.renderer.acquire_deform_set();
    assert_ne!(left_set, right_set, "a second puppet gets a second set");
    assert_eq!(ctx.renderer.live_deform_sets(), 2);

    let left = posed(&model, -SHIFT);
    let right = posed(&model, SHIFT);
    let mut left_list = RenderList::default();
    let mut right_list = RenderList::default();

    cache
        .refresh_puppet(&mut ctx.renderer, &model, &left, left_set, &mut left_list)
        .unwrap();
    ctx.render(&left_list, Some(CLEAR));
    let left_alone = drawn_span(&ctx.read_rgba().unwrap()).expect("the left puppet drew");

    cache
        .refresh_puppet(
            &mut ctx.renderer,
            &model,
            &right,
            right_set,
            &mut right_list,
        )
        .unwrap();
    ctx.render(&right_list, Some(CLEAR));
    let right_alone = drawn_span(&ctx.read_rgba().unwrap()).expect("the right puppet drew");

    assert!(
        left_alone.1 < right_alone.0,
        "the fixture must separate the two puppets: {left_alone:?} vs {right_alone:?}",
    );

    cache
        .refresh_puppet(&mut ctx.renderer, &model, &left, left_set, &mut left_list)
        .unwrap();
    cache
        .refresh_puppet(
            &mut ctx.renderer,
            &model,
            &right,
            right_set,
            &mut right_list,
        )
        .unwrap();
    let stats = ctx.render_many(&[&left_list, &right_list], Some(CLEAR));
    assert_eq!(stats.drawn_parts, 2, "both puppets drew");

    let both = ctx.read_rgba().unwrap();
    let span = drawn_span(&both).expect("the shared frame drew something");
    assert_eq!(
        span,
        (left_alone.0, right_alone.1),
        "the shared frame must span both poses; a span matching one of them \
         alone means the second puppet's deform upload overwrote the first's",
    );
    assert!(
        drawn_pixels(&both)
            > drawn_pixels(&{
                ctx.render(&left_list, Some(CLEAR));
                ctx.read_rgba().unwrap()
            }),
        "two puppets cover more than one",
    );
}

#[test]
fn a_released_deform_set_is_reused_and_starts_clean() {
    let model = one_quad_model();
    let mut ctx = ctx();
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .expect("prepare");

    let first = ctx.renderer.acquire_deform_set();
    let mut list = RenderList::default();
    cache
        .refresh_puppet(
            &mut ctx.renderer,
            &model,
            &posed(&model, SHIFT),
            first,
            &mut list,
        )
        .unwrap();
    ctx.render(&list, Some(CLEAR));
    let shifted = drawn_span(&ctx.read_rgba().unwrap()).expect("drew");

    ctx.renderer.release_deform_set(first);
    assert_eq!(ctx.renderer.live_deform_sets(), 1, "only FIRST is left");
    let second = ctx.renderer.acquire_deform_set();
    assert_eq!(second, first, "the index is recycled, not grown past");

    // The previous tenant's bytes stay in the atlas until this upload.
    let mut plain = Puppet::new(&model);
    plain.compute_transforms();
    cache
        .refresh_puppet(&mut ctx.renderer, &model, &plain, second, &mut list)
        .unwrap();
    ctx.render(&list, Some(CLEAR));
    let plain_span = drawn_span(&ctx.read_rgba().unwrap()).expect("drew");
    assert!(
        plain_span.0 < shifted.0,
        "the recycled set kept the released puppet's deform: {plain_span:?} vs {shifted:?}",
    );
}

#[test]
fn one_frame_of_many_puppets_takes_one_write_per_frame_buffer() {
    let model = one_quad_model();
    let mut ctx = ctx();
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .expect("prepare");

    let mut lists = Vec::new();
    for i in 0..8 {
        let set = if i == 0 {
            DeformSet::FIRST
        } else {
            ctx.renderer.acquire_deform_set()
        };
        let mut list = RenderList::default();
        cache
            .refresh_puppet(
                &mut ctx.renderer,
                &model,
                &posed(&model, i as f32 * 4.0 - 16.0),
                set,
                &mut list,
            )
            .unwrap();
        lists.push(list);
    }
    let borrowed: Vec<&RenderList> = lists.iter().collect();
    let stats = ctx.render_many(&borrowed, Some(CLEAR));
    let frame = ctx.renderer.frame_stats();

    assert_eq!(stats.drawn_parts, 8);
    // Eight one-list frames would each rewrite offset 0; the last would win.
    assert_eq!(frame.instance_buffer_writes, 1, "{frame:?}");
    assert_eq!(frame.part_uniform_buffer_writes, 1, "{frame:?}");
    assert_eq!(frame.camera_buffer_writes, 1, "{frame:?}");
    assert_eq!(frame.late_buffer_reallocs, 0, "{frame:?}");
    assert_eq!(frame.instance_slots_written, 8, "{frame:?}");
}
