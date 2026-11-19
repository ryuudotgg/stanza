---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Shared Prettier and Biome configs set the width

`--fix` takes the line width, tab width and overrides from a shared Prettier config that `package.json` or `.prettierrc` names by a string, and from a Biome config that `extends` a package export. It used to print a notice that it could not read the line width and fall back to the next source, so a guard the formatter keeps split could be joined.
