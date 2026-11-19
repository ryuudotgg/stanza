---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The hook still fixes absolute paths when its working directory is gone

When the `cwd` a hook event names no longer exists, `stanza hook` used to skip every file. It now fixes the files a successful PostToolUse call, a Stop transcript or a SubagentStop transcript names by absolute path, formats a PreToolUse Write to an absolute path, and prints those paths in full. Under a missing `cwd`, a relative tool path and a Stop without a transcript Stanza can read still change no file.
