# Changelog

## [0.2.0] - 2026-11-22

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

[0.2.0]: https://github.com/hyperlogue/catchlight/releases/tag/v0.2.0
