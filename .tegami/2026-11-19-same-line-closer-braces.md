---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces come off a body that a closing brace follows on the same line

A single statement body with no trailing semicolon, such as the `{ b() }` in `function f() { if (a) { b() } }`, used to keep its braces when the `}` that ends the statement followed on the same line. Bodies nested that way came off one level per internal pass, so a deep nest took time that grew with the square of its depth, and `--fix --changed --hunks` could need a second run. Every level now comes off in one pass, since no code can continue a statement past a `}`, and a check of 1,600 bodies nested on one line takes under a hundredth of the instructions it did.
