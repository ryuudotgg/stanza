---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Configs sharing an extends chain each get their own brace answer

Two ESLint configs that extend the same chain of files now get the same brace answer whether they are checked together or alone. Before, a config that reached the 32 file limit or an `extends` cycle left that cut behind for every later config through the same files, so a config that reads `curly` as off alone kept its braces and was named in the unread-config notice once another config had read the chain first. Now only the config whose own chain is cut short reads as unknown.
