---
packages:
  "@ryuugg/stanza": patch
---

### 🎨 Large repositories read each config once

A run that formats files on several threads now reads each ESLint, oxlint, Biome, Prettier and oxfmt config once and shares it, instead of once per thread. A repository whose `eslint.config.js` imports a 600 KB module checks in about 166 MB with four or eight threads, where it took 790 MB and 1.1 GB, and spends the CPU time of a single threaded run. Every file still gets the brace setting and line width it did before.
