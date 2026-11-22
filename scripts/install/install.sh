#!/bin/sh
# Install catchlight-editor from the GitHub releases:
#
#   curl -fsSL https://github.com/hyperlogue/catchlight/releases/latest/download/install.sh | sh
#
# CATCHLIGHT_VERSION   a release tag such as v0.2.0 (default: the latest)
# CATCHLIGHT_INSTALL_DIR  where the binary goes (default: ~/.local/bin)
# CATCHLIGHT_DOWNLOAD_URL a mirror holding the release assets (overrides both)
#
# The archive is checked against the release's SHA256SUMS before anything is
# installed. Everything runs from `main` at the last line, so a download cut
# short runs nothing.
set -eu

main() {
  repo=hyperlogue/catchlight
  case "$(uname -s)-$(uname -m)" in
    Linux-*)
      # The Linux builds link glibc.
      if ldd --version 2>&1 | grep -qi musl; then
        echo "no prebuilt catchlight-editor for musl; try: cargo install catchlight-editor" >&2
        exit 1
      fi
      ;;
  esac
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    *)
      echo "no prebuilt catchlight-editor for $(uname -s) $(uname -m); try: cargo install catchlight-editor" >&2
      exit 1
      ;;
  esac

  version=${CATCHLIGHT_VERSION:-latest}
  if [ -n "${CATCHLIGHT_DOWNLOAD_URL:-}" ]; then
    base=$CATCHLIGHT_DOWNLOAD_URL
  elif [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
  else
    base="https://github.com/$repo/releases/download/$version"
  fi
  dir=${CATCHLIGHT_INSTALL_DIR:-$HOME/.local/bin}
  asset=catchlight-editor-$target.tar.gz

  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT INT TERM
  fetch() {
    if command -v curl >/dev/null 2>&1; then
      curl --proto '=https,file' -fsSL "$1" -o "$2"
    else
      case "$1" in
        https://*) wget -qO "$2" "$1" ;;
        *) echo "refusing to download $1 over anything but https" >&2; exit 1 ;;
      esac
    fi
  }
  fetch "$base/$asset" "$tmp/$asset"
  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"

  expected=$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1 }' "$tmp/SHA256SUMS")
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
  else
    actual=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
  fi
  if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    echo "checksum mismatch for $asset; nothing was installed" >&2
    exit 1
  fi

  tar -xzf "$tmp/$asset" -C "$tmp"
  mkdir -p "$dir"
  cp "$tmp/catchlight-editor-$target/catchlight-editor" "$dir/catchlight-editor.new"
  chmod 755 "$dir/catchlight-editor.new"
  mv "$dir/catchlight-editor.new" "$dir/catchlight-editor"
  echo "installed $("$dir/catchlight-editor" --version) to $dir"
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "add $dir to your PATH to run catchlight-editor" ;;
  esac
}

main "$@"
