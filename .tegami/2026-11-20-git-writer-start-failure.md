---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A git call that cannot start its input thread names git

When the system refuses the thread that feeds a git command its input, Stanza now reports the git command that failed, such as `stanza: git check-attr failed: resource temporarily unavailable`, waits for git to exit and exits 2. Before, the run printed `stanza: internal error: failed to spawn thread` with the raw error.
