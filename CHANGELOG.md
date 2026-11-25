# Changelog

## Unreleased

Breaking for Rust callers of `catchlight-core` and `catchlight-wgpu`; the
next release is 0.3.0.

### New

- **Review threads: leave comments for an agent, in the editor.** Comment on
  a node, param, texture, slot or the whole model (`C` on a selection), collect
  as many as you like, then **Send to agent**. An agent waiting on
  `catchlight-editor review watch` receives them, edits the model in the shared
  session and answers on each thread with `review reply`; the tab shows it
  listening, working and replying. Threads follow renames, flag a target that
  was deleted, and remember the pose they were written at. Python sessions get
  the same calls (`Client.review_*`). A comment can carry images: paste,
  drop or capture the canvas in the tab and mark it up, `review watch
  --attachments-dir` hands them to the agent as files, and `review reply
  --attach` sends one back. Comments count as delivered only once the CLI
  has saved, printed and acknowledged them, so a failed run loses nothing;
  several agents can attend one review, each known by its harness session
  and holding threads with renewable `review claim`s; `--key` makes a
  repeated `reply` or `open` write once; and `CatchlightEditor`'s `reviewer`
  prop tells people sharing a review apart. A review is kept with a
  saved model, never in the `.clm`: `catchlight-editor` keeps it in a per-user
  data directory (`--keep beside` writes a `<model>.clm.kept/` sidecar
  instead; `catchlight-editor gc --dry-run` lists what models that no
  longer exist left behind), and an embedding host chooses its own store
  with `Editor::with_kept_store`. The in-tab editor keeps none yet. A host
  says how an agent joins through `CatchlightEditor`'s `agentConnect` prop,
  so the same editor serves a local process or a hosted one that offers its
  own agent. `review listen` returns at once and has a per-user background
  worker announce each Send in the agent's Claude Code, Codex or Antigravity
  session, so nothing blocks the agent's turn; `review watch` remains the
  fallback.
- **Saved versions: go back to an earlier result.** Save the model as a
  numbered version with an optional label (the selector beside the model
  tabs, Ctrl/⌘+Alt+S, or `catchlight-editor version save -l <label>`), open
  any version read-only beside the live model, and restore it as one
  undoable edit. Versions are kept with the review, deduplicated so shared
  textures are stored once, and outlive the session; `version export <n>`
  writes a version's exact bytes. An agent answering a review can point a
  reply at the version holding its fix (`review reply --save-version
  <label>` or `--version <n>`), shown as a chip that opens it. Python
  sessions get `Client.version_*`. `--reviews`/`CATCHLIGHT_REVIEWS` are now
  `--keep`/`CATCHLIGHT_KEEP` and govern both, `reviews gc` is `gc`, and the
  per-user directory moved from `catchlight/reviews` to `catchlight/kept`.
- **A redesigned editor.** One top bar holds the menus, open models, review
  status and save; the canvas runs edge to edge with its modes and tools
  floating over it; the tree, params and properties are denser, so more of the
  model fits on screen. `@catchlight/editor-ui` hosts that restyled tokens
  should recheck them: several `--cl-*` values changed and `--cl-gutter`,
  `--cl-control-height`, `--cl-well`, `--cl-agent` and `--cl-ok` are new.
- **Building a puppet is fast, and the second one is nearly free.** A mesh
  group finds the triangle under a vertex through bins instead of a per-texel
  grid, which makes `Puppet::new` about 40 times faster on a 100-part model
  and drops tens of megabytes per puppet. A model keeps its bake, so further
  puppets of the same model, and `Puppet::clone`, copy only a frame.
- **Fewer deform uploads.** Spines, mesh groups and welds no longer mark a
  deform changed when they rewrite the offsets it already held, so moving one
  parameter re-uploads the parts it moves rather than nearly all of them.
- **Sections a build does not recognise survive a save.** Opening a `.clm`
  written by a newer build and saving it keeps the sections this build cannot
  read, byte for byte. `formats` documents how the format may change.
- **Load limits cover the whole file.** A `.clm` structure section is held to
  `LoadLimits::structure_bytes` (64 MiB) before it is parsed. Spine joints,
  weld pairs, keyframes and extension values each have a limit, a JSON
  extension is capped at 1 MiB, and textures are held to 8192 texels a side
  and `decoded_texture_bytes` in total before any is decoded
  (`PrepareOptions::max_decoded_texture_bytes` in `catchlight-wgpu`).
- **Extensions that keep up with the model.** A `.clm` can carry typed,
  maintained extension data: a `{version, refs, payload}` envelope whose
  reference table names nodes, params, textures and slots. The core rewrites
  that table on every rename and marks what a removal leaves behind, whether
  or not the build knows the extension, and never reads the payload. A third
  party implements `Extension` for its own type and gets copy-on-write access
  through `Model::extension`, `extension_mut` and `extension_or_default`
  without registering; `catchlight_core::extension::register` adds addon
  merging and checking, and `Model::extension_warnings` reports stale
  references and each extension's own findings. Data a build cannot read,
  written by a newer version or failing to decode, is kept and saved as
  stored. Addon extensions merge into the base's data and come back out on
  uninstall. `catchlight-cli extension set --version <n> --refs <json>`
  writes a vendor's maintained extension with a JSON or byte payload, and
  warns about a reference the model lacks rather than refusing it.
- **Humanoid controls.** The official `catchlight.humanoid` extension
  (`catchlight-extension-humanoid`) defines 35 standard face and body
  controls and maps each onto one of a model's params by three stops, so one
  source of tracking can pose any model that maps them. `Humanoid::pose`
  applies `ControlValues`, and `controls_from_arkit` turns ARKit blend shapes
  into control values.
- **Expressions and visemes.** The official `catchlight.expressions`
  extension (`catchlight-extension-expressions`) names expressions, such as
  the five standard emotions, and beside them visemes, such as the 21
  standard mouth shapes of speech, each by values for humanoid controls or
  the model's own params. The two groups keep separate names and take
  separate weights, and blend around the control mappings (`blend_controls`,
  `blend_params`) with visemes over expressions, so speech takes the mouth
  while a sound plays and the expression returns between sounds.
  `standard_visemes()` offers default viseme settings.
- **Rig controls and expressions from the command line.** `catchlight-cli
  humanoid` lists the standard controls and maps, lists and unmaps a model's
  controls; `catchlight-cli expression` and `catchlight-cli viseme` each
  list their standard names, and set, list and remove a model's expressions
  or visemes, and `viseme add-standard` adds the standard visemes. Output is
  JSON, a typo in a control name is refused with the nearest standard name,
  and each edit prints the warnings about its group for the edited model.
  Texture bytes and other extensions are kept byte for byte. The rules are
  the extension crates' own: `catchlight_extension_humanoid::authoring` and
  `catchlight_extension_expressions::authoring` make these edits on a
  `Model` and refuse what the CLI refuses, and `Model::remove_extension`
  drops an emptied official extension. `render` and `bounds` take
  `--control name=value`, `--expression name=weight` and
  `--viseme name=weight`, and a render spec request takes `controls`,
  `expressions` and `visemes`.
- **Animate controls, expressions and visemes.** An animation lane can drive
  a humanoid control, or the weight of an expression or a viseme, by name,
  so a clip that blinks, smiles or speaks plays on any model that has them.
  `AnimationPlayer::advance_with` applies one frame in a fixed order: the
  host's own control values and weights, with a playing clip's lanes winning
  for the same name, then expressions and visemes, the control mappings and
  the clip's param lanes. A weight held below 1 gives the same value every
  frame, and motion responses do not react to a control lane's jump at a
  loop wrap. `check` reports a lane on a control the model does not map or on
  an expression or viseme it lacks. A render spec's clips take the same
  lanes, so a timed request can ramp controls, expressions and visemes; the
  CLI applies a request's controls and weights every frame under them, and
  refuses a lane on a control, expression or viseme the model lacks. Bevy
  puppets play these lanes through the model's own mappings.

### Fixed

- **The CLI names a Git LFS pointer.** A checkout without `git lfs pull`
  leaves pointer text where a `.clm` should be. `catchlight-cli` now says so
  and how to fetch it, instead of reporting unexpected magic bytes.
- **A stepped key shows at its own position.** A `Stepped` binding or
  animation lane resting exactly on a key showed the previous key's value. It
  now shows that key's, on both axes. Poses that sit on a stepped key render
  differently.
- **Files the runtime could not evaluate are refused.** Reading a `.clm` now
  rejects two bindings under one key, a node that masks itself, and a
  non-finite param default, node transform or z-order. Edits refuse the same
  values, and a mesh refit that returns the wrong number of offsets is
  refused where it is made.
- **Addons install and uninstall cleanly.** Installing an addon holds its
  welds to the rule every other weld meets, and uninstalling removes the
  installed nodes together with anything added beneath them since.
  `duplicate_subtree` copies the welds inside the subtree.
- **A `.clm` loads up to its size limit.** Texture payloads were charged
  twice, so a file loaded only under about half of `encoded_bytes`.
- **Saving cannot write a file that will not open.** A section over the
  reader's size cap is an error at save instead of a truncated length.
- **A texture at the device's size limit uploads.** A fully opaque texture
  came back from preparation 32 texels larger each way and was refused.
- **JSON extensions keep their values.** A NaN or infinity in a JSON
  extension was read back as `null`; it is now a decode error.
- **Several views in one submit keep their own draws.** A second view
  recorded into the same `Submission` overwrote the first view's per-part
  data, so both views drew the last one's transforms, opacity and tint.

### Removed

- **`update_node_kind` edits a kind's own fields only.** Swapping the kind, or
  changing a mesh, albedo, mask list, slot list or spine through it, returns
  `ModelError::KindEdit`. Use `set_node_mesh`, `set_part_albedo`, `mask_add`,
  `slot_delete`, `set_spine_joints` and `set_spine_targets`.
- **Error enums and `LoadLimits` are `#[non_exhaustive]`.** `ModelError`,
  `InstallError`, `ClmLoadError`, `ClmError`, `ContainerError`,
  `TextureError`, `IdError`, `DeformShapeError`, `GeometryError`,
  `NodeTreeError` and `LoadResource` need a wildcard arm, and `LoadLimits`
  is built from `LoadLimits::default()`. Several gained variants;
  `InstallError` wraps `ModelError` and is no longer `Clone` or `PartialEq`.
- **`prepare_textures` takes a size limit and a budget**, and `container::write`
  returns a `Result`. `TexturePrepCache`, `PrepareOptions::memoize_textures`
  and `alpha_bleed_inplace` are gone.
- **`Puppet::refold_with_node_edits`, `NodeEdits` and
  `Puppet::contribute_param_value` are gone**, as are `DeformSource::Test`,
  the unused `DeformShapeError` matrix variants and the
  `mesh_group_bitmap_cells` load limit.
- **The crate root lists its exports.** `downsample_box_filter`,
  `checked_affine_inverse`, `validate_id`, `validate_extension_key` and the
  `charge_*` functions are reached through their modules
  (`catchlight_core::components::checked_affine_inverse`).
- **`Model::extract` of a whole tree returns an empty fragment**, and
  `catchlight-cli requirements` reports a mask source as kind `drawable`.
- **`catchlight-wgpu` renders through `RenderTarget` and `FrameTargets`.**
  `Frame::render` and `WgpuRenderer::render_into` take a `RenderTarget`
  (view, optional texture, size) and a `FrameTargets` that sizes its own mask
  and composite targets. `Frame::render` no longer returns a `Result`, and
  naming the target's texture enables the Overlay, Color Burn and Linear Burn
  blends. `render_ext`, `StencilTarget`, `CompositePool` and
  `FramebufferSnapshotPool` are gone.
- **`catchlight-wgpu` has one error type.** Every entry point returns
  `RendererError`, which now wraps texture, load-limit, adapter, device and
  readback failures; `RendererError::TexturePrep` is gone.
  `RenderContext::with_renderer`, `render` and `render_many` are infallible,
  and `read_texture_to_rgba` takes its size from the texture.
- **`catchlight-wgpu` hides its internals.** `RenderList` is read through
  `root_drawables()`, `composite_children()`, `composites()` and
  `composite_mask_source()`. `upload_mesh`, `upload_texture`, `Texture`,
  `MeshBuffer`, `PartUniform`, `generate_mips` and the crate's modules are
  private; `RenderCache` is the way in, and the crate root lists its exports.
- **`create_headless_context_ext` is gone.** Nothing called it; build a
  device with timestamp queries directly to use the GPU profiler.
- **Animations are an extension.** `catchlight-core` no longer knows
  animations: clips live in the official `catchlight.animations` extension
  (`catchlight-extension-animations`) and play through `AnimationPlayer`,
  which a host advances before each `Puppet::tick` instead of asking the
  puppet to play. The structure section drops its `animations` field, so a
  file written before this change loses the clips stored there when it is
  loaded; re-import or re-author them. `catchlight-cli`, the editor and the
  importer register and use the extension.

## [0.2.2] - 2026-11-22

### New

- **Standalone local editor app.** Run the Catchlight editor directly on your
  desktop with zero setup. Running `catchlight-editor` (or simply
  `npx @catchlight/editor`) launches the visual web editor on `127.0.0.1` and
  opens it in your default browser. It introduces an interactive, multi-actor
  workflow where you can visually inspect and edit 2.5D character models in
  real time while an AI coding agent works alongside you on the exact same
  session. Prebuilt native binaries are available for Linux (x64, arm64),
  macOS (Apple Silicon, Intel), and Windows (x64) via standalone install
  scripts or Cargo.
- **Real-time AI agent collaboration.** Pair directly with AI coding assistants
  to automate model inspection and rigging. Run `catchlight-editor guide` to
  view agent workflows, or install the official agent skill (`npx skills add
  hyperlogue/catchlight --skill edit-catchlight-model`) so assistants like
  Claude or Antigravity can query parameters, pose deformation meshes, adjust
  dynamics, and apply edits live inside your running editor session.
- **Cross-platform Windows and macOS support for the editor & CLI.** The editor
  server and `catchlight-editor-cli` now communicate over local HTTP with
  automatic discovery in addition to Unix sockets. This brings full interactive
  model editing, command-line control, and agent automation to Windows and macOS
  without requiring Linux or WSL.
- **Lossless video export with alpha transparency.** The animation renderer
  (`catchlight-cli render --animation`) now supports exporting to FFV1 video in
  a Matroska container (`.mkv`). This provides true bit-for-bit lossless video
  output while fully preserving alpha transparency, making it ideal for
  archival, professional compositing, and video editing workflows alongside
  existing MP4, WebM, and GIF export formats.
- **Bundled WebAssembly in `@catchlight/core`.** The core web package now ships
  with the compiled WebAssembly engine bundled directly under
  `@catchlight/core/wasm` alongside an explicit initialization loader. This
  eliminates separate WASM build and distribution steps when embedding
  Catchlight character rendering into web apps.
- **Expressive face tracking with eyebrow dynamics.** The interactive webcam
  demo now tracks and drives character eyebrows to faithfully mirror your facial
  expressions, paired with improved jitter filtering for noticeably smoother
  and more stable real-time face puppeteering.

### Removed

- **Embeddable React editor component renamed to `@catchlight/editor-ui`.** The
  embeddable React component library has moved from `@catchlight/editor` to
  `@catchlight/editor-ui` to reserve the `@catchlight/editor` package name for
  the standalone local editor app. Existing web integrations should import
  `CatchlightEditor` and its theme stylesheet from `@catchlight/editor-ui`
  (e.g., `import { CatchlightEditor } from "@catchlight/editor-ui"` and
  `@catchlight/editor-ui/theme.css`).

[0.2.2]: https://github.com/hyperlogue/catchlight/releases/tag/v0.2.2
