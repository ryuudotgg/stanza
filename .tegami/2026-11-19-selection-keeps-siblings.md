---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A path that vanishes during selection no longer stops the run

When a folder or file passed to `stanza --check` or `--fix` disappears or cannot be resolved after git has listed it, Stanza prints a `cannot read` line for that path and still checks every other path. Before, the whole run stopped with `file selection failed` and checked nothing.
