---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A nested Biome config without formatter settings uses Biome's default width

A `biome.json` with `"root": false` that sets no formatter options now gives its files Biome's default width of 80, as Biome does, when a Biome config above it formats. The parent's `lineWidth` reaches them only through `"extends": "//"`. Before, `--fix` used the parent's width, so under a parent at 60 it kept a guard split that Biome would keep on one line.
