---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 One unresolvable changed file no longer stops --changed

When `stanza --check --changed` or `--fix --changed` cannot resolve the real path of one changed file, as when it is swapped for a broken symlink mid run, it now prints `cannot read <file>: <reason>`, checks every other changed file and exits 2. Before, the whole run stopped with `file selection failed` and checked nothing.
