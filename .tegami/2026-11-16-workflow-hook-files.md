---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Stop checks files Workflow agents wrote

`stanza hook` on Claude Code Stop now reads every transcript below the session's `subagents` folder, so files a Workflow agent wrote under `subagents/workflows/` are fixed and their remaining findings block, as a Task subagent's do. Before, only the transcripts directly in `subagents` were read. A directory symlink in that folder is not followed.
