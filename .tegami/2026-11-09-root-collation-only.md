---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 The binary is about 950 KB smaller

The `stanza` binary now carries only the root collation data it sorts paths and findings with, so each platform package downloads less. Sorting is unchanged.
