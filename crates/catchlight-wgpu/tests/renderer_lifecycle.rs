#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Resource-lifecycle invariants of `WgpuRenderer`, asserted on calls and
//! buffer state: each breaks only once a frame grows, so no pixel baseline
//! catches it.

mod common;

use catchlight_core::{Mesh, Model};
use catchlight_wgpu::{
    create_orthographic_camera, CompositePool, FrameStats, FramebufferSnapshotPool, Pipelines,
    RenderList, RenderStats, RendererError, StencilTarget, Submission, WgpuRenderer,
    CAMERA_RING_SLOTS,
};
use common::Scene;
use std::path::PathBuf;
use std::sync::Arc;

const W: u32 = 256;
const H: u32 = 256;

/// Matches the visual-test baselines' zoom, scaled to this viewport.
const CAMERA_HEIGHT: f32 = 512.0;

/// Initial capacities in `WgpuRenderer::from_pipelines`; keep in sync.
const INITIAL_INSTANCE_SLOTS: usize = 512;
const INITIAL_PART_UNIFORM_SLOTS: usize = 256;

struct Stage {
    device: wgpu::Device,
    queue: wgpu::Queue,
    shared: Arc<Pipelines>,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    stencil: StencilTarget,
    snapshots: FramebufferSnapshotPool,
    /// Counted by hand: the renderer's submits plus the test's own.
    submits: u32,
}

impl Stage {
    fn new() -> Self {
        let (device, queue) = common::shared_context();
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

    fn renderer(&self) -> WgpuRenderer {
        WgpuRenderer::from_pipelines(self.device.clone(), self.queue.clone(), self.shared.clone())
    }

    fn admit(&self, renderer: &mut WgpuRenderer, model: Model) -> Scene {
        let scene = Scene::new(renderer, model);
        renderer.update_camera(create_orthographic_camera(
            CAMERA_HEIGHT,
            W as f32 / H as f32,
        ));
        scene
    }

    /// No submit: the caller owns the frame boundary.
    fn record(
        &mut self,
        renderer: &mut WgpuRenderer,
        submission: &mut Submission,
        encoder: &mut wgpu::CommandEncoder,
        render_list: &RenderList,
        composites: &mut CompositePool,
        clear: Option<wgpu::Color>,
    ) -> (RenderStats, FrameStats) {
        let stats = self
            .try_record(
                renderer,
                submission,
                encoder,
                render_list,
                composites,
                clear,
            )
            .expect("render_into");
        (stats, renderer.frame_stats())
    }

    fn try_record(
        &mut self,
        renderer: &mut WgpuRenderer,
        submission: &mut Submission,
        encoder: &mut wgpu::CommandEncoder,
        render_list: &RenderList,
        composites: &mut CompositePool,
        clear: Option<wgpu::Color>,
    ) -> Result<RenderStats, RendererError> {
        renderer.render_into(
            submission,
            &[render_list],
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
    }

    fn frame(
        &mut self,
        renderer: &mut WgpuRenderer,
        scene: &mut Scene,
        composites: &mut CompositePool,
    ) -> (RenderStats, FrameStats) {
        let render_list = scene.frame(renderer, 0.0);
        let done = renderer
            .frame()
            .render_ext(
                &[&render_list],
                &self.view,
                &self.stencil,
                composites,
                Some(&self.target),
                Some(&mut self.snapshots),
                W,
                H,
                Some(CLEAR),
            )
            .expect("render");
        let stats = done.stats();
        done.submit();
        self.submits += 1;
        (stats, renderer.frame_stats())
    }

    fn readback(&self) -> Vec<u8> {
        pollster::block_on(catchlight_wgpu::read_texture_to_rgba(
            &self.device,
            &self.queue,
            &self.target,
            W,
            H,
        ))
        .expect("readback")
    }
}

const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

/// One instance and one part-uniform slot per quad.
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

#[test]
fn one_submit_per_frame_regardless_of_part_count() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);

    // `write_buffer` batches at submit start, so each per-frame buffer must
    // take exactly one queue write however many sites feed it.
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
        let mut scene = stage.admit(&mut renderer, model);

        let before = stage.submits;
        let (stats, fs) = stage.frame(&mut renderer, &mut scene, &mut pool);

        assert!(stats.drawn_parts > 0, "{label}: drew nothing");
        assert_eq!(
            stage.submits - before,
            1,
            "{label}: a frame must be exactly one submit",
        );
        assert_eq!(
            fs.queue_submits, 0,
            "{label}: a frame's own submit is not counted, so anything here is a \
             second submit the frame did not need",
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
            "{label}: one camera slot written per frame",
        );
        assert_eq!(
            fs.deform_buffer_writes, 0,
            "{label}: deforms upload from upload_deforms, outside the frame",
        );
        assert_eq!(
            fs.queue_writes, 3,
            "{label}: camera + instance + part-uniform, and nothing else",
        );
    }

    let mut scene = {
        let mut renderer = stage.renderer();
        let scene = stage.admit(&mut renderer, grid_model(40));
        (renderer, scene)
    };
    let (stats, _) = stage.frame(&mut scene.0, &mut scene.1, &mut pool);
    assert_eq!(stats.drawn_parts, 40);
}

#[test]
fn no_buffer_grows_mid_frame_when_the_frame_fits() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut scene = stage.admit(&mut renderer, grid_model(8));

    let (_, fs) = stage.frame(&mut renderer, &mut scene, &mut pool);
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
    // Must grow both buffers once each, in the sizing phase: a mid-frame
    // reallocation strands recorded passes on the freed buffer.
    let parts = INITIAL_INSTANCE_SLOTS.max(INITIAL_PART_UNIFORM_SLOTS) + 88;

    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut scene = stage.admit(&mut renderer, grid_model(parts));

    let (_, first) = stage.frame(&mut renderer, &mut scene, &mut pool);
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
        "the deform atlas grew before the frame, with the meshes",
    );
    assert_eq!(
        first.late_buffer_reallocs, 0,
        "growth after begin_frame_* strands the passes already recorded this frame",
    );

    for round in 2..=3 {
        let (_, again) = stage.frame(&mut renderer, &mut scene, &mut pool);
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

    let mut small_renderer = stage.renderer();
    let mut small = stage.admit(&mut small_renderer, grid_model(4));
    stage.frame(&mut small_renderer, &mut small, &mut pool);
    let (_, shrunk) = stage.frame(&mut renderer, &mut scene, &mut pool);
    assert_eq!(shrunk.instance_buffer_reallocs, 0);
    assert_eq!(shrunk.part_uniform_buffer_reallocs, 0);
    assert_eq!(shrunk.late_buffer_reallocs, 0);
}

#[test]
fn every_reserved_slot_is_written_exactly_once() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);

    for parts in [3usize, 7, 33] {
        let mut renderer = stage.renderer();
        let mut scene = stage.admit(&mut renderer, grid_model(parts));
        let (_, fs) = stage.frame(&mut renderer, &mut scene, &mut pool);

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

/// A shared pool must hold the deepest single model's slot count, not the sum.
#[test]
fn shared_composite_pool_costs_the_deepest_puppet_not_the_sum() {
    let stems = [
        "composite_masks",
        "nested_composite",
        "blend_modes_composite",
    ];
    let mut stage = Stage::new();

    let mut alone = Vec::new();
    for stem in stems.iter() {
        let mut pool = CompositePool::new(W, H);
        let mut renderer = stage.renderer();
        let mut scene = stage.admit(&mut renderer, load_test_model(stem));
        let (_, fs) = stage.frame(&mut renderer, &mut scene, &mut pool);
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

    let mut pool = CompositePool::new(W, H);
    let mut scenes: Vec<(WgpuRenderer, Scene)> = stems
        .iter()
        .map(|stem| {
            let mut r = stage.renderer();
            let scene = stage.admit(&mut r, load_test_model(stem));
            (r, scene)
        })
        .collect();

    let lists: Vec<RenderList> = scenes
        .iter_mut()
        .map(|(r, scene)| scene.frame(r, 0.0))
        .collect();

    let mut encoder = stage
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shared-pool-frame"),
        });
    for (i, ((renderer, _), list)) in scenes.iter_mut().zip(&lists).enumerate() {
        let clear = (i == 0).then_some(CLEAR);
        let mut submission = renderer.submission();
        let (_, fs) = stage.record(
            renderer,
            &mut submission,
            &mut encoder,
            list,
            &mut pool,
            clear,
        );
        assert_eq!(
            fs.late_buffer_reallocs, 0,
            "puppet {i} grew a buffer after its frame was sized",
        );
        assert_eq!(
            fs.camera_buffer_writes, 1,
            "puppet {i} must take a camera slot of its own in the shared submit",
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

/// Reuse across submissions is safe (queue order), so every owned frame binds
/// slot 0 and a long-running loop never exhausts the ring.
#[test]
fn every_owned_frame_binds_camera_slot_zero() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut scene = stage.admit(&mut renderer, grid_model(6));

    let (stats, _) = stage.frame(&mut renderer, &mut scene, &mut pool);
    assert!(stats.drawn_parts > 0, "frame 0 drew nothing");
    assert_eq!(renderer.camera_dynamic_offset(), 0, "frame 0 is slot 0");
    let first = stage.readback();

    let frames = CAMERA_RING_SLOTS + 2;
    let mut last = Vec::new();
    for i in 1..frames {
        let (stats, fs) = stage.frame(&mut renderer, &mut scene, &mut pool);
        assert!(stats.drawn_parts > 0, "frame {i} drew nothing");
        assert_eq!(
            fs.camera_buffer_writes, 1,
            "frame {i} must write exactly one camera slot",
        );
        assert_eq!(
            renderer.camera_dynamic_offset(),
            0,
            "frame {i} owns its submit, so it must bind slot 0 like every other",
        );
        last = stage.readback();
    }

    assert_eq!(
        stage.submits, frames,
        "one submit per frame, {frames} frames",
    );
    assert_eq!(
        first,
        last,
        "frame {} is past where the old ring wrapped and must match frame 0 \
         pixel for pixel",
        frames - 1,
    );
}

/// Two views in one submission must read different slots, or `write_buffer`
/// batching makes the second camera win for both.
#[test]
fn two_views_in_one_submission_bind_distinct_camera_slots() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut scene = stage.admit(&mut renderer, grid_model(6));
    let list = scene.frame(&mut renderer, 0.0);

    let mut encoder = stage
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("two-view-frame"),
        });

    let mut submission = renderer.submission();
    let aspect = W as f32 / H as f32;
    renderer.update_camera(create_orthographic_camera(CAMERA_HEIGHT, aspect));
    stage.record(
        &mut renderer,
        &mut submission,
        &mut encoder,
        &list,
        &mut pool,
        Some(CLEAR),
    );
    let first = renderer.camera_dynamic_offset();

    renderer.update_camera(create_orthographic_camera(CAMERA_HEIGHT * 2.0, aspect));
    stage.record(
        &mut renderer,
        &mut submission,
        &mut encoder,
        &list,
        &mut pool,
        None,
    );
    let second = renderer.camera_dynamic_offset();

    stage.queue.submit(std::iter::once(encoder.finish()));
    stage.submits += 1;

    assert_ne!(
        first, second,
        "two views sharing a submission must bind distinct camera offsets",
    );
    assert_eq!(submission.views_issued(), 2, "two views, two slots");
}

/// Views in one submit cull differently, so their instance and part-uniform
/// layouts differ; the second view must not overwrite the first's bytes.
#[test]
fn two_views_in_one_submission_keep_their_own_draw_data() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut scene = stage.admit(&mut renderer, grid_model(6));
    let list = scene.frame(&mut renderer, 0.0);

    let whole = create_orthographic_camera(64.0, W as f32 / H as f32);
    // Frames only the grid's last column.
    let partial = catchlight_wgpu::create_orthographic_camera_at(
        64.0,
        W as f32 / H as f32,
        catchlight_core::Vec2::new(34.0, 0.0),
    );
    let other = stage.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("second-view-target"),
        size: stage.target.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: stage.target.format(),
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let other_view = other.create_view(&wgpu::TextureViewDescriptor::default());

    let mut render = |stage: &mut Stage, second_view: bool| {
        let mut encoder = stage
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let mut submission = renderer.submission();
        renderer.update_camera(whole);
        let first = stage.record(
            &mut renderer,
            &mut submission,
            &mut encoder,
            &list,
            &mut pool,
            Some(CLEAR),
        );
        assert_eq!(first.0.drawn_parts, 6);
        if second_view {
            renderer.update_camera(partial);
            let second = renderer
                .render_into(
                    &mut submission,
                    &[&list],
                    &mut encoder,
                    &other_view,
                    &stage.stencil,
                    &mut pool,
                    Some(&other),
                    Some(&mut stage.snapshots),
                    W,
                    H,
                    Some(CLEAR),
                )
                .expect("render the second view");
            assert_eq!(second.drawn_parts, 2, "the second view culls four parts");
        }
        stage.queue.submit(std::iter::once(encoder.finish()));
        stage.readback()
    };

    let alone = render(&mut stage, false);
    let shared = render(&mut stage, true);
    assert!(
        alone == shared,
        "a second view in the submit changed what the first view drew",
    );
}

/// The view past the ring is refused before anything is recorded.
#[test]
fn a_submission_past_the_ring_is_refused_and_records_nothing() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut scene = stage.admit(&mut renderer, grid_model(4));
    let list = scene.frame(&mut renderer, 0.0);

    let mut encoder = stage
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ring-bound-frame"),
        });
    let mut submission = renderer.submission();
    for view in 0..CAMERA_RING_SLOTS {
        let clear = (view == 0).then_some(CLEAR);
        stage.record(
            &mut renderer,
            &mut submission,
            &mut encoder,
            &list,
            &mut pool,
            clear,
        );
    }
    assert_eq!(submission.views_issued(), CAMERA_RING_SLOTS);

    let before = renderer.frame_stats();
    let refused = stage.try_record(
        &mut renderer,
        &mut submission,
        &mut encoder,
        &list,
        &mut pool,
        None,
    );
    match refused {
        Err(RendererError::CameraViewsExhausted { limit }) => {
            assert_eq!(limit, CAMERA_RING_SLOTS);
        }
        other => panic!("the view past the ring must be refused, got {other:?}"),
    }
    assert_eq!(
        renderer.frame_stats(),
        before,
        "a refused view must record nothing: the counters are the last frame's",
    );
    assert_eq!(
        submission.views_issued(),
        CAMERA_RING_SLOTS,
        "a refused view must not be counted as issued",
    );

    stage.queue.submit(std::iter::once(encoder.finish()));
    stage.submits += 1;
}

#[test]
fn a_submission_from_another_renderer_is_refused() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut a = stage.renderer();
    let mut b = stage.renderer();
    let mut scene = stage.admit(&mut a, grid_model(4));
    let list = scene.frame(&mut a, 0.0);
    let _ = stage.admit(&mut b, grid_model(4));

    let mut encoder = stage
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("foreign-token-frame"),
        });
    let mut theirs = a.submission();
    let refused = stage.try_record(&mut b, &mut theirs, &mut encoder, &list, &mut pool, None);
    assert!(
        matches!(refused, Err(RendererError::ForeignSubmission)),
        "a token from another renderer must be refused, got {refused:?}",
    );
    assert_eq!(
        theirs.views_issued(),
        0,
        "a refused token must not have been spent",
    );
}

#[test]
fn a_dropped_frame_never_reaches_the_queue() {
    let mut stage = Stage::new();
    let mut pool = CompositePool::new(W, H);
    let mut renderer = stage.renderer();
    let mut scene = stage.admit(&mut renderer, grid_model(6));

    stage.frame(&mut renderer, &mut scene, &mut pool);
    let drawn = stage.readback();
    let submits = stage.submits;

    // The binding silences `#[must_use]`; the drop is the point.
    let list = scene.frame(&mut renderer, 0.0);
    {
        let _dropped = renderer
            .frame()
            .render_ext(
                &[&list],
                &stage.view,
                &stage.stencil,
                &mut pool,
                Some(&stage.target),
                Some(&mut stage.snapshots),
                W,
                H,
                Some(wgpu::Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
            )
            .expect("render");
    }

    assert_eq!(
        stage.submits, submits,
        "dropping a recorded frame must submit nothing",
    );
    assert_eq!(
        stage.readback(),
        drawn,
        "the target must still hold the submitted frame: the dropped one's \
         clear never ran",
    );
}
