---
packages:
  "@ryuugg/stanza": patch
---

### SubagentStop checks only the subagent's own writes

`stanza hook` now uses `agent_transcript_path` for SubagentStop without checking the main session or other agents. Missing, unreadable and empty agent transcripts leave repository files untouched and exit silently. Stop keeps its whole-session scope.
