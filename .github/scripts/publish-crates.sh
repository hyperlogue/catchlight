#!/usr/bin/env bash
set -euo pipefail

# Cargo verifies unpublished workspace dependencies together, then uploads in
# dependency order. On retry only the versions still absent are selected.
crates=(
  catchlight
  catchlight-core catchlight-wgpu catchlight-bevy catchlight-cli
  catchlight-editor-core catchlight-editor-protocol catchlight-editor-server catchlight-editor-cli
)
case "${1:-}" in
  dry-run) flags=(--dry-run) ;;
  publish)
    : "${CARGO_REGISTRY_TOKEN:?Expected a crates.io trusted publishing token}"
    flags=(--no-verify)
    ;;
  *) echo "Usage: $0 {dry-run|publish}" >&2; exit 2 ;;
esac

version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')
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
