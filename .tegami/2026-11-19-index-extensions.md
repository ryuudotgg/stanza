---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Untracked cache and resolved merges no longer add git processes

A repository with `core.untrackedCache` turned on, or one that has resolved a merge conflict, keeps extra data in its git index. Stanza used to stop reading the index when it found that data, so a hook event for an edited tracked file also started `git ls-files --others` and `git check-attr`, and `--check` on one file started `git check-attr`. Stanza now skips any index data git marks as optional, and these runs start the same git processes as in any other repository. What is selected, reported and fixed is unchanged.
