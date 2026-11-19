---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 `--changed --hunks` skips the diff of excluded files

`--changed --hunks` no longer diffs a changed file that `linguist-generated`, `-stanza` or `stanza=false` leaves out, so a large generated file costs a hunk run what it costs a run without `--hunks`. A 12,000 line generated file that changed throughout went from about 138 to 37 million instructions. What Stanza reports and fixes stays the same.
