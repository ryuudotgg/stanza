---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 An empty walk outside git no longer blames git

When a folder outside any repository, or any folder with git missing from `PATH`, selects nothing because `.gitignore` rules hide its files, the notice now says `a .gitignore hides its files`. Before, it said `git ignores its files`, even when no repository was involved, as when a `.gitignore` in a parent folder listed every file.
