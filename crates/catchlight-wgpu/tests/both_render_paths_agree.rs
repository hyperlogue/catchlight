#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The legacy path and the render cache must draw the same pixels.
//!
//! `visual-tests` still drives a `LegacyPuppet` through `collect_drawables`,
//! so it is the control: it pins the legacy path against committed baselines,
//! and this pins the render cache against the legacy path. Between them the
//! new path inherits the baselines without a second set of PNGs to keep in
//! step — and when cl-32i.8 moves `visual-tests` over, the two halves meet.
//!
//! Byte equality, not a tolerance. The two runtimes fold the same numbers in
//! the same order (`puppet_equivalence.rs` in core proves that per node), and
//! both hand the renderer the same vertex data and the same textures, so
//! anything that shows up here is a real difference rather than drift.
//!
//! What legitimately differs is slot numbering: mesh slots are dense in the
//! cache and node-indexed in the legacy puppet, and the model's root replaces
//! the legacy runtime's synthetic one, so every node slot shifts by one. None
//! of that is visible in a pixel.

mod common;

use catchlight_core::{LegacyPuppet, Model, ModelFormat, Puppet, Vec2};
use catchlight_wgpu::{collect_drawables, create_orthographic_camera, RenderContext, WgpuRenderer};
use common::{Rig, NO_ADAPTER};
use std::collections::HashMap;
use std::path::PathBuf;

const W: u32 = 512;
const H: u32 = 512;

/// White, so a pixel one path drops reads as a difference instead of blending
/// into the background.
const CLEAR: Option<wgpu::Color> = Some(wgpu::Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
});

/// `(fixture, camera height)`. The heights are `visual-tests`' own framing for
/// each model — `height / default_zoom` — so these render what the baselines
/// render.
const CASES: &[(&str, f32)] = &[
    ("composite_masks", 512.0),
    ("blend_modes_composite", 512.0),
    ("welded_seam", 512.0),
    ("mip_checker", 2048.0),
];

/// Poses to compare at, as a fraction across every param's range. `None` is
/// "leave every param at its default".
const POSES: &[Option<f32>] = &[None, Some(0.0), Some(0.5), Some(1.0)];

fn model_path(stem: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/models")
        .join(format!("{stem}.clm"))
}

/// Put every param `frac` of the way across its range, on both runtimes.
///
/// A model splits a legacy 2-D param into `<name>.x` / `<name>.y`, so the
/// model's params are the finer set and each legacy value is assembled from
/// them. Nothing is addressed by Id here on purpose: names are the only thing
/// the two runtimes share.
fn pose_both(model: &Model, puppet: &mut Puppet, legacy: &mut LegacyPuppet, frac: f32) {
    let mut legacy_values: HashMap<String, Vec2> = HashMap::new();
    for id in model.param_ids() {
        let Some(param) = model.param(id) else {
            continue;
        };
        let value = param.min + (param.max - param.min) * frac;
        puppet.set_param_value(id, value);

        let name = param.name.as_str();
        let (base, axis) = match name.strip_suffix(".x") {
            Some(base) => (base, 0),
            None => match name.strip_suffix(".y") {
                Some(base) => (base, 1),
                None => (name, 0),
            },
        };
        let entry = legacy_values.entry(base.to_string()).or_insert(Vec2::ZERO);
        entry[axis] = value;
    }
    for (name, value) in legacy_values {
        assert!(
            legacy.set_param_value_by_name(&name, value),
            "the legacy runtime has no param named {name}",
        );
    }
}

async fn render_both(stem: &str, camera_height: f32, frac: Option<f32>) -> (Vec<u8>, Vec<u8>) {
    let path = model_path(stem);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let format = ModelFormat::from_path(&path).expect("recognized model extension");

    let mut legacy = catchlight_core::load_model(&bytes, format, 0).expect("legacy load");
    let model = Model::from_clm_bytes(&bytes).expect("model load");
    let mut puppet = Puppet::new(&model);
    if let Some(frac) = frac {
        pose_both(&model, &mut puppet, &mut legacy, frac);
    }

    let camera = create_orthographic_camera(camera_height, W as f32 / H as f32);

    // Legacy: LegacyPuppet -> upload_puppet -> sync_deforms -> collect_drawables.
    let legacy_pixels = {
        let (device, queue) = catchlight_wgpu::create_headless_context()
            .await
            .expect(NO_ADAPTER);
        let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await;
        let mut ctx = RenderContext::with_renderer(renderer, W, H).expect("render context");
        ctx.renderer.upload_puppet(&legacy).expect("upload puppet");
        ctx.renderer.update_camera(camera);
        legacy.settle_physics();
        legacy.tick(&mut ctx.transforms, glam::Mat4::IDENTITY, 0.0);
        ctx.renderer.sync_deforms(&legacy);
        let render_list = collect_drawables(&legacy, &ctx.transforms);
        ctx.render(&render_list, CLEAR).expect("legacy render");
        ctx.read_rgba().expect("legacy readback")
    };

    // New: Model + Puppet + RenderCache -> collect.
    let cache_pixels = {
        let (device, queue) = catchlight_wgpu::create_headless_context()
            .await
            .expect(NO_ADAPTER);
        let renderer = WgpuRenderer::new(device, queue, wgpu::TextureFormat::Rgba8UnormSrgb).await;
        let mut ctx = RenderContext::with_renderer(renderer, W, H).expect("render context");
        let mut rig = Rig::new(&mut ctx.renderer, model);
        rig.puppet = puppet;
        ctx.renderer.update_camera(camera);
        rig.puppet.settle_physics(&rig.model);
        let render_list = rig.frame(&mut ctx.renderer, 0.0);
        ctx.render(&render_list, CLEAR).expect("cache render");
        ctx.read_rgba().expect("cache readback")
    };

    (legacy_pixels, cache_pixels)
}

/// Summarize a byte difference the way a reader can act on: how many pixels
/// differ at all, and by how much at worst.
fn describe(legacy: &[u8], cached: &[u8]) -> Option<String> {
    let mut differing = 0usize;
    let mut worst = 0u8;
    for (a, b) in legacy
        .as_chunks::<4>()
        .0
        .iter()
        .zip(cached.as_chunks::<4>().0)
    {
        let delta = (0..4).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
        if delta > 0 {
            differing += 1;
            worst = worst.max(delta);
        }
    }
    (differing > 0).then(|| {
        format!(
            "{differing} of {} pixels differ, worst channel delta {worst}",
            legacy.len() / 4,
        )
    })
}

#[test]
fn both_render_paths_produce_the_same_pixels() {
    let mut failures = Vec::new();
    for &(stem, camera_height) in CASES {
        for pose in POSES {
            let (legacy, cached) = pollster::block_on(render_both(stem, camera_height, *pose));
            assert_eq!(
                legacy.len(),
                cached.len(),
                "{stem}: the two paths read back different buffer sizes",
            );
            let label = match pose {
                None => format!("{stem} @ defaults"),
                Some(frac) => format!("{stem} @ {frac} of range"),
            };
            if let Some(summary) = describe(&legacy, &cached) {
                failures.push(format!("{label}: {summary}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the render cache and the legacy path disagree:\n  {}",
        failures.join("\n  "),
    );
}
