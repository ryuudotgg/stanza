---
packages:
  "@ryuugg/stanza": patch
---

### Skip unnecessary Git attribute checks

Path checks and fixes avoid a Git attribute subprocess when no reachable source can mark a file generated. Uncertain configuration and index attributes still use Git.
