#!/usr/bin/env bash

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "Usage: $0 <release-ref> <main-ref>" >&2
  exit 2
fi

release_ref=$1
main_ref=$2

if [[ $(git rev-parse --is-shallow-repository 2>/dev/null) == "true" ]]; then
  echo "::error::Cannot verify release ancestry from a shallow checkout. Fetch the complete main history (actions/checkout fetch-depth: 0) before publishing." >&2
  exit 2
fi

if ! release_commit=$(git rev-parse --verify "${release_ref}^{commit}" 2>/dev/null); then
  echo "::error::Release ref '${release_ref}' does not resolve to a commit." >&2
  exit 2
fi

if ! main_commit=$(git rev-parse --verify "${main_ref}^{commit}" 2>/dev/null); then
  echo "::error::Main ref '${main_ref}' does not resolve to a commit. Fetch origin/main before publishing." >&2
  exit 2
fi

set +e
git merge-base --is-ancestor "$release_commit" "$main_commit"
status=$?
set -e

case $status in
  0)
    echo "Verified release commit ${release_commit} is contained in ${main_ref} (${main_commit})."
    ;;
  1)
    echo "::error::Refusing to publish release commit ${release_commit}: it is not contained in ${main_ref} (${main_commit}). Merge the release commit into main before creating and pushing the tag." >&2
    exit 1
    ;;
  *)
    echo "::error::Git could not verify whether release commit ${release_commit} is contained in ${main_ref} (${main_commit}); merge-base exited with status ${status}." >&2
    exit "$status"
    ;;
esac
