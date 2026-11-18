---
packages:
  "@ryuugg/stanza": minor
---

### ✨ A terminal shows aligned, colored findings

When stdout is a terminal, `--check` and `--fix` over paths, `--changed` or `--staged` line up each file's findings in columns, with a blank line between files and before the summary. The location prints bold, the rule dim and `(by hand)` yellow. When stderr is a terminal, the `stanza:` prefix prints bold red on an error and bold yellow on a notice, and the summary's numbers print bold. `NO_COLOR` and `TERM=dumb` turn color off, and `FORCE_COLOR` colors piped output in the one finding per line form, though never the stdout of `--json`, `--stdin`, `stanza explain`, `--help` or the hook. Under a coding agent that sets `CLAUDECODE`, `AI_AGENT`, `CODEX_SANDBOX`, `CODEX_THREAD_ID`, `CURSOR_AGENT` or `GEMINI_CLI`, the output keeps one finding per line with no color, as it does in a pipe.
