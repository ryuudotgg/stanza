---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Submodules with names outside UTF-8 are reported under any locale

A directory run or `--changed` now names a skipped submodule whose `.gitmodules` name holds bytes outside UTF-8, whatever the locale. Before, under a UTF-8 locale such a submodule went unnamed, and a directory run holding only it said git ignores its files.
