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
| `catchlight-core` | The runtime: `Puppet`, params/bindings, deform stacks, mesh groups, welds, physics, the `.clp` format, the inochi2d importer. No GPU, wasm-safe. |
| `catchlight-wgpu` | The wgpu rendering backend. `drawable_collector` flattens a posed puppet into a `RenderList`; `renderer` draws it. |
| `catchlight-bevy` | Bevy integration: components, systems, and a render-graph node. |
| `catchlight-editor-core` | Editable puppet model: a stable-id `EditModel` that flattens to `.clp`. Pure, wasm-safe. |
| `catchlight-editor-protocol` | Wire types: transport-agnostic request/reply/event over newline-delimited JSON. |
| `catchlight-editor-server` | Multi-session editor server: holds `EditModel`s + a warm headless renderer, driven in-process or over a Unix socket. |
| `catchlight-editor` | The editor GUI (egui): one codebase for desktop (embeds the server + socket) and web (wasm via eframe `WebRunner`). |
| `catchlight-editor-cli` | Thin client that drives an editing session over the Unix socket. |
| `visual-tests` | Visual regression harness: render a curated matrix of param/camera configs and diff against committed baselines. `publish = false`. |
| `xtask` | Workspace automation, run as `cargo xtask <cmd>`. `publish = false`. |

# Build, test, lint

CI (`.github/workflows/ci.yml`) runs each of these through `nix develop -c`, so
the local commands are the CI commands:

```
cargo deny check
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
VK_ICD_FILENAMES=$CATCHLIGHT_LAVAPIPE_ICD cargo test --workspace
cargo build -p catchlight-core -p catchlight-wgpu --target wasm32-unknown-unknown
```

CI is Linux-only on purpose: `catchlight-editor-cli` talks over a Unix socket,
so a Windows/macOS runner buys nothing yet. The wasm job builds no bundle — it
exists to keep the core and the renderer compiling for the browser.

`tests/models/**.clp` and `tests/baselines/**.png` are Git LFS objects
(`.gitattributes`). Without them fetched, the render suites fail loading their
fixtures.

## Native headless rendering

`create_headless_context` (`crates/catchlight-wgpu/src/lib.rs`) requests
`Backends::PRIMARY` — deliberately not `all()`, because the `webgl` feature
unifies `wgc/gles` across the workspace and GL then tries to init EGL and panics
headless.

On a box with no GPU, point the Vulkan loader at mesa's CPU ICD (lavapipe).
`nix/shell.nix` exports the path as `CATCHLIGHT_LAVAPIPE_ICD` but deliberately
does **not** set `VK_ICD_FILENAMES`, which would force lavapipe over a real
driver for everyone in the shell. Set it per-command:

```
VK_ICD_FILENAMES=$CATCHLIGHT_LAVAPIPE_ICD cargo test --workspace
```

Without it, `request_adapter` fails and every GPU test panics with

```
NotFound { active_backends: Backends(0x0), ... }
```

`vulkaninfo --summary` shows which ICD the loader actually picked.

## Lints

- `unsafe_code = "forbid"` workspace-wide (`Cargo.toml`, `[workspace.lints.rust]`).
  There is no unsafe anywhere, and the discipline that keeps it that way is
  derived `bytemuck::Pod` for every GPU-uploaded struct and no hand-written
  `Send`/`Sync`. Code that wants unsafe needs a different design, not an allow.
- `unwrap_used` / `expect_used` / `panic` are denied workspace-wide; `clippy.toml`
  re-allows all three in tests. **Clippy scopes that to `#[test]` fns only**, so a
  helper at the top level of a `tests/` file still trips the deny. Those files
  open with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`
  (`crates/catchlight-wgpu/tests/clear_fallback.rs:1`). A file whose unwraps all
  sit inside `#[test]` fns needs no allow
  (`crates/visual-tests/tests/regression.rs`).
- `deny.toml`: advisories and licenses **deny** (each ignore carries a RUSTSEC id
  and a reason, each license an explanation of which crate needs it); duplicate
  versions **warn**, because the duplicates it finds are decisions already made
  (see Gotchas); wildcards **deny**, which is why internal workspace deps carry an
  explicit `version` next to `path`.

# Rendering invariants (`crates/catchlight-wgpu/src/renderer.rs`)

`renderer.rs` is one ~5.7k-line file on purpose. Every `queue.write_buffer` and
every buffer allocation is visible in one place, which is how the aliasing bugs
below were caught — twice. It gets split once targeted renderer tests cover the
invariants, not before.

- **`write_buffer` batches at submit start.** Two writes to the same buffer
  offset inside one frame both land before the submit, and the later one wins for
  *every* draw that reads that range — silently, and for the wrong parts. So each
  buffer offset is written exactly once per frame. Per-part instance and uniform
  data is staged in CPU-side buffers and flushed as a single `write_buffer` at
  frame end (`flush_instance_writes`, `flush_part_uniform_writes`, both called
  from `render_list_ext`).
- **Cursor allocation, never a bare offset 0.** Take instance slots with
  `reserve_instances(count)` and uniform slots with `write_part_uniform(..)`; both
  hand out offsets from a monotonic per-frame cursor. A helper that writes offset
  0 itself reintroduces the aliasing above.
- **One submit per frame.** `render_list` / `render_list_ext` record into the
  *caller's* encoder and submit nothing; the caller's submit is the frame's only
  one. The only `queue.submit` inside the renderer is `generate_mips`, at
  texture-upload time.
- **Never grow a GPU buffer mid-frame.** `begin_frame_instances` and
  `begin_frame_uniforms` size the frame up front, before any pass is recorded. A
  realloc after that strands already-recorded passes on the freed buffer.
- **One camera slot per view.** `reserve_camera` writes each `render_list`'s
  view-proj into its own slot of a `CAMERA_RING_SLOTS`-deep ring and binds it as a
  dynamic offset, so views sharing a submit can't alias. `begin_camera_submit`
  resets the count at the *external* submission boundary — do not call it between
  `render_list` calls that will be submitted together.
- **Masking blends need matching color and alpha factors.** A mode whose color
  component masks via `DstAlpha` or `Zero` (`ClipToLower`, `SliceFromLower`, and
  likewise `Multiply` / `ColorDodge`) must use the same factors on alpha. Alpha
  falling through to OVER writes α=1 where color is 0, producing opaque-black
  halos — invisible at identity pose, visible once a deform shrinks the mask.
  Pinned by `masking_blend_modes_have_matching_color_and_alpha_factors`.
- **Multi-puppet resource sharing.** `StencilTarget`, `CompositePool` and
  `FramebufferSnapshotPool` are caller-owned and passed into `render_list_ext`, so
  several puppets in one frame share them. Each puppet still gets its own
  `WgpuRenderer` (mesh ids from different puppets would collide in
  `mesh_buffers`) — see `crates/catchlight-bevy/src/prepare.rs`
  (`HashMap<RendererKey, WgpuRenderer>` beside one `FormatResources`) and
  `crates/visual-tests/src/harness.rs`, which serializes every render through one
  mutex for the same reason.
- **Z order: higher `z_order` draws in front.** `collect_drawables` accumulates
  `parent_z + node.z_order` down the tree and sorts ascending
  (`crates/catchlight-wgpu/src/drawable_collector.rs`), so the last draw is the
  frontmost. `.inx` is the opposite (lower `zsort` in front); the flip happens at
  import, never here.
- **The camera holds no axis flip.** `create_orthographic_camera_at` is a
  textbook Y-up ortho (`crates/catchlight-wgpu/src/lib.rs`). Catchlight world
  space is Y-up end to end.

Most of these are invisible to pixels, so they are pinned directly: the renderer
keeps per-frame lifecycle counters (queue writes and buffer reallocs per buffer,
slots reserved vs written) and `debug_assert!`s that every staged write starts at
or above a monotonic watermark. Targeted per-invariant tests live in
`crates/catchlight-wgpu/tests/`; `crates/visual-tests` covers the ones that do
reach pixels.

# Runtime invariants (`crates/catchlight-core`)

- **The per-frame pipeline is `Puppet::tick`** (`src/puppet.rs`), not a bare
  `compute_transforms`. Semantically it is: fold animations → pose the physics
  anchors and step the drivers → apply params → compute transforms → apply
  `translateChildren` mesh-group filters and recompute → propagate mesh-group
  deforms → apply welds → combine deforms. **The code is an optimized form of
  those semantics**, generation-cached in three places, and that caching is where
  a bug hides: the anchor pre-pass is skipped unless `param_generation` moved
  (`last_anchor_pose_generation`), the whole fold is skipped when neither params
  nor the pre-pass touched anything (`last_tick_folded_param_generation`), and the
  third transform walk runs only when `apply_translate_children_filter` actually
  shifted something. A pre-pass that ran forces the final apply, because it reset
  opacity/tint and deactivated every deform stack.
- **Mesh-group descent stops at a nested MG.** `descendant_drawables`
  (`src/meshgroup.rs`) recurses through Parts and Composites, collects Parts and
  nested MGs, and halts at each nested MG; the outer warp reaches the inner MG's
  children transitively through the pre-order propagation pass. Binding them
  directly would apply the warp twice. Same reasoning for
  `translate_children_targets`, which shifts only non-Drawable descendants. A
  Composite with `propagate_mesh_group = false` halts both walks.
- **Physics integrates in substeps sized by the driver, and damping is
  per-second.** `SimplePhysicsData::tick` (`src/physics.rs`) clamps `dt` to
  `PHYSICS_MAX_DT` and splits it by `max_substep()`, derived from
  `RK4_STABILITY_LIMIT * RK4_STEP_SAFETY` and capped at `PHYSICS_MAX_SUBSTEPS`.
  `angle_damping` is a fraction shed per **second** (`(1 - d).powf(dt)`), so the
  material a rig describes does not change with frame rate or substep count.
  Applying damping per step would make 60 Hz and 144 Hz render different hair.
- **Physics drivers work in a Y-down frame.** `physics_anchor` flips Y going in
  and `write_physics_param_outputs` conjugates `world_inverse` by the same flip
  coming out, matching the reference pendulum's gravity-toward-+Y convention.
- **`settle_physics` before the first render.** It iterates to the fixed point of
  "anchor → param value → transforms → anchor" so a freshly loaded rig renders
  settled instead of swinging into place, and it leaves the puppet *unposed* —
  `tick` is what folds a renderable pose.
- **Param ids vs node ids.** `Param.id` and `AnimationLane { param_id, axis }`
  (`src/params.rs`, `src/animation.rs`) are the param namespace. The **node**
  namespace is still spelled `uuid` (`Puppet::uuid_to_node`, `node_for_uuid`,
  `insert_child(.., uuid: Option<u32>)`) and is a plain `u32` inherited from
  inochi2d — not a UUID. Several param write paths (`set_param_value`,
  `param_value`) also still name their argument `uuid`; it is a `Param.id`.

# `.clp` format (`crates/catchlight-core/src/formats/`)

`.clp` is the editable source of truth; `.inx` / `.inp` are a one-time import
path only (`load_model` warns when you load one directly). `container.rs` frames
opaque sections and owns the version word; `clp.rs` gives them meaning
(`Structure` CBOR + verbatim `Textures`).

- `FORMAT_VERSION` is **0** and `decode_structure` accepts exactly that version.
  There is no migration path and no code that reads an older one.
- The structure is an arena: every cross-reference is an array index, topologically
  ordered (`parent < self`). The array position *is* the identity.
- CBOR maps keyed by field name give additive evolution. **Never** add
  `deny_unknown_fields`; a breaking change bumps `FORMAT_VERSION`.
- Stable ids live in memory, not in the file: `catchlight-editor-core`'s
  `EditModel` assigns indices only at the file edge
  (`crates/catchlight-editor-core/src/flatten.rs`).

# Import invariants (`.inx` → catchlight)

All of this lives under `crates/catchlight-core/src/importer/inochi2d/`. There are
**two independent reflection paths** and they must stay in step:

- `convert.rs` — `.inx` → `Puppet` (the legacy direct load).
- `to_clp.rs` — `.inx` → `.clp` (what `cargo xtask import` runs).

`.inx` is authored **Y-down with lower `zsort` in front**; catchlight is **Y-up
with higher z in front**. Both paths must negate exactly the same set:

- transform translation Y, rotation X, rotation Z (`reflect_transform_y`)
- mesh vertex Y and mesh origin Y (`reflect_mesh_y` / the loop in `convert_mesh`);
  UVs are texture space and stay as authored
- `zsort` (`reflect_z`, which maps `0.0` to `0.0`, not `-0.0`)
- the Y-bearing binding outputs: `TransformTY`, `TransformRX`, `TransformRZ`,
  `Deform` offsets' Y, and `ZSort` (`reflect_binding_outputs`)

Rotation Y and scale are **not** reflected, and neither are the non-Y transform
components.

**Change one path, change the other.**
`synthetic_rig_reflects_identically_on_both_paths` (`from_clp.rs`) guards this on
every checkout: it runs a hand-authored rig through both paths, asserts they
agree field for field, *and* asserts the absolute authored→runtime values with a
non-reflected control beside every reflected field — agreement alone would pass
if both paths forgot the same negation. The synthetic INX must therefore be
authored in the **source** convention (Y-down, lower-zsort-in-front).
`reference_clp_build_matches_inx_puppet` runs the same comparison over the full
private rig, and is `#[ignore]`d unless that rig is present.

**Texture strategy is `alpha_crop.rs` and nothing else** (same directory). It
crops each texture to the aligned bounding box of its *opaque* texels plus a
16-texel transparent mip skirt, keeping texture ids 1:1 with the source table so
only part UVs are rewritten. `atlas.rs` is gone.

# Gotchas

- **One wgpu across the whole workspace.** `catchlight-bevy` hands bevy's render
  world a `Device`, `Queue` and `Arc<Pipelines>` built by `catchlight-wgpu`
  (`crates/catchlight-bevy/src/prepare.rs`, via `device.wgpu_device()`), so
  `bevy_render` and `catchlight-wgpu` must resolve to a **single** wgpu — 29.0.3
  today, shared with `eframe` and `wgpu-profiler`. `bevy = "0.19"` is really a
  proxy for "the bevy built against wgpu 29". If the tree ever splits, the failure
  is a type mismatch between two identically named `wgpu::Device`s at the plugin
  boundary, which reads as nonsense: check `Cargo.lock` for a duplicate `wgpu`.
- **glam is pinned to bevy's 0.32**, not the crates.io latest, for the same
  reason — a newer glam re-splits the tree. `cargo deny`'s `multiple-versions` is
  at `warn` because these duplicates are the decision, not a defect.
- **Puppet textures are premultiplied linear stored in sRGB bytes**
  (`PUPPET_TEXTURE_FORMAT = Rgba8UnormSrgb`), so the sampler hands shaders
  premultiplied linear. CPU-side tints go through `srgb_to_linear_vec3` before
  upload; all fragment math is linear.
- **The stencil path has a WebGL fallback.** When `Pipelines::has_stencil` is
  false (Chromium swiftshader WebGL2 fails `Depth24PlusStencil8`), mask/masked
  draws sample `StencilTarget::mask_alpha_view` instead of stencil-testing. A
  masking change has to hold on both paths.
- **`base_instance` is native-only.** Part draws select an instance with a
  non-zero `first_instance` on Vulkan/DX12/Metal and re-slice vertex buffer 1
  per draw on GL/WebGL2 and the adapter-less constructors (`emit_part_draw`).

# Fixtures and tests

- **Hand-authored models** are generated by `cargo xtask gen-fixture <name>` from
  `crates/xtask/src/fixtures.rs` and committed under `tests/models/`.
  `committed_welded_seam_still_matches_the_generator` pins the committed bytes'
  *structure* (never byte equality) to the generator, so drift names itself.
  `cargo xtask` also has `import <model.inx|.inp> [-o <model.clp>]`.
- **Synthetic `.inx`** is built by `scripts/build_minimal_inx.py` (a `uv` inline-
  script; `uv` and `python3` are in the dev shell).
- **Visual regression** cases are hand-authored in
  `crates/visual-tests/src/config.rs` (`default_models` × `curated_configs` ×
  `camera_presets`, named `<model>__<label>__cam_<preset>`; a test asserts the
  names are unique, because two configs sharing a name would silently share one
  baseline PNG). `cargo test -p visual-tests` renders each config and diffs it
  against `tests/baselines/<model>/<config>.png` under the thresholds in
  `Thresholds::default`; failures land as expected/actual/diff/summary under
  `tmp/visual-test-failures/<config>/`. To **update baselines** after an intended
  change, run `cargo run -p visual-tests --release -- update` (add
  `--filter SUBSTR` to narrow, or `list` to see the matrix) and commit the PNGs.
  `update_all` is the only writer; the regression test never calls it.
- **Physics trajectory** (`crates/catchlight-core/tests/physics_trajectory.rs`)
  fingerprints the whole driver curve, not just the rest pose. Regenerate with
  `UPDATE_PHYSICS_BASELINE=1 cargo test -p catchlight-core --test physics_trajectory`.
- **Six tests are `#[ignore]`d**, all because they need the private reference rig
  at `example_models/reference/` (three in `to_clp.rs`, two in `from_clp.rs`, one
  in `crates/catchlight-wgpu/tests/deform_wiring.rs`). Drop a rig at that path and
  remove the attributes to run them.

# Tools

- `cargo run -p catchlight-wgpu --example load-model -- <model.clp> [--control]` —
  winit viewer; `--control` (or the `C` key) opens the egui panel of per-param
  sliders and stops animation playback. The interactive way to look at a rig.
- `cargo run -p catchlight-wgpu --example render-to-png -- <model.clp> [out.png] [w] [h] [cam_h]`
  — prints the render list and writes a PNG. It runs the full
  `settle_physics` + `tick` pipeline on purpose: with a bare `compute_transforms`
  the output stays byte-identical through a regression in params, mesh groups or
  welds, so it is the hash-stability check.
- `cargo run -p catchlight-editor [-- <model.clp>]` — the GUI. It embeds the
  server and calls `Editor::handle` in-process; the Unix socket runs on a
  background thread only so a CLI or agent can co-drive the same session.
- `cargo run -p catchlight-editor-cli` — that client. Unix-only by design.
- `scripts/dump_texture.rs` has no build path (there is no `scripts/Cargo.toml`).
  Treat it as a snippet to paste into an example, not a runnable tool.

# Decisions

- The editor targets **WebGPU in the browser first**; the desktop app is the
  secondary target. The runtime is tier-1 on **Windows**.
- The editor-server Unix socket is permanent **Linux-only dev/agent tooling**, not
  a product surface — hence Linux-only CI.
- The removed inspection examples (`mip-compare`, `param-sweep`, `rig-dump`) are
  meant to come back as one CLI, not as more examples.
