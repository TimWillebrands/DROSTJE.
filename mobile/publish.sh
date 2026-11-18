#!/usr/bin/env bash
set -euo pipefail
release_id=$(gh api --paginate "repos/$GITHUB_REPOSITORY/releases" \
    --jq '.[] | select(.tag_name == env.GITHUB_REF_NAME) | .id')
if [[ -n "$release_id" ]]; then
    gh release upload "$GITHUB_REF_NAME" dist/*.apk dist/SHA256SUMS --repo "$GITHUB_REPOSITORY"
    exit 0
fi
flags=(--repo "$GITHUB_REPOSITORY" --verify-tag --generate-notes --title "Android $VERSION_NAME")
if [[ "$PRERELEASE" == true ]]; then
    flags+=(--prerelease --latest=false)
fi
gh release create "$GITHUB_REF_NAME" dist/*.apk dist/SHA256SUMS "${flags[@]}"
