---
packages:
  "@ryuugg/stanza": patch
---

### 🚨 The hook registrations add SubagentStop

The Claude Code and Codex registrations in `stanza --help`, the README and the setup pages now add a SubagentStop entry beside PostToolUse and Stop, so each subagent's own files are fixed when it finishes instead of waiting for the end of the turn. A registration copied from an earlier release has no SubagentStop entry: add the one from `stanza --help`, and in Codex trust it again with `/hooks`, since Codex skips a new or edited hook until you do.

### ✨ `stanza --help` prints the Codex registration

`stanza --help` now prints the registration for `~/.codex/hooks.json` or `.codex/hooks.json` below the Claude Code one, so both can be copied without opening the docs.
