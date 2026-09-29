#!/usr/bin/env bash
set -euo pipefail

fail() {
  echo "release: $1" >&2
  exit 1
}

hash_of() {
  awk -v name="$2" '$2 == name { print $1; exit }' <<<"$1"
}

if [ "$#" -ne 1 ] || [ -z "${GITHUB_REF_NAME:-}" ]; then
  echo "usage: GITHUB_REF_NAME=<tag> scripts/release.sh <dist dir>" >&2
  exit 2
fi

tag=$GITHUB_REF_NAME
dist=$1
probe=$(mktemp)
trap 'rm -f "$probe"' EXIT

status=0
release=$(gh release view "$tag" --json isDraft,assets --jq '.isDraft, .assets[].name' 2>"$probe") || status=$?

if [ "$status" -ne 0 ]; then
  # gh has no distinct exit code for a missing release, only this message
  if ! grep -q "release not found" "$probe"; then
    cat "$probe" >&2
    fail "could not read the $tag release"
  fi

  flags=(--verify-tag --generate-notes)
  # piping git into awk's early exit can SIGPIPE git and trip pipefail
  tags=$(git tag --list 'v*' --sort=-v:refname)
  previous=$(awk -v tag="$tag" '/-/ { next } found { print; exit } $0 == tag { found = 1 }' <<<"$tags")

  if [[ "$tag" == *-* ]]; then
    flags+=(--prerelease)
  elif [ -n "$previous" ]; then
    flags+=(--notes-start-tag "$previous")
  fi

  gh release create "$tag" "$dist"/* "${flags[@]}"
  exit 0
fi

[ "$(head -n 1 <<<"$release")" = false ] || fail "the $tag release is still a draft, publish or delete it and rerun"
assets=$(tail -n +2 <<<"$release")
[ -f "$dist/SHA256SUMS" ] || fail "$dist/SHA256SUMS does not exist"
built=$(cat "$dist/SHA256SUMS")

missing=0
while read -r name; do
  if ! grep -Fqx -- "$name" <<<"$assets"; then
    echo "release: $name is missing from the existing $tag release" >&2
    missing=1
  fi
done < <(echo SHA256SUMS && awk 'NF { print $2 }' <<<"$built")

[ "$missing" -eq 0 ] || exit 1
published=$(gh release download "$tag" --pattern SHA256SUMS --output -)

differs=0
while read -r hash name; do
  [ "$(hash_of "$published" "$name")" != "$hash" ] || continue
  echo "release: $name differs from the existing $tag release" >&2
  differs=1
done <<<"$built"

while read -r _ name; do
  [ -n "$name" ] && [ -z "$(hash_of "$built" "$name")" ] || continue
  echo "release: $name differs from the existing $tag release" >&2
  differs=1
done <<<"$published"

[ "$differs" -eq 0 ] || exit 1
echo "release: the existing $tag release matches $dist"
