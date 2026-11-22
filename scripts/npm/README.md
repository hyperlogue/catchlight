# npm releases

Release `@catchlight/core`, `@catchlight/react`, and `@catchlight/editor-ui` in that
order, then the editor app (below). Core bundles the release WASM runtime
under `@catchlight/core/wasm`, with
explicit initialization. The generated workspace `@catchlight/wasm` package and
demo remain private; they are not additional npm dependencies.

From the Nix development shell, on a clean checkout:

```sh
python3 scripts/release/check.py
cargo xtask build-wasm --release
bun install --frozen-lockfile
bun run typecheck
bun test
bun run npm:pack
bun run npm:verify
bun run npm:publish
```

`npm:pack` rebuilds WASM in release mode and creates an explicit-initialization
web loader alongside its binary in core. It also creates JavaScript,
declarations, the editor stylesheet and logo,
READMEs, and full license/notice files under ignored `dist/npm/`. Workspace
exports continue to use source files for development. Only the staged manifests
point to emitted files; internal workspace peers become versioned npm ranges.
Source-package publishing is blocked by `prepublishOnly` to prevent accidentally
shipping the development manifests. Do not bypass it.

`npm:verify` installs the tarballs in an isolated temporary consumer and checks
Node ESM imports, TypeScript NodeNext declarations, assets, exports and notices.
It also bundles a production browser consumer and verifies that importing the
WASM subpath does not fetch its binary until initialization, then loads the
emitted asset and creates an editor session. Chromium is required (provided by
the Linux Nix shell, or set `CHROMIUM`). It needs registry access for React and
its types. `npm:publish` defaults to an npm
publish dry run. Both dry and live publication skip exact versions already on
npm, fail on lookup errors, and check archive integrity before proceeding.

## The editor app

`@catchlight/editor` is the downloadable editor, not a library: a launcher that
runs the `catchlight-editor` binary from one of five platform packages,
`@catchlight/editor-{linux-x64,linux-arm64,darwin-arm64,darwin-x64,win32-x64}`,
each restricted by `os`/`cpu`. The binaries come from `editor-app.yml`, which
`npm.yml` calls; `scripts/npm/editor-app.mjs <binaries-dir> [--verify]` packs
them into `dist/npm-app/`. `publish.mjs --with-app` publishes the platform
packages and then the launcher after the libraries. Until version 0.2.0 the
name held the component library now published as `@catchlight/editor-ui`.

To check the packaging locally with this machine's binary:

```sh
cargo xtask bundle-ui && cargo build --release -p catchlight-editor
mkdir -p /tmp/bins/x86_64-unknown-linux-gnu
cp target/release/catchlight-editor /tmp/bins/x86_64-unknown-linux-gnu/
node scripts/npm/editor-app.mjs /tmp/bins --verify
```

## First publication

Ensure you have publish rights to the `@catchlight` npm scope (create or join
the organization first). Authenticate locally with `npm login` and, after the
checks above pass, run:

```sh
node scripts/npm/publish.mjs --publish
```

Complete npm's authentication/2FA prompts as required by your account. Published
versions cannot be overwritten; re-running resumes a partial release. Do not
rebuild or edit the tarballs between verification and publishing.

## Subsequent GitHub releases

Both publishing jobs use the GitHub environment `release`. Set it up in the
repository's **Settings → Environments → New environment**. Under deployment
branches and tags, choose **Selected branches and tags**, add a **Tag** rule
`v*`, and save. Optionally add required reviewers; if you are the only releaser,
leave **Prevent self-review** unchecked so you can approve your own runs.
No environment secrets or variables are needed for publishing.
See [GitHub environment setup](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments).

Configure GitHub Actions as a trusted publisher on every public package:

| Field | crates.io | npm |
| --- | --- | --- |
| Repository owner | `hyperlogue` | `hyperlogue` |
| Repository name | `catchlight` | `catchlight` |
| Workflow filename | `crates-io.yml` | `npm.yml` |
| Environment | `release` | `release` |

On crates.io, open each crate's **Settings → Trusted Publishing → Add**, select
GitHub, enter the fields above, and save. Register all ten: `catchlight`,
`catchlight-core`, `catchlight-wgpu`, `catchlight-bevy`, `catchlight-cli`,
`catchlight-editor-core`, `catchlight-editor-protocol`, `catchlight-editor-server`,
`catchlight-editor-cli`, and `catchlight-editor`. A crate must already exist
and you must be an owner.
See [crates.io trusted publishing](https://crates.io/docs/trusted-publishing).

On npm, open **Settings → Trusted publishing** for each of `@catchlight/core`,
`@catchlight/react`, `@catchlight/editor-ui`, `@catchlight/editor`, and the five
`@catchlight/editor-<platform>` packages. A package must exist before it can
be registered, so publish each new name once locally (see First publication)
before relying on the workflow. Select GitHub Actions, enter the
fields above, and enable direct `npm publish` in allowed actions. Save each
registration. Workflow filenames are filenames only, without `.github/workflows/`.
See [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/).

If you previously registered the `npm` or `crates-io` environment, replace that
registration with `release`. Existing environment secrets are not needed by
these workflows. Push the updated workflow commit before creating a release tag
so both the default branch and the tagged commit contain the new configuration.

Use the shared version in `Cargo.toml` for all public Rust and npm packages.
Update internal Rust dependency versions, `Cargo.lock`, and `bun.lock` together.
`python3 scripts/release/check.py` rejects version drift before packaging or
publishing. Run CI, tag the validated commit `v<version>` and push the tag:
that starts **Publish npm packages** and **Publish Rust crates**. Run either by
hand for a dry run (the default) or to resume a partial release from the tag.
The verification job builds and tests tarballs without OIDC permission, and the
`app` job builds the editor binaries on every platform. A live run attaches the
binary archives, install scripts and `SHA256SUMS` to the `v<version>` GitHub
release. After environment approval, a fresh runner publishes those exact
tarballs with OIDC.

Registry uploads are not atomic: resume a partial release at the same version
before starting another release.

Dry runs validate packages but skip the publishing jobs, so they do not test
OIDC authentication or environment approvals. The next authorized live release
must succeed on both registries to verify the entire setup. If the scripts skip
an already-published version, that is not proof of npm publishing access.
