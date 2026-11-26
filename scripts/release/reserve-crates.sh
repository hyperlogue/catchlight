#!/usr/bin/env bash
set -euo pipefail

# crates.io accepts a trusted publisher only for a crate that already exists,
# and a new workspace crate cannot be uploaded ahead of the release that
# publishes its dependencies. This uploads an empty 0.0.0 under each name so
# the release workflow can publish the real version.
mode=${1:-}
case "$mode" in
  dry-run) flags=(--dry-run) ;;
  publish) flags=() ;;
  *) echo "Usage: $0 {dry-run|publish} <crate>..." >&2; exit 2 ;;
esac
shift
(( $# )) || { echo "Usage: $0 {dry-run|publish} <crate>..." >&2; exit 2; }

cd "$(dirname "$0")/../.."
mapfile -t package < <(python3 -c '
import tomllib
package = tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]
print(package["edition"], package["license"], package["repository"], sep="\n")')
repository=${package[2]}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

reserved=()
for crate in "$@"; do
  if [[ ! -f "crates/$crate/Cargo.toml" ]]; then
    echo "Not a workspace crate: $crate" >&2; exit 1
  fi
  status=$(curl --silent --show-error --retry 3 --max-time 30 \
    --output /dev/null --write-out '%{http_code}' --user-agent catchlight-release \
    "https://crates.io/api/v1/crates/$crate")
  case "$status" in
    200) echo "Already on crates.io: $crate"; continue ;;
    404) ;;
    *) echo "Cannot check $crate (HTTP $status)" >&2; exit 1 ;;
  esac

  mkdir -p "$work/$crate/src"
  cat > "$work/$crate/Cargo.toml" <<EOF
[package]
name = "$crate"
version = "0.0.0"
edition = "${package[0]}"
license = "${package[1]}"
repository = "$repository"
description = "Reserves the name for Catchlight; the first release follows"

[workspace]
EOF
  : > "$work/$crate/src/lib.rs"
  cargo publish --registry crates-io "${flags[@]}" --manifest-path "$work/$crate/Cargo.toml"
  reserved+=("$crate")
done

if [[ $mode == publish && ${#reserved[@]} -gt 0 ]]; then
  echo
  echo "Add a trusted publisher to each (repository ${repository#https://github.com/}," \
    "workflow release.yml, environment release):"
  for crate in "${reserved[@]}"; do
    echo "  https://crates.io/crates/$crate/settings"
  done
fi
