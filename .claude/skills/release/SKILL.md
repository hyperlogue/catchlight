---
name: release
metadata:
  internal: true
description: Prepare or cut a Catchlight release with changelog review, a shared Rust/npm version, and verified release-profile WASM packages.
---

# Releasing Catchlight

1. Inspect changes since the last release and the current registry versions.
   Resume an incomplete release before choosing a new version. Draft release
   notes in a temporary directory with a dated version heading, concise
   user-visible bullets with bold lead-ins, and only `New`, `Fixed`, and
   `Removed` sections (omit empty sections). Preserve existing changelog entries
   and include the version's compare link.
2. Following the available `publish-artifacts-to-r3` skill, publish the draft as
   a files artifact titled `Catchlight vX.Y.Z changelog`, version label `vX.Y.Z`.
   Share its URL and revise that artifact during review; keep repository release
   files unchanged until approval. The human archives the artifact to approve
   it. Verify the archived state and read final comments and the archive message;
   proceed after approval without asking for a second confirmation. If r3 is
   unavailable, report the blocker and ask for an alternative review channel.
3. Apply approved notes to `CHANGELOG.md` (create it for the first release).
   `Cargo.toml`'s workspace package version is the release source of truth.
   Update its internal path dependency versions and every public npm manifest
   under `packages/` to that exact version. All public crates inherit it,
   including the `catchlight` facade. Refresh `Cargo.lock` with Cargo and
   `bun.lock` with Bun; private demo and generated npm package versions are
   outside the public release set. Run `python3 scripts/release/check.py`.
4. Commit the approved release as `release: catchlight X.Y.Z`. Fixes that the
   checks below require land as separate commits after it; the tag goes last.
5. Run every `run:` step of every job in
   [ci.yml](../../../.github/workflows/ci.yml) locally under `nix develop -c`,
   not only the checks the release touched: a release ships every crate, and
   `cargo deny check` fails on advisories published since the last change.
   Then the npm packaging checks:

   ```sh
   cargo xtask build-wasm --release
   bun install --frozen-lockfile && bun run typecheck && bun test
   bun run npm:pack && bun run npm:verify && bun run npm:publish  # dry run
   ```

   Run `cargo xtask bundle-ui`, then
   `bash .github/scripts/publish-crates.sh dry-run` (it requires a clean
   tree), and package the editor app with this machine's binary:

   ```sh
   cargo build --release -p catchlight-editor
   mkdir -p /tmp/bins/x86_64-unknown-linux-gnu
   cp target/release/catchlight-editor /tmp/bins/x86_64-unknown-linux-gnu/
   node scripts/npm/editor-app.mjs /tmp/bins --verify
   ```

   Never bypass the source packages' `prepublishOnly` guard. Every check
   passes before tagging. Only CI's macOS and Windows rows stay
   unverified here; step 7 covers them.
6. Create an annotated `vX.Y.Z` tag on the last commit, with subject
   `Catchlight vX.Y.Z`. Both registries use this one tag. Leave pushing and
   publication to the user unless requested; changelog review alone does not
   authorize registry uploads.
7. Pushing the tag publishes: it starts `release.yml` live. Give the user this
   order: push `main`; wait for CI on that commit to pass; run `release.yml`
   from `main` as a dry run (the only main-branch run that builds and
   smoke-tests the editor app on all five platforms); push the tag only after
   both pass. Inspect the tag-triggered
   runs' actual results, and rerun a failed one from the tag with dry-run off.
   Preserve the workflow name and `release` environment: trusted publisher
   registrations depend on them. The live run publishes the tag's GitHub
   release (editor binaries, install scripts, `SHA256SUMS`, and the version's
   changelog section as its body), then npm, then crates.io. Releases are immutable once published; recover an incomplete one
   with a patch release. A crate or npm package publishing
   for the first time cannot use trusted publishing yet; publish it once locally
   (`npm login`, then `node scripts/npm/publish.mjs --publish`), then register
   it as a trusted publisher for `hyperlogue/catchlight`, workflow
   `release.yml`, environment `release`. While any `@catchlight/editor` version
   below 0.2.0 is not deprecated, deprecate those versions with a pointer to
   `@catchlight/editor-ui`, within the user's authorization. Rust
   uploads use `.github/scripts/publish-crates.sh publish` with a token
   supplied through `CARGO_REGISTRY_TOKEN`; never put credentials in
   repository files.
8. Verify every public package's exact version in both registries, and the
   GitHub release's editor assets, before reporting completion. Uploads across
   registries are not atomic. On a partial failure, report what remains and
   retry the same version using the scripts' already-published skipping
   behavior. Honor a rate limit's retry time; stop repeated auth or validation
   failures for correction. Preserve verified npm tarballs when retrying.
   Start a new version only after the coordinated release is complete; recover
   published mistakes with a new patch release.
