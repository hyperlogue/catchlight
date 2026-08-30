# Goal

The goal of `catchlight` is to create software stack for 2.5D character
animation based on mesh deformation.

`catchlight` is a pure Rust library. The core needs to run on Linux, Windows,
MacOS, iOS, Android, as well as Web browser through WebGPU with WebGL fallback.

# Tips

## Dev environment

The dev environment is managed by nix dev shell. It's likely already been setup
through direnv so there is no action for you to complete.
If you want to update the dev environment, look at `nix/shell.nix`.

## Make changes

Prefer making small, verifiable changes. Prioritize building test infra to make
sure all potential changes can be verified in a tight feedback loop.

## git commits

- Keep working on the current branch, unless you are told to create or jump to a
  branch.
- Before you stage, check for a concurrent committer. A non-empty index you
  didn't create means another agent is mid-commit. Run
  `git diff --cached --name-only`; if it lists anything, back off and re-check on
  a 5s → 10s → 30s schedule. Still staged after that? **Stop and ask the user** —
  never commit over another agent's staged work.
- **Commit only what you changed.** You can stage with `git add` or stage hunks
  through `git diff` and `git apply`, or with `git commit -m "<message>" --
<file_1> <file_2>` if the commit has only a few files. If staging or committing
  hits a blocker, pause and ask.
- Never amend or rewrite a commit unless the user explicitly asks.
- **Commit message** Subject: `<scope>: <imperative summary>`. lowercase, no
  trailing period, less than 72 chars. Body: (blank line, wrapped ~72) only when
  the _why_ isn't obvious from subject + diff; don't narrate the diff.

# Crate map

| Crate | What it is |
| --- | --- |
| `catchlight-core` | The runtime: `Puppet`, params/bindings, deform stacks, mesh groups, welds, physics, the `.clm` format, the inochi2d importer. No GPU, wasm-safe. |
| `catchlight-wgpu` | The wgpu rendering backend. `drawable_collector` flattens a posed puppet into a `RenderList`; `renderer` draws it. |
| `catchlight-bevy` | Bevy integration: components, systems, and a render-graph node. |
| `catchlight-editor-core` | Editable puppet model: a stable-id `EditModel` that flattens to `.clm`. Pure, wasm-safe. |
| `catchlight-editor-protocol` | Wire types: transport-agnostic request/reply/event over newline-delimited JSON. |
| `catchlight-editor-server` | Multi-session editor server: holds `EditModel`s + a warm headless renderer, driven in-process or over a Unix socket. |
| `catchlight-editor` | The editor GUI (egui): one codebase for desktop (embeds the server + socket) and web (wasm via eframe `WebRunner`). |
| `catchlight-editor-cli` | Thin client that drives an editing session over the Unix socket. |
| `visual-tests` | Visual regression harness: render a curated matrix of param/camera configs and diff against committed baselines. `publish = false`. |
| `xtask` | Workspace automation, run as `cargo xtask <cmd>`. `publish = false`. |

# Build, test, lint

CI (`.github/workflows/ci.yml`) runs the first four of these through
`nix develop -c`, so the local commands are the CI commands; the test suite is
local-only for now:

```
cargo deny check
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo build -p catchlight-core -p catchlight-wgpu --target wasm32-unknown-unknown
cargo test --workspace
```

The wasm job builds no bundle — it exists to keep the core and the renderer
compiling for the browser.

- GPU tests fall back to mesa's CPU Vulkan driver (lavapipe), which the dev
  shell puts on the loader's search path; see `create_headless_context` in
  `crates/catchlight-wgpu/src/lib.rs`.
- `tests/models/**.clm` and `tests/baselines/**.png` are Git LFS objects
  (`.gitattributes`). Without them fetched, the render suites fail loading their
  fixtures.
- **Four tests are `#[ignore]`d**, all because they need the private reference rig
  at `example_models/reference/` (three in `to_legacy.rs`, one in
  `crates/catchlight-wgpu/tests/deform_wiring.rs`). Drop a rig at that path and
  remove the attributes to run them.

# Where the invariants live

Non-obvious invariants and gotchas are documented in the `//!` doc of the module
that enforces them, not here. Add new ones there.

- `crates/catchlight-wgpu/src/renderer.rs` — buffer writes, submits, camera
  slots, masking blends, resource sharing, WebGL fallbacks
- `crates/catchlight-wgpu/src/collect.rs` — collecting drawables and z order
- `crates/catchlight-wgpu/src/render_cache.rs` — what `prepare` and `refresh`
  own, the generation gates, the Idx arena
- `crates/catchlight-wgpu/src/lib.rs` — headless context, orthographic camera
- `crates/catchlight-core/src/puppet/mod.rs` — `tick`, its caches, the
  generation gate, `settle_physics`
- `crates/catchlight-core/src/meshgroup.rs` — descent, `translateChildren`
- `crates/catchlight-core/src/physics.rs` — substeps, damping, the Y-down frame
- `crates/catchlight-core/src/interpolate.rs` — how a binding's grid is read
- `crates/catchlight-core/src/formats/` — the `.clm` container and structure
- `crates/catchlight-core/src/importer/inochi2d/mod.rs` — `.inx` reflection
- `crates/catchlight-bevy/src/lib.rs` — bevy, and one wgpu across the workspace
  (also `Cargo.toml`, at the `glam` / `wgpu` workspace deps)
- `crates/xtask/src/fixtures.rs` — hand-authored model fixtures
- `crates/visual-tests/src/lib.rs` — visual regression, updating baselines
- `crates/catchlight-core/tests/physics_trajectory.rs` — the physics baseline
- `clippy.toml`, `Cargo.toml` (`[workspace.lints]`), `deny.toml` — lint and
  dependency policy, each rationale in comments

# Tools

- `cargo run -p catchlight-wgpu --example load-model -- <model.clm> [--control]` —
  winit viewer with optional per-param sliders.
- `cargo run -p catchlight-wgpu --example render-to-png -- <model.clm> [out.png] [w] [h] [cam_h]`
  — print the render list and write a PNG.
- `cargo run -p catchlight-editor [-- <model.clm>]` — the editor GUI.
- `cargo run -p catchlight-editor-cli` — thin client for an editor session.

# Decisions

- The editor targets **WebGPU in the browser first**; the desktop app is the
  secondary target. The runtime is tier-1 on **Windows**.
- The editor-server Unix socket is permanent **Linux-only dev/agent tooling**, not
  a product surface — hence Linux-only CI.
- The removed inspection examples (`mip-compare`, `param-sweep`, `rig-dump`) are
  meant to come back as one CLI, not as more examples.
