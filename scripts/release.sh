#!/usr/bin/env bash
set -euo pipefail

fail() {
  echo "release: $1" >&2
  exit 1
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
release=$(gh release view "$tag" --json isDraft,assets --jq '.isDraft, (.assets[] | "\(.digest // "" | ltrimstr("sha256:"))  \(.name)")' 2>"$probe") || status=$?

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
published=$(tail -n +2 <<<"$release" | sort)
built=$( (cd "$dist" && shasum -a 256 -- *) | sort)
differing=$(comm -3 <(echo "$built") <(echo "$published") | awk '{ print $2 }' | sort -u)

for name in $differing; do
  echo "release: $name does not match the existing $tag release" >&2
done

[ -z "$differing" ] || exit 1
echo "release: the existing $tag release matches $dist"
