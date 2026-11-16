<p align="center">
  <img src="assets/logo.svg" alt="" width="140">
</p>

# Catchlight

Catchlight is a software stack for 2.5D character animation. It simulates
3D movement through deforming 2D parts.

## Applications and libraries

| Offering | Audience and purpose | Distribution |
| --- | --- | --- |
| **Catchlight Demo** | Choose the in-browser editor or camera-driven face tracking. | The hosted app in `apps/site/`, named `catchlight-demo` in the web workspace. |
| **Catchlight Studio** | Author locally with a person and an agent sharing editor sessions. | Planned local application: the editor server together with a web UI served on the local machine. The server and connected frontend exist; the bundled application and its launcher are not implemented yet. |
| **Catchlight libraries** | Embed animation or authoring in another application, or automate an editor. | Rust libraries, the `@catchlight/*` browser packages, and the [Python client](python/README.md). |

`@catchlight/editor` is the embeddable React frontend with the default theme.
The host chooses its backend and supplies the WASM module. It does not include
the native editor-server executable or a local application launcher.
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

## Testing

Lan (`tests/models/lan.clm`) and image baselines are stored in Git LFS. Fetch
these once with `git lfs pull`. Both suites use the checked-in fixture.

Run visual comparisons through the native headless renderer:

```sh
nix develop -c cargo test -p visual-tests --test regression lan_visual_regressions
```

Chromium E2E covers editor interactions, backend synchronization, graphics
fallback and canvas presentation. On a fresh checkout:

```sh
nix develop -c bun install --frozen-lockfile
nix develop -c cargo xtask build-wasm --debug
nix develop -c env E2E_BUILD=1 bun run --filter catchlight-demo e2e
```

The browser runner starts its own services and copies Lan before editing it.
Run `cargo test -p visual-tests` for the full native suite, including synthetic
renderer cases. See `crates/visual-tests/src/lib.rs` for baseline review and
update commands.

## Rust releases

The **Publish Rust crates** GitHub Actions workflow is manually dispatched and
defaults to a dry run. It packages and builds the eight crates listed in
[the release script](.github/scripts/publish-crates.sh), using Cargo's workspace
publishing support to resolve unpublished dependencies together and publish in
dependency order. The importer, browser WASM boundary, visual tests, and xtask
are excluded.

1. Update `workspace.package.version` and the internal dependency versions in
   `Cargo.toml` together, update `Cargo.lock`, and merge after CI passes.
2. Run **Publish Rust crates** on the release commit with `dry_run` enabled.
3. Configure the repository Actions secret `CRATES_IO_TOKEN` with a crates.io
   API token authorized to publish all eight crates, including creating them
   for the initial release.
4. Tag the validated commit `v<workspace version>` (initially `v0.1.0`) and
   push the tag. Select that tag when dispatching the workflow with `dry_run`
   disabled. The workflow checks the tag against the manifest version and
   repeats package verification before publishing.

Tag pushes alone do not publish. Publishing is not atomic across crates: if
an upload fails, inspect crates.io and the run logs before retrying; uploaded
versions cannot be overwritten. The workflow does not create tags or GitHub
releases. To run the same package verification locally:

```sh
nix develop -c bash .github/scripts/publish-crates.sh dry-run
```

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
