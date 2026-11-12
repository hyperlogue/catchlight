use visual_tests::{generate_configs, run_one, Config, RunOutcome, SharedHarness};

#[test]
fn lan_visual_regressions_fringe_coverage() {
    use visual_tests::render::{render_one_to_rgba, CachedModel, RenderContext};
    use visual_tests::{Camera, ParamSetting};
    let root = visual_tests::repo_root();
    let mut model =
        CachedModel::load(&root.join("tests/models/lan.clm"), 0).expect("load bundled Lan");
    // This rectangle lies inside the bangs above the right eye. Rendering at
    // 4 pixels per model unit catches thin skin-colored cracks that disappear
    // into the aggregate tolerance of the full-face baseline.
    let mut ctx = pollster::block_on(RenderContext::new(55, 135)).expect("renderer");
    for roll in [0.0, -2.0, 2.0] {
        let config = Config {
            name: format!("lan_fringe_coverage_{roll}"),
            model_stem: "lan".into(),
            params: vec![ParamSetting {
                name: "head_roll".into(),
                x: roll,
                y: 0.0,
            }],
            camera: Camera {
                x: 74.375,
                y: 833.125,
                zoom: 4.0,
            },
            frame_puppets: vec![],
            ticks_after_pose: 0,
        };
        let pixels = render_one_to_rgba(&mut ctx, &mut model, &config).expect("render fringe");
        let leaked = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] > 200 && p[1] > 125 && p[0].saturating_sub(p[1]) > 25)
            .count();
        if leaked > 2 {
            let out = visual_tests::failures_root(&root).join(&config.name);
            std::fs::create_dir_all(&out).expect("failure directory");
            image::RgbaImage::from_raw(55, 135, pixels)
                .expect("RGBA image")
                .save(out.join("actual.png"))
                .expect("save uncovered seam");
        }
        assert!(
            leaked <= 2,
            "head_roll={roll}: {leaked} skin pixels exposed inside the fringe"
        );
    }
}

#[test]
fn lan_visual_regressions() {
    let harness = SharedHarness::new().expect("failed to create visual-test harness");
    check(
        &harness,
        generate_configs()
            .into_iter()
            .filter(|c| c.model_stem == "lan"),
    );
}

#[test]
fn synthetic_visual_regressions() {
    let harness = SharedHarness::new().expect("failed to create visual-test harness");
    check(
        &harness,
        generate_configs()
            .into_iter()
            .filter(|c| c.model_stem != "lan"),
    );
}

fn check(harness: &SharedHarness, configs: impl Iterator<Item = Config>) {
    let mut failures = Vec::new();
    for config in configs {
        println!("rendering {}", config.name);
        match run_one(harness, &config) {
            Ok(RunOutcome::Pass(_)) => {}
            Ok(RunOutcome::Fail {
                metrics,
                expected,
                actual,
                diff,
                summary,
            }) => failures.push(format!(
                "{}: mean={:.3} p99={} max={} pct={:.4}%\n  expected: {}\n  actual: {}\n  diff: {}\n  summary: {}",
                config.name,
                metrics.mean,
                metrics.p99,
                metrics.max,
                metrics.pct_above_threshold,
                expected.display(),
                actual.display(),
                diff.display(),
                summary.display(),
            )),
            Err(error) => failures.push(format!("{}: {error}", config.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
