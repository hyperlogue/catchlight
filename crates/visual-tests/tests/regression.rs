
use visual_tests::{generate_configs, run_one, RunOutcome, SharedHarness};

#[test]
fn visual_regressions() {
    let configs = generate_configs();
    let harness = SharedHarness::new().expect("failed to create visual-test harness");
    let mut failures = Vec::new();
    for config in configs {
        match run_one(&harness, &config) {
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
