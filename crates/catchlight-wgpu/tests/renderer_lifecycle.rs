#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Resource-lifecycle invariants of `WgpuRenderer`, asserted on calls and
//! buffer state rather than on pixels.
//!
//! These are the properties a split of `renderer.rs` breaks silently:
//! each one still renders a small single-puppet frame correctly and only
//! goes wrong once the frame grows, so no pixel baseline catches them.
//! Each test names the invariant it pins:
//!
//! * `one_submit_per_frame_*` — the renderer records into the caller's
//!   encoder and never submits, and each per-frame buffer takes exactly
//!   one `queue.write_buffer`. `write_buffer` batches at submit start, so
//!   a second write to a live offset wins for *every* draw that reads it.
//! * `no_buffer_grows_mid_frame_*` — growth happens only in the
//!   `begin_frame_*` sizing phase; a mid-frame reallocation strands
//!   already-recorded passes on the freed buffer.
//! * `every_reserved_slot_*` / `part_uniform_offsets_*` — cursor
//!   discipline: offsets come from the allocators, are distinct and
//!   increasing, and each reserved slot is written exactly once. The
//!   duplicate-write guard lives in the writers as a `debug_assert`, so a
//!   debug run of any test here exercises it on every write.
//! * `shared_composite_pool_*` — several puppets in one frame share one
//!   caller-owned `CompositePool`, so its allocation is the deepest
//!   puppet's need, not the sum.

mod common;

use catchlight_core::{Mesh, Model, Vec3};
use catchlight_wgpu::{
    create_headless_context, create_orthographic_camera, CompositePool, FrameStats,
    FramebufferSnapshotPool, Pipelines, RenderList, RenderStats, StencilTarget, WgpuRenderer,
};
use common::{Rig, NO_ADAPTER};
use std::path::PathBuf;
use std::sync::Arc;

const W: u32 = 256;
const H: u32 = 256;

/// `height / zoom` at the zoom the visual-test baselines frame these
/// models with, scaled to this viewport.
const CAMERA_HEIGHT: f32 = 512.0;

/// Initial buffer capacities in `WgpuRenderer::from_pipelines`. The
/// reallocation test needs a frame above both; if either constant moves,
/// the "this frame reallocated" assertion fails and points back here.
const INITIAL_INSTANCE_SLOTS: usize = 512;
const INITIAL_PART_UNIFORM_SLOTS: usize = 256;

/// Everything a frame needs from the caller: the render target plus the
/// `StencilTarget` / `FramebufferSnapshotPool` that every puppet in the
/// frame shares. `CompositePool` is passed per call so a test can hand
/// the same pool to several renderers.
struct Stage {
    device: wgpu::Device,
    queue: wgpu::Queue,
    shared: Arc<Pipelines>,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    stencil: StencilTarget,
    snapshots: FramebufferSnapshotPool,
    /// `queue.submit` calls this stage made. The renderer adds none of
    /// its own, so this is the frame's total.
    submits: u32,
}

impl Stage {
    fn new() -> Self {
        let (device, queue) = pollster::block_on(create_headless_context()).expect(NO_ADAPTER);
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lifecycle-target"),
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let shared = Arc::new(Pipelines::new(&device, format));
        let stencil = StencilTarget::new_for_pipelines(&shared, &device, W, H);
        Self {
            device,
            queue,
            shared,
            target,
            view,
            stencil,
            snapshots: FramebufferSnapshotPool::new(W, H),
            submits: 0,
        }
    }

    /// A renderer on this stage's device, sharing its pipelines — the
    /// multi-puppet layout, where each puppet owns its instance /
    /// part-uniform buffers but nothing else.
    fn renderer(&self) -> WgpuRenderer {
        WgpuRenderer::from_pipelines(self.device.clone(), self.queue.clone(), self.shared.clone())
    }

    /// Prepare a cache for `model` and aim the camera at it.
    fn admit(&self, renderer: &mut WgpuRenderer, model: Model) -> Rig {
        let rig = Rig::new(renderer, model);
        renderer.update_camera(create_orthographic_camera(
            CAMERA_HEIGHT,
            W as f32 / H as f32,
        ));
        rig
    }

    /// Record one puppet's draw into `encoder`. No submit: the caller
    /// owns the frame boundary, which is what makes the write-batching
    /// invariant meaningful.
    fn record(
        &mut self,
        renderer: &mut WgpuRenderer,
        encoder: &mut wgpu::CommandEncoder,
        render_list: &RenderList,
        composites: &mut CompositePool,
        clear: Option<wgpu::Color>,
    ) -> (RenderStats, FrameStats) {
        let stats = renderer
            .render_list_ext(
                render_list,
                encoder,
                &self.view,
                &self.stencil,
                composites,
                Some(&self.target),
                Some(&mut self.snapshots),
                W,
                H,
                clear,
            )
            .expect("render_list_ext");
        (stats, renderer.frame_stats())
    }

    /// One puppet, one frame, exactly one submit.
    fn frame(
        &mut self,
        renderer: &mut WgpuRenderer,
        rig: &mut Rig,
        composites: &mut CompositePool,
    ) -> (RenderStats, FrameStats) {
        let render_list = rig.frame(renderer, 0.0);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lifecycle-frame"),
            });
        renderer.begin_camera_submit();
        let out = self.record(
            renderer,
            &mut encoder,
            &render_list,
            composites,
            Some(CLEAR),
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submits += 1;
        out
    }
}

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// `n` unmasked quads laid out on a grid inside the camera's view, all
/// sampling one shared texture. One instance and one part-uniform slot
/// each, so `n` is exactly what the frame's two buffers get sized for.
fn grid_model(n: usize) -> Model {
    let mut build = common::Build::new();
    let tex = build.texture(common::solid_texture(4, 4, [220, 90, 40, 255]));
    let root = build.root();
    let cols = (n as f32).sqrt().ceil().max(1.0) as usize;
    let mesh = common::mesh_to_clm(&Mesh::quad(6.0, 6.0));
    for i in 0..n {
        let col = (i % cols) as f32;
        let row = (i / cols) as f32;
        let span = cols as f32;
        let id = build.part(
            &root,
            &format!("part-{i}"),
            i as f32,
            mesh.clone(),
            &tex,
            |_| {},
        );
        build
            .model
            .update_node(&id, |node| {
                node.transform.translation =
                    [(col - span / 2.0) * 8.0, (row - span / 2.0) * 8.0, 0.0];
            })
            .expect("place the part");
    }
    build.model
}

fn model_path(stem: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/models")
        .join(format!("{stem}.clm"))
}

fn load_test_model(stem: &str) -> Model {
    let path = model_path(stem);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    Model::from_clm_bytes(&bytes).expect("load model")
}

// ---------------------------------------------------------------------
// A. One submit per frame; one queue write per per-frame buffer.
// ---------------------------------------------------------------------

#[test]
fn one_submit_per_frame_regardless_of_part_count() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);

    // A flat grid of unmasked parts collapses into a single draw run, so
    // it exercises one instance-write site. The models bring composites
    // and mask sources, which split the frame across several reserve /
    // write sites. The queue-write counts must come out the same either
    // way — one per per-frame buffer, however many sites feed it.
    let cases: Vec<(&str, Model)> = vec![
        ("3 parts", grid_model(3)),
        ("40 parts", grid_model(40)),
        ("composite_masks.clm", load_test_model("composite_masks")),
        (
            "blend_modes_composite.clm",
            load_test_model("blend_modes_composite"),
        ),
    ];

    for (label, model) in cases {
        let mut renderer = stage.renderer();
        let mut rig = stage.admit(&mut renderer, model);

        let before = stage.submits;
        let (stats, fs) = stage.frame(&mut renderer, &mut rig, &mut pool);

        assert!(stats.drawn_parts > 0, "{label}: drew nothing");
        assert_eq!(
            stage.submits - before,
            1,
            "{label}: a frame must be exactly one submit",
        );
        assert_eq!(
            fs.queue_submits, 0,
            "{label}: render_list must record into the caller's encoder and submit \
             nothing of its own",
        );
        assert_eq!(
            fs.instance_buffer_writes, 1,
            "{label}: instance data must reach the GPU in one write_buffer; \
             write_buffer batches at submit start, so a second write to a live \
             offset wins for every draw reading it",
        );
        assert_eq!(
            fs.part_uniform_buffer_writes, 1,
            "{label}: part uniforms must reach the GPU in one write_buffer",
        );
        assert_eq!(
            fs.camera_buffer_writes, 1,
            "{label}: one camera ring slot written per render_list",
        );
        assert_eq!(
            fs.deform_buffer_writes, 0,
            "{label}: deforms upload from sync_deforms, outside the frame",
        );
        assert_eq!(
            fs.queue_writes, 3,
            "{label}: camera + instance + part-uniform, and nothing else",
        );
    }

    // The grid cases must really have drawn every part, so the two
    // synthetic frames above aren't quietly filtering down to one draw.
    let mut rig = {
        let mut renderer = stage.renderer();
        let rig = stage.admit(&mut renderer, grid_model(40));
        (renderer, rig)
    };
    let (stats, _) = stage.frame(&mut rig.0, &mut rig.1, &mut pool);
    assert_eq!(stats.drawn_parts, 40);
}

// ---------------------------------------------------------------------
// B. No GPU buffer grows mid-frame.
// ---------------------------------------------------------------------

#[test]
fn no_buffer_grows_mid_frame_when_the_frame_fits() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut rig = stage.admit(&mut renderer, grid_model(8));

    let (_, fs) = stage.frame(&mut renderer, &mut rig, &mut pool);
    assert_eq!(fs.instance_buffer_reallocs, 0);
    assert_eq!(fs.part_uniform_buffer_reallocs, 0);
    assert_eq!(
        fs.deform_buffer_reallocs, 0,
        "the deform atlas is sized at mesh-upload time, never inside a frame",
    );
    assert_eq!(fs.late_buffer_reallocs, 0);
}

#[test]
fn no_buffer_grows_mid_frame_when_the_frame_outgrows_capacity() {
    // Above both initial capacities, so this frame must grow the instance
    // buffer and the part-uniform buffer — once each, in the sizing phase.
    let parts = INITIAL_INSTANCE_SLOTS.max(INITIAL_PART_UNIFORM_SLOTS) + 88;

    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut rig = stage.admit(&mut renderer, grid_model(parts));

    let (_, first) = stage.frame(&mut renderer, &mut rig, &mut pool);
    assert_eq!(
        first.instance_slots_budgeted, parts as u32,
        "one instance per unmasked part",
    );
    assert_eq!(
        first.part_uniform_slots_budgeted, parts as u32,
        "one uniform slot per root drawable",
    );
    assert_eq!(
        first.instance_buffer_reallocs, 1,
        "a {parts}-part frame outgrows the {INITIAL_INSTANCE_SLOTS}-slot instance \
         buffer and must grow it exactly once",
    );
    assert_eq!(
        first.part_uniform_buffer_reallocs, 1,
        "a {parts}-part frame outgrows the {INITIAL_PART_UNIFORM_SLOTS}-slot \
         part-uniform buffer and must grow it exactly once",
    );
    assert_eq!(
        first.deform_buffer_reallocs, 0,
        "the deform atlas grew during upload_puppet, before the frame",
    );
    assert_eq!(
        first.late_buffer_reallocs, 0,
        "growth after begin_frame_* strands the passes already recorded this frame",
    );

    // Same frame again: the capacity from frame 1 must carry over.
    for round in 2..=3 {
        let (_, again) = stage.frame(&mut renderer, &mut rig, &mut pool);
        assert_eq!(
            (
                again.instance_buffer_reallocs,
                again.part_uniform_buffer_reallocs,
                again.deform_buffer_reallocs,
                again.late_buffer_reallocs,
            ),
            (0, 0, 0, 0),
            "frame {round} is the same size as frame 1 and must reallocate nothing",
        );
    }

    // …and a smaller frame afterwards must not shrink (i.e. reallocate) either.
    let mut small_renderer = stage.renderer();
    let mut small = stage.admit(&mut small_renderer, grid_model(4));
    stage.frame(&mut small_renderer, &mut small, &mut pool);
    let (_, shrunk) = stage.frame(&mut renderer, &mut rig, &mut pool);
    assert_eq!(shrunk.instance_buffer_reallocs, 0);
    assert_eq!(shrunk.part_uniform_buffer_reallocs, 0);
    assert_eq!(shrunk.late_buffer_reallocs, 0);
}

// ---------------------------------------------------------------------
// C. Cursor discipline: every reserved slot written exactly once.
// ---------------------------------------------------------------------

#[test]
fn every_reserved_slot_is_written_exactly_once() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);

    // The writers carry a debug-only guard that every instance /
    // part-uniform offset written this frame is at or above the last
    // write's end — i.e. distinct and increasing, never a rewrite. A
    // debug run of this test exercises it once per write; these
    // assertions cover the release build.
    for parts in [3usize, 7, 33] {
        let mut renderer = stage.renderer();
        let mut rig = stage.admit(&mut renderer, grid_model(parts));
        let (_, fs) = stage.frame(&mut renderer, &mut rig, &mut pool);

        assert!(
            fs.instance_slots_reserved > 0,
            "{parts}-part frame reserved no instances",
        );
        assert_eq!(
            fs.instance_slots_written, fs.instance_slots_reserved,
            "{parts}-part frame: every reserved instance slot must be written, once",
        );
        assert!(
            fs.instance_slots_reserved <= fs.instance_slots_budgeted,
            "{parts}-part frame reserved {} instance slots past the {} it sized for",
            fs.instance_slots_reserved,
            fs.instance_slots_budgeted,
        );
        assert!(
            fs.instance_bytes_written > 0
                && fs.instance_bytes_written % u64::from(fs.instance_slots_written) == 0,
            "{parts}-part frame: instance writes must be whole stride-sized slots, \
             got {} bytes over {} slots",
            fs.instance_bytes_written,
            fs.instance_slots_written,
        );
        assert_eq!(
            fs.part_uniform_writes, parts as u32,
            "{parts}-part frame: one part-uniform write per drawable",
        );
        assert!(
            fs.part_uniform_writes <= fs.part_uniform_slots_budgeted,
            "{parts}-part frame wrote {} part-uniform slots past the {} it sized for",
            fs.part_uniform_writes,
            fs.part_uniform_slots_budgeted,
        );
    }
}

#[test]
fn part_uniform_offsets_are_distinct_and_increasing() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut rig = stage.admit(&mut renderer, grid_model(4));
    stage.frame(&mut renderer, &mut rig, &mut pool);

    // `write_part_uniform` hands back the dynamic offset it wrote at.
    // Draws bind these as distinct slots, so a repeat would make two
    // draws share one uniform — the failure the cursor exists to stop.
    let offsets: Vec<u32> = (0..5)
        .map(|i| renderer.write_part_uniform(1.0 - i as f32 * 0.1, Vec3::ONE, Vec3::ZERO, 0.5))
        .collect();
    for pair in offsets.windows(2) {
        assert!(
            pair[1] > pair[0],
            "part-uniform offsets must strictly increase, got {offsets:?}",
        );
    }
    let stride = offsets[1] - offsets[0];
    assert!(stride > 0);
    for (i, off) in offsets.iter().enumerate() {
        assert_eq!(
            *off,
            offsets[0] + stride * i as u32,
            "part-uniform slots must be evenly spaced, got {offsets:?}",
        );
    }
}

// ---------------------------------------------------------------------
// D. Multi-puppet sharing of the caller-owned CompositePool.
// ---------------------------------------------------------------------

/// Render each model alone, then all of them into one frame through one
/// shared `CompositePool` + `StencilTarget`. The shared pool must end up
/// holding the deepest single model's slot count — not the sum. That
/// sharing is the difference between a viewport-sized texture per world
/// and one per puppet.
#[test]
fn shared_composite_pool_costs_the_deepest_puppet_not_the_sum() {
    let stems = [
        "composite_masks",
        "nested_composite",
        "blend_modes_composite",
    ];
    let mut stage = Stage::new();

    // Each model alone, each with its own pool.
    let mut alone = Vec::new();
    for stem in stems.iter() {
        let mut pool = CompositePool::new(W, H);
        let mut renderer = stage.renderer();
        let mut rig = stage.admit(&mut renderer, load_test_model(stem));
        let (_, fs) = stage.frame(&mut renderer, &mut rig, &mut pool);
        let (peak, capacity, _, _) = pool.stats();
        assert_eq!(fs.late_buffer_reallocs, 0);
        println!("{stem}: composite pool peak={peak} capacity={capacity}");
        alone.push((*stem, peak, capacity));
    }

    let deepest = alone.iter().map(|(_, _, cap)| *cap).max().unwrap();
    let sum: usize = alone.iter().map(|(_, _, cap)| *cap).sum();
    assert!(
        deepest > 0,
        "these models must use composite slots for the test to mean anything: {alone:?}",
    );
    assert!(
        sum > deepest,
        "at least two models must need slots, else sharing is untested: {alone:?}",
    );

    // All three in one frame, one encoder, one submit, one pool.
    let mut pool = CompositePool::new(W, H);
    let mut scenes: Vec<(WgpuRenderer, Rig)> = stems
        .iter()
        .map(|stem| {
            let mut r = stage.renderer();
            let rig = stage.admit(&mut r, load_test_model(stem));
            (r, rig)
        })
        .collect();

    let lists: Vec<RenderList> = scenes
        .iter_mut()
        .map(|(r, rig)| rig.frame(r, 0.0))
        .collect();

    let mut encoder = stage
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shared-pool-frame"),
        });
    // One camera-submit boundary for the whole frame: every renderer's
    // views live in the same submission.
    for (r, _) in scenes.iter_mut() {
        r.begin_camera_submit();
    }
    for (i, ((renderer, _), list)) in scenes.iter_mut().zip(&lists).enumerate() {
        // Only the first puppet clears; the rest compose onto it, which
        // is what makes them one frame rather than three.
        let clear = (i == 0).then_some(CLEAR);
        let (_, fs) = stage.record(renderer, &mut encoder, list, &mut pool, clear);
        assert_eq!(
            fs.late_buffer_reallocs, 0,
            "puppet {i} grew a buffer after its frame was sized",
        );
        assert_eq!(
            fs.camera_buffer_writes, 1,
            "puppet {i} must take its own camera ring slot in the shared submit",
        );
        assert_eq!(
            fs.instance_buffer_writes, 1,
            "puppet {i} owns its instance buffer and writes it once",
        );
    }
    stage.queue.submit(std::iter::once(encoder.finish()));
    stage.submits += 1;

    let (_, shared_capacity, _, _) = pool.stats();
    println!(
        "shared pool over {} puppets: capacity={shared_capacity} (deepest alone={deepest}, \
         sum of alone={sum})",
        stems.len(),
    );
    assert_eq!(
        shared_capacity,
        deepest,
        "one pool across {} puppets must allocate the deepest puppet's slots \
         ({deepest}), not the sum ({sum}): {alone:?}",
        stems.len(),
    );
}
