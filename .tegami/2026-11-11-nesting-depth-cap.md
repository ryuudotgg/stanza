---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Deeply nested files are reported instead of crashing stanza

A file nested hundreds of thousands of levels deep, such as `[[[…]]]`, nested `if` blocks or a long run of `!`, used to abort the run with a stack overflow and exit 134, or take minutes. Stanza now reports such a file with a `parse` finding, `nests more than 10000 levels deep, left untouched`, before parsing it, and formats every other file in the run. The cap is lower when stanza runs on a smaller stack. The deepest file across five real repositories counts 43 levels. A lint or formatter config nested that deep is treated as one that does not parse, so the run goes on.
