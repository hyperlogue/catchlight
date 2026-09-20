<p align="center">
  <img src="assets/logo.svg" alt="" width="140">
</p>

# Catchlight

Catchlight is a software stack for 2.5D character animation. It simulates
3D movement through deforming 2D parts.

## Applications and libraries

| Offering | Audience and purpose | Distribution |
| --- | --- | --- |
| **Catchlight Demo** | Try authoring in a browser, with the editor running in the tab. | The hosted app in `apps/site/`, named `catchlight-demo` in the web workspace. |
| **Catchlight Studio** | Author locally with a person and an agent sharing editor sessions. | Planned local application: the editor server together with a web UI served on the local machine. The server and connected frontend exist; the bundled application and its launcher are not implemented yet. |
| **Catchlight libraries** | Embed animation or authoring in another application, or automate an editor. | Rust libraries, the `@catchlight/*` browser packages, and the [Python client](python/README.md). |

`@catchlight/editor` is the embeddable React frontend with the default theme.
The host chooses its backend and supplies the WASM module. It does not include
the native editor-server executable or a local application launcher.
`@catchlight/react` exposes the unstyled parts, and `@catchlight/core` supplies
the web-platform glue.

## Acknowledgement

Catchlight is heavily inspired by
[inochi2d](https://github.com/Inochi2D/inochi2d) and
[inox2d](https://github.com/Inochi2D/inox2d). Both serve as the reference for
catchlight's model format and rendering semantics, and the project began as an
attempt to show inochi2d models on WebGPU.

[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists which modules the
derivation reaches, and retains the upstream licences.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
