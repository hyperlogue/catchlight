#!/usr/bin/env bash
set -euo pipefail

# On retry only the versions still absent are published. Run `cargo xtask
# bundle-ui` first; this flag turns a missing page into a failure.
export CATCHLIGHT_EDITOR_REQUIRE_UI=1
crates=(
  catchlight
  catchlight-core catchlight-extension-animations catchlight-extension-humanoid catchlight-wgpu catchlight-bevy catchlight-cli
  catchlight-editor-core catchlight-editor-protocol catchlight-editor-server catchlight-editor-cli
  catchlight-editor
)
case "${1:-}" in
  dry-run) flags=(--dry-run) ;;
  publish)
    : "${CARGO_REGISTRY_TOKEN:?Expected a crates.io trusted publishing token}"
    flags=(--no-verify)
    ;;
  *) echo "Usage: $0 {dry-run|publish}" >&2; exit 2 ;;
esac

# The page is gitignored but packaged, which Cargo counts as uncommitted; check
# the checkout here so --allow-dirty admits only that page.
if [[ -n "$(git status --porcelain)" ]]; then
  echo "Commit or stash changes before publishing:" >&2; git status --short >&2; exit 1
fi
flags+=(--allow-dirty)

version=$(python3 scripts/release/check.py)
packages=()
for crate in "${crates[@]}"; do
  status=$(curl --silent --show-error --retry 3 --max-time 30 \
    --output /dev/null --write-out '%{http_code}' --user-agent catchlight-release \
    "https://crates.io/api/v1/crates/$crate/$version")
  case "$status" in
    200) echo "Already published: $crate $version" ;;
    404) packages+=(-p "$crate") ;;
    *) echo "Cannot check $crate $version (HTTP $status)" >&2; exit 1 ;;
  esac
done
if (( ${#packages[@]} )); then
  cargo publish --registry crates-io --locked "${flags[@]}" "${packages[@]}"
fi
