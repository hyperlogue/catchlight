# npm releases

Release `@catchlight/core`, `@catchlight/react`, and `@catchlight/editor` in that
order. Core bundles the release WASM runtime under `@catchlight/core/wasm`, with
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

Configure each package's npm trusted publisher for owner `hyperlogue`, repository
`catchlight`, workflow `npm.yml`, environment `npm`, with direct `npm publish`
allowed. Create the GitHub `npm` environment with release-tag restrictions and
required reviewers. No stored npm publishing token is used by the workflow.
See [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/).

Use the shared version in `Cargo.toml` for all public Rust and npm packages.
Update internal Rust dependency versions, `Cargo.lock`, and `bun.lock` together.
`python3 scripts/release/check.py` rejects version drift before packaging or
publishing. Run CI, tag the validated commit `v<version>`, then manually dispatch **Publish npm
packages** from that tag. It defaults to dry-run; disable it for publication.
The verification job builds and tests tarballs without OIDC permission. After
environment approval, a fresh runner publishes those exact tarballs with OIDC.

Dispatch **Publish Rust crates** from the same tag for the matching crates.io
release. Registry uploads are not atomic: resume a partial release at the same
version before starting another release.
