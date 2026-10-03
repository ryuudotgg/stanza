---
packages:
  "@ryuugg/stanza": patch
---

### Faster checks and fixes across many files

Once a run has parsed about half a megabyte of source, Stanza reads the parser's syntax tree from a shared buffer instead of decoding JSON, in the release binaries and in installs from npm or bunx. Output is unchanged, and short runs such as the agent hooks stay on JSON. A file the faster path cannot handle, such as a very deep expression or one over a million characters, is parsed the old way on its own. `STANZA_RAW_TRANSFER=0` turns the faster path off.
