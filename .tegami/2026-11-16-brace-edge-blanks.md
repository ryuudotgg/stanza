---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Removing braces takes the blank lines inside them too

When `--fix` removes a block's braces, blank lines that sat just inside them are removed with them. Before, a blank line could be left between the header and its body, between the body and `else` or `while`, or below the statement as a new gap.
