---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 The hook formats every file a Codex patch writes

After a Codex `apply_patch`, `stanza hook` now reads the patch's file headers the way Codex does, so it formats every file the patch adds, updates or moves to. A path holding U+2028 or U+2029 is formatted, where before the hook treated those characters as line breaks and skipped the file. A header with spaces before or after it is matched as Codex matches it, and an indented header inside an Update hunk stays a context line, as it is for Codex. A failed patch whose error message quotes Codex's success line between U+2028 characters no longer counts as applied.
