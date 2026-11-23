<p align="center">
  <img src="assets/title.jpg" alt="Catchlight — 2.5D character animation, featuring Lan" width="960">
</p>

# Catchlight

[![crates.io](https://img.shields.io/crates/v/catchlight.svg?color=72bffb)](https://crates.io/crates/catchlight)
[![npm](https://img.shields.io/npm/v/%40catchlight%2Feditor-ui.svg?color=72bffb)](https://www.npmjs.com/package/@catchlight/editor-ui)

Catchlight is a software stack for 2.5D character animation. It simulates
3D movement through deforming 2D parts.

## Applications and libraries

| Offering | Audience and purpose | Distribution |
| --- | --- | --- |
| **Catchlight Demo** | Choose the in-browser editor or camera-driven face tracking. | The hosted app in `apps/site/`, named `catchlight-demo` in the web workspace. |
| **Catchlight Studio** | Author locally with a person and an agent sharing editor sessions. | `catchlight-editor`: one binary that serves the editor UI on `127.0.0.1`, opens the browser, and runs agent commands against the same session. Install it below. |
| **Catchlight libraries** | Embed animation or authoring in another application, or automate an editor. | Rust libraries, the `@catchlight/*` browser packages, and the [Python client](python/README.md). |

Install the local editor from a [release](https://github.com/hyperlogue/catchlight/releases),
npm or crates.io, then run `catchlight-editor [model.clm]`:

```sh
curl -fsSL https://github.com/hyperlogue/catchlight/releases/latest/download/install.sh | sh
irm https://github.com/hyperlogue/catchlight/releases/latest/download/install.ps1 | iex  # Windows
npx @catchlight/editor
cargo install catchlight-editor
```

An agent drives the open editor with the same binary, for example
`catchlight-editor node tree`. Give your coding agent the skill:

```sh
npx skills add hyperlogue/catchlight --skill edit-catchlight-model --global
```

`@catchlight/editor-ui` is the embeddable React frontend with the default theme.
The host chooses its backend and initializes the WASM module bundled in
`@catchlight/core/wasm`. The component package does not include
the native editor server; `@catchlight/editor` is the packaged application.
`@catchlight/react` exposes the unstyled parts, and `@catchlight/core` supplies
the web-platform glue.

The landing page offers an editor demo (`?demo=editor`) and a face-tracking
demo (`?demo=face`). Both preload Lan from `tests/models/lan.clm`. Face tracking
also follows shoulder posture. It starts only after clicking **Start camera**,
processes frames locally, and downloads MediaPipe assets from Google and
jsDelivr. It requires HTTPS (or
localhost) and camera permission; stopping or leaving the tab releases the
camera. Tracking poses an ephemeral model without saving changes.
Fetch Git LFS objects before building. The empty-state starter action reopens
the same model. `CATCHLIGHT_LOCAL_MODEL` can override it for loopback development
only; production always bundles the repository model.

## Rust runtime

Start with [`catchlight`](crates/catchlight/README.md), the entry point for
embedding the animation runtime. Its default API re-exports `catchlight-core`.
Enable `wgpu` for `catchlight::wgpu`, or `bevy` for `catchlight::bevy` and the
renderer. Editor tools and command-line applications remain separate crates.

```toml
[dependencies]
catchlight = "0.1.0"
```

## Acknowledgement

Catchlight is heavily inspired by
[inochi2d](https://github.com/Inochi2D/inochi2d) and
[inox2d](https://github.com/Inochi2D/inox2d). The project began as an
attempt to show inochi2d models on WebGPU and has since grown its own model
format and semantics; `catchlight-importer` converts inochi2d models.

[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists which modules the
derivation reaches, and retains the upstream licences.

## License

### Models and images

All `.clm` model files and image assets, including PNG (`.png`) and JPEG
(`.jpg`, `.jpeg`) files, are dedicated to the public domain under
[CC0 1.0 Universal](https://creativecommons.org/publicdomain/zero/1.0/)
(`CC0-1.0`), unless a different license is explicitly stated for a particular
file.

By contributing a model file or image asset, you agree to release it under
CC0-1.0 unless you explicitly identify a different license for that file when
you contribute it.

### Software

The software is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any software contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
