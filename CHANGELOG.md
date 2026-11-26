# Changelog

## [0.3.0] - 2026-11-26

This release includes breaking changes for Rust callers of `catchlight-core`
and `catchlight-wgpu`, as well as `.clm` files that contain animations (see
Removed below).

### New

- **Review threads.** Leave comments on any part of your model in the editor
  and send them to an AI agent to edit the model live and reply on each
  thread.
- **Saved versions.** Save numbered versions of your model, view older ones
  read-only next to the live model, and restore any of them in a single
  undoable step.
- **Redesigned editor.** A single top bar, edge-to-edge canvas, and tighter
  panels leave much more room for your model on screen.
- **Humanoid controls.** Map your model's params to 35 standard face and body
  controls with `catchlight.humanoid` so any tracking source can drive any
  compatible model.
- **Expressions and visemes.** Define named expressions and 21 standard
  visemes using `catchlight.expressions`, where speech automatically layers
  over your active expression.
- **Portable animations.** Animation clips can now drive humanoid controls,
  expressions, and visemes directly, so clips for blinking, smiling, or
  talking work across any model that supports them.
- **Controls, Expressions, and Animations tabs.** Map humanoid controls, edit
  expressions and visemes, and play clips right from the editor dock while
  seeing changes live in the viewport.
- **Controls and expressions in the CLI.** Manage humanoid mappings,
  expressions, and visemes from `catchlight-cli` and `catchlight-editor`, and
  pose renders directly with `--control`, `--expression`, and `--viseme`.
- **Reference-aware extensions.** Custom `.clm` extensions can now reference
  nodes, params, textures, and slots that stay up to date across renames,
  with no per-extension size limit.
- **Faster puppet creation.** Creating a puppet is roughly 40x faster on
  100-part models, and extra puppets of the same model share the bake so they
  spin up almost instantly.

### Fixed

- **Stepped keys.** Stepped bindings and animation lanes sitting directly on
  a key now use that key's value instead of falling back to the previous one.
- **Helpful Git LFS errors.** `catchlight-cli` now spots unfetched Git LFS
  pointer files and tells you how to pull them instead of failing with a raw
  magic-bytes error.
- **Stricter `.clm` validation.** Files with duplicate binding keys,
  self-masking nodes, or non-finite values are now rejected on load rather
  than breaking at runtime.
- **Addon uninstall and subtree duplication.** Uninstalling an addon now
  cleans up everything added beneath its nodes, and duplicating a subtree
  brings its welds along with it.
- **Large `.clm` files.** Models can now load up to their full size limit
  instead of hitting it halfway, and saving catches oversized sections before
  writing an unreadable file.
- **Max-size opaque textures.** Fully opaque textures at your GPU's maximum
  size now upload cleanly instead of being rejected.
- **Multi-view submissions.** Drawing multiple views in one submission no
  longer lets later views overwrite earlier ones' transforms, opacity, and
  tint.

### Removed

- **Built-in core animations.** Animations now live in the
  `catchlight.animations` extension and play through `AnimationPlayer`, so
  older `.clm` files will drop their stored clips on load and need to be
  re-imported or re-authored.
- **`catchlight-core` API changes.** Error enums and `LoadLimits` are now
  `#[non_exhaustive]`, `update_node_kind` only edits fields on the node's
  current kind, and several unused functions and types have been removed.
- **`catchlight-wgpu` API changes.** Rendering now goes through
  `RenderTarget` and `FrameTargets`, all entry points return `RendererError`,
  and internal GPU types are now private behind `RenderCache`.

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

[0.3.0]: https://github.com/hyperlogue/catchlight/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/hyperlogue/catchlight/releases/tag/v0.2.2
