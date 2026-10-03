---
packages:
  "@ryuugg/stanza": patch
---

### Faster checks and fixes across many files from npm

Installs from npm or bunx now use the same shared buffer parsing as the release binaries once a run has parsed about half a megabyte of source. Output is unchanged, and `STANZA_RAW_TRANSFER=0` still turns it off.
