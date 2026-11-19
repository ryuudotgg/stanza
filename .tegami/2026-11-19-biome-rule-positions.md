---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Misplaced Biome brace settings no longer decide braces

Stanza reads Biome's `useBlockStatements` only from `linter.rules.style`, and its group and root switches only under `linter.rules`, at the top of `biome.json` or inside an override. A brace setting anywhere else, such as a top-level `style` block, a bare `useBlockStatements` key or a `rules` block outside `linter`, used to turn braces on or off silently even though Biome rejects that config. Such a config is now unknown: braces stay and the unread-config notice names it.
