---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Stop checks files the agent committed

`stanza hook` on Stop and SubagentStop now checks every file the agent wrote this session, whether or not it differs from HEAD, so committing a file before the turn ends no longer hides a finding it still has. Before, a written file with no change against HEAD was skipped. With `--hunks`, a written file with no change against HEAD is still left alone, and one changed since keeps its changed-line scope. The pass after an Edit or Codex patch follows the same rule, so it also fixes an edited file whose content matches HEAD. A Stop without a transcript Stanza can read still takes only changed files.
