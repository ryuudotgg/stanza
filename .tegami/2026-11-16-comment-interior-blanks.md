---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A blank line inside a block comment keeps the comment with its statement

A block comment with an empty line inside it now moves with the statement below it, so `--fix` puts the blank line it adds above the comment, as it does for any other leading comment. Before, the empty line inside the comment counted as a blank line between the comment and the statement, so the gap above was left alone and never reported. Only a blank line between two leading comments, or between the last one and its statement, sets the statement apart.
