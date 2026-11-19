---
packages:
  "@ryuugg/stanza": minor
---

### ✨ Explain takes a finding's location as printed

`stanza explain` now accepts the `path:line:col` a finding row prints, so `stanza explain src/orders.ts:6:3` explains the same line as `stanza explain src/orders.ts:6`; the column changes nothing. Paths holding a newline or a line separator are accepted too, and the header quotes such a name as a finding row does. A line past the end of the file is named as it was typed, however many digits it has.
