stanza_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if command -v bun >/dev/null 2>&1 && [ -d "$stanza_root/node_modules/oxc-parser" ]; then
  stanza=(bun run "$stanza_root/src/cli.ts")
elif [ -x "$stanza_root/bin/stanza" ]; then
  stanza=("$stanza_root/bin/stanza")
else
  echo "$launcher: neither $stanza_root/bin/stanza nor bun with installed dependencies is available; run 'bun install' or 'bun run build' in $stanza_root" >&2
  exit 1
fi
