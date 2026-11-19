---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Explain brace notes point where check does

The note `stanza explain` prints when a gap is decided after `--fix` removes braces now names each removed body by the line of its opening brace, the line `--check` reports, and lists each line once in ascending order. Before, an Allman body was named by its header line, nested bodies came inner first, and two bodies opening on one line repeated it, as in `lines 2 and 2`.
