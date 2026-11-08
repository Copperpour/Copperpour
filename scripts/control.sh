#!/usr/bin/env bash
# One negative control: break one guard, count the tests that fail, restore.
#
# usage: scripts/control.sh "<label>" <command that edits one guard>
#   CONTROL_TESTS=the_log_is_read_incrementally scripts/control.sh "fixed log tail" \
#     perl -pi -e 's/Some\(offset\) if offset <= len/Some(offset) if false \&\& offset <= len/' \
#     crates/copperpour-core/src/autorouting/freerouting/mcp.rs
#
# Commit or stash your work first: it refuses a tree with uncommitted changes
# to tracked files, applies the command, and restores every tracked file with
# `git checkout` on any exit, so a guard left broken never survives it.
#
# It tests the edited crates and every workspace crate that depends on them,
# since no other crate can see the edit, with --no-fail-fast so one failing
# crate does not hide the rest. An edit outside crates/ tests the workspace.
#
# CONTROL_TESTS="name1 name2" runs only tests whose names contain one of these
# (libtest filters), such as the PR's new tests. If the filter fails nothing,
# the whole set runs once, so a 0 never comes from the filter alone.
#
# CONTROL_TIMEOUT (seconds, default 900) cuts off a control that makes a test
# hang, and lists the hung tests.
#
# Output: "== <label>: N failed", the packages and filter used, then the names
# of the failing tests. A control that fails nothing means no test pins that
# guard.
set -u
[ $# -ge 2 ] || { sed -n '2,7p' "$0" >&2; exit 2; }
label=$1
shift
limit=${CONTROL_TIMEOUT:-900}

cd "$(git rev-parse --show-toplevel)" || exit 2
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "control.sh: uncommitted changes to tracked files; commit or stash them first" >&2
  exit 2
fi
target=${CARGO_TARGET_DIR:-$(cargo metadata --format-version 1 --no-deps --locked |
  jq -r .target_directory)}

# Restore tracked files and stop this target dir's test binaries on any exit.
cleanup() {
  pkill -KILL -f "^$target/[^ ]*/deps/" 2>/dev/null
  git checkout -q -- .
}
trap cleanup EXIT
trap 'exit 130' INT TERM

"$@" || { echo "control.sh: the edit command failed" >&2; exit 2; }
if git diff --quiet; then
  echo "control.sh: the edit command changed nothing" >&2
  exit 2
fi

pkgs=()
meta=$(cargo metadata --format-version 1 --no-deps --locked 2>/dev/null)
root=$(pwd)
edited=$(git diff --name-only | sed -n 's#^\(crates/[^/]*\)/.*#\1#p' | sort -u)
if [ -n "$meta" ] && [ -n "$edited" ] && ! git diff --name-only | grep -qv '^crates/'; then
  set=$(for d in $edited; do
    jq -r --arg m "$root/$d/Cargo.toml" '.packages[] | select(.manifest_path==$m) | .name' <<<"$meta"
  done | sort -u)
  # Add reverse dependencies until nothing new appears.
  while :; do
    next=$(jq -r --argjson s "$(jq -R . <<<"$set" | jq -s .)" \
      '.packages[] | select(any(.dependencies[]; .path != null and (.name as $n | $s | index($n)))) | .name' \
      <<<"$meta" | cat - <(echo "$set") | sort -u)
    [ "$next" = "$set" ] && break
    set=$next
  done
  for p in $set; do pkgs+=(-p "$p"); done
fi
[ ${#pkgs[@]} -eq 0 ] && pkgs=(--workspace)

run() {
  out=$(CARGO_TARGET_DIR=$target timeout --kill-after=10 "$limit" \
    cargo test --locked "${pkgs[@]}" --lib --tests --no-fail-fast -- "$@" 2>&1)
  status=$?
  failed=$( { echo "$out" | sed -n 's/^    \([A-Za-z0-9_:]*\)$/\1/p'
              echo "$out" | sed -n 's/^test \([A-Za-z0-9_:]*\) \.\.\. FAILED$/\1/p'; } | sort -u)
}
scope="all tests"
if [ -n "${CONTROL_TESTS:-}" ]; then
  # shellcheck disable=SC2086
  run $CONTROL_TESTS
  scope="filter: $CONTROL_TESTS"
  if [ -z "$failed" ] && [ $status -ne 124 ] && [ $status -ne 137 ]; then
    run
    scope="filter failed nothing; all tests"
  fi
else
  run
fi
hung=$(echo "$out" | sed -n 's/^test \([A-Za-z0-9_:]*\) has been running for over.*/\1/p' | sort -u)
n=$(printf '%s' "$failed" | grep -c .)
line="== $label: $n failed"
if [ $status -eq 124 ] || [ $status -eq 137 ]; then
  line="$line, TIMED OUT after ${limit}s with $(printf '%s' "$hung" | grep -c .) hung"
fi
echo "$line  [${pkgs[*]}; $scope]"
[ -n "$failed" ] && echo "$failed"
if [ -n "$hung" ] && { [ $status -eq 124 ] || [ $status -eq 137 ]; }; then
  echo "$hung" | sed 's/^/    hung: /'
fi
# A compile error fails every test: show it, so it is not read as a count.
echo "$out" | grep -E '^error(\[|:)' | grep -v 'test failed\|targets\? failed' | head -3
