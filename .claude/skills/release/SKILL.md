---
name: release
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
4. Run relevant repository checks from `AGENTS.md` and the packaging checks in
   [scripts/npm/README.md](../../../scripts/npm/README.md). `npm:pack` must build
   WASM with `cargo xtask build-wasm --release`, then use that release binary for
   core's explicit-init web loader. Run `npm:verify` against the resulting
   tarballs: it exercises a production browser consumer outside the workspace.
   Run `bash .github/scripts/publish-crates.sh dry-run` for Rust. Resolve failures
   before committing or tagging.
5. Commit the approved release and create an annotated `vX.Y.Z` tag on that
   commit, with subject `Catchlight vX.Y.Z`. Both registries use this one tag.
   Leave pushing and publication to the user unless requested; changelog review
   alone does not authorize registry uploads.
6. When publication is requested, use `crates-io.yml` and `npm.yml` from the same
   tag, initially with dry-run enabled. After checks pass, dispatch the live
   workflows within the user's authorization and inspect their actual results.
   Preserve the workflow names and environments (`crates-io` and `npm`): trusted
   publisher registrations depend on them. For local npm publishing or first
   publication, follow the linked npm release guide. Rust uploads use
   `.github/scripts/publish-crates.sh publish` with a token supplied through
   `CARGO_REGISTRY_TOKEN`; never put credentials in repository files.
7. Verify every public package's exact version in both registries before
   reporting completion. Uploads across registries are not atomic. On a partial
   failure, report what remains and retry the same version using the scripts'
   already-published skipping behavior. Honor a rate limit's retry time; stop
   repeated auth or validation failures for correction. Preserve verified npm
   tarballs when retrying. Start a new version only after the coordinated release
   is complete; recover published mistakes with a new patch release.
