---
packages:
  "@ryuugg/stanza": patch
---

### Fewer git processes during repository discovery

The CLI and hooks find plain repository roots in process and reuse each directory lookup within an invocation. Worktrees, submodules and uncertain configurations still use git.
