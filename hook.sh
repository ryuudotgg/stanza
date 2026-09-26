#!/bin/bash
[ "${AGENT_HOOKS:-1}" = "0" ] && exit 0

root="$(cd "$(dirname "$0")" && pwd)"
if command -v bun >/dev/null 2>&1 && [ -d "$root/node_modules/oxc-parser" ]; then
  stanza=(bun run "$root/src/cli.ts")
elif [ -x "$root/bin/stanza" ]; then
  stanza=("$root/bin/stanza")
else
  echo "stanza Stop hook: neither $root/bin/stanza nor bun with installed dependencies is available; run 'bun install' or 'bun run build' in $root" >&2
  exit 1
fi

"${stanza[@]}" hook ${STANZA_FLAGS-}
status=$?
[ "$status" -eq 2 ] && exit 1
exit "$status"
