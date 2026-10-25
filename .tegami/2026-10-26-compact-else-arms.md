---
packages:
  "@ryuugg/stanza": patch
---

### A braceless if/else that fits the formatter width counts as compact

A braceless `if` with an `else`, printed by Biome or oxfmt as `if (ready) send();` over `else close();`, now counts as compact when every arm fits the formatter width. In a body of two or three compact statements, `short-body` keeps it tight with no blank lines, so the formatter's layout no longer gets a `block-spacing` finding that `--fix` cannot settle.
