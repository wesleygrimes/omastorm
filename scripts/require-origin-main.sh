#!/usr/bin/env bash
# Require HEAD to be the full commit currently at origin's main.
# Used by mise release and mise engine-tag. Fetches that SHA, not the
# moving branch, so a tag or laptop release names a commit everyone has.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { printf '%s\n' "$@" >&2; exit 1; }

[[ $# -eq 1 ]] || die 'Usage: bash scripts/require-origin-main.sh <release|tag>'
purpose=$1
case $purpose in
  release|tag) ;;
  *) die 'Usage: bash scripts/require-origin-main.sh <release|tag>' ;;
esac
command -v git > /dev/null 2>&1 || die 'Need git on PATH.'

head=$(git rev-parse HEAD)
[[ $head =~ ^[0-9a-f]{40}$ ]] \
  || die "HEAD is not a full 40-character commit: $head"
git fetch -q origin "$head" \
  || die "origin does not have $head; push first so the $purpose names a commit everyone has."
remote=$(git ls-remote origin refs/heads/main | awk '{print $1}')
[[ $remote =~ ^[0-9a-f]{40}$ ]] \
  || die 'Could not resolve origin refs/heads/main to a full 40-character commit.'
[[ $head == "$remote" ]] \
  || die "HEAD is not the commit at origin/main; push or pull first so the $purpose names a commit everyone has."
