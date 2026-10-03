---
packages:
  "@ryuugg/stanza": patch
---

### Faster checks on large repos

`--check` and `--fix` walk each file's syntax tree with less work per node, so a whole repo run finishes sooner. Results are unchanged.
