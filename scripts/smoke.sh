#!/bin/sh
set -eu

fail() {
  echo "smoke: $1" >&2
  exit 1
}

mode=
case "${1:-}" in
  --without-bun | --without-node)
    mode=$1
    shift
    ;;
esac

if [ "$#" -ne 1 ]; then
  echo "usage: scripts/smoke.sh [--without-bun | --without-node] <stanza executable>" >&2
  exit 2
fi

stanza=$(CDPATH='' cd "$(dirname "$1")" && pwd)/$(basename "$1")

if [ -n "$mode" ]; then
  absent=${mode#--without-}
  case "$absent" in
    bun) runtime=node ;;
    node) runtime=bun ;;
  esac

  path=$(mktemp -d)
  trap 'rm -rf "$path"' EXIT

  for tool in "$runtime" basename cmp cp dirname git mktemp readlink rm uname; do
    found=$(command -v "$tool") || fail "$tool is required for $mode"
    ln -s "$found" "$path/$tool"
  done

  PATH=$path "$0" "$stanza"
  echo "smoke: ok without $absent $stanza"
  exit 0
fi

root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
before=$root/tests/fixtures/guard-join/guards.before.ts
after=$root/tests/fixtures/guard-join/guards.after.ts

if [ -n "${VERSION:-}" ]; then
  expected="stanza $VERSION${COMMIT:+ ($COMMIT)}"
  reported=$("$stanza" --version) || fail "--version failed"
  [ "$reported" = "$expected" ] || fail "--version printed '$reported', expected '$expected'"
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
