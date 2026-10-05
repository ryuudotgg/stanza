---
packages:
  "@ryuugg/stanza": patch
---

### Codex SubagentStop checks the subagent's own writes

A Codex SubagentStop hook now reads the subagent rollout at `agent_transcript_path` and checks the files that subagent patched, as Stop does for a Codex session. Before, it read only Claude Code transcripts, so a Codex subagent's files went unchecked.
