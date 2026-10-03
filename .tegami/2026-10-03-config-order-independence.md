---
packages:
  "@ryuugg/stanza": patch
---

### Brace answers no longer depend on which directories ran first

A lint config too large to read in time is reported as unread whether or not another directory in the same run already read a shared config it extends. Before, running `--check` or `--fix` on several directories at once could settle braces for such a config that a run on its directory alone left unread.
