#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
verify_script="$script_dir/verify-release-on-main.sh"
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT

fail() {
  echo "release ancestry test failed: $*" >&2
  exit 1
}

expect_failure() {
  local repository=$1
  local release_ref=$2
  local expected_status=$3
  local expected_message=$4
  local output
  local status

  set +e
  output=$(cd "$repository" && "$verify_script" "$release_ref" refs/remotes/origin/main 2>&1)
  status=$?
  set -e

  [[ $status -eq $expected_status ]] ||
    fail "expected status $expected_status, got $status: $output"
  [[ $output == *"$expected_message"* ]] ||
    fail "expected message '$expected_message', got: $output"
}

origin="$test_root/origin.git"
source_repository="$test_root/source"
git init --bare --quiet "$origin"
git init --quiet --initial-branch=main "$source_repository"
git -C "$source_repository" config user.name "StreamPainter CI"
git -C "$source_repository" config user.email "ci@example.invalid"

printf 'base\n' >"$source_repository/history.txt"
git -C "$source_repository" add history.txt
git -C "$source_repository" commit --quiet -m "base"

printf 'main release\n' >>"$source_repository/history.txt"
git -C "$source_repository" commit --quiet -am "main release"
main_release_commit=$(git -C "$source_repository" rev-parse HEAD)
git -C "$source_repository" tag -a v-main -m "main release" "$main_release_commit"
git -C "$source_repository" remote add origin "$origin"
git -C "$source_repository" push --quiet origin main refs/tags/v-main

git -C "$source_repository" switch --quiet -c feature HEAD~1
printf 'feature release\n' >>"$source_repository/history.txt"
git -C "$source_repository" commit --quiet -am "feature release"
feature_release_commit=$(git -C "$source_repository" rev-parse HEAD)
git -C "$source_repository" tag -a v-feature -m "feature release" "$feature_release_commit"
git -C "$source_repository" push --quiet origin feature refs/tags/v-feature

main_clone="$test_root/main-clone"
git clone --quiet --no-tags --depth=1 --branch main "file://$origin" "$main_clone"
git -C "$main_clone" fetch --quiet --depth=1 origin refs/tags/v-main:refs/tags/v-main
[[ $(git -C "$main_clone" rev-parse --is-shallow-repository) == "true" ]] ||
  fail "main test clone should start shallow"
expect_failure "$main_clone" refs/tags/v-main 2 "Cannot verify release ancestry from a shallow checkout"
git -C "$main_clone" fetch --quiet --no-tags --unshallow origin \
  +refs/heads/main:refs/remotes/origin/main
(
  cd "$main_clone"
  "$verify_script" refs/tags/v-main refs/remotes/origin/main
)

feature_clone="$test_root/feature-clone"
git clone --quiet --no-tags --depth=1 --branch feature "file://$origin" "$feature_clone"
git -C "$feature_clone" fetch --quiet --depth=1 origin \
  refs/tags/v-feature:refs/tags/v-feature
[[ $(git -C "$feature_clone" rev-parse --is-shallow-repository) == "true" ]] ||
  fail "feature test clone should start shallow"
git -C "$feature_clone" fetch --quiet --no-tags --unshallow origin \
  +refs/heads/main:refs/remotes/origin/main
expect_failure "$feature_clone" refs/tags/v-feature 1 "Refusing to publish release commit"

echo "release ancestry verification tests passed"
