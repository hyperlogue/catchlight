#!/usr/bin/env bash
set -euo pipefail

# Keep an explicit release boundary even if another workspace member becomes
# publishable. Cargo orders this set by dependency and verifies its archives
# together, including dependencies that have not reached crates.io yet.
args=(
  --registry crates-io
  --locked
  -p catchlight-core
  -p catchlight-wgpu
  -p catchlight-bevy
  -p catchlight-cli
  -p catchlight-editor-core
  -p catchlight-editor-protocol
  -p catchlight-editor-server
  -p catchlight-editor-cli
)

case "${1:-}" in
  dry-run) args+=(--dry-run) ;;
  publish) : "${CARGO_REGISTRY_TOKEN:?Set CRATES_IO_TOKEN in GitHub Actions secrets}" ;;
  *) echo "Usage: $0 {dry-run|publish}" >&2; exit 2 ;;
esac

cargo publish "${args[@]}"
