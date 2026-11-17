---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 A config built through a very long chain of constants no longer crashes a run

An ESLint, oxlint or Biome config that builds its value through more than 1,000 nested constants or object spreads, such as `const x2 = { ...x1 }` repeated a million times, used to abort `stanza --check` and `--fix` with a stack overflow. Stanza now stops reading such a value after 1,000 links, keeps braces and prints the notice that it cannot tell whether the config enforces them. Configs nested fewer than 1,000 links deep read as before.
