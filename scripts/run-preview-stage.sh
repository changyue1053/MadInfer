#!/usr/bin/env bash
# Fixed stage labels only. Raw compiler/installer/test output is local, never uploaded.
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-}" in
  host-tools|host-fetch|host-contracts|apple-prerequisites|package|device-consumer) stage=$1 ;;
  *) printf 'FAIL: invalid preview stage\n' >&2; exit 1 ;;
esac
shift
mkdir -p build/ios/logs
if "$@" >"build/ios/logs/$stage.log" 2>&1; then
  printf 'PASS: %s\n' "$stage"
else
  printf 'FAIL: %s\n' "$stage" >&2
  exit 1
fi
