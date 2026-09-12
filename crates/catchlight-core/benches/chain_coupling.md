# Chain solver benchmark

Enable coupling with `Puppet::set_chain_solver(ChainSolver::Direct)`.
`OneWay` remains the default. Solver invariants live in
[`physics/coupled.rs`](../src/physics/coupled.rs).

```sh
cargo bench -p catchlight-core --bench chain_coupling -- frequency
cargo bench -p catchlight-core --bench chain_coupling -- quality
```

`frequency [repeat]` times complete CPU puppet frames; `timing` also compares
the one-way solver. `trace [subdivisions]` exports joint positions and
`trace-puppet` exports driven parameters for comparisons between builds.

## Performance

Native measurements at 60 FPS: 20 locks, 20 vertices per lock, 160 px per
lock, 4 Hz bend response, damping 0.3, gravity 9,800 px/s², and max bend 14°.
Head translation is a 1 Hz sinusoid with ±10 px amplitude. Each result is
the median of three run medians: 3,000 frames after 240 warm-up frames.
Variant/frequency order rotates between runs. Posing, settling and GPU work
are excluded. These shared-environment timings are not browser guarantees.

| Physics rate | Two links, before → after | Eight links, before | Optimized, no reuse | Optimized + reuse |
| ---: | ---: | ---: | ---: | ---: |
| 240 Hz | 0.068 → 0.061 ms | 0.227 ms | 0.164 ms | 0.154 ms |
| 480 Hz | 0.116 → 0.095 ms | 0.418 ms | 0.281 ms | 0.266 ms |
| 960 Hz | 0.220 → 0.178 ms | 0.771 ms | 0.516 ms | 0.496 ms |

Caching, angular state, quadratic matrix assembly and SIMD account for most
of the gain. Guarded factor reuse saves another 3.8–6.1% in mild motion,
but costs 0.6–3.1% extra in the ±120 px stress sweep. Two-link solves do not
reuse factors. The production step limit remains 1/240 s; higher rates were
measured in isolated builds.

## Accuracy

Tests compare 600-frame traces of mild/strong translation, rotation,
nonuniform scale, animated pose and a displacement kick. Across all tested
rates, optimizations without reuse differ from the original by at most
0.0096 px per joint; SIMD and scalar exported bends match exactly. Reuse
adds at most 0.043 px or 0.023° of bend versus optimized exact solves.
These are sample maxima, not error bounds for arbitrary content.

Against a 7,680 Hz numerical reference, final eight-link RMS tip error is
0.520/5.339 px for mild/stress motion at 240 Hz and 0.149/1.486 px at 960 Hz.
The reference itself differs from 3,840 Hz by 0.022/0.225 px RMS. Time
integration remains the dominant error; this is not measured real hair.

The local comparison scripts and original source snapshot are in
`workspace/chain-coupling/`; raw traces and timings are under its
`optimizations/` directory. That workspace is ignored scratch.
