---
packages:
  "@ryuugg/stanza": patch
---

### `--fix` parses each rewritten file once fewer

`--fix` no longer parses a rewritten file a second time when Stanza already parsed that exact text, so fixing a large repo is faster. Fixed text that does not parse is still left untouched.
