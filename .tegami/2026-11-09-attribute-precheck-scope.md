---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Hook events skip git check-attr for unrelated .gitattributes

A repository that tracks a `.gitattributes` no longer makes every `stanza hook` event, `--changed` run and directory run spawn `git check-attr`. Stanza now asks git only when a `.gitattributes` on the file's path names `linguist-generated` or `stanza`, or when git would read one from the index because the worktree copy is missing or marked skip-worktree. Which files are selected is unchanged.
