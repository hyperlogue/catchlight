#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Opt-in frame-budget check for the public demo's actual model.
//! Run `cargo test -p catchlight-core --release --test demo_perf -- --ignored --nocapture`.
//! Requires the Lan Git LFS object. Timing is excluded from ordinary CI because
//! shared runners and debug builds do not provide a meaningful frame budget.
use catchlight_core::{formats::clm, Model, ParamId, Puppet};

#[test]
#[ignore = "timing measurement; run in release with the Lan LFS fixture"]
fn lan_pose_tick() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/models/lan.clm"
    ))
    .unwrap();
    let file = clm::decode(&bytes).unwrap();
    let model = Model::from_clm_file(&file).unwrap();
    let mut puppet = Puppet::new(&model);
    let param = ParamId::new("body_yaw").unwrap();
    assert!(
        model.param(&param).is_some(),
        "the demo slider must drive a param"
    );
    let mut times = Vec::new();
    for i in 0..30 {
        puppet.set_param_value(&param, (i as f32 * 0.2).sin() * 10.);
        let start = std::time::Instant::now();
        puppet.tick(&model, 1. / 60.);
        times.push(start.elapsed().as_secs_f64() * 1000.);
    }
    times.sort_by(f64::total_cmp);
    eprintln!("tick ms: median {:.2}, max {:.2}", times[15], times[29]);
    assert!(times[15] < 16., "a pose tick exceeds the frame budget");
}
