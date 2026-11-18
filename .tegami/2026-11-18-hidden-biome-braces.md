---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Braces stay under a hidden Biome config

`.biome.json` and `.biome.jsonc` now count as Biome lint config, both in the directory walk and through `extends: "//"`, so `--fix` keeps braces that `useBlockStatements` enforces there. Before, Stanza ignored both names and removed those braces.
