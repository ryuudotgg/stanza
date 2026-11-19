---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Configs that share an import chain read the same in any order

When two lint or Prettier configs import the same chain of files, each one now gets the answer it gets when read alone. Before, a config that reached past the 32 file import limit could make a second config that enters the chain lower down read as unknown too, so `--check` kept braces and named both configs in the notice even though the second one turns `curly` off.
