---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 The Stop hook tells the agent what to fix, file by file

When findings stop an agent's turn, the block reason now opens with `These need a hand fix in the files you changed:`, then lists the findings under each file, in byte order, as `line N: message (rule)`, so a failure reads like any finding, as in `line 1: Unexpected token (parse)`. It names every file the pass rewrote, not only those that still have findings, so the agent reads them again before editing, and closes by pointing to `stanza explain <file>:<line>`. The rule summaries and the column numbers are gone, since each message already says what to change. A path holding a control character, a line or paragraph separator, `"` or `\` prints quoted, as in a finding row. The JSON fields and exit codes are unchanged.
