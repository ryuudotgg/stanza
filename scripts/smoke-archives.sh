#!/bin/sh
set -eu

fail() {
  echo "smoke-archives: $1" >&2
  exit 1
}

if [ "$#" -ne 2 ]; then
  echo "usage: scripts/smoke-archives.sh <dist> <platform>" >&2
  exit 2
fi

root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd)
dist=$(CDPATH='' cd "$1" && pwd) || fail "cannot open $1"
platform=$2
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' 0

checksum() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

cd "$dist"
checksum -c SHA256SUMS || fail "checksum verification failed"

selected=
while read -r digest name; do
  case "$name" in
    *.tar.xz) ;;
    *) continue ;;
  esac

  stage=$(mktemp -d "$scratch/archive.XXXXXX")
  tar -xJf "$name" -C "$stage" || fail "cannot extract $name"
  entries=$(find "$stage" ! -path "$stage" | wc -l)
  [ "$entries" -eq 1 ] && [ -f "$stage/stanza" ] && [ ! -L "$stage/stanza" ] && [ -x "$stage/stanza" ] || fail "$name must contain only an executable stanza file"

  raw=${name%.tar.xz}
  expected=$(awk -v name="$raw" '$2 == name { print $1 }' SHA256SUMS)
  actual=$(checksum "$stage/stanza")
  [ -n "$expected" ] && [ "${actual%% *}" = "$expected" ] || fail "$name differs from $raw"

  if [ "$raw" = "stanza-$platform" ]; then
    selected=$stage/stanza
  fi
done <SHA256SUMS

[ -n "$selected" ] || fail "missing archive for $platform"
if [ "$(uname -s)" = Darwin ]; then
  codesign -v "$selected" || fail "signature verification failed for $platform"
fi

"$root/scripts/smoke.sh" "$selected"
echo "smoke-archives: ok $platform"
