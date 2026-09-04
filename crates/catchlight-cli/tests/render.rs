#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `render` end to end: the binary draws a committed fixture on a headless
//! device and writes a PNG of the size it was asked for.
//!
//! This is the one suite here that needs a GPU. It runs on mesa's CPU Vulkan
//! driver (lavapipe), which the dev shell puts on the loader's search path,
//! and fails rather than skips when no adapter is there — the same choice the
//! `catchlight-wgpu` suites make, so a missing driver is loud instead of a
//! green run that tested nothing.

mod common;

use std::collections::HashSet;

/// A fixture with two parts over one another, so a correct frame is never one
/// flat colour and a blank one is caught.
const MODEL: &str = "quad_over_bg";

#[test]
fn the_binary_writes_a_png_of_the_requested_size_and_prints_the_render_list() {
    let dir = common::tmp("render-png");
    let out = dir.join("out.png");
    let (code, stdout, stderr) = common::run(&[
        "render",
        common::fixture(MODEL).to_str().unwrap(),
        out.to_str().unwrap(),
        "64",
        "96",
    ]);
    assert_eq!(code, 0, "stderr: {stderr}");

    // The render list is the inspection half of this command.
    assert!(stdout.contains("Render list: 2 root drawables"), "{stdout}");
    assert!(
        stdout.contains("=== ROOT DRAWABLES (sorted by z-order) ==="),
        "{stdout}"
    );
    assert!(stdout.contains("wrote"), "{stdout}");

    let image = image::open(&out).expect("the png decodes").to_rgba8();
    assert_eq!((image.width(), image.height()), (64, 96));

    let colours: HashSet<[u8; 4]> = image.pixels().map(|p| p.0).collect();
    assert!(
        colours.len() > 1,
        "the frame is one flat colour, so nothing was drawn: {:?}",
        colours
    );
}

#[test]
fn a_file_that_is_not_a_clm_is_refused_before_any_gpu_work() {
    let dir = common::tmp("render-not-a-clm");
    let input = dir.join("model.inx");
    std::fs::write(&input, b"not a model").unwrap();
    let (code, _, stderr) = common::run(&[
        "render",
        input.to_str().unwrap(),
        dir.join("out.png").to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "an error exits 2");
    assert!(stderr.contains("is not a .clm"), "{stderr}");
    assert!(stderr.contains("cargo xtask import"), "{stderr}");
}
