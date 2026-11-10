---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Stanza skips a FIFO, socket or device named as a path

A FIFO, socket or device file with a TypeScript or JavaScript extension, named to `--check`, `--fix` or `stanza explain` directly or through a symlink, used to stop the run on a read that never returned. Stanza now prints `stanza: not a regular file: <path>`, exits 2 and goes on with the other paths. Directories, `--changed` and `--staged` pick files as before.
