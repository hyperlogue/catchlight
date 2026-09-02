#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! One [`RenderCache`] serving several puppets of one model.
//!
//! The property under test is the one a shared cache costs: a puppet's
//! deforms must reach the GPU without disturbing any other puppet's. Every
//! draw of a frame lands in one submit and `queue.write_buffer` batches at
//! submit start, so before deform sets the second puppet's upload won the
//! whole atlas and *both* puppets drew the second one's pose. These assert on
//! pixels, because that failure is invisible in every counter.

mod common;

use catchlight_core::{Mesh, Model, Puppet, Vec2};
use catchlight_wgpu::{
    apply_uniform_scratch_deform, create_headless_context, create_orthographic_camera, DeformSet,
    PrepareOptions, RenderCache, RenderContext, RenderList, WgpuRenderer,
};
use common::NO_ADAPTER;

const W: u32 = 128;
const H: u32 = 128;
const CAMERA_HEIGHT: f32 = 100.0;
/// Half the camera's height: a puppet shifted this far sits well clear of one
/// shifted the other way, so "did A move" is answerable per pixel column.
const SHIFT: f32 = 25.0;

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// One orange quad on a transparent field, small enough that a `SHIFT` in
/// either direction lands it entirely in one half of the viewport.
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
    let (device, queue) = pollster::block_on(create_headless_context()).expect(NO_ADAPTER);
    let renderer = pollster::block_on(WgpuRenderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8Unorm,
    ));
    let mut ctx = RenderContext::with_renderer(renderer, W, H).expect("render context");
    ctx.renderer
        .update_camera(create_orthographic_camera(CAMERA_HEIGHT, 1.0));
    ctx
}

/// A puppet posed by shifting every vertex, which is the shortest path from a
/// number in a test to bytes in the deform atlas.
fn posed(model: &Model, shift: f32) -> Puppet {
    let mut puppet = Puppet::new(model);
    puppet.compute_transforms();
    apply_uniform_scratch_deform(&mut puppet, Vec2::new(shift, 0.0));
    puppet
}

/// Columns of non-background pixels, as `(leftmost, rightmost)`.
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

    // One cache, one renderer, two puppets: the left one on the set every
    // renderer has, the right one on a set of its own.
    let left_set = DeformSet::FIRST;
    let right_set = ctx.renderer.acquire_deform_set();
    assert_ne!(left_set, right_set, "a second puppet gets a second set");
    assert_eq!(ctx.renderer.live_deform_sets(), 2);

    let left = posed(&model, -SHIFT);
    let right = posed(&model, SHIFT);
    let mut left_list = RenderList::default();
    let mut right_list = RenderList::default();

    // Each puppet alone, for the spans the shared frame has to reproduce.
    cache
        .refresh_puppet(&mut ctx.renderer, &model, &left, left_set, &mut left_list)
        .unwrap();
    ctx.render(&left_list, Some(CLEAR)).unwrap();
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
    ctx.render(&right_list, Some(CLEAR)).unwrap();
    let right_alone = drawn_span(&ctx.read_rgba().unwrap()).expect("the right puppet drew");

    assert!(
        left_alone.1 < right_alone.0,
        "the fixture must separate the two puppets: {left_alone:?} vs {right_alone:?}",
    );

    // Both in one frame, one submit, off one cache.
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
    let stats = ctx
        .render_many(&[&left_list, &right_list], Some(CLEAR))
        .unwrap();
    assert_eq!(stats.drawn_parts, 2, "both puppets drew");

    let both = ctx.read_rgba().unwrap();
    let span = drawn_span(&both).expect("the shared frame drew something");
    assert_eq!(
        span,
        (left_alone.0, right_alone.1),
        "the shared frame must span both poses; a span matching one of them \
         alone means the second puppet's deform upload overwrote the first's",
    );
    // Both quads, not one drawn twice at the same place.
    assert!(
        drawn_pixels(&both)
            > drawn_pixels(&{
                ctx.render(&left_list, Some(CLEAR)).unwrap();
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
    ctx.render(&list, Some(CLEAR)).unwrap();
    let shifted = drawn_span(&ctx.read_rgba().unwrap()).expect("drew");

    ctx.renderer.release_deform_set(first);
    assert_eq!(ctx.renderer.live_deform_sets(), 1, "only FIRST is left");
    let second = ctx.renderer.acquire_deform_set();
    assert_eq!(second, first, "the index is recycled, not grown past");

    // An undeformed puppet on the recycled index must draw undeformed: the
    // previous tenant's bytes are still in the atlas until this upload.
    let mut plain = Puppet::new(&model);
    plain.compute_transforms();
    cache
        .refresh_puppet(&mut ctx.renderer, &model, &plain, second, &mut list)
        .unwrap();
    ctx.render(&list, Some(CLEAR)).unwrap();
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
    let stats = ctx.render_many(&borrowed, Some(CLEAR)).unwrap();
    let frame = ctx.renderer.frame_stats();

    assert_eq!(stats.drawn_parts, 8);
    // The whole point of one frame call rather than eight: one cursor, one
    // flush. Eight `render_list_ext` calls in this submit would each rewrite
    // offset 0 and the last would win for all of them.
    assert_eq!(frame.instance_buffer_writes, 1, "{frame:?}");
    assert_eq!(frame.part_uniform_buffer_writes, 1, "{frame:?}");
    assert_eq!(frame.camera_buffer_writes, 1, "{frame:?}");
    assert_eq!(frame.late_buffer_reallocs, 0, "{frame:?}");
    assert_eq!(frame.instance_slots_written, 8, "{frame:?}");
}
