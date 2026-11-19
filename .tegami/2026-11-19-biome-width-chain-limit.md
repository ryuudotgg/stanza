---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Biome extends chains stop at 10 files for the width

`--fix` reads a Biome config's width through at most 10 extended files, as Biome does. A longer chain, which Biome refuses, used to give the width at its end and could join a guard; now Stanza prints a notice that it cannot read the line width from that config and takes the next source, without reading the rest of the chain.
