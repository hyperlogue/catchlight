#!/usr/bin/env bash
# Attach the editor app's release files to the `v<version>` GitHub release,
# creating the release first if needed.
#
#   attach-release-assets.sh <dir>
#
# Needs `gh` authenticated (GH_TOKEN) and GITHUB_REPOSITORY. Resumes a partial
# upload without replacing assets already attached: a rebuilt binary would
# differ from the one npm already published.
set -euo pipefail

dir=${1:?usage: attach-release-assets.sh <dir>}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY names the repository}
tag="v$(python3 "$(dirname "$0")/../../scripts/release/check.py")"

if ! gh release view "$tag" >/dev/null 2>&1; then
  gh release create "$tag" --verify-tag --title "Catchlight $tag" \
    --notes "See [CHANGELOG.md](https://github.com/$repo/blob/$tag/CHANGELOG.md)."
fi
existing=$(gh release view "$tag" --json assets --jq '.assets[].name')
for file in "$dir"/*; do
  name=$(basename "$file")
  if grep -qxF -- "$name" <<<"$existing"; then
    echo "Already attached: $name"
  else
    gh release upload "$tag" "$file"
  fi
done
