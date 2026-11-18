---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A lone do while above an else loses its braces

`--fix` removes the braces around a single `do ... while` that sits above an `else`, even when the loop body is an `if` without braces. Before, it kept them and `explain` said the `else` would attach to that inner `if`, though the `while` closes it.
