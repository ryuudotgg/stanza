---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Guards with emoji, combining marks or tabs join only when the formatter keeps them

`--fix` now measures a joined guard the way the governing formatter measures it, character by character and tab by tab, so it no longer joins a line the formatter then splits or keeps apart one the formatter would join. Before, Stanza used Bun's character widths and tab stops for every formatter, so `if (a) log("भारत");` joined at a width of 20 although Prettier, oxfmt and Biome all split it there. Prettier and oxfmt count an emoji sequence such as 👍🏽 as 2 where Biome adds up its parts, Prettier counts a tab after the indentation as nothing, and with no formatter Stanza measures as Prettier does.
