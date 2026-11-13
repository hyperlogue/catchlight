# Strand benchmark

Hair chains are root-driven strands stepped at the model's
`ClmPhysics::physics_rate` (default 120 Hz). Solver invariants live in
[`physics/strand.rs`](../src/physics/strand.rs).

```sh
cargo bench -p catchlight-core --bench strand -- frequency
cargo bench -p catchlight-core --bench strand -- quality
```

`frequency` times complete CPU puppet frames at 60, 120 and 240 Hz over three
runs; an optional run number selects just that run. `timing` varies lock count
and mesh work at 120 Hz. `quality` reports tip error against a 960 Hz run, and
the 960 Hz run against 1,000 Hz. `trace [subdivisions]` exports joint positions
and `trace-puppet` exports driven parameters for comparisons between builds.

The fixture is 20 locks of 2 or 8 links, 160 px per lock, 4 Hz stiffness,
gravity 9,800 px/s² and 20 vertices per lock when meshed, driven by a 1 Hz
head translation of ±10 or ±120 px at 60 FPS. The timed `Puppet::tick`
includes pose evaluation, physics and mesh deformation; setup, settling and
GPU work are excluded. Native timings are not browser guarantees.
