#!/bin/sh
set -eu

fail() {
  echo "smoke: $1" >&2
  exit 1
}

without_bun=0
if [ "${1:-}" = "--without-bun" ]; then
  without_bun=1
  shift
fi

if [ "$#" -ne 1 ]; then
  echo "usage: scripts/smoke.sh [--without-bun] <stanza executable>" >&2
  exit 2
fi

stanza=$(CDPATH='' cd "$(dirname "$1")" && pwd)/$(basename "$1")

if [ "$without_bun" -eq 1 ]; then
  node=$(command -v node) || fail "node is required for --without-bun"
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  mkdir "$scratch/path"
  ln -s "$node" "$scratch/path/node"

  message='stanza: needs Bun 1.4 or later (https://bun.sh) or a release binary (https://github.com/ryuudotgg/stanza/releases/latest)'
  printf '%s\n' "$message" >"$scratch/expected"

  status=0
  env -i PATH="$scratch/path" "$stanza" --version >"$scratch/stdout" 2>"$scratch/stderr" || status=$?
  [ "$status" -eq 2 ] || fail "--version without bun exited $status, expected 2"
  [ ! -s "$scratch/stdout" ] || fail "--version without bun printed to stdout"
  cmp -s "$scratch/stderr" "$scratch/expected" || fail "--version without bun printed unexpected stderr"

  status=0
  env -i PATH="$scratch/path" "$stanza" hook >"$scratch/stdout" 2>"$scratch/stderr" || status=$?
  [ "$status" -eq 1 ] || fail "hook without bun exited $status, expected 1"
  [ ! -s "$scratch/stdout" ] || fail "hook without bun printed to stdout"
  cmp -s "$scratch/stderr" "$scratch/expected" || fail "hook without bun printed unexpected stderr"

  echo "smoke: ok without bun $stanza"
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
