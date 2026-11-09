---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 `--hunks` keeps up with long changed lines

`--hunks` now sorts each changed line into touched and untouched code in time that grows with the line's length, where a long line such as minified code used to take time growing with its square. Unchanged lines in a changed file are compared as written instead of being copied and hashed, so on a repository with 20 edited files the work `--hunks` adds on top of `--changed` drops by about three quarters. Which lines count as touched stays the same.
