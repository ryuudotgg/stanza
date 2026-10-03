---
packages:
  "@ryuugg/stanza": patch
---

### Faster checks and fixes across many files in the release binaries

Once a run has parsed about half a megabyte of source, the release binaries read the parser's syntax tree from a shared buffer instead of decoding JSON. Output is unchanged, and short runs such as the agent hooks stay on JSON. A file the faster path cannot handle, such as a very deep expression or one over a million characters, is parsed the old way on its own. Installs from npm or bunx keep using JSON, and `STANZA_RAW_TRANSFER=0` turns the faster path off.
