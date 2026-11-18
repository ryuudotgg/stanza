---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 `--hunks` keeps up with rewritten header runs and whole-file rewrites

`--changed --hunks` and `--staged --hunks` check a long run of headers ending in `)`, `else` or `do` that an edit rewrote onto one line with far less work. 1,600 of them used to cost over a thousand times the instructions of the same check without `--hunks`, and now cost barely more, whether the edit changed the headers or the new line repeats them. A file whose every line changed now takes less than half the instructions it did, and one that shrinks or grows by thousands of lines about a quarter. Which lines count as touched stays the same.
