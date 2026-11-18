---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A braceless body on its own line counts as one line

A braceless `if`, `for` or `while` whose body sits on the next line now gets the same `after-multiline`, `block-spacing` and `wall` findings as its one-line twin when the joined line fits the formatter width, so running your formatter no longer changes what Stanza reports. An `if` with `else` whose arms each fit on their own line counts the same way. Before, these asked for blank lines around them and broke a wall that the joined form did not. A comment between the header and the body, or a body too wide to join, still makes it multi-line.
