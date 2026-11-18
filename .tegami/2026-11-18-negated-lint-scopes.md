---
packages:
  "@ryuugg/stanza": patch
---

### 🐛 Negated override patterns keep braces

A `!` pattern in an oxlint or ESLint override's `files`, in oxlint `excludeFiles` or in legacy ESLint `excludedFiles` now leaves Stanza unsure which files the override covers, so braces stay. `--fix` keeps braces where such an override turns `curly` on, and such an override can no longer turn `curly` off. Before, an override with `"files": ["!excluded/**"]` reached no file, so its `curly` setting was ignored.
