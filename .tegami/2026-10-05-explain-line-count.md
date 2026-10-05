---
packages:
  "@ryuugg/stanza": patch
---

### Explain counts lines like an editor

`stanza explain` treats a final newline as the end of the last line rather than the start of another, so a line beyond the end of the file now gets the right count, such as `a.ts has 2 lines, not 3` for a two line file ending in a newline, and an empty file has 0 lines.
