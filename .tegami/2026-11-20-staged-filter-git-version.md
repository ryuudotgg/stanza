---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Staged conversions name the git they need

When a staged file's `filter` or other conversion attributes differ between the index and the working tree and git refuses `--attr-source`, `--check --staged` now exits 2 saying that the option is unsupported and that git 2.40 or later is needed. Before, it printed only `git cat-file failed: unknown option`.
