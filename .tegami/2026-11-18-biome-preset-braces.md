---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces stay under a Biome `preset: "all"`

`--fix` keeps braces when `biome.json` sets `"preset": "all"` in `linter.rules` or in `linter.rules.style`, since that turns `useBlockStatements` on. Before, Stanza ignored the preset and removed them. Biome's order decides the answer: a `useBlockStatements` setting or a `style` severity beats the style preset, and the style preset beats the root one. As in Biome, `overrides` from an extended config apply after the config's own rules, so an inherited override that enforces braces keeps them, and an `extends` Stanza cannot read keeps braces with the notice.
