#!/bin/sh
set -eu

fail() {
  echo "smoke: $1" >&2
  exit 1
}

if [ "$#" -ne 1 ]; then
  echo "usage: scripts/smoke.sh <stanza executable>" >&2
  exit 2
fi

stanza=$(CDPATH='' cd "$(dirname "$1")" && pwd)/$(basename "$1")
root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
before=$root/tests/fixtures/guard-join/guards.before.ts
after=$root/tests/fixtures/guard-join/guards.after.ts

if [ -n "${VERSION:-}" ]; then
  reported=$("$stanza" --version | sed -n 1p) || fail "--version failed"
  case "$reported" in
    "stanza $VERSION" | "stanza $VERSION "*) ;;
    *) fail "--version printed '$reported', expected stanza $VERSION" ;;
  esac
fi

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cp "$before" "$scratch/sample.ts"
cd "$scratch"

status=0
"$stanza" --check sample.ts >/dev/null || status=$?
[ "$status" -eq 1 ] || fail "--check on the unformatted fixture exited $status, expected 1"

"$stanza" --fix sample.ts >/dev/null || fail "--fix failed"
cmp -s sample.ts "$after" || fail "--fix output differs from $after"
"$stanza" --check sample.ts >/dev/null || fail "--check after --fix still reports findings"

"$stanza" --fix --stdin sample.ts <"$before" >stdout || fail "--fix --stdin failed"
cmp -s stdout "$after" || fail "--fix --stdin output differs from $after"

echo "smoke: ok $stanza"
