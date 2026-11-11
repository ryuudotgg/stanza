---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Output stanza cannot write exits 2

When stanza could not write its findings or fixed text, on a full disk for example, the run exited 1, which reads as "findings remain" and hid a parse error's 2. It now exits 2, or 1 for `stanza hook`, after the same `stanza: cannot write output` line. A warning or notice that cannot be written no longer changes the exit code, so a clean run whose stderr is full still exits 0.
