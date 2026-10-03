#!/bin/sh
set -eu

fail() {
  echo "archive: $1" >&2
  exit 1
}

if [ "$#" -ne 1 ]; then
  echo "usage: scripts/archive.sh <dist>" >&2
  exit 2
fi

dist=$(CDPATH='' cd "$1" && pwd) || fail "cannot open $1"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' 0
LC_ALL=C
export LC_ALL

if tar --version 2>/dev/null | grep -q bsdtar; then
  owner="--uid 0 --gid 0 --numeric-owner"
else
  owner="--owner=0 --group=0 --numeric-owner"
fi

pids=
for file in "$dist"/stanza-*; do
  [ -f "$file" ] || continue
  case "$file" in
    *.tar.xz) continue ;;
  esac

  own=$(mktemp -d "$stage/archive.XXXXXX")
  cp "$file" "$own/stanza"
  chmod 755 "$own/stanza"
  TZ=UTC touch -t 198001010000 "$own/stanza"
  COPYFILE_DISABLE=1 tar --format ustar $owner -cJf "$file.tar.xz" -C "$own" stanza &
  pids="$pids $!:$(basename "$file")"
done

failed=
for job in $pids; do
  wait "${job%%:*}" || failed="$failed ${job#*:}"
done

[ -z "$failed" ] || fail "cannot archive$failed"

cd "$dist"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum stanza-* >"$stage/checksums" || fail "cannot checksum assets"
else
  shasum -a 256 stanza-* >"$stage/checksums" || fail "cannot checksum assets"
fi

sort -k 2 "$stage/checksums" >SHA256SUMS

echo "archive: ok $dist"
