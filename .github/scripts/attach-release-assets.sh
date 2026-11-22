#!/usr/bin/env bash
# Publish the `v<version>` GitHub release with the editor app's release files,
# its notes taken from that version's CHANGELOG.md section.
#
#   attach-release-assets.sh <dir>
#
# Needs `gh` authenticated (GH_TOKEN) and GITHUB_REPOSITORY. Releases here are
# immutable: once published, nothing can be attached or replaced. `gh release
# create` with files keeps the release a draft until every upload lands, then
# publishes it, so an interrupted run leaves a draft that a rerun finishes.
set -euo pipefail

dir=${1:?usage: attach-release-assets.sh <dir>}
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY names the repository}"
check="$(dirname "$0")/../../scripts/release/check.py"
tag="v$(python3 "$check")"
files=("$dir"/*)

if ! draft=$(gh release view "$tag" --json isDraft --jq .isDraft 2>/dev/null); then
  notes=$(mktemp)
  python3 "$check" --notes > "$notes"
  gh release create "$tag" "${files[@]}" --verify-tag --title "Catchlight $tag" --notes-file "$notes"
  exit 0
fi

existing=$(gh release view "$tag" --json assets --jq '.assets[].name')
missing=()
for file in "${files[@]}"; do
  grep -qxF -- "$(basename "$file")" <<<"$existing" || missing+=("$file")
done
if [[ $draft == false ]]; then
  if (( ${#missing[@]} )); then
    echo "$tag is published without ${missing[*]##*/}; it is immutable, so release a new patch version" >&2
    exit 1
  fi
  echo "$tag is already published with every file"
  exit 0
fi
# A draft left by an interrupted run: finish its uploads, then publish it.
if (( ${#missing[@]} )); then
  gh release upload "$tag" "${missing[@]}"
fi
gh release edit "$tag" --draft=false
