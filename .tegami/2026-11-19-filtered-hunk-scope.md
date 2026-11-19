---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 `--changed --hunks` checks files with a clean filter

A file whose `filter` attribute runs a clean filter now counts as changed throughout under `--changed --hunks`, as it already did under `--staged --hunks`. Before, git reported the edited lines in the filtered text, so an edit could be missed or the wrong lines checked whenever the filter added or removed lines, and the Stop hook let it through.
