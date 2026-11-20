---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A package.json that is not valid JSON sets no width

`--fix` now skips a `package.json` that is not valid JSON when looking for a Prettier width, as Prettier does, so the width comes from the next Prettier config in that directory or above, or the default 80. Before, a manifest with a trailing comma, a comment, single quotes or a leading byte order mark still set the width from its `prettier` key, and it hid a `.prettierrc.json` beside it.
