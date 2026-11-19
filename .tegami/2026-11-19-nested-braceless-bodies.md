---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Deeply nested braceless bodies check faster

A braceless `if`, `for` or `while` nested under thousands of braceless headers, each on its own line, is now measured in time that grows with the depth instead of its square. A check of 8,000 such levels takes about half the instructions it did. Findings and fixes stay the same.
