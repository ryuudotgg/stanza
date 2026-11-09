---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 EditorConfig sections with many stars no longer stall a run

A section header such as `[*a*a*a*a*a*a*a*ab]` in `.editorconfig` used to hold up every run and hook call in the repository for seconds to minutes. Matching a section now takes at most time proportional to the pattern length multiplied by the path length, and every section matches the same files as before.
