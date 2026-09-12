# Chain solver benchmark

All hair chains use two-way coupling. `ClmPhysics::chain_substeps` sets
equal steps per frame for the whole model (default 4). Solver invariants live in
[`physics/coupled.rs`](../src/physics/coupled.rs).

```sh
cargo bench -p catchlight-core --bench chain_coupling -- frequency
cargo bench -p catchlight-core --bench chain_coupling -- quality
```

`frequency` times complete CPU puppet frames at 4, 8 and 16 steps over three
runs; an optional run number selects just that run. `timing` varies lock count
and mesh work. `trace [subdivisions]` exports joint positions and `trace-puppet`
exports driven parameters for comparisons between builds.

## Performance

Native measurements at 60 FPS: 20 locks, 20 vertices per lock, 160 px per
lock, 4 Hz bend response, damping 0.3, gravity 9,800 px/s², and max bend 14°.
Head translation is a 1 Hz sinusoid with ±10 px amplitude. Each result is
the median of three run medians: 3,000 frames after 240 warm-up frames.
Build order alternates and frequency order rotates between runs. The timed
`Puppet::tick` includes pose evaluation, physics and mesh deformation; setup,
settling and GPU work are excluded. Native timings are not browser guarantees.

Removing factor reuse, before → after (2026-09-12):

| Steps/frame | Physics rate | Two links | Eight links |
| ---: | ---: | ---: | ---: |
| 4 | 240 Hz | 0.061 → 0.059 ms | 0.151 → 0.150 ms |
| 8 | 480 Hz | 0.102 → 0.094 ms | 0.272 → 0.257 ms |
| 16 | 960 Hz | 0.187 → 0.167 ms | 0.494 → 0.494 ms |

The ±120 px stress sweep improves by 6–8% for eight-link locks. Run-to-run
noise limits small comparisons. Every substep now solves its current matrix;
geometry caching, angular state, quadratic matrix assembly and SIMD remain.

## Accuracy

Comparing 600-frame traces before and after removal at four substeps gives
a maximum difference of 0.0054 px per joint across mild/strong translation,
rotation, nonuniform scale, animated pose and a displacement kick. This is
a sample maximum, not an error bound for arbitrary content.

Current eight-link RMS tip error over 600 frames against the `quality`
command's 128-substep (7,680 Hz) numerical reference:

| Steps/frame | Mild motion | Stress motion |
| ---: | ---: | ---: |
| 4 | 0.521 px | 5.339 px |
| 8 | 0.289 px | 2.951 px |
| 16 | 0.146 px | 1.483 px |

The reference is checked against 255 substeps (15,300 Hz), differing by
0.011/0.114 px RMS for mild/stress motion. Time integration remains the
dominant error; this is not measured real hair.
