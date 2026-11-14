---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Unreadable folders are reported under `--changed` too

`--changed` now prints `stanza: cannot read <path>` and exits 2 for a changed file under a folder stanza cannot search and for an untracked folder git cannot open, as a path run does, where it used to skip them in silence. A file deleted from the working tree is still skipped without a word.

### 🐛 Path runs report every unreadable folder git names

A path run now reports an unreadable folder whose name holds U+2028 or U+2029, which it used to drop. For a folder whose name git masks, such as `c?r`, it prints git's own reason instead of `ENOENT`.
