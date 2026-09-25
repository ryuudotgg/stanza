#!/bin/bash
[ "${AGENT_HOOKS:-1}" = "0" ] && exit 0

input=$(cat)
printf '%s' "$input" | grep -q '"stop_hook_active": *true' && exit 0

root="$(cd "$(dirname "$0")" && pwd)"
cwd=$(printf '%s' "$input" | bun -e 'const input = await Bun.stdin.text(); process.stdout.write(JSON.parse(input).cwd ?? "")' 2>/dev/null)
[ -n "$cwd" ] || cwd=$PWD
git -C "$cwd" rev-parse --show-toplevel >/dev/null 2>&1 || exit 0

stanza=("$root/bin/stanza")
[ -x "${stanza[0]}" ] || stanza=(bun run "$root/src/cli.ts")

(cd "$cwd" && "${stanza[@]}" --fix --changed ${STANZA_FLAGS-} >/dev/null 2>&1)

findings=$(cd "$cwd" && "${stanza[@]}" --check --changed ${STANZA_FLAGS-} 2>&1)
status=$?
[ "$status" -eq 0 ] && exit 0
[ "$status" -ne 1 ] && findings="stanza could not check the changed files (exit $status):
$findings"

shown=$(printf '%s\n' "$findings" | head -12 | sed 's/^/  /')
count=$(printf '%s\n' "$findings" | wc -l | tr -d ' ')
more=""
[ "$count" -gt 12 ] && more=$(printf '\n  ... and %d more' $((count - 12)))

reason="Style findings stanza could not fix in the files you changed (blank lines between the steps of a body, braces):
${shown}${more}
Rules: one blank line between steps, none inside a step; a blank line after every multi-line statement; a single-line declaration joins the if that guards it or the return that consumes it; no braces around a single statement body. Fix them, then reply again."

printf '%s' "$reason" | bun -e 'const reason = await Bun.stdin.text(); console.log(JSON.stringify({ decision: "block", reason }))'
