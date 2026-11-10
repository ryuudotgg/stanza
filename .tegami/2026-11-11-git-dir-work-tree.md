---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Git hooks in linked worktrees respect the root attributes

When `GIT_DIR` is set, as git sets it for hooks and aliases in a linked worktree, stanza now uses the work tree git itself would use. A `--fix` run from a pre-commit hook there leaves files the root `.gitattributes` marks `linguist-generated` or `-stanza` alone, skips files the root `.gitignore` ignores, and a relative `GIT_DIR` no longer fails on a path in a subdirectory. Before, each file's own directory was taken as the work tree.
