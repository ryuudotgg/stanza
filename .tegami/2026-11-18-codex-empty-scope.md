---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Codex Stop leaves hand edits alone when Codex patched nothing

`stanza hook` on Codex Stop now changes no file and prints nothing when the session rollout holds no completed patch. Before, it fixed and blocked on every changed file in the repository, including files you left dirty by hand. A rollout Stanza does not recognise still falls back to the changed files.
