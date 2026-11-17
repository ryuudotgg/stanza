---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces stay under a Biome `preset: "all"`

`--fix` keeps braces when `biome.json` sets `"preset": "all"` in `linter.rules` or in `linter.rules.style`, since that turns `useBlockStatements` on. Before, Stanza ignored the preset and removed them. Biome's order decides the answer: a `useBlockStatements` setting or a `style` severity beats the style preset, and the style preset beats the root one, across `extends` and `overrides` too.
