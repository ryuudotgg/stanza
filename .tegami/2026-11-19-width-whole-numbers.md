---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Explain prints every line width in whole digits

`stanza explain` prints a configured width such as `printWidth: 1e21` as `1000000000000000000000` columns rather than `1e+21`, and its note about a config it cannot read now says `cannot read the line width` in place of `could not read the line width`.
