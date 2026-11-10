---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Biome extends chains and shared Prettier options no longer stall a run

A `biome.json` whose `extends` chain names the same config twice at each level, or a JavaScript Prettier config whose override options share one object twice at each level, used to hold up every run and hook call in the repository for seconds to minutes. Each Biome config in a chain is now merged once per run and each shared options object is scanned once, so the time grows with the size of the config, and every file still gets the line width it did before.
