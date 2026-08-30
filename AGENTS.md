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
| `catchlight-core` | The model and the runtime: `Model` and its Ids, params/bindings, deform stacks, mesh groups, seams/welds, physics, addons, animations, `Puppet`, and the `.clm` format. No GPU, wasm-safe. |
| `catchlight-import-inochi2d` | One-time import of inochi2d `.inx` / `.inp` into a `Model`. Depends on core, never the reverse; wasm-safe. |
| `catchlight-clm` | File-level operations on a `.clm`: patch a field, swap a texture, extract or merge an addon, list its requirements, diff two files. Decodes no images. |
| `catchlight-wgpu` | The wgpu rendering backend. `render_cache` holds the GPU copy of a model; `collect` flattens a posed puppet into a `RenderList`; `renderer` draws it. |
| `catchlight-bevy` | Bevy integration: components, systems, and a render-graph node. |
| `catchlight-editor-core` | Authoring tools over a `Model`: `WorkingMesh` (triangulation, automesh) and `Manifest`. Pure, wasm-safe. |
| `catchlight-editor-protocol` | Wire types: transport-agnostic request/reply/event over newline-delimited JSON, keyed by the model's own Ids. |
| `catchlight-editor-server` | Multi-session editor server: a `Model` per session plus a warm headless renderer, driven in-process or over a Unix socket. |
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
cargo build -p catchlight-core -p catchlight-wgpu -p catchlight-import-inochi2d --target wasm32-unknown-unknown
cargo test --workspace
```

The wasm job builds no bundle — it exists to keep the core, the renderer and
the importer compiling for the browser.

- GPU tests fall back to mesa's CPU Vulkan driver (lavapipe), which the dev
  shell puts on the loader's search path; see `create_headless_context` in
  `crates/catchlight-wgpu/src/lib.rs`.
- `tests/models/**.clm` and `tests/baselines/**.png` are Git LFS objects
  (`.gitattributes`). Without them fetched, the render suites fail loading their
  fixtures.
- **Five tests are `#[ignore]`d.** Four need the private reference model at
  `example_models/reference/` — three in
  `crates/catchlight-import-inochi2d/src/to_clm.rs`, one in
  `crates/catchlight-wgpu/tests/deform_wiring.rs`; drop a model at that path and
  remove the attributes to run them. The fifth is a timing measurement in
  `crates/catchlight-core/src/model/mod.rs`.

# Where the invariants live

Non-obvious invariants and gotchas are documented in the `//!` doc of the module
that enforces them, not here. Add new ones there.

- `crates/catchlight-core/src/id.rs` — the Id charset, what a `/` in one does
  not mean, `Name` is never a key
- `crates/catchlight-core/src/model/mod.rs` — the generation clock, the identity
  nonce, what a Model's tree always holds
- `crates/catchlight-core/src/model/file.rs` — writing is total, reading trusts
  nothing, complete model vs addon fragment
- `crates/catchlight-core/src/model/addon.rs` — what an addon may provide, and
  `install` against `extract`
- `crates/catchlight-core/src/puppet/mod.rs` — `tick`'s one order, the
  generation gate, what a rebake carries, `settle_physics`
- `crates/catchlight-core/src/puppet/bake.rs` — Ids become slots, and what the
  hot loops never look up
- `crates/catchlight-core/src/meshgroup.rs` — descent, `translate_children`
- `crates/catchlight-core/src/physics.rs` — substeps, damping, the Y-down frame
- `crates/catchlight-core/src/interpolate.rs` — how a binding's grid is read
- `crates/catchlight-core/src/texture.rs` — the whole texture strategy: decode,
  premultiply, alpha crop, the UV crop it hands back
- `crates/catchlight-core/src/load.rs` — `.clm` is the only load path
- `crates/catchlight-core/src/formats/clm.rs` — the `.clm` document: keyed by
  Id, byte-stable, and what it refuses
- `crates/catchlight-import-inochi2d/src/lib.rs` — the single reflection, Ids
  minted from position, the reader is total
- `crates/catchlight-clm/src/lib.rs` — why the file ops are their own binary,
  and what "no image is decoded" rests on
- `crates/catchlight-wgpu/src/render_cache.rs` — what `prepare` and `refresh`
  own, the generation gate, the Idx arena
- `crates/catchlight-wgpu/src/collect.rs` — z order, composites, and slots
  rather than Ids
- `crates/catchlight-wgpu/src/renderer.rs` — buffer writes, submits, camera
  slots, masking blends, resource sharing, WebGL fallbacks
- `crates/catchlight-wgpu/src/lib.rs` — on `create_headless_context`, the
  backend and adapter choice; on `create_orthographic_camera_at`, that the
  camera holds no axis flip
- `crates/catchlight-editor-protocol/src/lib.rs` — Ids on the wire, the document
  path against the presence path
- `crates/catchlight-editor-server/src/lib.rs` — a drag never snapshots, the
  undo budget, one render cache per previewed session
- `crates/catchlight-editor/src/app.rs` — drag against commit, what recording
  never authors
- `crates/catchlight-editor/src/mesh_edit.rs` — when the seam tool is reachable,
  seam edits are document edits
- `crates/catchlight-editor/src/params_panel.rs` — a param is a scalar, a row
  addresses its own binding
- `crates/catchlight-editor/src/viewport.rs` — no readback, one renderer holding
  one session's cache
- `crates/catchlight-bevy/src/lib.rs` — who owns model, puppet and cache; bevy,
  and one wgpu across the workspace (also `Cargo.toml`, at the `glam` / `wgpu`
  workspace deps)
- `crates/xtask/src/fixtures.rs` — hand-authored model fixtures
- `crates/visual-tests/src/lib.rs` — visual regression, updating baselines
- `crates/catchlight-core/tests/evaluated_frame.rs` — the frame baseline, and
  where its numbers came from
- `crates/catchlight-core/tests/physics_trajectory.rs` — the physics baseline
- `clippy.toml`, `Cargo.toml` (`[workspace.lints]`), `deny.toml` — lint and
  dependency policy, each rationale in comments

# Tools

`.clm` is the only model file catchlight loads; convert an inochi2d export once
with `cargo xtask import <model.inx|.inp> [-o <model.clm>]`.

- `cargo run -p catchlight-wgpu --example load-model -- <model.clm> [--control]` —
  winit viewer with optional per-param sliders.
- `cargo run -p catchlight-wgpu --example render-to-png -- <model.clm> [out.png] [w] [h] [cam_h]`
  — print the render list and write a PNG.
- `cargo run -p catchlight-editor [-- <model.clm>]` — the editor GUI.
- `cargo run -p catchlight-editor-cli` — thin client for an editor session.
- `cargo run -p catchlight-clm -- <patch|replace-texture|extract|merge|requirements|diff>`
  — file-level operations on a `.clm`; `--help` documents the Id charset and the
  exit statuses.

# Decisions

- The editor targets **WebGPU in the browser first**; the desktop app is the
  secondary target. The runtime is tier-1 on **Windows**.
- The editor-server Unix socket is permanent **Linux-only dev/agent tooling**, not
  a product surface — hence Linux-only CI.
- The removed inspection examples (`mip-compare`, `param-sweep`, `rig-dump`) are
  meant to come back as one CLI, not as more examples.
