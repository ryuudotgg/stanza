---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Linked worktrees and submodules run hooks as fast as a main checkout

In a linked worktree or a submodule checkout, `stanza hook` and git backed selections now find the repository and rule out attributes from the files on disk, as they already did in a main checkout. A PostToolUse event there spawns three git processes instead of four, and PreToolUse Write no longer spawns `git check-attr` when no attribute source names `linguist-generated` or `stanza`. Which files are selected and what is printed are unchanged.
