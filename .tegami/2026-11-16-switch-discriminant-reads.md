---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A switch reads its discriminant from outside

A `switch` whose value names a binding from the statement above now joins to it, even when one of its cases declares the same name with `let` or `const`. Before, the case declaration hid the discriminant's read, so `const a = 1;` stayed apart from `switch (a) { case 1: let a = 2; }` with no `consume-join` finding.
