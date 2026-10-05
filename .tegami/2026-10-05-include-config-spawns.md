---
packages:
  "@ryuugg/stanza": patch
---

### Fewer Git processes when a Git config uses include or includeIf

Finding the repository root no longer starts an extra Git process when a Git config file uses `include` or `includeIf`, so a check in such a setup starts as many Git processes as one without them. A repository whose path ends in a space also works now, where `--check` used to exit 2 with "cannot change to" and the path without its trailing space.
