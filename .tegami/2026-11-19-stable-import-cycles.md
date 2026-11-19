---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Configs that import back into another config read the same in any order

When a shared file imports from a lint config that also extends it, only that config now reads as unknown. A second config that imports or extends the same shared file gets the answer it gets when read alone. Before, whichever config was read first decided both, so `--check` could keep braces for the second config and name it in the notice even though it turns `curly` off.
