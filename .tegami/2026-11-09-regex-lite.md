---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 The binary is about 1.2 MB smaller

The `stanza` binary now matches generated file headers, ignore directives and git warnings with a smaller pattern engine, so each platform package downloads less, and a file with no header comment no longer prepares the header patterns at all. Which files count as generated, and every directive, behave as before.
